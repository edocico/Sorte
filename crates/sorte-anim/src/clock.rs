//! The vsync-quantized animation clock (design doc, § Qualità delle animazioni and
//! § Architettura, "Ciclo di frame").
//!
//! Slint, winit and wgpu report no vsync count or presentation time, so the clock fits a
//! grid of refreshes to the instants at which frames start, and animated time advances by
//! whole refresh periods: jitter in those instants never reaches the picture. The grid's
//! period is fitted online, because the monitor's nominal rate is not the real one
//! (165.001 Hz nominal against 164.70 Hz of hardware vblanks on the M0 machine); counting at
//! the nominal rate would let the picture drift away from the audio, which is scheduled on
//! wall-clock time.
//!
//! Each rule answers something the M0 spike measured on hardware (docs/api-notes.md,
//! "Frame timing"): anchored rounding with a phase loop, because in steady state frames start
//! within ±0.1 period of a grid; a least-squares period fit over ~2 s of in-lock frames,
//! because the nominal rate is off; a warm-up after every re-anchor, because after idleness
//! the first frames come in a burst while the swapchain refills, yet each is shown on a
//! refresh of its own; re-centring a grid that settled whole periods off the frames, so real
//! drops still count; stalls that advance time by the wall-clock gap; a gentle pull towards
//! wall time while the fit converges; and a free-running mode for irregular cadences (VRR).
//!
//! Two rules came after the spike, for inputs its machine never produced. A nominal rate in
//! whole hertz (Windows; macOS when it falls back to the display link) can be off by up to
//! 1 Hz: 59.94 Hz reads 59, more than the phase loop can follow below 100 Hz, so such a rate
//! is checked against a line fitted through the frames, and replaced when it is off. And
//! while the monitor's rate is unknown and not yet seeded from the frames, time follows the
//! wall clock instead of an assumed 60 Hz.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::Seconds;

/// Gaps longer than max(STALL_SECONDS, STALL_PERIODS · P) are stalls or idleness.
const STALL_SECONDS: Seconds = 0.25;
const STALL_PERIODS: f64 = 30.0;
/// Frames after a (re)anchor that count one refresh each, while the swapchain refills;
/// the grid anchors on the last half of them.
const WARMUP_FRAMES: u32 = 16;
/// A frame within this many periods of its grid point is "in lock".
const LOCK_BAND: f64 = 0.25;
/// How much of each in-lock residual the phase loop absorbs.
const PHASE_GAIN: f64 = 0.05;
/// Frames a whole-period offset must hold before the grid is re-centred.
const RECENTER_FRAMES: u32 = 8;
/// In-lock frames after an anchor that the period fit ignores.
const FIT_SKIP_FRAMES: u32 = 30;
/// The period fit: samples needed, time span needed, and the sliding window.
const FIT_MIN_SAMPLES: usize = 90;
const FIT_MIN_SPAN_SECONDS: Seconds = 1.5;
const FIT_WINDOW_SECONDS: Seconds = 3.0;
/// A fitted period further than this from the reference one (the nominal period, or the
/// period the rate check measured) is rejected.
const FIT_TOLERANCE: f64 = 0.01;
/// A nominal rate in whole hertz can be off by up to 1 Hz: Windows reports whole hertz,
/// truncating some rates (59.94 Hz reads 59 or 60), and so does macOS's fallback. Below this
/// rate 1 Hz is more than FIT_TOLERANCE, so such rates are checked against the frames.
const CHECK_BELOW_HZ: f64 = 1.0 / FIT_TOLERANCE;
/// The rate check measures the period on this many frames (a second at 60 Hz): fewer would
/// let irregular frames pass for a grid (see `measure_period`).
const CHECK_FRAMES: usize = 60;
/// A measurement counts only if the frames' RMS distance from the fitted line, in periods,
/// is within this, and changes from frame to frame as jitter does: Durbin and Watson's ratio
/// at least this (see `measure_period`).
const CHECK_MAX_SCATTER: f64 = LOCK_BAND / 2.0;
const CHECK_MIN_DURBIN_WATSON: f64 = 1.0;
/// A measurement within this share of the reference period confirms it: the phase loop holds
/// lock on it, with a residual of error / PHASE_GAIN = 0.1 periods at most, and the fit takes
/// it from there. Up to FIT_TOLERANCE itself the residual would reach 0.2, too close to
/// LOCK_BAND with any jitter.
const CHECK_CONFIRM: f64 = FIT_TOLERANCE / 2.0;
/// Two measurements in a row further off than that, which agree within this share, replace
/// the reference period...
const CHECK_AGREEMENT: f64 = FIT_TOLERANCE / 4.0;
/// ...if they are within this many hertz of the nominal rate: its 1 Hz of error, plus a
/// margin for the measurement's (59.99 Hz truncated to 59 can measure above 60).
const CHECK_BAND_HZ: f64 = 1.5;
/// Share of the gap between animated and wall-clock time closed at each frame. It keeps
/// t on the audio's clock while the period fit is still converging; with the right
/// period the gap stays near zero and so do the corrections.
const WALL_GAIN: f64 = 0.02;
/// Residual history used to detect an irregular cadence.
const MISS_HISTORY: usize = 64;
/// Shares of out-of-lock frames in that history that enter and leave Free mode.
const FREE_ENTER: f64 = 0.30;
const FREE_LEAVE: f64 = 0.05;
/// How much of each new interval the Free mode's filtered interval takes in.
const FREE_GAIN: f64 = 0.2;
/// With no nominal rate, the period is seeded from the median of this many first intervals.
const SEED_FRAMES: usize = 16;
/// A nominal rate that differs by more than this share from the current one resets the period.
const NOMINAL_CHANGE: f64 = 0.005;
/// Nominal rates above this are bogus (no monitor refreshes faster), like those of 1 Hz or below.
const MAX_NOMINAL_HZ: f64 = 10_000.0;
/// The period assumed until the monitor's rate is known or seeded: 60 Hz.
const DEFAULT_PERIOD: Seconds = 1.0 / 60.0;

/// How the clock placed a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Just (re)anchored: counting one refresh per frame while the phase settles.
    Warmup,
    /// On the fitted grid.
    Locked,
    /// Irregular cadence (VRR, or a compositor that does not hold a fixed rate):
    /// time follows filtered timestamps instead of whole periods.
    Free,
    /// This frame came after a stall or idleness: time jumped by the wall-clock gap.
    Stall,
}

