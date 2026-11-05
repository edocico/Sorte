//! Vsync-quantized animation clock and frame statistics (design doc, § Qualità delle
//! animazioni).
//!
//! Slint, winit and wgpu report no vsync count or presentation time, so the clock
//! fits a grid of refreshes to the instants at which frames start, and animated time
//! advances by whole refresh periods: jitter in those instants never reaches the
//! picture. The grid's period is fitted online, because the monitor's nominal rate is
//! not the real one (165.001 Hz nominal against 164.70 Hz of hardware vblanks on the
//! M0 machine); counting at the nominal rate would let the picture drift away from the
//! audio, which is scheduled on wall-clock time.
//!
//! Design from the M0 frame-pacing research: anchored rounding with a phase loop, a
//! least-squares period fit over ~2 s of in-lock frames, a warm-up after every
//! re-anchor, stalls that advance by the wall-clock gap, and a free-running mode for
//! irregular cadences (VRR).

use std::collections::VecDeque;
use std::time::Instant;

/// Gaps longer than max(STALL_SECONDS, STALL_PERIODS · P) are stalls or idleness.
const STALL_SECONDS: f64 = 0.25;
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
const FIT_MIN_SPAN_SECONDS: f64 = 1.5;
const FIT_WINDOW_SECONDS: f64 = 3.0;
/// A fitted period further than this from the nominal one is rejected.
const FIT_TOLERANCE: f64 = 0.01;
/// Share of the gap between animated and wall-clock time closed at each frame. It keeps
/// t on the audio's clock while the period fit is still converging; with the right
/// period the gap stays near zero and so do the corrections.
const WALL_GAIN: f64 = 0.02;
/// Residual history used to detect an irregular cadence.
const MISS_HISTORY: usize = 64;
const FREE_ENTER: f64 = 0.30;
const FREE_LEAVE: f64 = 0.05;
/// Recent frame intervals kept for the overlay graph.
const GRAPH_LEN: usize = 240;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Just (re)anchored: counting from intervals while the phase settles.
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
    /// Animated time in seconds since the clock started. Only differences matter.
    pub t: f64,
    /// Refreshes counted since the clock started.
    pub vsync: u64,
    /// Refreshes skipped since the previous frame (a heuristic: a late frame may still
    /// have been shown on time from the swapchain queue).
    pub dropped: u32,
    /// Raw seconds since the previous frame.
    pub interval: f64,
    /// Distance of this frame from its grid point, in periods.
    pub residual: f64,
    pub mode: Mode,
}

pub struct FrameClock {
    epoch: Instant,
    /// The monitor's period, when known.
    nominal: Option<f64>,
    /// The period in use: nominal at first, then fitted.
    period: f64,
    /// Seconds since `epoch` of refresh 0 on the grid.
    origin: f64,
    /// The previous frame: (seconds since epoch, refresh index).
    prev: Option<(f64, u64)>,
    t: f64,
    mode: Mode,
    warm: u32,
    warm_residuals: Vec<f64>,
    /// Consecutive frames sitting a whole number of periods off the grid.
    offset_streak: u32,
    /// In-lock frames since the grid was last anchored.
    since_anchor: u32,
    /// In-lock (refresh index, seconds) samples for the period fit.
    fit: VecDeque<(f64, f64)>,
    misses: VecDeque<bool>,
    /// Free mode: the filtered interval.
    free_dt: f64,
    /// (seconds since epoch, t) at the last (re)anchor: wall time since then is what t
    /// must keep pace with.
    anchor: (f64, f64),
    /// Intervals of the first frames, to seed the period when the monitor's is unknown.
    seed: Vec<f64>,
}

impl Default for FrameClock {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameClock {
    pub fn new() -> Self {
        Self {
            epoch: Instant::now(),
            nominal: None,
            period: 1.0 / 60.0,
            origin: 0.0,
            prev: None,
            t: 0.0,
            mode: Mode::Warmup,
            warm: WARMUP_FRAMES,
            warm_residuals: Vec::new(),
            offset_streak: 0,
            since_anchor: 0,
            fit: VecDeque::new(),
            misses: VecDeque::new(),
            free_dt: 1.0 / 60.0,
            anchor: (0.0, 0.0),
            seed: Vec::new(),
        }
    }

    /// The monitor's refresh rate as winit reports it. A change of more than 0.5 %
    /// resets the period and re-warms the grid.
    pub fn set_nominal_hz(&mut self, hz: f64) {
        if hz <= 1.0 {
            return;
        }
        let p = 1.0 / hz;
        if self.nominal.is_none_or(|n| (p / n - 1.0).abs() > 0.005) {
            self.nominal = Some(p);
            self.period = p;
            self.fit.clear();
            self.rewarm();
        }
    }

    fn rewarm(&mut self) {
        self.warm = WARMUP_FRAMES;
        self.warm_residuals.clear();
        self.mode = Mode::Warmup;
        self.offset_streak = 0;
        self.since_anchor = 0;
    }

    /// The period in use, in seconds.
    pub fn period(&self) -> f64 {
        self.period
    }

