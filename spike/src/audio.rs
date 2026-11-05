//! Wheel ticks scheduled ahead on Kira's audio clock (design doc, § Qualità delle
//! animazioni: "Audio programmato").
//!
//! Kira starts a sound when one of its clocks reaches a given time, but it publishes a
//! clock's time only once per audio callback and reports no output latency. So one clock
//! runs at 1000 ticks per second for as long as `Audio` lives; `ClockSync` learns from the
//! steps of that staircase which clock time the audio thread renders at a given instant,
//! and `TickPlan` picks the ticks to hand over and when. Both work on plain seconds, so
//! they are tested without a device. A thread of its own reads the clock for `ClockSync`
//! after random pauses, so that readings land at every phase of the callbacks, spin or
//! not. The output latency stays unknown: it is part of the per-device `advance`
//! calibration.

use std::collections::VecDeque;
use std::f32::consts::{PI, TAU};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use kira::backend::cpal::CpalBackendSettings;
use kira::backend::cpal::cpal::{
    self,
    traits::{DeviceTrait as _, HostTrait as _},
};
use kira::clock::{ClockHandle, ClockSpeed, ClockTime};
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings};
use kira::{AudioManager, AudioManagerSettings, DefaultBackend, Frame, Tween};

/// How far ahead ticks are handed to the audio thread.
pub const LOOKAHEAD: f64 = 0.200;
/// Above this rate ticks are thinned out (the design doc switches to a ratchet sound).
pub const MAX_TICKS_PER_SECOND: f64 = 25.0;

/// Clock ticks per second: 1 ms ticks, and the fraction carries sub-millisecond targets.
const CLOCK_RATE: f64 = 1000.0;
/// Frames per Kira processing chunk (Kira's default is 128). Sounds start on chunk
/// boundaries, so this is the scheduling resolution: 0.67 ms at 48 kHz.
const CHUNK_FRAMES: usize = 32;
/// Seconds of clock steps the mapping keeps.
const SYNC_WINDOW: f64 = 3.0;
/// Steps needed in the window before the mapping is trusted.
const SYNC_STEPS: usize = 10;
/// When every step of the last SLIP_SPAN seconds (or SYNC_STEPS step gaps, if longer) sits
/// more than SLIP above the older ones, and at least SYNC_STEPS of them are fresh, the
/// audio clock has lost time (an underrun, or Kira restarting the stream on a new default
/// device) and the older steps are dropped.
const SLIP_SPAN: f64 = 0.5;
const SLIP: f64 = 0.008;
/// A step seen at most FRESH after the reading before it was seen at most that late. Only
/// fresh steps can show a slip: between spins readings are sparse and see a step up to a
/// callback period late, and with long periods half a second of them can all sit above
/// the envelope by chance.
const FRESH: f64 = 0.005;
/// With no clock step for STALL seconds (or STALL_GAPS step gaps, if longer: callbacks can
/// be that far apart) the audio thread has stopped, as while Kira restarts the stream, and
/// the mapping is dropped until the clock moves again.
const STALL: f64 = 0.150;
const STALL_GAPS: f64 = 5.0;
/// Range of the observer's random pause while a spin needs the mapping: steps are seen
/// fresh, so the envelope is tight and a slip can show.
const BUSY_PAUSE: (f64, f64) = (0.001, 0.003);
/// Range of its pause between spins: enough readings to keep the mapping, few wakeups.
const IDLE_PAUSE: (f64, f64) = (0.020, 0.040);
/// A tick due longer ago than this is skipped: played that late it would sound wrong.
const LATE_TOLERANCE: f64 = 0.020;
/// Smallest gap between two scheduled ticks, less 1 ns so that ticks exactly at the rate
/// limit survive rounding.
const MIN_GAP: f64 = 1.0 / MAX_TICKS_PER_SECOND - 1e-9;
/// The synthesized tick peaks at −6 dBFS; this plays it at −9 dBFS.
const TICK_VOLUME_DB: f32 = -3.0;

#[derive(Clone, Debug, Default)]
pub struct AudioStatus {
    pub sample_rate: u32,
    /// Whether the audio-clock-to-Instant mapping is estimated: not yet after start, nor
    /// while the audio clock stalls.
    pub synced: bool,
    /// Ticks handed to Kira and not yet played.
    pub pending: usize,
    /// Ticks dropped by the rate limit during the current spin.
    pub thinned: usize,
    /// Peak share of the audio callback's time budget since the latest spin started (since
    /// `Audio` opened, before any), when Kira reported any.
    pub cpu_usage: Option<f32>,
}

/// The wheel's tick sound on the default output device, scheduled on Kira's clock.
pub struct Audio {
    /// First, so that its thread stops before the stream goes.
    observer: Observer,
    manager: AudioManager<DefaultBackend>,
    /// Started once and never stopped or dropped while `Audio` lives: dropping it cancels
    /// every waiting sound, and `stop()` rewinds it to 0. Shared with the observer.
    clock: Arc<ClockHandle>,
    tick: StaticSoundData,
    sample_rate: u32,
    /// Half a processing chunk, in seconds. Kira starts a sound at the start of the chunk
    /// in which its clock reaches the target, up to one chunk early; aiming half a chunk
    /// late centres that rounding.
    half_chunk: f64,
    /// Origin of the seconds `ClockSync` and `TickPlan` work in.
    epoch: Instant,
    plan: TickPlan,
    /// Ticks handed to Kira, until Kira reports them stopped.
    scheduled: Vec<Scheduled>,
    /// Peak share of the callback budget among the reports read since the last `start()`.
    cpu_peak: Option<f32>,
}

