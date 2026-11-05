//! Rotational motion blur and the final composite, as one wgpu compute pass:
//! `out = top over blur(moving)`.
//!
//! Each pixel of the moving layer within `radius` of the centre averages samples taken
//! along the arc it sweeps during the exposure, about one pixel apart (at most 64), in
//! linear light. The sharp layer then goes over it in sRGB-encoded space, as Vello
//! blends, so without blur the output matches a single Vello render of both layers
//! (to 1/255, where the top layer's antialiased edges were quantized once more).

/// What the choreography asked for, converted to pixels by the stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlurParams {
    /// Centre of rotation, in physical pixels.
    pub center: [f32; 2],
    /// Signed arc swept during the exposure, in radians (0 = no blur, plain composite).
    pub angle: f32,
    /// Outside this radius (pixels) nothing moves: those pixels are copied unblurred.
    pub radius: f32,
}

/// Size of `Params` in blur.wgsl: a vec2 (8-byte aligned) then two f32, no padding.
const UNIFORM_SIZE: u64 = 16;
/// Side of the shader's square workgroup, `@workgroup_size(8, 8)`.
const WORKGROUP_SIDE: u32 = 8;

impl BlurParams {
    /// `Params` as the shader reads it, in field order (every GPU we target is little-endian).
    fn to_uniform(self) -> [[u8; 4]; 4] {
        [self.center[0], self.center[1], self.angle, self.radius].map(f32::to_le_bytes)
    }
}

pub struct BlurPass {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
}

impl BlurPass {
    pub fn new(device: &wgpu::Device) -> Self {
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let sampled = wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blur"),
            entries: &[
                entry(0, sampled),
                entry(1, sampled),
                entry(2, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
                entry(
                    3,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
                entry(
                    4,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                    },
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blur"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blur.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blur.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("blur"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("blur: linear, clamp to edge"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("blur params"),
            size: UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bind_group_layout, sampler, uniforms }
    }

    /// Records the pass into `encoder`.
    /// - `moving`: the moving layer as Vello wrote it (Rgba8Unorm, straight alpha,
    ///   opaque because it was rendered over the background).
    /// - `top`: the sharp layer (Rgba8Unorm, straight alpha, transparent background).
    /// - `out`: an Rgba8Unorm view of a texture with STORAGE_BINDING, `size` pixels.
    ///
    /// `params` go through one uniform buffer written with `Queue::write_buffer`, which
    /// takes effect at the next submit: submit `encoder` before encoding the pass again.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        moving: &wgpu::TextureView,
        top: &wgpu::TextureView,
        out: &wgpu::TextureView,
        size: (u32, u32),
        params: BlurParams,
    ) {
        queue.write_buffer(&self.uniforms, 0, params.to_uniform().as_flattened());
        // A new bind group each time: the views change whenever the window resizes.
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("blur"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(moving) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(top) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(out) },
                wgpu::BindGroupEntry { binding: 4, resource: self.uniforms.as_entire_binding() },
            ],
        });
        let descriptor = wgpu::ComputePassDescriptor { label: Some("motion blur"), timestamp_writes: None };
        let mut pass = encoder.begin_compute_pass(&descriptor);
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(size.0.div_ceil(WORKGROUP_SIDE), size.1.div_ceil(WORKGROUP_SIDE), 1);
    }
}

#[cfg(test)]
mod tests {
    //! Runs the pass on a real adapter (wgpu's default, or the one `WGPU_ADAPTER_NAME`
    //! names) and checks every pixel against the same math done on the CPU in f64.

    use std::f64::consts::TAU;

    use super::{BlurParams, BlurPass};
    use crate::gpu::{AdapterChoice, Gpu, read_rgba};

    const SIZE: u32 = 96;
    /// Off the pixel grid on purpose; also the centre of the test pattern.
    const CENTER: [f32; 2] = [45.3, 50.6];

    /// Straight-alpha RGBA8, rows top to bottom.
    struct Image(Vec<[u8; 4]>);