/// Where a frame falls in time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTick {
    /// Animated time since the clock started. Only differences matter: a choreography's t is
    /// this minus its value at the choreography's first frame.
    pub t: Seconds,
    /// Refreshes counted since the clock started.
    pub vsync: u64,
    /// Refreshes skipped since the previous frame (a heuristic: a late frame may still
    /// have been shown on time from the swapchain queue).
    pub dropped: u32,
    /// Raw time since the previous frame.
    pub interval: Seconds,
    /// Distance of this frame from its grid point, in periods.
    pub residual: f64,
    /// How the clock placed this frame.
    pub mode: Mode,
}

/// Turns the instants at which frames start into animated time that advances in whole
/// refresh periods, and counts the refreshes that frames missed.
///
/// Call [`tick`](Self::tick) once per frame, with the instant the frame's work starts
/// (winit's `RedrawRequested`), and [`tick_restart`](Self::tick_restart) instead on the first
/// frame of an animation.
#[derive(Clone, Debug)]
pub struct FrameClock {
    /// The instant the clock measures from.
    epoch: Instant,
    /// The monitor's period as winit reports it, when known.
    nominal: Option<Seconds>,
    /// The period the fit is held to: the nominal one, or the one the rate check measured.
    /// None while the monitor's rate is unknown: the fit then follows the period in use.
    reference: Option<Seconds>,
    /// The period in use: nominal at first, then fitted.
    period: Seconds,
    /// Time since `epoch` of refresh 0 on the grid.
    origin: Seconds,
    /// The previous frame: (time since `epoch`, refresh index).
    prev: Option<(Seconds, u64)>,
    /// Animated time.
    t: Seconds,
    mode: Mode,
    /// Warm-up frames left before the grid anchors.
    warmup_left: u32,
    /// Residuals of the warm-up frames so far.
    warmup_residuals: Vec<f64>,
    /// Consecutive frames sitting a whole number of periods off the grid.
    offset_streak: u32,
    /// In-lock frames since the grid was last anchored.
    since_anchor: u32,
    /// In-lock (refresh index, time since `epoch`) samples for the period fit.
    fit_samples: VecDeque<(f64, Seconds)>,
    /// Whether each of the latest frames was out of lock, oldest first.
    out_of_lock: VecDeque<bool>,
    /// Free mode: the filtered interval.
    free_interval: Seconds,
    /// (time since `epoch`, t) at the last (re)anchor: wall time since then is what t
    /// must keep pace with.
    anchor: (Seconds, Seconds),
    /// Intervals of the first frames, to seed the period when the monitor's is unknown.
    seed_intervals: Vec<Seconds>,
    /// While a nominal rate in whole hertz below CHECK_BELOW_HZ awaits confirmation by the
    /// frames: the measurement under way.
    rate_check: Option<RateCheck>,
}

