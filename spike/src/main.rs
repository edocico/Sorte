//! Sorte M0 spike: a Vello scene in a Slint window through the wgpu-30 bridge, with a
//! spinning wheel, motion blur, ticks on Kira's clock and a vello_cpu fallback.
//! Throwaway code: README.md lists the flags and what to measure.

mod audio;
mod blur;
mod clock;
mod cpu;
mod draw;
mod frame;
mod gpu;
mod labels;
mod overlay;
mod present;
mod spin;
mod text;
mod wheel;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rand::rand_core::UnwrapErr;
use rand::rngs::{SysRng, Xoshiro256PlusPlus};
use rand::{Rng as _, RngExt, SeedableRng};
use slint::wgpu_30::WGPUConfiguration;
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Image, RenderingState, Rgba8Pixel, SharedPixelBuffer};
use vello::peniko::Color;

use crate::audio::Audio;
use crate::clock::{FrameClock, FrameRecord, FrameStats, percentile};
use crate::cpu::CpuStage;
use crate::frame::Frame;
use crate::gpu::{AdapterChoice, Gpu, GpuStage, StageTiming};
use crate::labels::LabelSet;
use crate::present::{Feedback, PresentationFeedback};
use crate::text::TextEngine;
use crate::wheel::{Spin, Wheel};

slint::include_modules!();

const BACKGROUND: Color = Color::from_rgb8(0xf5, 0xf2, 0xec);
/// Entry counts offered by the window's combo box, in its order.
const ENTRY_PRESETS: [usize; 5] = [2, 12, 60, 200, 1000];
const BENCH_SECONDS: f64 = 10.0;

const HELP: &str = "\
Sorte M0 spike: a Vello scene in a Slint window through the wgpu bridge.

USAGE: sorte-spike [OPTIONS]

Rendering
  --cpu                  vello_cpu instead of Vello on the GPU (Slint then uses GL FemtoVG, or software)
  --adapter NAME         wgpu adapter: auto (default; honours WGPU_ADAPTER_NAME), low, high,
                         or part of an adapter name (see --list-adapters)
  --allow-cpu-adapter    let the wgpu path run on a software adapter such as llvmpipe
  --threads N            vello_cpu worker threads (default 4)
  --no-blur              start with motion blur off
  --hook                 draw each frame from winit's RedrawRequested (default except on macOS)
  --notifier             draw each frame from Slint's BeforeRendering notifier (default on macOS)
  --list-adapters        print the wgpu adapters and exit

Wheel
  --entries N            number of entries (2..=1000; default: the window's preset, 12)
  --labels SET           names, scripts or numbered
  --speed L              speed level 1..=10 (default: the window's slider)
  --duration S           spin duration in seconds, 1..=30 (default: the window's slider)
  --seed N               fixed seed for the draw and the landing point
  --fonts DIR            font folder (default: spike/fonts)
  --no-audio             no ticks

Measuring
  --bench                spin for 10 s shortly after start-up, print the measurements, exit
  --stay                 with --bench: keep the window open afterwards
  --trace FILE.csv       with --bench: write one line per frame
  --overlay              start with the debug overlay shown (F12 toggles it)
  --gate N               N pairs of 10 s spins, alternating the platform baseline
                         (--null-scene) and the scene, then a comparison; then exit
  --window WxH           window size in logical pixels (default 1280x820)
  --fullscreen           start full screen (lets the compositor scan the window out directly)
  --null-scene           keep the frame loop but draw no new scene: Slint's and the
                         compositor's own pacing, as a baseline for --bench

Offscreen
  --snapshot FILE.png    render frames offscreen (no window) and exit
  --at T[,T...]          spin times to render, in seconds (default 1)
  --size WxH             snapshot size in pixels (default 1600x1000)
  --scale S              snapshot scale factor (default 1)
";

/// Where per-frame work runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drive {
    /// winit's RedrawRequested, just before Slint draws. No rendering notifier: with
    /// one, femtovg-wgpu rebuilds its pipelines every frame (M0 frame-pacing research).
    Hook,
    /// Slint's BeforeRendering. Needed on macOS, where Slint draws from a display link
    /// without RedrawRequested.
    Notifier,
}

#[derive(Clone, Debug)]
struct Options {
    cpu: bool,
    adapter: AdapterChoice,
    allow_cpu_adapter: bool,
    threads: u16,
    blur: bool,
    drive: Drive,
    list_adapters: bool,
    entries: Option<usize>,
    labels: Option<LabelSet>,
    speed: Option<u8>,
    duration: Option<f64>,
    seed: Option<u64>,
    fonts: PathBuf,
    audio: bool,
    bench: bool,
    stay: bool,
    trace: Option<PathBuf>,
    overlay: bool,
    window: Option<(f32, f32)>,
    null_scene: bool,
    fullscreen: bool,
    gate: usize,
    snapshot: Option<PathBuf>,
    at: Vec<f64>,
    size: (u32, u32),
    scale: f64,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut o = Options {
            cpu: false,
            adapter: AdapterChoice::Auto,
            allow_cpu_adapter: false,
            threads: 4,
            blur: true,
            drive: if cfg!(target_os = "macos") { Drive::Notifier } else { Drive::Hook },
            list_adapters: false,
            entries: None,
            labels: None,
            speed: None,
            duration: None,
            seed: None,
            fonts: Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts"),
            audio: true,
            bench: false,
            stay: false,
            trace: None,
            overlay: false,
            window: None,
            null_scene: false,
            fullscreen: false,
            gate: 0,
            snapshot: None,
            at: vec![1.0],
            size: (1600, 1000),
            scale: 1.0,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().with_context(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--cpu" => o.cpu = true,
                "--adapter" => o.adapter = AdapterChoice::parse(&value()?),
                "--allow-cpu-adapter" => o.allow_cpu_adapter = true,
                "--threads" => o.threads = value()?.parse()?,
                "--no-blur" => o.blur = false,
                "--hook" => o.drive = Drive::Hook,
                "--notifier" => o.drive = Drive::Notifier,
                "--list-adapters" => o.list_adapters = true,
                "--entries" => o.entries = Some(value()?.parse::<usize>()?.clamp(2, 1000)),
                "--labels" => {
                    let v = value()?;
                    o.labels = Some(LabelSet::parse(&v).with_context(|| format!("unknown label set {v}"))?);
                }
                "--speed" => o.speed = Some(value()?.parse::<u8>()?.clamp(1, 10)),
                "--duration" => o.duration = Some(value()?.parse::<f64>()?.clamp(1.0, 30.0)),
                "--seed" => o.seed = Some(value()?.parse()?),
                "--fonts" => o.fonts = PathBuf::from(value()?),
                "--no-audio" => o.audio = false,
                "--bench" => o.bench = true,
                "--stay" => o.stay = true,
                "--trace" => o.trace = Some(PathBuf::from(value()?)),
                "--overlay" => o.overlay = true,
                "--window" => {
                    let v = value()?;
                    let (w, h) = v.split_once('x').with_context(|| format!("--window wants WxH, got {v}"))?;
                    o.window = Some((w.parse()?, h.parse()?));
                }
                "--null-scene" => o.null_scene = true,
                "--fullscreen" => o.fullscreen = true,
                "--gate" => o.gate = value()?.parse::<usize>()?.max(1),
                "--snapshot" => o.snapshot = Some(PathBuf::from(value()?)),
                "--at" => o.at = value()?.split(',').map(|s| s.trim().parse()).collect::<Result<_, _>>()?,
                "--size" => {
                    let v = value()?;
                    let (w, h) = v.split_once('x').with_context(|| format!("--size wants WxH, got {v}"))?;
                    o.size = (w.parse()?, h.parse()?);
                }
                "--scale" => o.scale = value()?.parse()?,
                "--help" | "-h" => {
                    print!("{HELP}");
                    std::process::exit(0);
                }
                other => bail!("unknown option {other} (see --help)"),
            }
        }
        Ok(o)
    }

    fn label_set(&self) -> LabelSet {
        self.labels.unwrap_or(LabelSet::Names)
    }
}

fn main() -> Result<()> {
    let options = Options::parse()?;
    if options.list_adapters {
        return list_adapters();
    }
    if options.snapshot.is_some() {
        return snapshot(&options);
    }
    run_app(options)
}

fn list_adapters() -> Result<()> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all())) {
        let i = adapter.get_info();
        println!("{} | {:?} | {:?} | {} {}", i.name, i.device_type, i.backend, i.driver, i.driver_info);
    }
    Ok(())
}