    impl Image {
        /// `texel` gets each pixel centre relative to `CENTER`.
        fn new(texel: impl Fn(f64, f64) -> [u8; 4]) -> Self {
            let [cx, cy] = CENTER.map(f64::from);
            let pixels = (0..SIZE)
                .flat_map(|y| (0..SIZE).map(move |x| (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy)))
                .map(|(dx, dy)| texel(dx, dy))
                .collect();
            Self(pixels)
        }

        /// Clamps to the edge, like the pass's sampler.
        fn texel(&self, x: i64, y: i64) -> [u8; 4] {
            let clamp = |v: i64| v.clamp(0, i64::from(SIZE) - 1) as usize;
            self.0[clamp(y) * SIZE as usize + clamp(x)]
        }

        /// Bilinear filtering of the encoded values at `(x, y)`, in pixels (centres at +0.5).
        fn bilinear(&self, x: f64, y: f64) -> [f64; 3] {
            let (tx, ty) = (x - 0.5, y - 0.5);
            let (x0, y0) = (tx.floor(), ty.floor());
            let (fx, fy) = (tx - x0, ty - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let taps = [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                (x0 + 1, y0, fx * (1.0 - fy)),
                (x0, y0 + 1, (1.0 - fx) * fy),
                (x0 + 1, y0 + 1, fx * fy),
            ];
            let mut rgb = [0.0; 3];
            for (tx, ty, weight) in taps {
                let texel = self.texel(tx, ty);
                for (out, value) in rgb.iter_mut().zip(texel) {
                    *out += weight * f64::from(value) / 255.0;
                }
            }
            rgb
        }
    }

    fn srgb_to_linear(c: f64) -> f64 {
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn linear_to_srgb(l: f64) -> f64 {
        if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }
    }

    struct Expected {
        /// Before rounding to 8 bits.
        rgb: [f64; 3],
        /// The moving layer was copied, not blurred.
        copied: bool,
    }

    /// The pass's output at `(x, y)`, or None where the radius test or the sample count
    /// sits so close to a threshold that f32 on the GPU may round the other way.
    fn expected(moving: &Image, top: &Image, params: BlurParams, x: u32, y: u32) -> Option<Expected> {
        let [cx, cy] = params.center.map(f64::from);
        let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
        let r = dx.hypot(dy);
        let radius = f64::from(params.radius);
        let angle = f64::from(params.angle);
        let arc = angle.abs() * r;
        let n = arc.ceil().clamp(1.0, 64.0);
        if (r - radius).abs() < 1e-3 || ((arc - arc.round()).abs() < 1e-3 && (1.0..64.0).contains(&arc.round())) {
            return None;
        }
        let copied = r > radius || n == 1.0;
        let blurred = if copied {
            let texel = moving.texel(x.into(), y.into());
            [0, 1, 2].map(|c| f64::from(texel[c]) / 255.0)
        } else {
            let mut sum = [0.0; 3];
            for i in 0..n as u32 {
                let (sin, cos) = (angle * ((f64::from(i) + 0.5) / n - 0.5)).sin_cos();
                let filtered = moving.bilinear(cx + dx * cos - dy * sin, cy + dx * sin + dy * cos);
                for (total, value) in sum.iter_mut().zip(filtered) {
                    *total += srgb_to_linear(value);
                }
            }
            sum.map(|total| linear_to_srgb(total / n))
        };
        let over = top.texel(x.into(), y.into()).map(|v| f64::from(v) / 255.0);
        let rgb = [0, 1, 2].map(|c| over[c] * over[3] + blurred[c] * (1.0 - over[3]));
        Some(Expected { rgb, copied })
    }

    fn texture(gpu: &Gpu, usage: wgpu::TextureUsages) -> wgpu::Texture {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blur test"),
            size: wgpu::Extent3d { width: SIZE, height: SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        })
    }