impl Audio {
    /// Opens the default output device. Fails when there is none: the app then runs silent.
    pub fn new() -> anyhow::Result<Self> {
        // Kira has no sample-rate getter: read the default config from cpal and give Kira
        // the same one, so the tick is synthesized at the rate it plays at.
        let device = cpal::default_host().default_output_device().context("no default audio output device")?;
        let config = device.default_output_config()?;
        let sample_rate = config.sample_rate();
        let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings {
            internal_buffer_size: CHUNK_FRAMES,
            // No device: naming one would stop Kira from following default-device changes.
            backend_settings: CpalBackendSettings { device: None, config: Some(config.config()) },
            ..Default::default()
        })?;
        let mut clock = manager.add_clock(ClockSpeed::TicksPerSecond(CLOCK_RATE))?;
        clock.start();
        let clock = Arc::new(clock);
        let epoch = Instant::now();
        let observer = Observer::spawn(Arc::clone(&clock), epoch).context("can't start the audio clock observer")?;
        let tick = StaticSoundData {
            sample_rate,
            frames: synth_tick(sample_rate),
            settings: StaticSoundSettings::new().volume(TICK_VOLUME_DB),
            slice: None,
        };
        Ok(Self {
            observer,
            manager,
            clock,
            tick,
            sample_rate,
            half_chunk: CHUNK_FRAMES as f64 / 2.0 / f64::from(sample_rate),
            epoch,
            plan: TickPlan::default(),
            scheduled: Vec::new(),
            cpu_peak: None,
        })
    }

    /// Starts the ticks of a spin. `times`: tick times in seconds from the spin's
    /// t = 0, sorted. `zero`: the instant the frame showing t = 0 began rendering.
    /// `advance`: seconds by which every tick sounds earlier, the per-device
    /// calibration of display latency against audio latency.
    ///
    /// Ticks of the previous spin that have not started are cancelled: a new or
    /// replanned spin replaces it.
    pub fn start(&mut self, times: Vec<f64>, zero: Instant, advance: f64) {
        debug_assert!(times.is_sorted(), "tick times must be sorted");
        self.stop_waiting(|_| true);
        // The CPU peak is per spin. Not per `status()`: the overlay calls that every frame.
        self.drain_cpu_usage();
        self.cpu_peak = None;
        let origin = self.seconds(zero) - advance;
        // Readings every few milliseconds until the last tick is due: they keep the mapping
        // tight and the clock's latest advance one callback long, for `stop_waiting`.
        self.observer.busy_until(times.last().map_or(origin, |last| origin + last));
        self.plan = TickPlan::new(times, origin);
    }

    /// Hands Kira the ticks due before now + LOOKAHEAD, at most MAX_TICKS_PER_SECOND of
    /// them per second. Call it every frame while a spin runs.
    ///
    /// Nothing is handed over while the clock mapping is unsynced, and a tick due more than
    /// 20 ms ago is skipped, not played late. When the mapping is lost because the audio
    /// clock stalled, the ticks waiting in Kira are cancelled: aimed with the old mapping,
    /// they would sound late once the clock moves again.
    pub fn pump(&mut self) {
        self.forget_stopped();
        self.drain_cpu_usage();
        let Some(offset) = self.observer.offset() else {
            self.stop_waiting(|_| true);
            return;
        };
        // The time now, not the frame's start: rendering may have taken milliseconds since.
        let now = self.seconds(Instant::now());
        for (t, at) in self.plan.due(now) {
            let due = at - offset;
            let start = ClockTime::from_ticks_f64(self.clock.id(), (due + self.half_chunk).max(0.0) * CLOCK_RATE);
            // At most LOOKAHEAD × MAX_TICKS_PER_SECOND + 1 ticks wait at once, far below the
            // track's 128 sounds, so the only error (a full track) can't come from ticks.
            match self.manager.play(self.tick.start_time(start)) {
                Ok(handle) => self.scheduled.push(Scheduled { t, due, handle }),
                Err(err) => eprintln!("audio: tick at {t:.3} s dropped: {err}"),
            }
        }
    }

    /// Manual stop: cancels the ticks already scheduled after spin time `t` and
    /// forgets the remaining ones.
    ///
    /// A tick that has started, or may have, plays out.
    #[allow(dead_code)] // the manual stop (F7) comes with v1; the spike does not wire it
    pub fn cancel_after(&mut self, t: f64) {
        self.plan.cancel_after(t);
        self.stop_waiting(|tick| tick > t);
    }

    /// `cpu_usage` is the peak since the latest spin started.
    pub fn status(&mut self) -> AudioStatus {
        self.forget_stopped();
        self.drain_cpu_usage();
        AudioStatus {
            sample_rate: self.sample_rate,
            synced: self.observer.offset().is_some(),
            pending: self.scheduled.iter().filter(|s| s.waiting()).count(),
            thinned: self.plan.thinned,
            cpu_usage: self.cpu_peak,
        }
    }

    /// Cancels the scheduled ticks whose spin time matches and that Kira has not started.
    ///
    /// A started tick plays out, since cutting it would click. Kira reports a start only at
    /// the callback after it, so a tick still at position 0 may be playing already: the
    /// latest callback rendered clock time from its reading to about one clock advance
    /// later, and the ticks due before that are left to play whole.
    fn stop_waiting(&mut self, cancel: impl Fn(f64) -> bool) {
        let rendered_until = self.clock_seconds() + self.observer.advance();
        // Kira's default tween is a 10 ms fade, which lets a tick due within it leak out.
        let at_once = Tween { duration: Duration::ZERO, ..Default::default() };
        for s in &mut self.scheduled {
            if cancel(s.t) && s.waiting() && s.due >= rendered_until {
                s.handle.stop(at_once);
            }
        }
    }

    fn forget_stopped(&mut self) {
        self.scheduled.retain(|s| s.handle.state() != PlaybackState::Stopped);
    }

    /// Folds Kira's CPU reports into `cpu_peak`. Kira queues one per callback and drops new
    /// ones once 100 wait, so `pump` reads them every frame.
    fn drain_cpu_usage(&mut self) {
        let backend = self.manager.backend_mut();
        while let Some(usage) = backend.pop_cpu_usage() {
            self.cpu_peak = Some(self.cpu_peak.map_or(usage, |peak| peak.max(usage)));
        }
    }

    /// The clock's time in seconds, as published by the latest audio callback.
    fn clock_seconds(&self) -> f64 {
        // A read that raced a publication is retried: the next one comes milliseconds later.
        loop {
            if let Some(seconds) = read_clock(&self.clock) {
                return seconds;
            }
        }
    }

    /// Seconds from `epoch` to `instant`, negative before it.
    fn seconds(&self, instant: Instant) -> f64 {
        match instant.checked_duration_since(self.epoch) {
            Some(after) => after.as_secs_f64(),
            None => -(self.epoch - instant).as_secs_f64(),
        }
    }
}