/// The state of the rate check (see `FrameClock::check_rate`).
#[derive(Clone, Debug, Default)]
struct RateCheck {
    /// The frames since the last warm-up, as (refreshes counted interval by interval, time
    /// since `epoch`).
    frames: VecDeque<(f64, Seconds)>,
    /// The previous measurement, if it was clean and off the reference.
    candidate: Option<Seconds>,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameClock {
    /// A clock that starts now, assuming 60 Hz until it learns the monitor's rate.
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            nominal: None,
            reference: None,
            period: DEFAULT_PERIOD,
            origin: 0.0,
            prev: None,
            t: 0.0,
            mode: Mode::Warmup,
            warmup_left: WARMUP_FRAMES,
            warmup_residuals: Vec::new(),
            offset_streak: 0,
            since_anchor: 0,
            fit_samples: VecDeque::new(),
            out_of_lock: VecDeque::new(),
            free_interval: DEFAULT_PERIOD,
            anchor: (0.0, 0.0),
            seed_intervals: Vec::new(),
            rate_check: None,
        }
    }

    /// Sets the monitor's refresh rate as winit reports it (`refresh_rate_millihertz() /
    /// 1000`). A change of more than 0.5 % resets the period and re-warms the grid. Rates
    /// that are not finite or outside (1 Hz, 10 kHz] are ignored: they come from no real
    /// monitor.
    ///
    /// A rate in whole hertz below 100 Hz (Windows reports whole hertz, so 59.94 Hz reads 59)
    /// is taken as accurate to 1 Hz only, and checked against the frames: when two
    /// measurements in a row find them more than 0.5 % off it, and within 1.5 Hz, the clock
    /// adopts the period they show. `nominal_period` still returns the rate given here.
    pub fn set_nominal_hz(&mut self, hz: f64) {
        if !(hz > 1.0 && hz <= MAX_NOMINAL_HZ) {
            return;
        }
        let p = 1.0 / hz;
        if self.nominal.is_none_or(|n| (p / n - 1.0).abs() > NOMINAL_CHANGE) {
            self.nominal = Some(p);
            self.reference = Some(p);
            self.period = p;
            self.fit_samples.clear();
            self.rate_check = (hz.fract() == 0.0 && hz < CHECK_BELOW_HZ).then(RateCheck::default);
            self.rewarm();
        }
    }

    fn rewarm(&mut self) {
        self.warmup_left = WARMUP_FRAMES;
        self.warmup_residuals.clear();
        self.mode = Mode::Warmup;
        self.offset_streak = 0;
        self.since_anchor = 0;
    }

    /// The refresh period in use: the nominal one at first (or the one measured when a
    /// nominal rate in whole hertz turned out to be off), then the fitted one.
    pub fn period(&self) -> Seconds {
        self.period
    }

    /// The monitor's refresh period as winit reports it, if its rate is known.
    pub fn nominal_period(&self) -> Option<Seconds> {
        self.nominal
    }

    /// The clock's mode after the latest frame. A stall shows only in that frame's tick:
    /// the clock itself is warming up again by then.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The instant at which refresh `n` falls on the fitted grid: anchoring audio there
    /// rather than at a frame's jittery start keeps sound and picture on one timeline.
    /// None only if that instant cannot be represented, which no refresh this clock has
    /// counted can reach.
    pub fn vsync_instant(&self, n: u64) -> Option<Instant> {
        let since_epoch = Duration::try_from_secs_f64((self.origin + n as f64 * self.period).max(0.0)).ok()?;
        self.epoch.checked_add(since_epoch)
    }

    /// Places a frame that starts at `now`, and advances animated time.
    pub fn tick(&mut self, now: Instant) -> FrameTick {
        self.tick_with(now, false)
    }

    /// Like [`tick`](Self::tick), for the first frame of an animation: whatever came before
    /// (idleness, or the odd redraw of a UI change) says nothing about the burst that follows
    /// while the swapchain refills, so the grid re-anchors here and warms up again.
    pub fn tick_restart(&mut self, now: Instant) -> FrameTick {
        self.tick_with(now, true)
    }

    fn tick_with(&mut self, now: Instant, restart: bool) -> FrameTick {
        let s = now.saturating_duration_since(self.epoch).as_secs_f64();
        let Some((ps, pn)) = self.prev else {
            self.origin = s;
            self.prev = Some((s, 0));
            self.anchor = (s, self.t);
            self.rewarm();
            return FrameTick { t: self.t, vsync: 0, dropped: 0, interval: 0.0, residual: 0.0, mode: self.mode };
        };
        // Instants never go back, but a caller's might: a frame that claims to start before
        // the previous one is taken to start with it, so t never runs backwards.
        let s = s.max(ps);
        let dt = s - ps;
        self.seed_period(dt);
        let p = self.period;

        let stalled = dt > STALL_SECONDS.max(STALL_PERIODS * p);
        if restart || stalled {
            // Idle or stalled: time follows the wall clock (and so stays on the audio
            // clock); the grid re-anchors here and warms up again.
            let n = pn + (dt / p).round().max(1.0) as u64;
            self.t += dt;
            self.origin = s - n as f64 * p;
            self.anchor = (s, self.t);
            // The fitted period survives: the display did not change while we waited.
            self.fit_samples.clear();
            self.out_of_lock.clear();
            self.prev = Some((s, n));
            self.rewarm();
            // A forced restart after a short gap is a warm-up frame, not a stall.
            let mode = if stalled { Mode::Stall } else { Mode::Warmup };
            return FrameTick { t: self.t, vsync: n, dropped: 0, interval: dt, residual: 0.0, mode };
        }

        let r = (s - self.origin) / p;
        let n = if self.warmup_left > 0 {
            // Refilling the swapchain after idleness: the first frames come in a burst
            // whose timestamps say nothing about vsyncs, but each is shown on a refresh
            // of its own. Count one each, and no drops.
            pn + 1
        } else {
            // Under FIFO every presented frame takes at least one refresh.
            (r.round().max(0.0) as u64).max(pn + 1)
        };
        let mut e = r - n as f64;

        // Anchored on a burst, the grid can settle whole periods off the frames: the
        // phase is right, but the max(prev + 1) rule would then absorb a real drop.
        // Re-centre it once a whole-period offset holds for a few frames.
        let whole = e.round();
        if self.warmup_left == 0 && whole != 0.0 && (e - whole).abs() < LOCK_BAND {
            self.offset_streak += 1;
            if self.offset_streak >= RECENTER_FRAMES {
                self.origin += whole * p;
                e -= whole;
                self.fit_samples.clear();
                self.offset_streak = 0;
            }
        } else {
            self.offset_streak = 0;
        }
        // Lock is judged against the nearest grid point, whole offsets aside.
        let wrapped = e - e.round();
        let in_lock = wrapped.abs() < LOCK_BAND;
        if self.out_of_lock.len() == MISS_HISTORY {
            self.out_of_lock.pop_front();
        }
        self.out_of_lock.push_back(!in_lock);

        if self.warmup_left > 0 {
            self.warmup_residuals.push(e);
            self.warmup_left -= 1;
            if self.warmup_left == 0 {
                // Anchor on the last frames only: the burst is over by then.
                let settled = self.warmup_residuals.len().saturating_sub(WARMUP_FRAMES as usize / 2);
                self.origin += median(&mut self.warmup_residuals[settled..]) * p;
                self.warmup_residuals.clear();
                self.mode = Mode::Locked;
                self.since_anchor = 0;
            }
        } else if in_lock {
            self.origin += PHASE_GAIN * wrapped * p;
            self.since_anchor += 1;
            // Fit the period on settled frames only, against the grid point each sits on.
            if self.since_anchor > FIT_SKIP_FRAMES {
                self.fit_samples.push_back((n as f64 + e.round(), s));
                while self.fit_samples.front().is_some_and(|&(_, s0)| s - s0 > FIT_WINDOW_SECONDS) {
                    self.fit_samples.pop_front();
                }
                let span = self.fit_samples.back().map_or(0.0, |b| b.1) - self.fit_samples.front().map_or(0.0, |f| f.1);
                let reference = self.reference.unwrap_or(p);
                if self.fit_samples.len() >= FIT_MIN_SAMPLES
                    && span >= FIT_MIN_SPAN_SECONDS
                    && let Some((origin, slope)) = fit_line(&self.fit_samples)
                    && (slope / reference - 1.0).abs() < FIT_TOLERANCE
                {
                    self.period = slope;
                    self.origin = origin;
                }
            }
        }

        let miss_share = self.out_of_lock.iter().filter(|&&m| m).count() as f64 / MISS_HISTORY as f64;
        match self.mode {
            Mode::Locked if self.out_of_lock.len() == MISS_HISTORY && miss_share > FREE_ENTER => {
                self.mode = Mode::Free;
                self.free_interval = dt;
            }
            Mode::Free if miss_share < FREE_LEAVE => self.rewarm(),
            _ => {}
        }

        let dn = n - pn;
        let (step, dropped) = if self.mode == Mode::Free {
            self.free_interval += FREE_GAIN * (dt - self.free_interval);
            (self.free_interval, 0)
        } else if !self.period_known() {
            // Nothing to count refreshes with yet: an assumed 60 Hz would run t ahead of the
            // wall clock (and the audio) by 2.7 times on a 165 Hz monitor, and the pull below
            // would take seconds to undo it. Follow the wall clock until the period is seeded.
            (dt, 0)
        } else {
            // Accumulated, never n·P: a refined period must not make t jump.
            (dn as f64 * p, u32::try_from(dn - 1).unwrap_or(u32::MAX))
        };
        // Close a little of the gap to wall time: zero with the right period, and
        // bounded below a millisecond while the fit converges or in Free mode.
        let wall_t = self.anchor.1 + (s - self.anchor.0);
        let next = self.t + step;
        self.t = next + WALL_GAIN * (wall_t - next).max(-step);
        self.prev = Some((s, n));
        let tick = FrameTick { t: self.t, vsync: n, dropped, interval: dt, residual: e, mode: self.mode };
        self.check_rate(s, dt);
        tick
    }

    /// Whether the clock has a period to count refreshes with: the monitor's, or one seeded
    /// from the first intervals.
    fn period_known(&self) -> bool {
        self.nominal.is_some() || self.seed_intervals.len() >= SEED_FRAMES
    }

    /// With no nominal rate, the period starts as the median of the first intervals.
    fn seed_period(&mut self, dt: Seconds) {
        if self.period_known() || dt <= 0.0 || dt > STALL_SECONDS {
            return;
        }
        self.seed_intervals.push(dt);
        if self.seed_intervals.len() == SEED_FRAMES {
            self.period = median(&mut self.seed_intervals);
            self.fit_samples.clear();
            self.rewarm();
        }
    }

    /// The rate check, for a nominal rate in whole hertz below CHECK_BELOW_HZ. When 59.94 Hz
    /// reads 59, the phase loop cannot follow a period 1.6 % off, so frames never stay in
    /// lock, and the fit, which takes only frames in lock, never corrects the period: the
    /// clock spends most of its time in Free mode and counts no drops.
    ///
    /// Each measurement fits a line through CHECK_FRAMES frames placed since the last warm-up,
    /// with refreshes counted interval by interval: that needs no grid, only a period within a
    /// few percent. If the frames sit on the line as frames on a grid do, its slope is the
    /// real period. Within CHECK_CONFIRM of the reference, it confirms it and the check ends,
    /// having changed nothing. Further off, it replaces the reference only if the measurement
    /// before it agrees and it is within CHECK_BAND_HZ of the nominal rate: irregular frames
    /// can line up by chance, but not twice in a row. The grid then re-warms on the new period,
    /// and the check goes on until a measurement confirms it.
    fn check_rate(&mut self, s: Seconds, dt: Seconds) {
        let (Some(check), Some(nominal), Some(reference)) = (&mut self.rate_check, self.nominal, self.reference) else {
            return;
        };
        if self.mode == Mode::Warmup {
            // The burst after a re-anchor says nothing about the period.
            check.frames.clear();
            return;
        }
        let count = check.frames.back().map_or(0.0, |&(n, _)| n + (dt / self.period).round());
        check.frames.push_back((count, s));
        if check.frames.len() < CHECK_FRAMES {
            return;
        }
        let measured = measure_period(&check.frames);
        check.frames.clear();
        let previous = check.candidate.take();
        let Some(period) = measured else {
            // An irregular cadence (VRR), or a refresh miscounted: measure again.
            return;
        };
        if (period / reference - 1.0).abs() < CHECK_CONFIRM {
            self.rate_check = None;
        } else if (1.0 / period - 1.0 / nominal).abs() < CHECK_BAND_HZ {
            match previous {
                Some(before) if (period / before - 1.0).abs() < CHECK_AGREEMENT => {
                    let period = (before + period) / 2.0;
                    self.reference = Some(period);
                    self.period = period;
                    self.fit_samples.clear();
                    // Misses counted on the wrong period would send the new grid to Free mode.
                    self.out_of_lock.clear();
                    self.rewarm();
                }
                _ => check.candidate = Some(period),
            }
        }
    }
}