    fn upload(gpu: &Gpu, image: &Image) -> wgpu::Texture {
        let texture = texture(gpu, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        gpu.queue.write_texture(
            texture.as_image_copy(),
            image.0.as_flattened(),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * SIZE), rows_per_image: None },
            texture.size(),
        );
        texture
    }

    #[test]
    fn matches_cpu_reference() {
        let gpu = match Gpu::new(&AdapterChoice::Auto, true) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("skipped: no usable GPU ({e:#})");
                return;
            }
        };
        // Hard-edged sectors in red, a smooth angular wave in green, rings about the
        // centre in blue (which a blur about the right centre leaves almost unchanged).
        let moving = Image::new(|dx, dy| {
            let theta = dy.atan2(dx);
            let sector = ((theta / TAU + 0.5) * 12.0) as usize % 12;
            let red = [230, 20, 120, 250, 60, 0, 200, 90, 160, 30, 255, 110][sector];
            let green = (128.0 + 100.0 * (5.0 * theta).sin()) as u8;
            let blue = if ((dx.hypot(dy) / 5.0) as u32).is_multiple_of(2) { 230 } else { 20 };
            [red, green, blue, 255]
        });
        // An opaque hub on the centre, an alpha ramp along the top, an opaque corner.
        let top = Image::new(|dx, dy| {
            if dx.abs() < 6.0 && dy.abs() < 6.0 {
                [250, 240, 10, 255]
            } else if dy < -30.0 {
                [0, 200, 255, ((dx + 50.0) * 2.5) as u8]
            } else if dx > 30.0 && dy > 20.0 {
                [255, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let textures = [
            upload(&gpu, &moving),
            upload(&gpu, &top),
            texture(&gpu, wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC),
        ];
        let [moving_view, top_view, out_view] =
            textures.each_ref().map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let pass = BlurPass::new(&gpu.device);

        let (mut worst, mut skipped, mut blurred) = (0.0_f64, 0, 0);
        // No blur; a slight one; both directions; the 64-sample cap with samples off the edge.
        for (angle, radius) in [(0.0, 30.0), (0.04, 30.0), (0.3, 30.0), (-0.3, 30.0), (2.5, 200.0)] {
            let params = BlurParams { center: CENTER, angle, radius };
            let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            pass.encode(
                &gpu.device,
                &gpu.queue,
                &mut encoder,
                &moving_view,
                &top_view,
                &out_view,
                (SIZE, SIZE),
                params,
            );
            gpu.queue.submit([encoder.finish()]);
            let rgba = read_rgba(&gpu.device, &gpu.queue, &textures[2]).expect("reading the output back");

            for (i, got) in rgba.as_chunks::<4>().0.iter().enumerate() {
                let (x, y) = (i as u32 % SIZE, i as u32 / SIZE);
                let at = || format!("pixel ({x}, {y}) with {params:?}");
                let over = top.texel(x.into(), y.into());
                assert_eq!(got[3], 255, "{}: the output is opaque", at());
                if over[3] == 255 {
                    assert_eq!(got[..3], over[..3], "{}: an opaque top pixel wins", at());
                }
                let Some(want) = expected(&moving, &top, params, x, y) else {
                    skipped += 1;
                    continue;
                };
                if want.copied && over[3] == 0 {
                    assert_eq!(*got, moving.texel(x.into(), y.into()), "{}: copied exactly", at());
                }
                blurred += usize::from(!want.copied);
                for (value, reference) in got.iter().zip(want.rgb) {
                    let error = (f64::from(*value) - reference * 255.0).abs();
                    assert!(error <= 2.0, "{}: got {got:?}, expected {:?}", at(), want.rgb.map(|v| v * 255.0));
                    worst = worst.max(error);
                }
            }
        }
        assert!(skipped < 100, "{skipped} pixels too close to a threshold to check");
        eprintln!(
            "{}: {blurred} blurred pixels checked, worst error {worst:.2}/255 (2 allowed), {skipped} skipped",
            gpu.describe()
        );
    }
}