/// A tick handed to Kira.
struct Scheduled {
    /// Spin time of the tick.
    t: f64,
    /// Clock time, in seconds, at which the tick should sound.
    due: f64,
    handle: StaticSoundHandle,
}

impl Scheduled {
    /// Kira reports a sound waiting for its start time as playing at position exactly 0;
    /// the position moves at the first callback after the sound starts.
    fn waiting(&self) -> bool {
        self.handle.position() == 0.0 && self.handle.state() != PlaybackState::Stopped
    }
}

/// The clock's time in seconds as the latest audio callback published it, or None when the
/// read raced the publication: Kira stores the ticks and the fraction one after the other,
/// and a read between the two stores mixes two callbacks' times, up to 1 ms off.
fn read_clock(clock: &ClockHandle) -> Option<f64> {
    let (time, again) = (clock.time(), clock.time());
    (time == again).then(|| (time.ticks as f64 + time.fraction) / CLOCK_RATE)
}

/// Reads the audio clock on a thread of its own, after random pauses, and publishes what
/// `ClockSync` makes of it.
///
/// Readings taken at a steady rate, as by a frame loop, can lock to the callback period
/// and see every step equally late: the envelope then settles that late. Readings must
/// also go on between spins, or the mapping expires with its window.
struct Observer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

/// What the observer thread and `Audio` share.
struct Shared {
    /// `ClockSync::offset` after the latest reading, NaN while unsynced.
    offset: AtomicF64,
    /// `ClockSync::advance` after the latest reading.
    advance: AtomicF64,
    /// Until when, in `Audio::seconds`, a spin needs the mapping: pauses are short until then.
    busy_until: AtomicF64,
    /// Set when `Audio` is dropped.
    quit: AtomicBool,
}

impl Observer {
    fn spawn(clock: Arc<ClockHandle>, epoch: Instant) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            offset: AtomicF64::new(f64::NAN),
            advance: AtomicF64::new(0.0),
            busy_until: AtomicF64::new(f64::NEG_INFINITY),
            quit: AtomicBool::new(false),
        });
        let thread = thread::Builder::new().name("audio clock".into()).spawn({
            let shared = Arc::clone(&shared);
            move || observe(&clock, epoch, &shared)
        })?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// The instant, in `Audio::seconds`, at which the audio thread renders clock time 0.
    fn offset(&self) -> Option<f64> {
        Some(self.shared.offset.load()).filter(|offset| !offset.is_nan())
    }

    /// Clock seconds the latest callback seen renders, or a few callbacks' between spins.
    fn advance(&self) -> f64 {
        self.shared.advance.load()
    }

    fn busy_until(&self, until: f64) {
        self.shared.busy_until.store(until);
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            // The thread checks `quit` after every pause, so this waits one pause at most.
            let _ = thread.join();
        }
    }
}

/// The observer thread's loop.
fn observe(clock: &ClockHandle, epoch: Instant, shared: &Shared) {
    let mut sync = ClockSync::default();
    // Any seed does: the pauses need only be unrelated to the callbacks.
    let mut rng = 0x2545_F491_4F6C_DD1D;
    while !shared.quit.load(Ordering::Relaxed) {
        let reading = read_clock(clock);
        // Stamped right after the read: a stamp from before it could predate the step it
        // sees, and the envelope relies on stamps never being early.
        let seen = epoch.elapsed().as_secs_f64();
        if let Some(reading) = reading {
            sync.observe(seen, reading);
            shared.offset.store(sync.offset().unwrap_or(f64::NAN));
            shared.advance.store(sync.advance);
        }
        // thread::sleep, not a timed wait: on Windows only sleep uses the high-resolution
        // timer, which millisecond pauses need.
        thread::sleep(Duration::from_secs_f64(pause(&mut rng, seen < shared.busy_until.load())));
    }
}

/// The observer's pause before its next reading, in seconds: random, so that readings land
/// at every phase of the audio callbacks whatever their period.
fn pause(rng: &mut u64, busy: bool) -> f64 {
    let (shortest, longest) = if busy { BUSY_PAUSE } else { IDLE_PAUSE };
    shortest + (longest - shortest) * uniform(rng)
}

/// Uniform in [0, 1), from splitmix64: cheap, and the observer's pauses need no quality
/// beyond being unrelated to the audio callbacks.
fn uniform(state: &mut u64) -> f64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    ((z ^ (z >> 31)) >> 11) as f64 / (1_u64 << 53) as f64
}

