//! The GPU stage: our wgpu device (with the limits Vello needs), Vello rendering of a
//! `Frame` into the scene texture, the motion-blur pass, and headless readback.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use vello::peniko::{Color, Fill};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

use crate::blur::{BlurParams, BlurPass};
use crate::frame::{Cmd, Frame};

/// Which adapter to run on: hybrid laptops have two GPUs, plus a software one.
#[derive(Clone, Debug, Default)]
pub enum AdapterChoice {
    #[default]
    Auto,
    LowPower,
    HighPerformance,
    /// The first adapter whose name contains this text, ignoring case.
    Named(String),
}

impl AdapterChoice {
    pub fn parse(s: &str) -> Self {
        match s {
            "auto" => Self::Auto,
            "low" => Self::LowPower,
            "high" => Self::HighPerformance,
            name => Self::Named(name.to_owned()),
        }
    }
}

pub struct Gpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
}

impl Gpu {
    /// `allow_cpu`: accept a software adapter such as llvmpipe (Slint's own check never
    /// looks at the adapter we choose, so this is the only gate).
    pub fn new(choice: &AdapterChoice, allow_cpu: bool) -> Result<Self> {
        // No GL: Slint avoids it too ("rendering artifacts"); WGPU_BACKEND still overrides.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::from_env().unwrap_or(wgpu::Backends::PRIMARY),
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let adapter = match choice {
            // Honours WGPU_ADAPTER_NAME and WGPU_POWER_PREF.
            AdapterChoice::Auto => pollster::block_on(wgpu::util::initialize_adapter_from_env_or_default(&instance, None))
                .context("no wgpu adapter")?,
            AdapterChoice::Named(name) => {
                let needle = name.to_lowercase();
                pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
                    .into_iter()
                    .find(|a| a.get_info().name.to_lowercase().contains(&needle))
                    .with_context(|| format!("no adapter whose name contains {name:?}"))?
            }
            AdapterChoice::LowPower | AdapterChoice::HighPerformance => {
                let power_preference = if matches!(choice, AdapterChoice::LowPower) {
                    wgpu::PowerPreference::LowPower
                } else {
                    wgpu::PowerPreference::HighPerformance
                };
                let options = wgpu::RequestAdapterOptions { power_preference, ..Default::default() };
                pollster::block_on(instance.request_adapter(&options)).context("no wgpu adapter")?
            }
        };
        let info = adapter.get_info();
        if !allow_cpu && info.device_type == wgpu::DeviceType::Cpu {
            bail!("{} is a CPU adapter: use the vello_cpu path (or --allow-cpu-adapter)", info.name);
        }
        check_vello_support(&adapter)?;

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("sorte-spike"),
            // Timestamps (when the adapter has them) measure our GPU time per frame.
            required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
            // WebGPU defaults for Vello, with the adapter's texture size for big windows.
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&descriptor)).context("creating the wgpu device")?;
        // Debug builds abort at exit otherwise: Slint's thread-local context drops the
        // queue after wgpu-core's thread-local lock tracer, unless a submit touched it first.
        queue.submit([]);
        Ok(Self { instance, adapter, device, queue, info })
    }

    pub fn describe(&self) -> String {
        format!("{} ({:?}, {})", self.info.name, self.info.backend, self.info.driver)
    }
}

/// Vello needs compute shaders and the WebGPU default storage limits. Without them its
/// pipelines fail validation while `Renderer::new` still returns Ok, so check first.
fn check_vello_support(adapter: &wgpu::Adapter) -> Result<()> {
    let caps = adapter.get_downlevel_capabilities();
    if !caps.flags.contains(wgpu::DownlevelFlags::COMPUTE_SHADERS) {
        bail!("{}: no compute shaders", adapter.get_info().name);
    }
    if !wgpu::Limits::default().check_limits(&adapter.limits()) {
        bail!("{}: limits below the WebGPU defaults Vello needs", adapter.get_info().name);
    }
    Ok(())
}

/// The scene texture handed to Slint: Vello writes it as storage, Slint samples it.
const OUTPUT_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::STORAGE_BINDING
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::RENDER_ATTACHMENT)
    .union(wgpu::TextureUsages::COPY_SRC);