    pub fn nominal_period(&self) -> Option<f64> {
        self.nominal
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The instant at which refresh `n` falls on the fitted grid.
    pub fn vsync_instant(&self, n: u64) -> Instant {
        self.epoch + std::time::Duration::from_secs_f64((self.origin + n as f64 * self.period).max(0.0))
    }

    /// Places a frame that starts at `now`, and advances animated time.
    pub fn tick(&mut self, now: Instant) -> FrameTick {
        self.tick_with(now, false)
    }

    /// Like `tick`, for the first frame of an animation: whatever came before (idleness,
    /// or the odd redraw of a UI change) says nothing about the burst that follows while
    /// the swapchain refills, so the grid re-anchors here and warms up again.
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
            self.fit.clear();
            self.misses.clear();
            self.prev = Some((s, n));
            self.rewarm();
            // A forced restart after a short gap is a warm-up frame, not a stall.
            let mode = if stalled { Mode::Stall } else { Mode::Warmup };
            return FrameTick { t: self.t, vsync: n, dropped: 0, interval: dt, residual: 0.0, mode };
        }

        let r = (s - self.origin) / p;
        let n = if self.warm > 0 {
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
        if self.warm == 0 && whole != 0.0 && (e - whole).abs() < LOCK_BAND {
            self.offset_streak += 1;
            if self.offset_streak >= RECENTER_FRAMES {
                self.origin += whole * p;
                e -= whole;
                self.fit.clear();
                self.offset_streak = 0;
            }
        } else {
            self.offset_streak = 0;
        }
        // Lock is judged against the nearest grid point, whole offsets aside.
        let wrapped = e - e.round();
        let in_lock = wrapped.abs() < LOCK_BAND;
        if self.misses.len() == MISS_HISTORY {
            self.misses.pop_front();
        }
        self.misses.push_back(!in_lock);

        if self.warm > 0 {
            self.warm_residuals.push(e);
            self.warm -= 1;
            if self.warm == 0 {
                // Anchor on the last frames only: the burst is over by then.
                let settled = self.warm_residuals.len().saturating_sub(WARMUP_FRAMES as usize / 2);
                self.origin += median(&mut self.warm_residuals[settled..]) * p;
                self.warm_residuals.clear();
                self.mode = Mode::Locked;
                self.since_anchor = 0;
            }
        } else if in_lock {
            self.origin += PHASE_GAIN * wrapped * p;
            self.since_anchor += 1;
            // Fit the period on settled frames only, against the grid point each sits on.
            if self.since_anchor > FIT_SKIP_FRAMES {
                self.fit.push_back((n as f64 + e.round(), s));
                while self.fit.front().is_some_and(|&(_, s0)| s - s0 > FIT_WINDOW_SECONDS) {
                    self.fit.pop_front();
                }
                let span = self.fit.back().map_or(0.0, |b| b.1) - self.fit.front().map_or(0.0, |f| f.1);
                if self.fit.len() >= FIT_MIN_SAMPLES && span >= FIT_MIN_SPAN_SECONDS {
                    if let Some((a, slope)) = fit_line(&self.fit) {
                        let reference = self.nominal.unwrap_or(p);
                        if (slope / reference - 1.0).abs() < FIT_TOLERANCE {
                            self.period = slope;
                            self.origin = a;
                        }
                    }
                }
            }
        }

        let miss_share = self.misses.iter().filter(|&&m| m).count() as f64 / MISS_HISTORY as f64;
        match self.mode {
            Mode::Locked if self.misses.len() == MISS_HISTORY && miss_share > FREE_ENTER => {
                self.mode = Mode::Free;
                self.free_dt = dt;
            }
            Mode::Free if miss_share < FREE_LEAVE => self.rewarm(),
            _ => {}
        }

        let dn = n - pn;
        let (step, dropped) = if self.mode == Mode::Free {
            self.free_dt += 0.2 * (dt - self.free_dt);
            (self.free_dt, 0)
        } else {
            // Accumulated, never n·P: a refined period must not make t jump.
            (dn as f64 * p, (dn - 1) as u32)
        };
        // Close a little of the gap to wall time: zero with the right period, and
        // bounded below a millisecond while the fit converges or in Free mode.
        let wall_t = self.anchor.1 + (s - self.anchor.0);
        let next = self.t + step;
        self.t = next + WALL_GAIN * (wall_t - next).max(-step);
        self.prev = Some((s, n));
        FrameTick { t: self.t, vsync: n, dropped, interval: dt, residual: e, mode: self.mode }
    }