/// Every font file in `dir` (none if it does not exist: system fonts only).
fn font_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = std::fs::read_dir(dir) else {
        eprintln!("font folder {} not found: using system fonts only", dir.display());
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            matches!(ext.as_str(), "ttf" | "otf" | "ttc")
        })
        .collect();
    files.sort();
    files
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

/// CPU time used by this process so far (user + system), where we can read it.
fn process_cpu_time() -> Option<Duration> {
    #[cfg(target_os = "linux")]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage fills the struct we pass; RUSAGE_SELF is always valid.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: getrusage succeeded, so the struct is initialized.
        let u = unsafe { usage.assume_init() };
        let secs = |tv: libc::timeval| Duration::new(tv.tv_sec as u64, (tv.tv_usec as u32) * 1000);
        Some(secs(u.ru_utime) + secs(u.ru_stime))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Where the scene is drawn.
enum Stage {
    Gpu { stage: GpuStage, device: wgpu::Device, queue: wgpu::Queue },
    Cpu { stage: CpuStage, buffers: [SharedPixelBuffer<Rgba8Pixel>; 2], next: usize },
}

impl Stage {
    fn cpu(threads: u16) -> Self {
        Stage::Cpu {
            stage: CpuStage::new(threads),
            buffers: [SharedPixelBuffer::new(1, 1), SharedPixelBuffer::new(1, 1)],
            next: 0,
        }
    }
}

/// The spin on screen.
struct Active {
    spin: Spin,
    /// Animated time of the frame that showed t = 0, known once that frame renders.
    start_t: Option<f64>,
    /// Frame number of that first frame, to match presentation feedback.
    first_frame: u64,
    bench: bool,
    cpu_at_start: Option<Duration>,
}

/// Ground truth from wp_presentation (Wayland only), for the frames of a benchmark.
#[derive(Default)]
struct PresentRecord {
    /// Frames shown, as (frame number, MSC, time).
    presented: Vec<(u64, u64, Instant)>,
    discarded: u64,
    /// From the frame's start (our callback) to the moment it became visible.
    latency_ms: Vec<f32>,
    refresh_ns: u32,
}

struct App {
    options: Options,
    stage: Stage,
    renderer: String,
    text: TextEngine,
    wheel: Wheel,
    /// Entry count and label set the wheel was built from.
    list: (usize, LabelSet),
    list_dirty: bool,
    rest: f64,
    active: Option<Active>,
    clock: FrameClock,
    stats: FrameStats,
    timing: StageTiming,
    monitor_hz: Option<f64>,
    audio: Option<Audio>,
    outcome: Box<dyn rand::Rng>,
    cosmetic: Xoshiro256PlusPlus,
    /// The last frame asked for another one (an animation runs).
    animating: bool,
    /// Something changed while idle: the timer-driven fallback must draw once.
    dirty: bool,
    frame_no: u64,
    feedback: Option<PresentationFeedback>,
    feedback_tried: bool,
    /// Start instants of recent frames, to measure display latency from feedback.
    frame_starts: VecDeque<(u64, Instant)>,
    /// Latest display latency seen, for the overlay.
    latency_ms: Option<f32>,
    present: Option<PresentRecord>,
    /// A finished benchmark waiting for the feedback of its last frames, which arrives
    /// a few refreshes late: the spin, its last frame number, and when to report.
    pending_report: Option<(Active, u64, Instant)>,
    /// GPU times of our work during a benchmark, in milliseconds.
    gpu_times: Vec<f32>,
    /// Draw nothing new (the platform baseline); `--gate` switches it between spins.
    null_scene: bool,
    gate: Option<Gate>,
}

/// What one benchmark spin measured, for `--gate`'s comparison.
#[derive(Clone, Copy, Debug)]
struct BenchResult {
    seconds: f64,
    frames: usize,
    /// Refreshes skipped, from wp_presentation where available.
    missed: Option<u64>,
    /// Refreshes skipped, judged by the clock.
    heuristic: u64,
    stalls: u64,
    work_p99_ms: f32,
    gpu_p99_ms: Option<f32>,
}

impl BenchResult {
    /// The best count available: the compositor's, else the clock's.
    fn misses(&self) -> u64 {
        self.missed.unwrap_or(self.heuristic)
    }
}

/// `--gate N`: spins alternating the platform baseline and the scene.
struct Gate {
    /// The spins still to run: `true` is the baseline (nothing new drawn).
    plan: VecDeque<bool>,
    results: Vec<(bool, BenchResult)>,
    /// When the next spin starts (the frame loop keeps running until then).
    next_at: Option<Instant>,
}

impl Gate {
    fn new(pairs: usize) -> Self {
        let plan = (0..pairs).flat_map(|_| [true, false]).collect();
        Self { plan, results: Vec::new(), next_at: None }
    }

    /// The comparison, as plain measurements: pass or fail is the user's call.
    fn summary(&self, period_ms: f64, renderer: &str) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== Sorte gate: the scene against the platform baseline ===");
        let _ = writeln!(out, "renderer       {renderer}; refresh period {period_ms:.4} ms");
        let total = |baseline: bool| {
            self.results.iter().filter(|(b, _)| *b == baseline).fold((0.0, 0u64, 0u64, 0u64), |acc, (_, r)| {
                (acc.0 + r.seconds, acc.1 + r.misses(), acc.2 + r.stalls, acc.3 + u64::from(r.missed.is_some()))
            })
        };
        let (tb, mb, sb, _) = total(true);
        let (ts, ms, ss, ground) = total(false);
        let source = if ground > 0 { "wp_presentation" } else { "clock heuristic" };
        let per_min = |m: u64, t: f64| if t > 0.0 { 60.0 * m as f64 / t } else { f64::NAN };
        let _ = writeln!(out, "baseline       {mb} missed in {tb:.0} s = {:.2} per minute; {sb} stalls ({source})", per_min(mb, tb));
        let _ = writeln!(out, "scene          {ms} missed in {ts:.0} s = {:.2} per minute; {ss} stalls", per_min(ms, ts));
        let _ = writeln!(out, "excess         {:+.2} missed per minute", per_min(ms, ts) - per_min(mb, tb));
        // One-sided exact test: given all the misses, how likely is it that the scene got
        // at least its share if both ran at the same rate (binomial, split by exposure).
        let k = mb + ms;
        let share = if tb + ts > 0.0 { ts / (tb + ts) } else { 0.5 };
        let p = (ms..=k).map(|x| binomial(k, x, share)).sum::<f64>();
        let _ = writeln!(out, "p              {p:.3} (scene at least as many misses with equal rates)");
        let work = self.results.iter().filter(|(b, _)| !b).map(|(_, r)| r.work_p99_ms).fold(0.0_f32, f32::max);
        let gpu = self.results.iter().filter(|(b, _)| !b).filter_map(|(_, r)| r.gpu_p99_ms).fold(f32::NAN, f32::max);
        let pct = |ms: f32| 100.0 * f64::from(ms) / period_ms;
        let _ = writeln!(
            out,
            "headroom       our CPU p99 {work:.2} ms ({:.0} % of the period), our GPU p99 {} (worst scene spin)",
            pct(work),
            if gpu.is_nan() { "n/a".into() } else { format!("{gpu:.2} ms ({:.0} %)", pct(gpu)) }
        );
        out
    }
}