/// Intermediate layers of the blur path: Vello writes them, the blur pass samples them.
const LAYER_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::STORAGE_BINDING.union(wgpu::TextureUsages::TEXTURE_BINDING);

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Target {
    fn new(device: &wgpu::Device, (width, height): (u32, u32), usage: wgpu::TextureUsages, label: &str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

struct Cached {
    scene: Scene,
    last_used: u64,
}

/// GPU time of our own work per frame (the Vello renders and the blur pass), from
/// timestamps written by empty compute passes submitted just before and just after it.
/// Queue order puts Vello's own submissions in between. Results come back a few frames
/// later through asynchronous mapping: nothing ever blocks the frame loop.
struct GpuTimer {
    slots: Vec<TimerSlot>,
    /// Nanoseconds per timestamp tick.
    period_ns: f64,
    /// Completed measurements not collected yet, in milliseconds.
    done: Vec<f32>,
}

struct TimerSlot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// IDLE, IN_FLIGHT (mapping requested), READY (mapped) or FAILED.
    state: std::sync::Arc<std::sync::atomic::AtomicU8>,
}

const IDLE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const READY: u8 = 2;

impl GpuTimer {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let slots = (0..4)
            .map(|_| TimerSlot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("frame timer"),
                    ty: wgpu::QueryType::Timestamp,
                    count: 2,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("frame timer resolve"),
                    size: 16,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                readback: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("frame timer readback"),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                state: std::sync::Arc::new(std::sync::atomic::AtomicU8::new(IDLE)),
            })
            .collect();
        Some(Self { slots, period_ns: f64::from(queue.get_timestamp_period()), done: Vec::new() })
    }

    /// Writes the start timestamp into a free slot, if there is one.
    fn begin(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<usize> {
        use std::sync::atomic::Ordering;
        self.collect(device);
        let slot = self.slots.iter().position(|s| s.state.load(Ordering::Acquire) == IDLE)?;
        self.write(device, queue, slot, Some(0), None);
        Some(slot)
    }

    /// Writes the end timestamp, resolves both and starts mapping them.
    fn end(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, slot: usize) {
        use std::sync::atomic::Ordering;
        self.write(device, queue, slot, None, Some(1));
        let s = &self.slots[slot];
        s.state.store(IN_FLIGHT, Ordering::Release);
        let state = s.state.clone();
        s.readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            state.store(if r.is_ok() { READY } else { IDLE }, Ordering::Release);
        });
    }

    fn write(&self, device: &wgpu::Device, queue: &wgpu::Queue, slot: usize, start: Option<u32>, end: Option<u32>) {
        let s = &self.slots[slot];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame timer") });
        drop(encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("frame timer"),
            timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                query_set: &s.queries,
                beginning_of_pass_write_index: start,
                end_of_pass_write_index: end,
            }),
        }));
        if end.is_some() {
            encoder.resolve_query_set(&s.queries, 0..2, &s.resolve, 0);
            encoder.copy_buffer_to_buffer(&s.resolve, 0, &s.readback, 0, 16);
        }
        queue.submit([encoder.finish()]);
    }

    /// Reads the slots whose mapping completed, without waiting for the others.
    fn collect(&mut self, device: &wgpu::Device) {
        use std::sync::atomic::Ordering;
        let _ = device.poll(wgpu::PollType::Poll);
        for s in &self.slots {
            if s.state.load(Ordering::Acquire) != READY {
                continue;
            }
            if let Ok(data) = s.readback.slice(..).get_mapped_range() {
                let t0 = u64::from_le_bytes(data[0..8].try_into().expect("8 bytes"));
                let t1 = u64::from_le_bytes(data[8..16].try_into().expect("8 bytes"));
                // Timestamps can wrap.
                self.done.push((t1.wrapping_sub(t0) as f64 * self.period_ns / 1e6) as f32);
            }
            s.readback.unmap();
            s.state.store(IDLE, Ordering::Release);
        }
    }
}