    /// With no nominal rate, the period starts as the median of the first intervals.
    fn seed_period(&mut self, dt: f64) {
        if self.nominal.is_some() || self.seed.len() >= 16 || dt <= 0.0 || dt > STALL_SECONDS {
            return;
        }
        self.seed.push(dt);
        if self.seed.len() == 16 {
            self.period = median(&mut self.seed);
            self.fit.clear();
            self.rewarm();
        }
    }
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Least squares s = a + slope · n, centred for precision. None when degenerate.
fn fit_line(samples: &VecDeque<(f64, f64)>) -> Option<(f64, f64)> {
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

/// One rendered frame, for the overlay and the benchmark report.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRecord {
    pub interval_ms: f32,
    pub dropped: u32,
    pub residual: f32,
    /// Our CPU work for the frame: building, encoding and submitting the scene.
    pub work_ms: f32,
    pub encode_ms: f32,
    pub submit_ms: f32,
    /// Spin time shown by the frame, in seconds.
    pub grid_s: f64,
    pub mode: Option<Mode>,
}

/// Rolling statistics for the overlay, plus a full record while a benchmark runs.
#[derive(Default)]
pub struct FrameStats {
    graph: VecDeque<f32>,
    pub frames: u64,
    pub dropped: u64,
    pub stalls: u64,
    pub recording: Option<Vec<FrameRecord>>,
}

impl FrameStats {
    pub fn push(&mut self, tick: &FrameTick, record: FrameRecord) {
        if tick.mode == Mode::Stall {
            self.stalls += 1;
            return;
        }
        self.frames += 1;
        self.dropped += u64::from(tick.dropped);
        if self.graph.len() == GRAPH_LEN {
            self.graph.pop_front();
        }
        self.graph.push_back(record.interval_ms);
        if let Some(r) = &mut self.recording {
            r.push(record);
        }
    }

    pub fn reset_counters(&mut self) {
        self.frames = 0;
        self.dropped = 0;
        self.stalls = 0;
    }

    /// Recent intervals in milliseconds, oldest first.
    pub fn graph(&self) -> Vec<f32> {
        self.graph.iter().copied().collect()
    }
}

/// p-th percentile (0..=100) of `values`, nearest rank.
pub fn percentile(values: &[f32], p: f64) -> f32 {
    if values.is_empty() {
        return f32::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f32::total_cmp);
    let rank = ((p / 100.0) * (v.len() - 1) as f64).round() as usize;
    v[rank.min(v.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Real hardware period on the M0 machine; the monitor claims 165.001 Hz.
    const P_TRUE: f64 = 0.006_071_8;
    const NOMINAL_HZ: f64 = 165.001;

    /// Feeds frame start times (seconds after an arbitrary start) through a clock.
    fn run(times: &[f64]) -> (FrameClock, Vec<FrameTick>) {
        let mut clock = FrameClock::new();
        clock.set_nominal_hz(NOMINAL_HZ);
        let base = clock.epoch + Duration::from_secs(1);
        let ticks = times
            .iter()
            .map(|&s| {
                let at = if s >= 0.0 { base + Duration::from_secs_f64(s) } else { base - Duration::from_secs_f64(-s) };
                clock.tick(at)
            })
            .collect();
        (clock, ticks)
    }

    /// Deterministic jitter in [-amp, amp].
    fn jitter(i: usize, amp: f64) -> f64 {
        let x = ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11) as f64 / (1u64 << 53) as f64;
        (2.0 * x - 1.0) * amp
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
    #[allow(clippy::approx_constant)] // 6.28 is a measured interval in ms, not 2π
    fn a_burst_after_idleness_neither_drops_nor_shifts_the_grid() {
        // Measured on the M0 machine: the first intervals of a spin after idleness, while
        // the swapchain refills. Each of those frames was shown on a refresh of its own.
        let burst_ms = [9.1, 6.67, 4.59, 3.3, 6.16, 6.28, 6.2, 7.88, 4.36, 5.91];
        let mut times = vec![0.0];
        for ms in burst_ms {
            times.push(times.last().unwrap() + ms / 1000.0);
        }
        let start = *times.last().unwrap();
        times.extend((1..=(8.0 / P_TRUE) as usize).map(|i| start + i as f64 * P_TRUE + jitter(i, 0.00015)));
        let (clock, ticks) = run(&times);
        assert_eq!(dropped(&ticks), 0, "{:?}", ticks.iter().position(|t| t.dropped > 0));
        let last = ticks.last().unwrap();
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
        // like a skipped refresh, and is counted as one.)
        clock.origin += clock.period;
        let base = clock.epoch + Duration::from_secs(1);
        let ticks: Vec<FrameTick> =
            times[400..].iter().map(|&s| clock.tick(base + Duration::from_secs_f64(s))).collect();
        assert_eq!(dropped(&ticks), 1);
        assert!(ticks.last().unwrap().residual.abs() < 0.1);
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
            let last = *times.last().unwrap();
            times.push(last + 0.0085 + jitter(i, 0.0025));
        }
        let (_, ticks) = run(&times);
        assert!(ticks.iter().any(|t| t.mode == Mode::Free));
        let t = ticks.last().unwrap().t - ticks[0].t;
        let wall = times.last().unwrap() - times[0];
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
    fn percentiles_use_nearest_rank() {
        let v = [5.0, 1.0, 3.0, 2.0, 4.0];
        assert_eq!(percentile(&v, 50.0), 3.0);
        assert_eq!(percentile(&v, 100.0), 5.0);
    }
}