/// P(X = x) for X ~ Binomial(n, p).
fn binomial(n: u64, x: u64, p: f64) -> f64 {
    let ln_choose = (1..=x).map(|i| ((n - x + i) as f64 / i as f64).ln()).sum::<f64>();
    (ln_choose + x as f64 * p.ln() + (n - x) as f64 * (1.0 - p).ln()).exp()
}

impl App {
    fn new(options: Options, stage: Stage, renderer: String) -> Result<Self> {
        let text = TextEngine::new(&font_files(&options.fonts))?;
        let n = options.entries.unwrap_or(12);
        let set = options.label_set();
        let wheel = Wheel::new(labels::entries(set, n), vec![1; n]);
        let audio = if options.audio {
            Audio::new().map_err(|e| eprintln!("audio unavailable ({e:#}): running silent")).ok()
        } else {
            None
        };
        // Outcome first, staging second: the outcome comes from the OS generator (or a
        // fixed seed), the landing point from a separate cosmetic generator.
        let (outcome, cosmetic): (Box<dyn rand::Rng>, _) = match options.seed {
            Some(seed) => {
                (Box::new(Xoshiro256PlusPlus::seed_from_u64(seed)), Xoshiro256PlusPlus::seed_from_u64(!seed))
            }
            None => {
                let mut os = UnwrapErr(SysRng);
                let cosmetic = Xoshiro256PlusPlus::seed_from_u64(os.next_u64());
                (Box::new(os), cosmetic)
            }
        };
        let null_scene = options.null_scene;
        let gate = (options.gate > 0).then(|| Gate::new(options.gate));
        Ok(Self {
            options,
            stage,
            renderer,
            text,
            wheel,
            list: (n, set),
            list_dirty: false,
            rest: 0.0,
            active: None,
            clock: FrameClock::new(),
            stats: FrameStats::default(),
            timing: StageTiming::default(),
            monitor_hz: None,
            audio,
            outcome,
            cosmetic,
            animating: false,
            dirty: true,
            frame_no: 0,
            feedback: None,
            feedback_tried: false,
            frame_starts: VecDeque::new(),
            latency_ms: None,
            present: None,
            pending_report: None,
            gpu_times: Vec::new(),
            null_scene,
            gate,
        })
    }