/// The period shown by frames given as (refresh index, time): the slope of the line through
/// them, if they sit on it as frames on a grid of refreshes do. Their distances from the line
/// must be small (CHECK_MAX_SCATTER), and must change from frame to frame as much as jitter
/// does: frames that wander off a grid and back, as an irregular cadence does, drift slowly,
/// and can line up at a slope 1 % off by chance. Durbin and Watson's ratio, the mean square
/// change of the distance over its mean square, is about 2 for jitter and near 0 for a wander.
/// Distances below FIT_TOLERANCE of a period are trusted anyway: they cannot tilt the line
/// that much, and on exact timestamps they are only rounding.
fn measure_period(frames: &VecDeque<(f64, Seconds)>) -> Option<Seconds> {
    let (origin, slope) = fit_line(frames)?;
    let (mut squares, mut changes, mut previous) = (0.0, 0.0, None);
    for &(n, s) in frames {
        let distance = s - origin - slope * n;
        squares += distance * distance;
        if let Some(previous) = previous {
            changes += (distance - previous) * (distance - previous);
        }
        previous = Some(distance);
    }
    let scatter = (squares / frames.len() as f64).sqrt() / slope;
    let jitter_like = scatter < FIT_TOLERANCE || changes >= CHECK_MIN_DURBIN_WATSON * squares;
    // A NaN scatter (a flat line) fails.
    (scatter <= CHECK_MAX_SCATTER && jitter_like).then_some(slope)
}