/// What the last `render` cost on the CPU side.
#[derive(Clone, Copy, Debug, Default)]
pub struct StageTiming {
    /// Building the Vello scenes from the frame's display lists.
    pub encode_ms: f32,
    /// Handing the work to wgpu (Vello's own encoding and submission included).
    pub submit_ms: f32,
    pub blurred: bool,
    /// GPU time of our work in a recent frame (timestamps arrive a few frames late).
    pub gpu_ms: Option<f32>,
}

pub struct GpuStage {
    renderer: Renderer,
    timer: Option<GpuTimer>,
    blur: BlurPass,
    pictures: HashMap<u64, Cached>,
    scene: Scene,
    top: Scene,
    size: (u32, u32),
    outputs: Vec<Target>,
    layers: Option<(Target, Target)>,
    next: usize,
    frame_no: u64,
    pub timing: StageTiming,
}

impl GpuStage {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self> {
        let options = RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            // Parallel pipeline creation: single-threaded takes seconds on NVIDIA.
            num_init_threads: None,
            pipeline_cache: None,
        };
        let renderer = Renderer::new(device, options).map_err(|e| anyhow!("Vello renderer: {e}"))?;
        Ok(Self {
            renderer,
            timer: GpuTimer::new(device, queue),
            blur: BlurPass::new(device),
            pictures: HashMap::new(),
            scene: Scene::new(),
            top: Scene::new(),
            size: (0, 0),
            outputs: Vec::new(),
            layers: None,
            next: 0,
            frame_no: 0,
            timing: StageTiming::default(),
        })
    }

    fn ensure_size(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.size == size {
            return;
        }
        self.size = size;
        self.outputs = (0..2).map(|i| Target::new(device, size, OUTPUT_USAGE, &format!("scene {i}"))).collect();
        self.layers = None;
        self.next = 0;
    }

    /// Renders `frame` and returns the texture holding it. Two textures alternate, so
    /// the one Slint shows is never the one being written.
    pub fn render(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame, blur: bool) -> Result<wgpu::Texture> {
        let start = Instant::now();
        self.frame_no += 1;
        let size = (frame.width.max(1), frame.height.max(1));
        self.ensure_size(device, size);

        let blur_params = frame
            .motion_blur
            .filter(|_| blur)
            .map(|m| BlurParams {
                center: [m.center.x as f32, m.center.y as f32],
                angle: frame.blur_angle() as f32,
                radius: (m.radius + 2.0) as f32,
            })
            // Below half a pixel of travel at the rim the blur is invisible.
            .filter(|p| p.angle.abs() * p.radius >= 0.5);

        let render_params = |base_color: Color| RenderParams {
            base_color,
            width: size.0,
            height: size.1,
            antialiasing_method: AaConfig::Area,
        };

        let frame_no = self.frame_no;
        let out = self.next;
        match blur_params {
            Some(p) => {
                self.scene.reset();
                encode(&mut self.scene, frame.moving.cmds(), &mut self.pictures, frame_no);
                self.top.reset();
                encode(&mut self.top, frame.top.cmds(), &mut self.pictures, frame_no);
                let encoded = Instant::now();
                let (moving, top) = self.layers.get_or_insert_with(|| {
                    (Target::new(device, size, LAYER_USAGE, "moving"), Target::new(device, size, LAYER_USAGE, "top"))
                });
                let slot = self.timer.as_mut().and_then(|t| t.begin(device, queue));
                self.renderer
                    .render_to_texture(device, queue, &self.scene, &moving.view, &render_params(frame.background))
                    .map_err(|e| anyhow!("Vello: {e}"))?;
                self.renderer
                    .render_to_texture(device, queue, &self.top, &top.view, &render_params(Color::TRANSPARENT))
                    .map_err(|e| anyhow!("Vello: {e}"))?;
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("blur") });
                self.blur.encode(device, queue, &mut encoder, &moving.view, &top.view, &self.outputs[out].view, size, p);
                queue.submit([encoder.finish()]);
                if let (Some(timer), Some(slot)) = (self.timer.as_mut(), slot) {
                    timer.end(device, queue, slot);
                }
                self.timing = StageTiming {
                    encode_ms: ms(encoded - start),
                    submit_ms: ms(encoded.elapsed()),
                    blurred: true,
                    gpu_ms: self.timing.gpu_ms,
                };
            }
            None => {
                self.scene.reset();
                encode(&mut self.scene, frame.moving.cmds(), &mut self.pictures, frame_no);
                encode(&mut self.scene, frame.top.cmds(), &mut self.pictures, frame_no);
                let encoded = Instant::now();
                let slot = self.timer.as_mut().and_then(|t| t.begin(device, queue));
                self.renderer
                    .render_to_texture(device, queue, &self.scene, &self.outputs[out].view, &render_params(frame.background))
                    .map_err(|e| anyhow!("Vello: {e}"))?;
                if let (Some(timer), Some(slot)) = (self.timer.as_mut(), slot) {
                    timer.end(device, queue, slot);
                }
                self.timing = StageTiming {
                    encode_ms: ms(encoded - start),
                    submit_ms: ms(encoded.elapsed()),
                    blurred: false,
                    gpu_ms: self.timing.gpu_ms,
                };
            }
        }

        // Forget pictures nobody drew for a few seconds.
        self.pictures.retain(|_, c| frame_no - c.last_used < 600);
        self.next = (out + 1) % self.outputs.len();
        Ok(self.outputs[out].texture.clone())
    }

    /// GPU times (ms) of the frames measured since the last call, oldest first. They
    /// trail the frames by a few refreshes; empty without timestamp support.
    pub fn take_gpu_times(&mut self) -> Vec<f32> {
        let Some(timer) = self.timer.as_mut() else { return Vec::new() };
        let times = std::mem::take(&mut timer.done);
        if let Some(&last) = times.last() {
            self.timing.gpu_ms = Some(last);
        }
        times
    }
}