/// An f64 shared between threads as its bits. Relaxed: each value stands alone, and
/// nothing else is published through it.
struct AtomicF64(AtomicU64);

impl AtomicF64 {
    fn new(value: f64) -> Self {
        Self(AtomicU64::new(value.to_bits()))
    }

    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }

    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}

/// Learns which audio-clock time the audio thread renders at a given instant.
///
/// Each audio callback publishes the clock time at the start of the buffer it renders,
/// so a reading is seen some time after the callback that published it: `seen − reading`
/// is the callbacks' offset plus that delay, never less. With readings at every phase of
/// the callbacks, the lower envelope of `seen − reading` over a few seconds of steps
/// converges on the offset, and clock time c is rendered at `offset + c`. Only steps
/// count: an unchanged reading may be old, and the first one may be the 0 published
/// before the clock started.
#[derive(Debug, Default)]
struct ClockSync {
    last_reading: Option<f64>,
    /// When `last_reading` was taken.
    last_seen: f64,
    /// The steps in the window, oldest first.
    steps: VecDeque<Step>,
    /// Median wall time between consecutive steps in the window: the callback period when
    /// readings come more often than callbacks, the time between readings otherwise.
    gap: f64,
    /// Clock seconds the latest step advanced: one callback's buffer, or a few callbacks'
    /// when readings are sparse.
    advance: f64,
}

/// A clock reading that differs from the one before it.
#[derive(Clone, Copy, Debug)]
struct Step {
    seen: f64,
    /// `seen − reading`.
    offset: f64,
    /// Seen at most FRESH after the reading before it, so at most FRESH late.
    fresh: bool,
}

impl ClockSync {
    /// Records a clock `reading`, in seconds, taken at or before `seen`.
    fn observe(&mut self, seen: f64, reading: f64) {
        let since_last = seen - std::mem::replace(&mut self.last_seen, seen);
        // Exact comparison: an unchanged reading is the very same published value.
        let Some(last) = self.last_reading.replace(reading).filter(|&last| last != reading) else {
            // No step. Without one for a while the audio thread has stopped, and a mapping
            // kept from before would aim ticks at a clock that isn't moving. The latest
            // advance stands in for the gap until there are two steps to measure it.
            let stall = STALL.max(STALL_GAPS * self.gap.max(self.advance));
            if self.steps.back().is_some_and(|step| seen - step.seen > stall) {
                self.steps.clear();
            }
            return;
        };
        self.advance = reading - last;
        self.steps.push_back(Step { seen, offset: seen - reading, fresh: since_last <= FRESH });
        while self.steps.front().is_some_and(|step| step.seen < seen - SYNC_WINDOW) {
            self.steps.pop_front();
        }
        self.gap = median_gap(&self.steps);
        let span = SLIP_SPAN.max(SYNC_STEPS as f64 * self.gap);
        let recent = self.steps.partition_point(|step| step.seen < seen - span);
        let fresh = self.steps.range(recent..).filter(|step| step.fresh).count();
        if fresh >= SYNC_STEPS && lowest(self.steps.range(recent..)) > lowest(self.steps.range(..recent)) + SLIP {
            self.steps.drain(..recent);
        }
    }

    /// The instant (in the caller's seconds) at which the audio thread renders clock
    /// time 0, once enough steps are in the window.
    fn offset(&self) -> Option<f64> {
        (self.steps.len() >= SYNC_STEPS).then(|| lowest(self.steps.iter()))
    }
}

fn lowest<'a>(steps: impl Iterator<Item = &'a Step>) -> f64 {
    steps.map(|step| step.offset).fold(f64::INFINITY, f64::min)
}

/// Median wall time between consecutive steps, 0 with fewer than two.
fn median_gap(steps: &VecDeque<Step>) -> f64 {
    let mut gaps: Vec<f64> = steps.iter().zip(steps.iter().skip(1)).map(|(a, b)| b.seen - a.seen).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    let middle = gaps.len() / 2;
    *gaps.select_nth_unstable_by(middle, f64::total_cmp).1
}

/// Which ticks of a spin to hand over, and when.
#[derive(Debug, Default)]
struct TickPlan {
    /// Tick times in spin seconds, sorted.
    times: Vec<f64>,
    /// When spin time 0 must sound, in `Audio::seconds`: `zero − advance`.
    origin: f64,
    /// First tick neither handed over nor skipped yet.
    next: usize,
    /// Spin time of the last tick handed over.
    last: Option<f64>,
    /// Ticks skipped by the rate limit.
    thinned: usize,
}

impl TickPlan {
    fn new(times: Vec<f64>, origin: f64) -> Self {
        Self { times, origin, ..Self::default() }
    }

    /// The ticks to hand over at `now`, as (spin time, instant it must sound): those due
    /// before `now + LOOKAHEAD`, except the ones over LATE_TOLERANCE late and those
    /// closer than 1 / MAX_TICKS_PER_SECOND to the previous one.
    fn due(&mut self, now: f64) -> Vec<(f64, f64)> {
        let mut due = Vec::new();
        while let Some(&t) = self.times.get(self.next) {
            let at = self.origin + t;
            if at > now + LOOKAHEAD {
                break;
            }
            self.next += 1;
            if at < now - LATE_TOLERANCE {
                continue;
            }
            if self.last.is_some_and(|last| t - last < MIN_GAP) {
                self.thinned += 1;
                continue;
            }
            self.last = Some(t);
            due.push((t, at));
        }
        due
    }