/// The upper median, or 0 for no values.
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Least squares s = a + slope · n, centred for precision. None when degenerate.
fn fit_line(samples: &VecDeque<(f64, Seconds)>) -> Option<(Seconds, Seconds)> {
    let len = samples.len() as f64;
    let (mn, ms) = samples.iter().fold((0.0, 0.0), |(a, b), &(n, s)| (a + n, b + s));
    let (mn, ms) = (mn / len, ms / len);
    let (mut sxx, mut sxy) = (0.0, 0.0);
    for &(n, s) in samples {
        sxx += (n - mn) * (n - mn);
        sxy += (n - mn) * (s - ms);
    }
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    Some((ms - slope * mn, slope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Real hardware period on the M0 machine; the monitor claims 165.001 Hz.
    const P_TRUE: f64 = 0.006_071_8;
    const NOMINAL_HZ: f64 = 165.001;

    /// Feeds frame start times (seconds after an arbitrary start) through a clock.
    fn run(times: &[f64]) -> (FrameClock, Vec<FrameTick>) {
        run_with(Some(NOMINAL_HZ), times)
    }

    /// Like `run`, with the given nominal rate, or none.
    fn run_with(nominal_hz: Option<f64>, times: &[f64]) -> (FrameClock, Vec<FrameTick>) {
        let mut clock = FrameClock::new();
        if let Some(hz) = nominal_hz {
            clock.set_nominal_hz(hz);
        }
        let ticks = times.iter().map(|&s| clock.tick(at(&clock, s))).collect();
        (clock, ticks)
    }

    /// The instant `s` seconds after a point one second past the clock's epoch.
    fn at(clock: &FrameClock, s: f64) -> Instant {
        let base = clock.epoch + Duration::from_secs(1);
        if s >= 0.0 { base + Duration::from_secs_f64(s) } else { base - Duration::from_secs_f64(-s) }
    }

    /// Deterministic jitter in [-amp, amp].
    fn jitter(i: usize, amp: f64) -> f64 {
        let x = ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64 / (1u64 << 53) as f64;
        (2.0 * x - 1.0) * amp
    }

    /// Pseudo-random values in [-1, 1], independent from one `i` to the next. (`jitter` is a
    /// low-discrepancy sequence: its running sums stay bounded, so it cannot make a frame
    /// cadence wander.)
    fn noise(i: usize) -> f64 {
        let mut z = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    fn dropped(ticks: &[FrameTick]) -> u32 {
        ticks.iter().map(|t| t.dropped).sum()
    }

    #[test]
    fn a_wrong_nominal_rate_causes_no_false_drops_and_no_drift() {
        // 10 s at the true period with 0.3 ms of jitter, against a 0.19 % wrong nominal.
        let frames = (10.0 / P_TRUE) as usize;
        let times: Vec<f64> = (0..frames).map(|i| i as f64 * P_TRUE + jitter(i, 0.0003)).collect();
        let (clock, ticks) = run(&times);
        assert_eq!(dropped(&ticks), 0);
        assert!((clock.period() / P_TRUE - 1.0).abs() < 2e-4, "period {}", clock.period());
        // Animated time must keep pace with the wall clock (the audio's clock).
        let elapsed = times[frames - 1] - times[0];
        let t = ticks[frames - 1].t - ticks[0].t;
        assert!((t - elapsed).abs() < 0.001, "t {t} vs wall {elapsed}");
    }

    #[test]
    #[allow(clippy::approx_constant, reason = "6.28 is a measured interval in ms, not 2π")]
    fn a_burst_after_idleness_neither_drops_nor_shifts_the_grid() {
        // Measured on the M0 machine: the first intervals of a spin after idleness, while
        // the swapchain refills. Each of those frames was shown on a refresh of its own.
        let burst_ms = [9.1, 6.67, 4.59, 3.3, 6.16, 6.28, 6.2, 7.88, 4.36, 5.91];
        let mut times = vec![0.0];
        for ms in burst_ms {
            times.push(times[times.len() - 1] + ms / 1000.0);
        }
        let start = times[times.len() - 1];
        times.extend((1..=(8.0 / P_TRUE) as usize).map(|i| start + i as f64 * P_TRUE + jitter(i, 0.00015)));
        let (clock, ticks) = run(&times);
        assert_eq!(dropped(&ticks), 0, "{:?}", ticks.iter().position(|t| t.dropped > 0));
        let last = ticks[ticks.len() - 1];
        assert_eq!(last.mode, Mode::Locked);
        assert!(last.residual.abs() < 0.1, "residual {}", last.residual);
        assert!((clock.period() / P_TRUE - 1.0).abs() < 2e-4, "period {}", clock.period());
    }

    #[test]
    fn a_grid_one_period_off_is_recentred_so_drops_still_count() {
        let mut times: Vec<f64> = (0..600).map(|i| i as f64 * P_TRUE + jitter(i, 0.0002)).collect();
        // A real drop late in the run must still be seen after the grid settled.
        for t in &mut times[500..] {
            *t += P_TRUE;
        }
        let (mut clock, _) = run(&times[..400]);
        // Knock the grid a whole period behind the count (residual −1), as a warm-up
        // anchored on a burst did on the M0 machine. (A grid a period ahead looks exactly
        // like a skipped refresh, and is counted as one.) That re-anchor had also emptied the
        // period fit: with its old samples the next fit would restore the grid by itself.
        clock.origin += clock.period;
        clock.fit_samples.clear();
        clock.since_anchor = 0;
        let ticks: Vec<FrameTick> = times[400..].iter().map(|&s| clock.tick(at(&clock, s))).collect();
        assert_eq!(dropped(&ticks), 1);
        assert!(ticks[ticks.len() - 1].residual.abs() < 0.1);
    }

    #[test]
    fn jitter_below_a_third_of_a_period_never_skips() {
        let times: Vec<f64> = (0..1650).map(|i| i as f64 * P_TRUE + jitter(i, 0.0018)).collect();
        let (_, ticks) = run(&times);
        assert_eq!(dropped(&ticks), 0, "{:?}", ticks.iter().find(|t| t.dropped > 0));
    }

    #[test]
    fn a_frame_late_by_one_period_counts_one_drop() {
        let mut times: Vec<f64> = (0..400).map(|i| i as f64 * P_TRUE + jitter(i, 0.0005)).collect();
        for t in &mut times[300..] {
            *t += P_TRUE;
        }
        let (_, ticks) = run(&times);
        assert_eq!(dropped(&ticks), 1);
        assert_eq!(ticks[300].dropped, 1);
    }

    #[test]
    fn a_stall_advances_time_by_the_wall_clock_without_counting_drops() {
        let mut times: Vec<f64> = (0..200).map(|i| i as f64 * P_TRUE).collect();
        let stall = 0.4;
        times.extend((0..200).map(|i| stall + (200 + i) as f64 * P_TRUE));
        let (_, ticks) = run(&times);
        assert_eq!(ticks[200].mode, Mode::Stall);
        assert_eq!(dropped(&ticks), 0);
        let t = ticks[399].t - ticks[0].t;
        let wall = times[399] - times[0];
        assert!((t - wall).abs() < 0.002, "t {t} vs wall {wall}");
    }

    #[test]
    fn an_irregular_cadence_switches_to_free_time_that_follows_the_wall_clock() {
        // VRR-like: intervals between 6 and 11 ms.
        let mut times = vec![0.0];
        for i in 0..1500 {
            let last = times[times.len() - 1];
            times.push(last + 0.0085 + jitter(i, 0.0025));
        }
        let (_, ticks) = run(&times);
        assert!(ticks.iter().any(|t| t.mode == Mode::Free));
        let t = ticks[ticks.len() - 1].t - ticks[0].t;
        let wall = times[times.len() - 1] - times[0];
        assert!((t - wall).abs() < 0.02, "t {t} vs wall {wall}");
    }

    #[test]
    fn the_period_is_seeded_when_the_monitor_is_unknown() {
        let mut clock = FrameClock::new();
        let base = clock.epoch;
        for i in 0..400 {
            clock.tick(base + Duration::from_secs_f64(i as f64 / 144.0));
        }
        let hz = 1.0 / clock.period();
        assert!((hz - 144.0).abs() < 0.05, "{hz}");
    }

    #[test]
    fn with_the_rate_unknown_time_follows_the_wall_clock_until_the_period_is_seeded() {
        // No nominal rate (Wayland before the window enters an output) on the M0 machine's
        // monitor: counting refreshes of an assumed 60 Hz ran t 2.7 times too fast, 154 ms
        // ahead of the wall clock (and of the audio) once the period was seeded, a lead the
        // pull towards wall time then took ten seconds to undo.
        let times: Vec<f64> = (0..2000).map(|i| i as f64 * P_TRUE).collect();
        let (clock, ticks) = run_with(None, &times);
        for (i, (tick, &s)) in ticks.iter().zip(&times).enumerate() {
            let lead = (tick.t - ticks[0].t) - (s - times[0]);
            // Exactly on it until the 16th interval seeds the period, then on the seeded grid.
            let bound = if i < SEED_FRAMES { 1e-9 } else { 0.002 };
            assert!(lead.abs() < bound, "frame {i}: t {:.3} ms off the wall clock", lead * 1e3);
        }
        assert_eq!(dropped(&ticks), 0);
        assert!((clock.period() / P_TRUE - 1.0).abs() < 1e-9, "period {}", clock.period());
    }

    #[test]
    fn with_the_rate_unknown_no_drops_are_counted_on_an_assumed_60_hz() {
        // Frames 0.3 s apart are neither seed intervals nor stalls, so the period stays
        // unknown; on the assumed 60 Hz grid each frame skipped 17 refreshes.
        let times: Vec<f64> = (0..40).map(|i| f64::from(i) * 0.3).collect();
        let (clock, ticks) = run_with(None, &times);
        assert_eq!(clock.period(), DEFAULT_PERIOD);
        assert_eq!(ticks[39].mode, Mode::Locked);
        assert_eq!(dropped(&ticks), 0);
        assert!((ticks[39].t - ticks[0].t - 39.0 * 0.3).abs() < 1e-9);
    }

    #[test]
    fn a_whole_hertz_rate_that_is_off_is_measured_and_drops_count_again() {
        // Windows reports 59.94 Hz as 59: 1.6 % off, more than the phase loop can follow, so
        // the spike's clock spent 95 % of its time in Free mode and counted no drop. Here: 20 s
        // at the real rate with 0.3 ms of jitter, and a real drop at frame 900.
        let p = 1001.0 / 60_000.0;
        let mut times: Vec<f64> = (0..1200).map(|i| i as f64 * p + jitter(i, 0.0003)).collect();
        for t in &mut times[900..] {
            *t += p;
        }
        let (clock, ticks) = run_with(Some(59.0), &times);
        // Two measurements of a second each, a warm-up, and the grid is on the real period.
        assert!((clock.period() / p - 1.0).abs() < 2e-4, "period {}", clock.period());
        assert_eq!(clock.nominal_period(), Some(1.0 / 59.0));
        let locked = 200;
        assert!(
            ticks[locked..].iter().all(|t| t.mode == Mode::Locked),
            "{:?}",
            ticks[locked..].iter().find(|t| t.mode != Mode::Locked)
        );
        // The real drop counts, and nothing else does.
        assert_eq!(ticks[900].dropped, 1);
        assert_eq!(dropped(&ticks), 1);
        let t = ticks[1199].t - ticks[0].t;
        let wall = times[1199] - times[0];
        assert!((t - wall).abs() < 0.002, "t {t} vs wall {wall}");
    }

    #[test]
    fn a_whole_hertz_rate_that_is_off_is_measured_even_in_short_animations() {
        // Spins of 1.7 s (100 frames at 59.94 Hz), each after an idle gap, so each re-anchors
        // and warms up again: one measurement fits in each, and they are compared across spins.
        // (One that ran on across a gap would miss the refreshes the gap held.)
        let p = 1001.0 / 60_000.0;
        let mut clock = FrameClock::new();
        clock.set_nominal_hz(59.0);
        let mut ticks = Vec::new();
        for spin in 0..6 {
            let start = f64::from(spin) * 3.0;
            for i in 0..100 {
                let now = at(&clock, start + f64::from(i) * p + jitter(i as usize, 0.0003));
                ticks.push(if i == 0 { clock.tick_restart(now) } else { clock.tick(now) });
            }
        }
        assert!((clock.period() / p - 1.0).abs() < 2e-4, "period {}", clock.period());
        let last_spin = &ticks[500 + WARMUP_FRAMES as usize..];
        assert!(last_spin.iter().all(|t| t.mode == Mode::Locked));
        assert_eq!(dropped(last_spin), 0);
    }

    #[test]
    fn a_measured_period_is_refined_by_the_fit() {
        // The fit is held to the measured period, not to the nominal one it replaced, so it
        // refines a measurement like any other reference (one 0.3 % off here, to show).
        let p = 1001.0 / 60_000.0;
        let mut clock = FrameClock::new();
        clock.set_nominal_hz(59.0);
        clock.reference = Some(p * 1.003);
        clock.period = p * 1.003;
        clock.rate_check = None;
        for i in 0..600 {
            clock.tick(at(&clock, i as f64 * p + jitter(i, 0.0003)));
        }
        assert!((clock.period() / p - 1.0).abs() < 2e-4, "period {}", clock.period());
    }

    #[test]
    fn a_whole_hertz_rate_that_is_right_is_confirmed_and_left_alone() {
        // 59.94 Hz read as 60: 0.1 % off, which the fit corrects as for any rate. The check
        // confirms the rate and ends without touching the grid.
        let p = 1001.0 / 60_000.0;
        let times: Vec<f64> = (0..600).map(|i| i as f64 * p + jitter(i, 0.001)).collect();
        let (clock, ticks) = run_with(Some(60.0), &times);
        assert!(clock.rate_check.is_none());
        assert_eq!(clock.reference, Some(1.0 / 60.0));
        assert!(ticks[WARMUP_FRAMES as usize..].iter().all(|t| t.mode == Mode::Locked));
        assert_eq!(dropped(&ticks), 0);
        assert!((clock.period() / p - 1.0).abs() < 2e-4, "period {}", clock.period());
    }

    #[test]
    fn irregular_frames_never_replace_a_whole_hertz_rate() {
        // Frames that wander, with intervals of 1/59.2 s ± 5 % as under VRR: a measurement can
        // show a period 1.3 % off the nominal 60 Hz, but the frames never sit on a grid.
        let mut times = vec![0.0];
        for i in 0..3000 {
            times.push(times[i] + (1.0 + 0.05 * noise(i)) / 59.2);
        }
        let (clock, _) = run_with(Some(60.0), &times);
        assert_eq!(clock.reference, Some(1.0 / 60.0));
    }

    /// Feeds frames at `hz`, from `start`, to the rate check alone, as frames after a warm-up.
    fn measure(clock: &mut FrameClock, start: Seconds, hz: f64) -> Seconds {
        let mut s = start;
        for _ in 0..CHECK_FRAMES {
            s += 1.0 / hz;
            clock.check_rate(s, 1.0 / hz);
        }
        s
    }

    /// A clock whose nominal rate is `hz`, past its warm-up.
    fn locked_at(hz: f64) -> FrameClock {
        let mut clock = FrameClock::new();
        clock.set_nominal_hz(hz);
        clock.mode = Mode::Locked;
        clock
    }

    #[test]
    fn a_measured_period_replaces_the_reference_only_when_two_in_a_row_agree() {
        let mut clock = locked_at(59.0);
        // One measurement off the nominal rate is not enough...
        let s = measure(&mut clock, 10.0, 59.94);
        assert_eq!(clock.reference, Some(1.0 / 59.0));
        // ...nor a second one 0.6 % off the first...
        let s = measure(&mut clock, s, 59.6);
        assert_eq!(clock.reference, Some(1.0 / 59.0));
        // ...but a third that agrees with the second is.
        let s = measure(&mut clock, s, 59.6);
        let reference = clock.reference.unwrap();
        assert!((reference * 59.6 - 1.0).abs() < 1e-9, "{reference}");
        assert_eq!(clock.period(), reference);
        assert_eq!(clock.mode(), Mode::Warmup);
        // The check goes on until a measurement confirms the new period.
        assert!(clock.rate_check.is_some());
        clock.mode = Mode::Locked;
        measure(&mut clock, s, 59.6);
        assert!(clock.rate_check.is_none());
        assert_eq!(clock.reference, Some(reference));
    }

    #[test]
    fn a_measured_period_more_than_1_5_hz_from_a_whole_hertz_rate_is_not_taken() {
        // 61 Hz frames cannot be the monitor a reading of 59 Hz describes.
        let mut clock = locked_at(59.0);
        let s = measure(&mut clock, 10.0, 61.0);
        measure(&mut clock, s, 61.0);
        assert_eq!(clock.reference, Some(1.0 / 59.0));
        assert!(clock.rate_check.is_some());
    }

    #[test]
    fn a_measurement_within_half_a_percent_confirms_and_one_further_off_replaces() {
        // 74.2 Hz read as 74 (0.27 %): the phase loop holds lock on it.
        let mut clock = locked_at(74.0);
        measure(&mut clock, 10.0, 74.2);
        assert!(clock.rate_check.is_none());
        assert_eq!(clock.reference, Some(1.0 / 74.0));
        // 74.6 Hz (0.81 %) is within the fit's tolerance, but its residual of 0.16 periods
        // leaves no room for jitter, and the fit could not move the period beyond 1 %.
        let mut clock = locked_at(74.0);
        let s = measure(&mut clock, 10.0, 74.6);
        measure(&mut clock, s, 74.6);
        assert!((clock.reference.unwrap() * 74.6 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn only_whole_hertz_rates_below_100_hz_are_checked() {
        for (hz, checked) in [(59.0, true), (23.0, true), (99.0, true), (59.94, false), (100.0, false), (144.0, false)]
        {
            let mut clock = FrameClock::new();
            clock.set_nominal_hz(hz);
            assert_eq!(clock.rate_check.is_some(), checked, "{hz} Hz");
        }
    }

    #[test]
    fn a_period_is_measured_only_on_frames_that_sit_on_a_grid() {
        let hz = 59.94;
        let frames = |offset: &dyn Fn(usize) -> f64| -> VecDeque<(f64, Seconds)> {
            (0..CHECK_FRAMES).map(|i| (i as f64, 3.0 + (i as f64 + offset(i)) / hz)).collect()
        };
        // Exact timestamps: nothing to judge the scatter by, and nothing needed.
        let exact = measure_period(&frames(&|_| 0.0)).unwrap();
        assert!((exact * hz - 1.0).abs() < 1e-12, "{exact}");
        // Jitter of up to a tenth of a period around the grid.
        let jittered = measure_period(&frames(&|i| 0.1 * noise(i))).unwrap();
        assert!((jittered * hz - 1.0).abs() < 2e-3, "{jittered}");
        // Frames that wander off the grid and back by less than that, step by step: the
        // distance from the line changes too little from frame to frame.
        let mut walk = 0.0;
        let wander: Vec<f64> = (0..CHECK_FRAMES)
            .map(|i| {
                walk += 0.03 * noise(i);
                walk
            })
            .collect();
        assert_eq!(measure_period(&frames(&|i| wander[i])), None);
        // Jitter of up to 0.4 periods: too much scatter.
        assert_eq!(measure_period(&frames(&|i| 0.4 * noise(i))), None);
    }

    #[test]
    fn the_first_frame_is_refresh_zero() {
        let mut clock = FrameClock::new();
        let first = clock.tick(at(&clock, 0.0));
        assert_eq!(first, FrameTick { t: 0.0, vsync: 0, dropped: 0, interval: 0.0, residual: 0.0, mode: Mode::Warmup });
    }

    #[test]
    fn a_restart_follows_the_wall_clock_and_warms_up_again() {
        let times: Vec<f64> = (0..300).map(|i| i as f64 * P_TRUE).collect();
        let (mut clock, ticks) = run(&times);
        assert_eq!(clock.mode(), Mode::Locked);
        // A short pause, as between a UI redraw and the first frame of a spin: 16.06 true
        // periods, or 16.09 nominal ones, so the count does not hinge on the fit.
        let gap = 0.0975;
        let restart = clock.tick_restart(at(&clock, times[299] + gap));
        assert_eq!(restart.mode, Mode::Warmup);
        assert_eq!(restart.dropped, 0);
        assert!((restart.t - ticks[299].t - gap).abs() < 1e-8);
        assert_eq!(restart.vsync, ticks[299].vsync + 16);
        assert_eq!(clock.mode(), Mode::Warmup);
    }

    #[test]
    fn bogus_nominal_rates_are_ignored() {
        let mut clock = FrameClock::new();
        for hz in [f64::NAN, f64::INFINITY, -60.0, 0.0, 1.0, 1e9] {
            clock.set_nominal_hz(hz);
            assert_eq!(clock.nominal_period(), None, "{hz} Hz");
        }
        clock.set_nominal_hz(144.0);
        assert_eq!(clock.nominal_period(), Some(1.0 / 144.0));
    }

    #[test]
    fn only_a_real_change_of_nominal_rate_resets_the_grid() {
        let times: Vec<f64> = (0..300).map(|i| i as f64 * P_TRUE).collect();
        let (mut clock, _) = run(&times);
        assert_eq!(clock.mode(), Mode::Locked);
        // winit rounding the same monitor differently.
        clock.set_nominal_hz(NOMINAL_HZ * 1.001);
        assert_eq!(clock.mode(), Mode::Locked);
        assert_eq!(clock.nominal_period(), Some(1.0 / NOMINAL_HZ));
        // The window moved to another monitor.
        clock.set_nominal_hz(60.0);
        assert_eq!(clock.mode(), Mode::Warmup);
        assert_eq!(clock.period(), 1.0 / 60.0);
    }

    #[test]
    fn vsync_instants_fall_on_the_frames_once_locked() {
        let times: Vec<f64> = (0..1000).map(|i| i as f64 * P_TRUE + jitter(i, 0.0003)).collect();
        let (clock, ticks) = run(&times);
        for (&s, tick) in times.iter().zip(&ticks).skip(900) {
            let grid = clock.vsync_instant(tick.vsync).unwrap();
            let frame = at(&clock, s);
            let gap = if grid > frame { grid - frame } else { frame - grid };
            assert!(gap.as_secs_f64() < 0.2 * P_TRUE, "{gap:?} at refresh {}", tick.vsync);
        }
    }

    #[test]
    fn a_vsync_instant_out_of_range_does_not_panic() {
        // Billions of years away: None where `Instant` counts seconds in an i64 (Linux,
        // macOS), a far instant where it counts them in a u64 (Windows).
        let mut clock = FrameClock::new();
        clock.set_nominal_hz(1.5);
        assert!(clock.vsync_instant(u64::MAX).is_none_or(|far| far > clock.epoch));
    }

    /// Frame intervals of every kind the clock meets: steady with jitter, bursts, late
    /// frames, stalls, and timestamps out of order.
    fn interval() -> impl Strategy<Value = f64> {
        prop_oneof![
            8 => 0.004..0.009f64,
            2 => 0.0..0.004f64,
            1 => 0.009..0.04f64,
            1 => 0.3..2.0f64,
            1 => -0.01..0.0f64,
        ]
    }

    proptest! {
        /// A choreography is a function of t: if t went back, the wheel would turn back.
        #[test]
        fn time_never_runs_backwards_and_refreshes_always_advance(
            nominal_hz in prop::option::of(prop_oneof![24.0..400.0f64, (23..100u32).prop_map(f64::from)]),
            frames in prop::collection::vec((interval(), prop::bool::weighted(0.03)), 1..600),
        ) {
            let mut clock = FrameClock::new();
            if let Some(hz) = nominal_hz {
                clock.set_nominal_hz(hz);
            }
            let mut s = 10.0;
            let mut previous: Option<FrameTick> = None;
            for (interval, restart) in frames {
                s += interval;
                let now = at(&clock, s);
                let tick = if restart { clock.tick_restart(now) } else { clock.tick(now) };
                prop_assert!(tick.t.is_finite());
                if let Some(previous) = previous {
                    prop_assert!(tick.t >= previous.t, "t went from {} to {}", previous.t, tick.t);
                    prop_assert!(tick.vsync > previous.vsync, "refresh {} after {}", tick.vsync, previous.vsync);
                }
                previous = Some(tick);
            }
        }

        /// However a nominal rate in whole hertz below 100 Hz is off, within 1 Hz, the clock
        /// ends locked on a period within the fit's tolerance of the real one, and counts real
        /// drops, with frames jittered by up to a tenth of a period: 30 s of frames, the last
        /// 5 s one refresh late. (A period confirmed within CHECK_CONFIRM stays as it is below
        /// 30 Hz, where the fit never gathers its samples within its window.)
        #[test]
        fn a_whole_hertz_rate_up_to_1_hz_off_ends_on_the_real_period(
            nominal in 23..100u32,
            off_hz in -1.0..1.0f64,
            amplitude in 0.0..0.1f64,
            sequence in 0..1_000_000usize,
        ) {
            let p = 1.0 / (f64::from(nominal) + off_hz);
            let frames = (30.0 / p) as usize;
            let late = (25.0 / p) as usize;
            let times: Vec<f64> = (0..frames)
                .map(|i| (i as f64 + amplitude * noise(sequence + i) + if i >= late { 1.0 } else { 0.0 }) * p)
                .collect();
            let (clock, ticks) = run_with(Some(f64::from(nominal)), &times);
            prop_assert!((clock.period() / p - 1.0).abs() < FIT_TOLERANCE, "period {} against {p}", clock.period());
            prop_assert_eq!(clock.mode(), Mode::Locked);
            prop_assert_eq!(ticks[late].dropped, 1);
            prop_assert_eq!(dropped(&ticks[late / 2..]), 1);
        }
    }
}