fn ms(d: std::time::Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

/// Replays display-list commands into a Vello scene. A picture is encoded once into
/// its own scene and appended under the command's transform afterwards: Vello's
/// append is a copy, while re-encoding would re-expand every colour emoji on the CPU.
fn encode(scene: &mut Scene, cmds: &[Cmd], pictures: &mut HashMap<u64, Cached>, frame_no: u64) {
    for cmd in cmds {
        match cmd {
            Cmd::Fill { path, transform, color } => {
                scene.fill(Fill::NonZero, *transform, *color, None, &**path);
            }
            Cmd::Stroke { path, transform, style, color } => {
                scene.stroke(style, *transform, *color, None, &**path);
            }
            Cmd::Glyphs { run, transform, color } => {
                scene
                    .draw_glyphs(&run.font)
                    .font_size(run.font_size)
                    .transform(*transform)
                    .normalized_coords(&run.normalized_coords)
                    .brush(*color)
                    .hint(false)
                    .draw(Fill::NonZero, run.glyphs.iter().map(|g| vello::Glyph { id: g.id, x: g.x, y: g.y }));
            }
            Cmd::Picture { picture, transform } => {
                let id = picture.id();
                if !pictures.contains_key(&id) {
                    let mut inner = Scene::new();
                    encode(&mut inner, picture.cmds(), pictures, frame_no);
                    pictures.insert(id, Cached { scene: inner, last_used: frame_no });
                }
                let cached = pictures.get_mut(&id).expect("just inserted");
                cached.last_used = frame_no;
                scene.append(&cached.scene, Some(*transform));
            }
        }
    }
}

/// Copies a texture back to the CPU as tightly packed RGBA8 (as stored: straight alpha).
pub fn read_rgba(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Result<Vec<u8>> {
    let (width, height) = (texture.width(), texture.height());
    let row = width * 4;
    let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded * height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |r| r.expect("mapping the readback buffer"));
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let mut out = Vec::with_capacity((row * height) as usize);
    {
        let data = slice.get_mapped_range()?;
        for line in data.chunks(padded as usize) {
            out.extend_from_slice(&line[..row as usize]);
        }
    }
    buffer.unmap();
    Ok(out)
}