    /// Forgets the ticks after spin time `t` that are not handed over yet.
    #[allow(dead_code)] // see Audio::cancel_after
    fn cancel_after(&mut self, t: f64) {
        let kept = self.times.partition_point(|&x| x <= t);
        self.times.truncate(kept.max(self.next));
    }
}

/// A 30 ms "plastic flapper" tick at `sample_rate`: a high-passed noise snap, two
/// decaying partials and a low knock, with raised-cosine edges so it starts and ends
/// on zero, peaking at −6 dBFS.
fn synth_tick(sample_rate: u32) -> Arc<[Frame]> {
    let rate = sample_rate as f32;
    let len = (0.030 * rate).round() as usize;
    let attack = (0.0003 * rate).max(1.0);
    let release = (0.002 * rate).max(1.0);
    // First-order high-pass at 1.5 kHz, applied to the noise.
    let rc = 1.0 / (TAU * 1500.0);
    let high_pass = rc / (rc + 1.0 / rate);
    // xorshift32 with a fixed seed: the same tick every run.
    let mut seed = 0x9E37_79B9_u32;
    let (mut noise_in, mut noise_out) = (0.0_f32, 0.0_f32);
    let samples: Vec<f32> = (0..len)
        .map(|i| {
            let t = i as f32 / rate;
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let white = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
            noise_out = high_pass * (noise_out + white - noise_in);
            noise_in = white;
            let snap = 0.5 * noise_out * (-t / 0.0008).exp();
            let body = 0.6 * (TAU * 2400.0 * t).sin() * (-t / 0.004).exp()
                + 0.3 * (TAU * 5300.0 * t).sin() * (-t / 0.0015).exp();
            let knock = 0.25 * (TAU * 900.0 * t).sin() * (-t / 0.006).exp();
            let edges = rise(i as f32 / attack) * rise((len - 1 - i) as f32 / release);
            (snap + body + knock) * edges
        })
        .collect();
    let peak = samples.iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
    samples.into_iter().map(|x| Frame::from_mono(x * 0.5 / peak)).collect()
}