    /// Starts the next spin of `--gate`, or the single `--bench` spin.
    fn start_measuring(&mut self, ui: &AppWindow) {
        if let Some(gate) = &mut self.gate {
            let Some(baseline) = gate.plan.pop_front() else { return };
            self.null_scene = baseline;
        }
        self.start_spin(ui, true);
    }

    fn poll_monitor(&mut self, ui: &AppWindow) {
        if self.monitor_hz.is_some() {
            return;
        }
        // Wayland reports it only after the compositor's first `enter` event.
        let mhz = ui.window().with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz()));
        if let Some(mhz) = mhz.flatten() {
            let hz = f64::from(mhz) / 1000.0;
            self.monitor_hz = Some(hz);
            self.clock.set_nominal_hz(hz);
        }
    }

    fn list_changed(&mut self, ui: &AppWindow) {
        let index = usize::try_from(ui.get_entries_index()).unwrap_or(1).min(ENTRY_PRESETS.len() - 1);
        self.list = (ENTRY_PRESETS[index], LabelSet::from_index(ui.get_label_set()));
        // The list is frozen while a spin runs: rebuild when it ends.
        self.list_dirty = true;
        if self.active.is_none() {
            self.rebuild_wheel();
        }
        self.request_frame(ui);
    }

    fn rebuild_wheel(&mut self) {
        let (n, set) = self.list;
        self.wheel = Wheel::new(labels::entries(set, n), vec![1; n]);
        self.list_dirty = false;
    }

    fn request_frame(&mut self, ui: &AppWindow) {
        self.dirty = true;
        ui.window().request_redraw();
    }

    fn start_spin(&mut self, ui: &AppWindow, bench: bool) {
        if self.active.is_some() {
            return;
        }
        let winner = draw::draw_weighted(&self.wheel.weights, &mut *self.outcome);
        let u = self.cosmetic.random_range(spin::LANDING_MARGIN..=1.0 - spin::LANDING_MARGIN);
        let duration =
            if bench { BENCH_SECONDS } else { self.options.duration.unwrap_or(f64::from(ui.get_duration())) };
        let level = self.options.speed.unwrap_or(ui.get_speed().round() as u8).clamp(1, 10);
        let spin =
            Spin::new(&self.wheel, winner, self.rest, duration.clamp(1.0, 30.0), spin::omega_for_level(level), u);
        self.active = Some(Active { spin, start_t: None, first_frame: 0, bench, cpu_at_start: None });
        ui.set_spinning(true);
        ui.set_result("".into());
        self.request_frame(ui);
    }

    fn finish_spin(&mut self, ui: &AppWindow) {
        let Some(active) = self.active.take() else { return };
        self.rest = active.spin.rest();
        let winner = active.spin.winner;
        // The staging must land on the outcome the draw decided.
        let shown = self.wheel.slice_at(self.rest);
        if shown != winner {
            eprintln!("BUG: the wheel stopped on entry {shown}, the draw chose {winner}");
        }
        ui.set_result(format!("Estratto: {}", self.wheel.entries[winner]).into());
        ui.set_spinning(false);
        if active.bench {
            // Presentation feedback lags 4-5 refreshes: keep drawing a moment before reporting.
            let wait = if self.feedback.is_some() { Duration::from_millis(250) } else { Duration::ZERO };
            self.pending_report = Some((active, self.frame_no, Instant::now() + wait));
        }
        if self.list_dirty {
            self.rebuild_wheel();
        }
    }

    fn report_when_due(&mut self, ui: &AppWindow, now: Instant) {
        if self.pending_report.as_ref().is_some_and(|&(_, _, due)| now >= due) {
            let (active, last_frame, _) = self.pending_report.take().expect("checked");
            if let Some(record) = &mut self.present {
                record.presented.retain(|&(frame, _, _)| frame <= last_frame);
            }
            let result = self.report_bench(&active, ui);
            let Some(gate) = &mut self.gate else {
                if !self.options.stay {
                    let _ = slint::quit_event_loop();
                }
                return;
            };
            gate.results.push((self.null_scene, result));
            if gate.plan.is_empty() {
                print!("{}", gate.summary(self.clock.period() * 1000.0, &self.renderer));
                if !self.options.stay {
                    let _ = slint::quit_event_loop();
                }
            } else {
                gate.next_at = Some(now + Duration::from_secs(1));
            }
        }
        // The next spin of --gate, once its pause is over.
        if self.active.is_none() && self.pending_report.is_none() {
            if let Some(gate) = &mut self.gate {
                if gate.next_at.is_some_and(|at| now >= at) {
                    gate.next_at = None;
                    self.start_measuring(ui);
                }
            }
        }
    }

    /// Draws one frame. Runs in winit's RedrawRequested (before Slint draws), in
    /// Slint's BeforeRendering, or from a timer on the software renderer.
    fn render_frame(&mut self, ui: &AppWindow) -> Result<()> {
        let start = Instant::now();
        self.frame_no += 1;
        self.poll_monitor(ui);
        // The first frame of a spin re-anchors the clock: the swapchain refill burst follows.
        let spin_starts = self.active.as_ref().is_some_and(|a| a.start_t.is_none());
        let tick = if spin_starts { self.clock.tick_restart(start) } else { self.clock.tick(start) };

        // Spin time comes from the vsync-quantized clock (CLAUDE.md: t advances in
        // whole periods, never from measured frame intervals).
        let mut spin_t = None;
        if let Some(active) = &mut self.active {
            if active.start_t.is_none() {
                active.start_t = Some(tick.t);
                active.first_frame = self.frame_no;
                active.cpu_at_start = process_cpu_time();
                if let Some(audio) = &mut self.audio {
                    // Anchor the ticks on the frame's vsync on the fitted grid, not on
                    // the callback's jittery timestamp.
                    let zero = self.clock.vsync_instant(tick.vsync);
                    let advance = f64::from(ui.get_audio_offset_ms()) / 1000.0;
                    audio.start(active.spin.ticks.iter().map(|k| k.t).collect(), zero, advance);
                }
                self.stats.reset_counters();
                if active.bench {
                    self.stats.recording = Some(Vec::new());
                    self.gpu_times.clear();
                    // Ground truth only where the compositor gives it (Wayland).
                    self.present = self.feedback.is_some().then(PresentRecord::default);
                }
            }
            let t = tick.t - active.start_t.unwrap_or(tick.t);
            spin_t = Some(t.min(active.spin.duration()));
        }

        // --null-scene: past the first frames nothing new is drawn, so what is left is the
        // pacing of Slint and the compositor themselves, with our frame loop around them.
        if !self.null_scene || self.frame_no <= 2 {
            let (w, h) = (ui.get_scene_px_width().max(1) as u32, ui.get_scene_px_height().max(1) as u32);
            let scale = f64::from(ui.window().scale_factor());
            let mut frame = Frame::new(w, h, scale, BACKGROUND, self.clock.period());
            let (theta, omega, tilt) = match (&self.active, spin_t) {
                (Some(a), Some(t)) => (a.spin.theta(t), a.spin.omega(t), a.spin.pointer_tilt(t)),
                _ => (self.rest, 0.0, 0.0),
            };
            self.wheel.render(&mut frame, &mut self.text, theta, omega, tilt);
            if ui.get_overlay() {
                let lines = self.overlay_lines(spin_t, omega, (w, h), scale);
                let period_ms = (self.clock.period() * 1000.0) as f32;
                overlay::draw(&mut frame.top, &mut self.text, scale, &lines, &self.stats.graph(), period_ms);
            }

            self.timing = match &mut self.stage {
                Stage::Gpu { stage, device, queue } => {
                    let texture = stage.render(device, queue, &frame, ui.get_blur())?;
                    ui.set_scene(Image::try_from(texture).context("importing the scene texture into Slint")?);
                    stage.timing
                }
                Stage::Cpu { stage, buffers, next } => {
                    let buffer = &mut buffers[*next];
                    if buffer.width() != w || buffer.height() != h {
                        *buffer = SharedPixelBuffer::new(w, h);
                    }
                    stage.render(&frame, buffer.make_mut_bytes());
                    ui.set_scene(Image::from_rgba8_premultiplied(buffer.clone()));
                    *next ^= 1;
                    // "submit" here is rasterizing: vello_cpu's equivalent of the GPU's work.
                    let t = stage.timing;
                    StageTiming {
                        encode_ms: t.encode_ms + t.flush_ms,
                        submit_ms: t.render_ms,
                        blurred: false,
                        gpu_ms: None,
                    }
                }
            };
        }
        if let Stage::Gpu { stage, .. } = &mut self.stage {
            let times = stage.take_gpu_times();
            if self.stats.recording.is_some() {
                self.gpu_times.extend(times);
            }
        }

        self.presentation_feedback(ui, start);

        // The audio clock is observed on Audio's own thread, at random phases.
        if let Some(audio) = &mut self.audio {
            audio.pump();
        }

        // Only frames of a running spin count (not the wait for a benchmark's feedback).
        if self.animating && self.active.is_some() {
            let record = FrameRecord {
                interval_ms: (tick.interval * 1000.0) as f32,
                dropped: tick.dropped,
                residual: tick.residual as f32,
                work_ms: ms(start.elapsed()),
                encode_ms: self.timing.encode_ms,
                submit_ms: self.timing.submit_ms,
                grid_s: spin_t.unwrap_or(0.0),
                mode: Some(tick.mode),
            };
            self.stats.push(&tick, record);
        }

        if let (Some(active), Some(t)) = (&self.active, spin_t) {
            if t >= active.spin.duration() {
                self.finish_spin(ui);
            }
        }
        self.report_when_due(ui, Instant::now());
        let gate_waiting = self.gate.as_ref().is_some_and(|g| g.next_at.is_some());
        self.animating = self.active.is_some() || self.pending_report.is_some() || gate_waiting;
        self.dirty = false;
        Ok(())
    }

    /// Requests wp_presentation feedback for this frame and collects what arrived.
    /// Wayland only; elsewhere `feedback` stays None.
    fn presentation_feedback(&mut self, ui: &AppWindow, start: Instant) {
        if !self.feedback_tried {
            self.feedback_tried = true;
            self.feedback = ui.window().with_winit_window(PresentationFeedback::new).flatten();
        }
        let Some(feedback) = &mut self.feedback else { return };
        feedback.request(self.frame_no);
        if self.frame_starts.len() == 128 {
            self.frame_starts.pop_front();
        }
        self.frame_starts.push_back((self.frame_no, start));
        // Feedback for a benchmark's last frames arrives after the spin ended.
        let first_bench_frame = self
            .active
            .as_ref()
            .filter(|a| a.bench)
            .or(self.pending_report.as_ref().map(|(a, _, _)| a))
            .map(|a| a.first_frame);
        for event in feedback.poll() {
            match event {
                Feedback::Presented(p) => {
                    let started = self.frame_starts.iter().find(|(n, _)| *n == p.frame).map(|&(_, s)| s);
                    let latency = started.map(|s| ms(p.time.saturating_duration_since(s)));
                    if latency.is_some() {
                        self.latency_ms = latency;
                    }
                    if let (Some(record), Some(first)) = (&mut self.present, first_bench_frame) {
                        if p.frame >= first {
                            record.presented.push((p.frame, p.msc, p.time));
                            record.latency_ms.extend(latency);
                            record.refresh_ns = p.refresh_ns;
                        }
                    }
                }
                Feedback::Discarded { frame } => {
                    if let (Some(record), Some(first)) = (&mut self.present, first_bench_frame) {
                        if frame >= first {
                            record.discarded += 1;
                        }
                    }
                }
            }
        }
    }

    fn overlay_lines(&mut self, spin_t: Option<f64>, omega: f64, (w, h): (u32, u32), scale: f64) -> Vec<String> {
        let graph = self.stats.graph();
        let max = graph.iter().copied().fold(0.0_f32, f32::max);
        let monitor = self.monitor_hz.map_or("?".into(), |hz| format!("{hz:.3} Hz"));
        let fitted = 1.0 / self.clock.period();
        let mut lines = vec![
            self.renderer.clone(),
            format!(
                "monitor {monitor} · fitted {fitted:.3} Hz · clock {:?} · drive {:?}",
                self.clock.mode(),
                self.options.drive
            ),
            format!(
                "frames {} · dropped {} · stalls {} · interval p50 {:.2} p99 {:.2} max {:.2} ms",
                self.stats.frames,
                self.stats.dropped,
                self.stats.stalls,
                percentile(&graph, 50.0),
                percentile(&graph, 99.0),
                max
            ),
            format!(
                "scene {w}x{h} @{scale:.2} · blur {} · encode {:.2} ms · submit {:.2} ms · gpu {}",
                if self.timing.blurred { "on" } else { "off" },
                self.timing.encode_ms,
                self.timing.submit_ms,
                self.timing.gpu_ms.map_or("n/a".into(), |g| format!("{g:.2} ms"))
            ),
            format!(
                "entries {} · labels {} · t {} · omega {omega:.1} rad/s{}",
                self.wheel.entries.len(),
                self.wheel.labels_drawn,
                spin_t.map_or("-".into(), |t| format!("{t:.2} s")),
                self.active.as_ref().map_or(String::new(), |a| format!(
                    " · K {} n {:.2}",
                    a.spin.plan.turns, a.spin.plan.n
                )),
            ),
        ];
        if let Some(latency) = self.latency_ms {
            lines.push(format!("display latency {latency:.1} ms (wp_presentation)"));
        }
        if let Some(audio) = &mut self.audio {
            let s = audio.status();
            lines.push(format!(
                "audio {} Hz · {} · pending {} · thinned {}",
                s.sample_rate,
                if s.synced { "synced" } else { "syncing" },
                s.pending,
                s.thinned
            ));
        }
        lines
    }

    fn report_bench(&mut self, active: &Active, ui: &AppWindow) -> BenchResult {
        let records = self.stats.recording.take().unwrap_or_default();
        let intervals: Vec<f32> = records.iter().map(|r| r.interval_ms).collect();
        let residuals: Vec<f32> = records.iter().map(|r| r.residual).collect();
        let work: Vec<f32> = records.iter().map(|r| r.work_ms).collect();
        let encode: Vec<f32> = records.iter().map(|r| r.encode_ms).collect();
        let submit: Vec<f32> = records.iter().map(|r| r.submit_ms).collect();
        let dropped: u64 = records.iter().map(|r| u64::from(r.dropped)).sum();
        let duration = active.spin.duration();
        let size = ui.window().size();
        let stats = |v: &[f32]| {
            format!(
                "p50 {:.2}  p95 {:.2}  p99 {:.2}  max {:.2}",
                percentile(v, 50.0),
                percentile(v, 95.0),
                percentile(v, 99.0),
                v.iter().copied().fold(f32::NAN, f32::max)
            )
        };

        let mut out = String::new();
        let _ = writeln!(out, "=== Sorte spike benchmark ===");
        let _ = writeln!(out, "renderer       {} · drive {:?}", self.renderer, self.options.drive);
        let nominal = self.clock.nominal_period().map_or("unknown".into(), |p| format!("{:.3} Hz", 1.0 / p));
        let fitted = 1.0 / self.clock.period();
        let _ = writeln!(out, "refresh        monitor {nominal}, fitted {fitted:.3} Hz, clock mode {:?}", self.clock.mode());
        let _ = writeln!(
            out,
            "window         {}x{} px, scale {:.3}; entries {}, labels drawn {}; blur {}",
            size.width,
            size.height,
            ui.window().scale_factor(),
            self.wheel.entries.len(),
            self.wheel.labels_drawn,
            match (&self.stage, ui.get_blur()) {
                (Stage::Cpu { .. }, _) => "n/a (vello_cpu)",
                (_, true) => "on",
                (_, false) => "off",
            }
        );
        let _ = writeln!(
            out,
            "spin           {duration:.1} s, K {} turns, n {:.2}, omega0 {:.1} rad/s; {} frames for ~{:.0} refreshes",
            active.spin.plan.turns,
            active.spin.plan.n,
            active.spin.plan.omega0,
            records.len(),
            duration * fitted
        );
        let _ = writeln!(out, "DROPPED        {dropped} refreshes by the clock heuristic; {} stalls", self.stats.stalls);
        let mut missed = None;
        if let Some(record) = self.present.take() {
            let mut msc_skips = 0u64;
            let mut consecutive = 0u64;
            for pair in record.presented.windows(2) {
                let ((f0, m0, _), (f1, m1, _)) = (pair[0], pair[1]);
                if f1 == f0 + 1 {
                    consecutive += 1;
                    msc_skips += m1.saturating_sub(m0).saturating_sub(1);
                }
            }
            missed = Some(msc_skips);
            let _ = writeln!(
                out,
                "PRESENTED      {} frames shown, {} discarded; {msc_skips} vsyncs without a new frame over {consecutive} consecutive pairs (wp_presentation ground truth)",
                record.presented.len(),
                record.discarded
            );
            let _ = writeln!(
                out,
                "display        latency ms {} · compositor refresh {:.4} ms",
                stats(&record.latency_ms),
                f64::from(record.refresh_ns) / 1e6
            );
        }
        let _ = writeln!(out, "interval ms    {}", stats(&intervals));
        let _ = writeln!(
            out,
            "residual       p1 {:.3}  p99 {:.3} periods (frame start minus grid)",
            percentile(&residuals, 1.0),
            percentile(&residuals, 99.0)
        );
        let _ = writeln!(out, "our CPU ms     {}", stats(&work));
        let _ = writeln!(out, "  encode ms    {}", stats(&encode));
        let _ = writeln!(out, "  submit ms    {}", stats(&submit));
        let mut gpu_p99_ms = None;
        if !self.gpu_times.is_empty() {
            let gpu = std::mem::take(&mut self.gpu_times);
            gpu_p99_ms = Some(percentile(&gpu, 99.0));
            let _ = writeln!(out, "our GPU ms     {}  ({} frames timed)", stats(&gpu), gpu.len());
        }
        if let (Some(before), Some(after)) = (active.cpu_at_start, process_cpu_time()) {
            let cpu = (after - before).as_secs_f64();
            let _ = writeln!(out, "process CPU    {cpu:.2} s over the {duration:.0} s spin ({:.0} % of one core)", 100.0 * cpu / duration);
        }
        if let Some(audio) = &mut self.audio {
            let s = audio.status();
            let _ = writeln!(
                out,
                "audio          {} Hz, synced {}, thinned {} ticks, callback load peak {}",
                s.sample_rate,
                s.synced,
                s.thinned,
                s.cpu_usage.map_or("n/a".into(), |u| format!("{:.1} %", u * 100.0))
            );
        }
        let result = BenchResult {
            seconds: duration,
            frames: records.len(),
            missed,
            heuristic: dropped,
            stalls: self.stats.stalls,
            work_p99_ms: percentile(&work, 99.0),
            gpu_p99_ms,
        };
        match &self.gate {
            // One line per spin; the comparison comes at the end.
            Some(gate) => println!(
                "spin {:>2}  {:<8}  frames {}  missed {}  heuristic {}  stalls {}  our CPU p99 {:.2} ms  our GPU p99 {}",
                gate.results.len() + 1,
                if self.null_scene { "baseline" } else { "scene" },
                result.frames,
                result.missed.map_or("n/a".into(), |m| m.to_string()),
                result.heuristic,
                result.stalls,
                result.work_p99_ms,
                result.gpu_p99_ms.map_or("n/a".into(), |g| format!("{g:.2} ms")),
            ),
            None => print!("{out}"),
        }
        if let Some(path) = &self.options.trace {
            let mut csv = String::from("frame,interval_ms,dropped,residual,work_ms,encode_ms,submit_ms,t_s,mode\n");
            for (i, r) in records.iter().enumerate() {
                let _ = writeln!(
                    csv,
                    "{i},{:.4},{},{:.4},{:.4},{:.4},{:.4},{:.5},{:?}",
                    r.interval_ms,
                    r.dropped,
                    r.residual,
                    r.work_ms,
                    r.encode_ms,
                    r.submit_ms,
                    r.grid_s,
                    r.mode.unwrap_or(clock::Mode::Locked)
                );
            }
            match std::fs::write(path, csv) {
                Ok(()) => println!("trace          {}", path.display()),
                Err(e) => eprintln!("could not write {}: {e}", path.display()),
            }
        }
        result
    }
}