/// Half a raised cosine: 0 at x = 0, 1 from x = 1 on.
fn rise(x: f32) -> f32 {
    if x < 1.0 { 0.5 - 0.5 * (PI * x).cos() } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

    use super::*;

    /// 48 kHz with 512-frame callbacks, as `device_smoke` measures under PipeWire.
    const PERIOD: f64 = 512.0 / 48_000.0;
    /// A 144 Hz frame loop.
    const POLL: f64 = 1.0 / 144.0;

    /// Uniform in [0, 1) from a hash of `n`: repeatable jitter.
    fn noise(n: u64) -> f64 {
        let mut state = n;
        uniform(&mut state)
    }

    /// Audio callbacks every `period`: callback k publishes clock time k·period at
    /// `offset + k·period`, up to `jitter` late, and `slip` later from callback
    /// `slip_from` on, the clock having stood still in between.
    struct Callbacks {
        period: f64,
        offset: f64,
        jitter: f64,
        slip: f64,
        slip_from: u64,
    }

    impl Callbacks {
        fn steady(offset: f64, jitter: f64) -> Self {
            Self { period: PERIOD, offset, jitter, slip: 0.0, slip_from: u64::MAX }
        }

        fn at(&self, k: u64) -> f64 {
            let slip = if k >= self.slip_from { self.slip } else { 0.0 };
            self.offset + slip + k as f64 * self.period + self.jitter * noise(k)
        }

        /// The clock reading at `seen`: 0 until the first callback.
        fn reading(&self, seen: f64) -> f64 {
            // Jitter and slip only delay callbacks, so none after the one the plain
            // schedule has due by `seen` can have come.
            let mut k = ((seen - self.offset) / self.period).floor();
            while k >= 0.0 && self.at(k as u64) > seen {
                k -= 1.0;
            }
            k.max(0.0) * self.period
        }
    }

    /// Reads the clock like the observer thread, from time 0: random pauses, each
    /// overshooting by up to 0.2 ms as sleeps do.
    struct Reader {
        sync: ClockSync,
        seen: f64,
        rng: u64,
        busy: bool,
    }

    impl Reader {
        fn new(seed: u64, busy: bool) -> Self {
            Self { sync: ClockSync::default(), seen: 0.0, rng: seed, busy }
        }

        /// Takes one reading of `callbacks`; returns the mapping's offset after it.
        fn read(&mut self, callbacks: &Callbacks) -> Option<f64> {
            self.sync.observe(self.seen, callbacks.reading(self.seen));
            self.seen += pause(&mut self.rng, self.busy) + 0.0002 * uniform(&mut self.rng);
            self.sync.offset()
        }

        /// Reads `callbacks` until `to`; returns the mapping's offset then.
        fn read_until(&mut self, callbacks: &Callbacks, to: f64) -> Option<f64> {
            while self.seen < to {
                self.read(callbacks);
            }
            self.sync.offset()
        }
    }

    #[test]
    fn clock_mapping_converges_on_a_jittery_staircase() {
        // Readings start before the clock does, when it still reads 0.
        let callbacks = Callbacks::steady(0.25, 0.001);
        let mut reader = Reader::new(1, true);
        let early = callbacks.at(SYNC_STEPS as u64 - 1);
        assert_eq!(reader.read_until(&callbacks, early), None, "synced before {SYNC_STEPS} steps");
        let offset = reader.read_until(&callbacks, 2.0).expect("not synced after 2 s");
        // Never early, and within a fraction of a millisecond of the callbacks' schedule.
        assert!((0.25..0.2505).contains(&offset), "offset {offset}");
    }

    #[test]
    fn readings_at_random_phases_converge_where_a_locked_frame_loop_does_not() {
        // A frame loop as fast as the callbacks, 4 ms after each on their schedule: it
        // sees every step equally late, and its envelope settles 4 ms late.
        let callbacks = Callbacks::steady(0.25, 0.0005);
        let mut locked = ClockSync::default();
        for frame in 0..600 {
            let seen = 0.254 + f64::from(frame) * PERIOD;
            locked.observe(seen, callbacks.reading(seen));
        }
        let late = locked.offset().unwrap() - 0.25;
        assert!(late > 0.003, "the locked loop settled only {late} s late: the test can't tell");

        // The observer's random pauses, on the same callbacks for the same 6.4 s.
        // Sparse readings take longer to find the envelope; a spin reads every few ms.
        for (busy, tolerance) in [(true, 0.0003), (false, 0.0015)] {
            let late = Reader::new(2, busy).read_until(&callbacks, 0.254 + 600.0 * PERIOD).unwrap() - 0.25;
            assert!((0.0..tolerance).contains(&late), "busy {busy}: settled {late} s late");
        }
    }

    #[test]
    fn clock_mapping_follows_a_slip() {
        // From callback 300 (3.45 s) on, the clock runs one period behind, as after an
        // underrun: the smallest slip there is.
        let callbacks = Callbacks { slip: PERIOD, slip_from: 300, ..Callbacks::steady(0.25, 0.001) };
        let mut reader = Reader::new(3, true);
        let slip_at = callbacks.at(300);
        assert!((0.25..0.2505).contains(&reader.read_until(&callbacks, slip_at).unwrap()));
        // Only the steps since the slip are left, so the envelope is a little coarser.
        let offset = reader.read_until(&callbacks, slip_at + 0.6).unwrap() - PERIOD;
        assert!((0.25..0.251).contains(&offset), "offset {offset} 0.6 s after the slip");
    }

    #[test]
    fn long_callbacks_keep_the_mapping_and_show_a_slip() {
        // 200 ms callbacks, further apart than STALL, slipping one period from callback 30
        // (6.45 s) on: a slip shows only once SYNC_STEPS steps came after it.
        let callbacks = Callbacks { period: 0.2, slip: 0.2, slip_from: 30, ..Callbacks::steady(0.25, 0.001) };
        let mut reader = Reader::new(4, true);
        let slip_at = callbacks.at(30);
        let offset = reader.read_until(&callbacks, slip_at).expect("lost the mapping between callbacks");
        assert!((0.25..0.252).contains(&offset), "offset {offset}");
        let offset = reader.read_until(&callbacks, slip_at + 2.1).unwrap() - 0.2;
        assert!((0.25..0.252).contains(&offset), "offset {offset} 2.1 s after the slip");
    }

    #[test]
    fn sparse_readings_never_fake_a_slip() {
        // With 2048-frame callbacks, readings between spins see each step anywhere up to
        // 40 ms late, and half a second of them often all sits more than SLIP above the
        // envelope.
        let callbacks = Callbacks { period: 2048.0 / 48_000.0, ..Callbacks::steady(0.25, 0.001) };
        let mut reader = Reader::new(5, false);
        reader.read_until(&callbacks, SYNC_WINDOW);
        let mut latest = 0.0_f64;
        while reader.seen < 120.0 {
            latest = latest.max(reader.read(&callbacks).expect("lost the mapping") - 0.25);
        }
        // Sparse readings leave the envelope at most a few ms late; a fake slip, over SLIP.
        assert!(latest < 0.006, "the mapping went {latest} s late");
    }

    #[test]
    fn a_stalled_clock_drops_the_mapping_until_it_moves_again() {
        // After callback 199 the audio thread stops for 0.4 s, as while Kira restarts the
        // stream on a new default device; the clock then goes on from where it stood.
        let callbacks = Callbacks { slip: 0.4, slip_from: 200, ..Callbacks::steady(0.25, 0.001) };
        let mut reader = Reader::new(6, true);
        let stopped = callbacks.at(199);
        let offset = reader.read_until(&callbacks, stopped + STALL - 0.01);
        assert!(offset.is_some_and(|offset| (0.25..0.2505).contains(&offset)), "lost the mapping too soon: {offset:?}");
        let offset = reader.read_until(&callbacks, stopped + STALL + 0.01);
        assert_eq!(offset, None, "still synced {STALL} s into the stall");
        let resumed = callbacks.at(200);
        assert_eq!(reader.read_until(&callbacks, resumed), None, "synced while the clock stood still");
        let offset = reader.read_until(&callbacks, resumed + (SYNC_STEPS + 1) as f64 * PERIOD);
        let offset = offset.expect("not synced again") - 0.4;
        assert!((0.25..0.252).contains(&offset), "offset {offset} after the stall");
    }

    /// Runs `plan` like a 144 Hz frame loop from `from` until `to`; returns the spin times
    /// handed over.
    fn run(plan: &mut TickPlan, from: f64, to: f64) -> Vec<f64> {
        let mut handed = Vec::new();
        let mut now = from;
        while now < to {
            handed.extend(plan.due(now).into_iter().map(|(t, _)| t));
            now += POLL;
        }
        handed
    }

    fn spin_times(due: Vec<(f64, f64)>) -> Vec<f64> {
        due.into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn thinning_keeps_at_most_25_ticks_per_second() {
        let fast: Vec<f64> = (0..300).map(|i| f64::from(i) / 100.0).collect();
        let mut plan = TickPlan::new(fast, 0.0);
        let handed = run(&mut plan, -0.5, 3.5);
        assert_eq!(handed.len() + plan.thinned, 300);
        assert!(handed.windows(2).all(|w| w[1] - w[0] >= MIN_GAP));
        for second in 0..3 {
            let from = f64::from(second);
            let in_second = handed.iter().filter(|&&t| (from..from + 1.0).contains(&t)).count();
            assert!(in_second <= 25, "{in_second} ticks in second {second}");
        }

        // Exactly at the limit, nothing is thinned.
        let at_limit: Vec<f64> = (0..75).map(|i| f64::from(i) / MAX_TICKS_PER_SECOND).collect();
        let mut plan = TickPlan::new(at_limit, 0.0);
        assert_eq!(run(&mut plan, -0.5, 3.5).len(), 75);
        assert_eq!(plan.thinned, 0);
    }

    #[test]
    fn ticks_more_than_20_ms_late_are_skipped() {
        // Spin time t must sound at 1 + t.
        let mut plan = TickPlan::new(vec![0.0, 0.1, 0.181, 0.25, 0.39, 0.45, 0.6, 0.7], 1.0);
        assert_eq!(spin_times(plan.due(1.2)), [0.181, 0.25, 0.39]);
        assert_eq!(spin_times(plan.due(1.3)), [0.45]);
        assert_eq!(spin_times(plan.due(1.69)), [0.7]);
        assert_eq!(plan.thinned, 0);
    }

    #[test]
    fn cancel_after_keeps_the_earlier_ticks() {
        let tenths: Vec<f64> = (0..=20).map(|i| f64::from(i) / 10.0).collect();
        let mut plan = TickPlan::new(tenths.clone(), 0.0);
        assert_eq!(spin_times(plan.due(0.0)), [0.0, 0.1, 0.2]);
        plan.cancel_after(0.55);
        assert_eq!(run(&mut plan, 0.0, 3.0), [0.3, 0.4, 0.5]);

        // Cancelling before ticks already handed over leaves them to `Audio`, which stops
        // their sounds; the plan just hands over nothing more.
        let mut plan = TickPlan::new(tenths, 0.0);
        assert_eq!(spin_times(plan.due(0.0)), [0.0, 0.1, 0.2]);
        plan.cancel_after(0.05);
        assert!(run(&mut plan, 0.0, 3.0).is_empty());
    }

    #[test]
    fn tick_is_30_ms_from_zero_to_zero_peaking_at_minus_6_dbfs() {
        let tick = synth_tick(48_000);
        assert_eq!(tick.len(), 1440);
        assert_eq!((tick[0].left, tick[tick.len() - 1].left), (0.0, 0.0));
        let peak = tick.iter().fold(0.0_f32, |peak, f| peak.max(f.left.abs()));
        assert!((peak - 0.5).abs() < 1e-6, "peak {peak}");
    }

    /// Min, median and max, in milliseconds.
    fn spread(values: &[f64]) -> String {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let ms = |i: usize| sorted[i] * 1e3;
        format!("{:.3} / {:.3} / {:.3} ms", ms(0), ms(sorted.len() / 2), ms(sorted.len() - 1))
    }

    /// Plays a fake spin on the default output device. Prints when the observer synced,
    /// the clock staircase, how steady the mapping is and when each tick started against
    /// its due time, then checks that a manual stop lets a tick that may be playing play
    /// whole. Run it with `cargo test device_smoke -- --ignored --nocapture`.
    #[test]
    #[ignore = "plays sound on the default output device"]
    fn device_smoke() {
        let device = cpal::default_host().default_output_device().expect("no output device");
        let config = device.default_output_config().expect("no default output config");
        println!(
            "default output: {} Hz, {} channels, buffer {:?}",
            config.sample_rate(),
            config.channels(),
            config.buffer_size()
        );
        let mut audio = Audio::new().expect("can't open the output");

        // The first second, idle for the observer. Alongside it, a reader pausing like a
        // busy observer times the staircase and gives a reference envelope.
        let mut reference = ClockSync::default();
        let mut rng = 6;
        let mut synced_at = None;
        while audio.seconds(Instant::now()) < 1.0 {
            if let Some(reading) = read_clock(&audio.clock) {
                reference.observe(audio.seconds(Instant::now()), reading);
            }
            if synced_at.is_none() && audio.observer.offset().is_some() {
                synced_at = Some(audio.seconds(Instant::now()));
            }
            sleep(Duration::from_secs_f64(pause(&mut rng, true)));
        }
        let envelope = reference.offset().expect("the reference reader isn't synced after 1 s");
        let synced_at = synced_at.expect("the observer isn't synced after 1 s");
        let idle_error = audio.observer.offset().unwrap() - envelope;
        let steps: Vec<Step> = reference.steps.iter().copied().collect();
        let reading = |step: &Step| step.seen - step.offset;
        let gaps: Vec<f64> = steps.windows(2).map(|w| w[1].seen - w[0].seen).collect();
        let advances: Vec<f64> = steps.windows(2).map(|w| reading(&w[1]) - reading(&w[0])).collect();
        let lateness: Vec<f64> = steps.iter().map(|step| step.offset - envelope).collect();
        println!("{} steps in the first second (min / median / max):", steps.len());
        println!("  wall time between steps {}", spread(&gaps));
        println!("  clock advance per step  {}", spread(&advances));
        println!("  step seen after the envelope by {}", spread(&lateness));
        println!(
            "  observer synced at {:.0} ms; after 1 s of idle readings it is {:+.3} ms off the reference",
            synced_at * 1e3,
            idle_error * 1e3
        );
        let first: Vec<String> =
            steps.iter().take(6).map(|s| format!("{:.1}→{:.3}", s.seen * 1e3, reading(s) * 1e3)).collect();
        println!("  first steps, wall ms → clock ms: {}", first.join(", "));

        // A 2 s spin slowing from 40 ticks/s, pumped like a 165 Hz frame loop. A manual stop
        // cancels the ticks after 1.5 s as soon as the first of them is scheduled.
        let times: Vec<f64> = (0..40).map(|i| 2.0 * (1.0 - (1.0 - (f64::from(i) + 0.5) / 40.0).sqrt())).collect();
        let zero = Instant::now() + Duration::from_millis(50);
        audio.start(times.clone(), zero, 0.0);
        let mut scheduled: Vec<f64> = Vec::new();
        let mut started: Vec<(f64, f64)> = Vec::new();
        let mut cancelled: Option<Vec<f64>> = None;
        // The observer's estimates against the reference: in the first 0.1 s, while its
        // idle envelope sharpens, and after.
        let (mut sharpening, mut settled) = (Vec::new(), Vec::new());
        let settled_from = Instant::now() + Duration::from_millis(100);
        while Instant::now() < zero + Duration::from_millis(2450) {
            let estimates = if Instant::now() < settled_from { &mut sharpening } else { &mut settled };
            estimates.push(audio.observer.offset().expect("lost the mapping") - envelope);
            if cancelled.is_none() && audio.scheduled.iter().any(|s| s.t > 1.5) {
                let stopped = audio.scheduled.iter().filter(|s| s.t > 1.5).map(|s| s.t).collect();
                let spin_time = audio.seconds(Instant::now()) - audio.plan.origin;
                println!("manual stop at spin time {spin_time:.3} s stops {stopped:?}");
                audio.cancel_after(1.5);
                cancelled = Some(stopped);
            }
            for s in &audio.scheduled {
                if !scheduled.contains(&s.t) {
                    scheduled.push(s.t);
                }
                // Each callback publishes the clock and every sound's position, so the two
                // date the start while the 30 ms tick still plays.
                let (before, position, after) = (audio.clock_seconds(), s.handle.position(), audio.clock_seconds());
                let fresh = position > 0.0 && position < 0.025 && before == after;
                if fresh && !started.iter().any(|&(t, _)| t == s.t) {
                    started.push((s.t, before - position - s.due));
                }
            }
            audio.pump();
            sleep(Duration::from_millis(6));
        }
        sleep(Duration::from_millis(100));
        let status = audio.status();
        let cancelled = cancelled.expect("no tick after 1.5 s was ever scheduled");

        for (i, t) in times.iter().enumerate() {
            let outcome = match started.iter().find(|s| s.0 == *t) {
                Some((_, error)) => format!("started {:+.3} ms from its due time", error * 1e3),
                None if cancelled.contains(t) => "scheduled, cancelled".to_owned(),
                None if scheduled.contains(t) => "scheduled, start not seen".to_owned(),
                None if *t > 1.5 => "dropped by the stop".to_owned(),
                None => "thinned".to_owned(),
            };
            println!("tick {i:2} at {:6.1} ms: {outcome}", t * 1e3);
        }
        let errors: Vec<f64> = started.iter().map(|s| s.1).collect();
        println!("start − due over {} ticks: {}", errors.len(), spread(&errors));
        println!("observer estimate against the reference, first 0.1 s of the spin: {}", spread(&sharpening));
        println!("observer estimate against the reference, rest of the spin: {}", spread(&settled));
        println!("{status:?}");

        let thinned = times.iter().filter(|t| **t <= 1.5 && !scheduled.contains(t)).count();
        assert_eq!(thinned, status.thinned, "ticks neither scheduled nor thinned");
        assert!(cancelled.iter().all(|t| !started.iter().any(|s| s.0 == *t)), "a cancelled tick played");
        assert!(errors.iter().all(|e| e.abs() < audio.half_chunk + 0.0002), "a tick started off its due time");
        assert_eq!(status.pending, 0);

        // A manual stop just after the latest callback reached a tick, which Kira reports
        // only at the next callback: the tick must play whole, not be cut a few ms in.
        audio.start(vec![0.25], Instant::now(), 0.0);
        let deadline = Instant::now() + Duration::from_secs(1);
        let (due_in, advance) = loop {
            assert!(Instant::now() < deadline, "the tick never came within the latest callback");
            audio.pump();
            let (reading, advance) = (audio.clock_seconds(), audio.observer.advance());
            if let Some(s) = audio.scheduled.iter().find(|s| s.waiting() && s.due < reading + advance) {
                break (s.due - reading, advance);
            }
            sleep(Duration::from_micros(200));
        };
        audio.cancel_after(0.0);
        let tick = audio.scheduled.iter().find(|s| s.t == 0.25).expect("the tick was forgotten");
        // Cut, it would stop at the callback that first reports its position, within the
        // microseconds Kira takes to render a buffer. Played whole, the 30 ms tick goes on
        // for at least one more callback.
        let mut playing = None;
        while tick.handle.state() != PlaybackState::Stopped && Instant::now() < deadline {
            if tick.handle.position() > 0.0 {
                playing.get_or_insert((Instant::now(), Instant::now())).1 = Instant::now();
            }
            sleep(Duration::from_micros(200));
        }
        let playing_for = playing.map_or(0.0, |(first, last)| (last - first).as_secs_f64());
        println!(
            "manual stop with a tick due {:.3} ms into the latest callback's {:.3} ms: seen playing for {:.1} ms",
            due_in * 1e3,
            advance * 1e3,
            playing_for * 1e3
        );
        assert!(playing_for > advance / 2.0, "the tick was cut");
        assert_eq!(tick.handle.state(), PlaybackState::Stopped, "the tick never ended");
    }
}