fn run_app(options: Options) -> Result<()> {
    // Vello on our own wgpu device, shared with Slint; vello_cpu when that is impossible.
    let mut gpu = None;
    if !options.cpu {
        match Gpu::new(&options.adapter, options.allow_cpu_adapter) {
            Ok(g) => {
                let config = WGPUConfiguration::Manual {
                    instance: g.instance.clone(),
                    adapter: g.adapter.clone(),
                    device: g.device.clone(),
                    queue: g.queue.clone(),
                };
                match slint::BackendSelector::new().require_wgpu_30(config).select() {
                    Ok(()) => gpu = Some(g),
                    Err(e) => eprintln!("Slint cannot render with wgpu here ({e}): falling back to vello_cpu"),
                }
            }
            Err(e) => eprintln!("no usable GPU ({e:#}): falling back to vello_cpu"),
        }
    }

    let (stage, renderer) = match &gpu {
        Some(g) => {
            let stage = GpuStage::new(&g.device, &g.queue)?;
            (
                Stage::Gpu { stage, device: g.device.clone(), queue: g.queue.clone() },
                format!("Vello GPU · {}", g.describe()),
            )
        }
        None => {
            let gl = slint::BackendSelector::new()
                .backend_name("winit".into())
                .renderer_name("femtovg".into())
                .select()
                .is_ok();
            if !gl {
                slint::BackendSelector::new()
                    .backend_name("winit".into())
                    .renderer_name("software".into())
                    .select()
                    .context("no Slint renderer available")?;
            }
            let slint_renderer = if gl { "GL FemtoVG" } else { "software" };
            (Stage::cpu(options.threads), format!("vello_cpu · {} threads · Slint {slint_renderer}", options.threads))
        }
    };

    let ui = AppWindow::new()?;
    ui.set_renderer(renderer.clone().into());
    ui.set_overlay(options.overlay);
    ui.set_blur(options.blur);
    if options.fullscreen {
        ui.window().set_fullscreen(true);
    }
    if let Some((w, h)) = options.window {
        ui.window().set_size(slint::LogicalSize::new(w, h));
    }
    if let Some(n) = options.entries {
        if let Some(i) = ENTRY_PRESETS.iter().position(|&p| p == n) {
            ui.set_entries_index(i as i32);
        }
    }
    if let Some(set) = options.labels {
        ui.set_label_set(match set {
            LabelSet::Names => 0,
            LabelSet::Scripts => 1,
            LabelSet::Numbered => 2,
        });
    }
    if let Some(level) = options.speed {
        ui.set_speed(f32::from(level));
    }
    if let Some(d) = options.duration {
        ui.set_duration(d as f32);
    }

    let bench = options.bench || options.gate > 0;
    let drive = options.drive;
    let app = Rc::new(RefCell::new(App::new(options, stage, renderer)?));

    let with_app = |f: fn(&mut App, &AppWindow)| {
        let app = app.clone();
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                f(&mut app.borrow_mut(), &ui);
            }
        }
    };
    ui.on_spin(with_app(|app, ui| app.start_spin(ui, false)));
    ui.on_bench(with_app(|app, ui| app.start_spin(ui, true)));
    ui.on_list_changed(with_app(App::list_changed));
    ui.on_redraw(with_app(App::request_frame));

    // Keeps the software renderer's timer alive when that path is taken.
    let mut _frame_timer = None;
    match drive {
        Drive::Hook => {
            let app = app.clone();
            let weak = ui.as_weak();
            ui.window().on_winit_window_event(move |window, event| {
                if matches!(event, winit::event::WindowEvent::RedrawRequested) {
                    if let Some(ui) = weak.upgrade() {
                        let mut app = app.borrow_mut();
                        if let Err(e) = app.render_frame(&ui) {
                            eprintln!("frame failed: {e:#}");
                        }
                        if app.animating {
                            // winit's, not Slint's: Slint clears its pending flag in the
                            // draw() that follows this hook, which would swallow ours.
                            window.with_winit_window(|w| w.request_redraw());
                        }
                    }
                }
                EventResult::Propagate
            });
        }
        Drive::Notifier => {
            let notifier = {
                let app = app.clone();
                let weak = ui.as_weak();
                ui.window().set_rendering_notifier(move |state, _| {
                    if let (RenderingState::BeforeRendering, Some(ui)) = (state, weak.upgrade()) {
                        let mut app = app.borrow_mut();
                        if let Err(e) = app.render_frame(&ui) {
                            eprintln!("frame failed: {e:#}");
                        }
                        if app.animating {
                            ui.window().request_redraw();
                        }
                    }
                })
            };
            if notifier.is_err() {
                // The software renderer has no notifier: draw from a timer instead.
                eprintln!("no rendering notifier on this renderer: drawing from a timer");
                let timer = slint::Timer::default();
                let app = app.clone();
                let weak = ui.as_weak();
                timer.start(slint::TimerMode::Repeated, Duration::from_millis(2), move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let mut app = app.borrow_mut();
                    if app.animating || app.dirty {
                        if let Err(e) = app.render_frame(&ui) {
                            eprintln!("frame failed: {e:#}");
                        }
                    }
                });
                _frame_timer = Some(timer);
            }
        }
    }

    if bench {
        let start = with_app(App::start_measuring);
        slint::Timer::single_shot(Duration::from_millis(1500), start);
    }

    ui.run()?;
    Ok(())
}

/// Renders a scripted spin offscreen at the requested times and writes PNGs.
fn snapshot(options: &Options) -> Result<()> {
    let path = options.snapshot.as_ref().expect("snapshot mode");
    let (w, h) = options.size;
    let scale = options.scale;
    let mut text = TextEngine::new(&font_files(&options.fonts))?;
    let n = options.entries.unwrap_or(12);
    let mut wheel = Wheel::new(labels::entries(options.label_set(), n), vec![1; n]);
    let seed = options.seed.unwrap_or(1);
    let mut outcome = Xoshiro256PlusPlus::seed_from_u64(seed);
    let mut cosmetic = Xoshiro256PlusPlus::seed_from_u64(!seed);
    let winner = draw::draw_weighted(&wheel.weights, &mut outcome);
    let u = cosmetic.random_range(spin::LANDING_MARGIN..=1.0 - spin::LANDING_MARGIN);
    let level = options.speed.unwrap_or(5);
    let spin = Spin::new(&wheel, winner, 0.0, options.duration.unwrap_or(8.0), spin::omega_for_level(level), u);
    let period = 1.0 / 165.0;

    let mut gpu = None;
    if !options.cpu {
        let g = Gpu::new(&options.adapter, options.allow_cpu_adapter)?;
        let stage = GpuStage::new(&g.device, &g.queue)?;
        eprintln!("snapshot on {}", g.describe());
        gpu = Some((g, stage));
    }
    let mut cpu = options.cpu.then(|| CpuStage::new(options.threads));

    for &t in &options.at {
        let mut frame = Frame::new(w, h, scale, BACKGROUND, period);
        wheel.render(&mut frame, &mut text, spin.theta(t), spin.omega(t), spin.pointer_tilt(t));
        if options.overlay {
            let lines = vec![format!("snapshot t {t:.3} s · omega {:.2} rad/s", spin.omega(t))];
            overlay::draw(&mut frame.top, &mut text, scale, &lines, &[], (period * 1000.0) as f32);
        }
        let rgba = if let Some((g, stage)) = &mut gpu {
            let texture = stage.render(&g.device, &g.queue, &frame, options.blur)?;
            gpu::read_rgba(&g.device, &g.queue, &texture)?
        } else {
            let stage = cpu.as_mut().expect("cpu stage");
            let mut buffer = vec![0u8; (w * h * 4) as usize];
            stage.render(&frame, &mut buffer);
            unpremultiply(&mut buffer);
            buffer
        };
        let file = if options.at.len() == 1 {
            path.clone()
        } else {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("snapshot");
            path.with_file_name(format!("{stem}-{t:.2}.png"))
        };
        write_png(&file, w, h, &rgba)?;
        println!("{} (t {t:.3} s, omega {:.2} rad/s)", file.display(), spin.omega(t));
    }
    println!("entry drawn: {} · labels drawn {}", wheel.entries[winner], wheel.labels_drawn);
    Ok(())
}

fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u16::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::binomial;

    #[test]
    fn binomial_probabilities_are_exact_and_sum_to_one() {
        assert!((binomial(4, 2, 0.5) - 6.0 / 16.0).abs() < 1e-12);
        assert!((binomial(10, 0, 0.3) - 0.7f64.powi(10)).abs() < 1e-12);
        let total: f64 = (0..=25).map(|x| binomial(25, x, 0.37)).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }
}
