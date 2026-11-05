//! Closed-form wheel spin (design doc, § Spin della ruota).
//!
//! Angles are radians, clockwise on screen (y points down). Rotating the wheel by θ
//! shows its wheel-local angle α at the screen angle α + θ; the pointer sits at the
//! screen angle `pointer`. Every quantity is a pure function of t, so the same t
//! always gives the same frame, whatever the refresh rate.

use std::f64::consts::TAU;

/// Bounds of the easing exponent n in E(x) = 1 − (1 − x)^n.
pub const N_MIN: f64 = 2.5;
pub const N_MAX: f64 = 6.0;
/// Exponent preferred when several whole-turn counts K are feasible.
pub const N_PREFERRED: f64 = 3.5;
/// The landing point stays this far from both slice edges, as a fraction of the slice.
pub const LANDING_MARGIN: f64 = 0.15;

/// One slice in wheel-local angles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    pub start: f64,
    pub sweep: f64,
}

impl Slice {
    pub fn mid(&self) -> f64 {
        self.start + 0.5 * self.sweep
    }
}

/// Slices proportional to the weights, the first starting at 0: a_i = 2π·w_i / W.
/// Cumulative integer sums keep the last edge exactly at 2π.
pub fn layout(weights: &[u32]) -> Vec<Slice> {
    let total: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    assert!(total > 0, "at least one entry with a positive weight");
    let mut acc = 0u64;
    weights
        .iter()
        .map(|&w| {
            let start = TAU * acc as f64 / total as f64;
            acc += u64::from(w);
            let end = TAU * acc as f64 / total as f64;
            Slice { start, sweep: end - start }
        })
        .collect()
}

/// Index of the slice under the pointer when the wheel is rotated by `theta`.
pub fn slice_at(slices: &[Slice], pointer: f64, theta: f64) -> usize {
    let a = (pointer - theta).rem_euclid(TAU);
    match slices.binary_search_by(|s| s.start.total_cmp(&a)) {
        Ok(i) => i,
        Err(i) => i.saturating_sub(1),
    }
}

/// Landing angle φ, `u` of the way across the slice; `u` comes from the cosmetic
/// generator, uniform in [LANDING_MARGIN, 1 − LANDING_MARGIN].
pub fn landing_angle(slice: Slice, u: f64) -> f64 {
    debug_assert!((LANDING_MARGIN..=1.0 - LANDING_MARGIN).contains(&u));
    slice.start + u * slice.sweep
}

/// A pointer tick: the edge at the start of slice `edge` passes under the pointer at `t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tick {
    pub t: f64,
    pub edge: usize,
}

/// θ(τ) = θ₀ + Δ·E(τ/T) with E(x) = 1 − (1 − x)^n, so ω₀ = n·Δ/T.
#[derive(Clone, Copy, Debug)]
pub struct SpinPlan {
    pub theta0: f64,
    /// Total rotation Δ = ((p − φ − θ₀) mod 2π) + 2πK.
    pub delta: f64,
    /// T, in seconds.
    pub duration: f64,
    pub n: f64,
    /// K, the whole turns added to the shortest rotation.
    pub turns: u32,
    /// Initial angular velocity actually used, in rad/s.
    pub omega0: f64,
}

impl SpinPlan {
    /// Plans a spin from rotation `theta0` that stops with the wheel-local angle `phi`
    /// under the pointer. When no whole number of turns gives an exponent within
    /// [N_MIN, N_MAX], ω₀ moves to the nearest feasible value (this happens when
    /// ω₀·T is below about 27 rad).
    pub fn new(theta0: f64, phi: f64, pointer: f64, duration: f64, omega_target: f64) -> Self {
        assert!(duration > 0.0 && omega_target > 0.0);
        let delta0 = (pointer - phi - theta0).rem_euclid(TAU);
        let delta_of = |k: u32| delta0 + TAU * f64::from(k);
        let reach = omega_target * duration; // ω₀·T = n·Δ

        // Feasible K: Δ(K) within [reach / N_MAX, reach / N_MIN]. Prefer n near N_PREFERRED.
        let k_lo = ((reach / N_MAX - delta0) / TAU).ceil().max(0.0) as u32;
        let k_hi = ((reach / N_MIN - delta0) / TAU).floor();
        let mut best: Option<(u32, f64)> = None;
        if k_hi >= 0.0 {
            for k in k_lo..=(k_hi as u32) {
                let d = delta_of(k);
                if d <= 0.0 {
                    continue;
                }
                let n = reach / d;
                let closer = best.is_none_or(|(_, bn)| {
                    (n - N_PREFERRED).abs() < (bn - N_PREFERRED).abs()
                });
                if closer {
                    best = Some((k, n));
                }
            }
        }

        let (turns, omega0) = match best {
            Some((k, _)) => (k, omega_target),
            None => {
                // Each K accepts ω₀ in [N_MIN·Δ/T, N_MAX·Δ/T]: take the closest interval.
                let k_max = ((reach / N_MIN - delta0) / TAU).ceil().max(0.0) as u32 + 1;
                (0..=k_max)
                    .filter(|&k| delta_of(k) > 0.0)
                    .map(|k| {
                        let d = delta_of(k);
                        let w = omega_target.clamp(N_MIN * d / duration, N_MAX * d / duration);
                        (k, w)
                    })
                    .min_by(|a, b| (a.1 - omega_target).abs().total_cmp(&(b.1 - omega_target).abs()))
                    .expect("K = 1 is always a candidate")
            }
        };

        let delta = delta_of(turns);
        let n = (omega0 * duration / delta).clamp(N_MIN, N_MAX);
        Self { theta0, delta, duration, n, turns, omega0: n * delta / duration }
    }

    fn progress(&self, t: f64) -> f64 {
        (t / self.duration).clamp(0.0, 1.0)
    }

    /// Wheel rotation at time t (not reduced mod 2π).
    pub fn theta(&self, t: f64) -> f64 {
        let x = self.progress(t);
        self.theta0 + self.delta * (1.0 - (1.0 - x).powf(self.n))
    }

    /// Angular velocity at time t, in rad/s.
    pub fn omega(&self, t: f64) -> f64 {
        let x = self.progress(t);
        self.delta * self.n * (1.0 - x).powf(self.n - 1.0) / self.duration
    }

    /// Rotation at rest after the spin, reduced to [0, 2π): the next spin starts from here.
    pub fn rest(&self) -> f64 {
        (self.theta0 + self.delta).rem_euclid(TAU)
    }

    /// Time at which the wheel has rotated by `b` (0 ≤ b ≤ Δ): the inverse of θ.
    /// τ_b = T·(1 − (1 − b/Δ)^(1/n)).
    pub fn time_at_rotation(&self, b: f64) -> f64 {
        let r = (1.0 - b / self.delta).max(0.0);
        self.duration * (1.0 - r.powf(1.0 / self.n))
    }

    /// Every edge crossing during the spin, in time order, in closed form. The pointer
    /// sees the wheel-local angle α = p − θ, which decreases as the wheel turns: it
    /// meets edge s_i after a rotation b = ((α₀ − s_i) mod 2π) + 2πm, m = 0, 1, …
    pub fn ticks(&self, slices: &[Slice], pointer: f64) -> Vec<Tick> {
        let alpha0 = (pointer - self.theta0).rem_euclid(TAU);
        let mut first: Vec<(f64, usize)> = slices
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let b = (alpha0 - s.start).rem_euclid(TAU);
                // Starting exactly on an edge: the pointer leaves it at once and meets it a turn later.
                (if b == 0.0 { TAU } else { b }, i)
            })
            .collect();
        first.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut out = Vec::new();
        for m in 0u32.. {
            let offset = TAU * f64::from(m);
            if first.first().is_none_or(|&(b, _)| b + offset > self.delta) {
                break;
            }
            for &(b, edge) in &first {
                let b = b + offset;
                if b > self.delta {
                    break;
                }
                out.push(Tick { t: self.time_at_rotation(b), edge });
            }
        }
        out
    }
}

/// Initial angular velocity ω₀, in rad/s, for the speed levels 1..=10 of the settings.
pub fn omega_for_level(level: u8) -> f64 {
    // TODO(edoardo): the speed curve is a feel decision; see the note in the session.
    // Placeholder: a geometric progression from 1 to 10 turns per second.
    let level = f64::from(level.clamp(1, 10));
    TAU * 10f64.powf((level - 1.0) / 9.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64: a tiny portable generator, so the tests need no dependency.
    struct Mix(u64);
    impl Mix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.unit()
        }
    }

    fn random_case(rng: &mut Mix) -> (Vec<Slice>, usize, f64, f64, SpinPlan) {
        let n = 1 + (rng.next() % 60) as usize;
        let weights: Vec<u32> = (0..n).map(|_| 1 + (rng.next() % 1000) as u32).collect();
        let slices = layout(&weights);
        let k = (rng.next() % n as u64) as usize;
        let pointer = rng.range(-TAU, TAU);
        let phi = landing_angle(slices[k], rng.range(LANDING_MARGIN, 1.0 - LANDING_MARGIN));
        let plan = SpinPlan::new(
            rng.range(-100.0, 100.0),
            phi,
            pointer,
            rng.range(1.0, 30.0),
            omega_for_level(1 + (rng.next() % 10) as u8),
        );
        (slices, k, phi, pointer, plan)
    }

    #[test]
    fn layout_covers_the_circle_in_proportion_to_weights() {
        let s = layout(&[1, 2, 1]);
        assert_eq!(s[0].start, 0.0);
        assert!((s[1].sweep - TAU / 2.0).abs() < 1e-12);
        assert_eq!(s[2].start + s[2].sweep, TAU);
    }

    #[test]
    fn spin_lands_inside_the_drawn_slice_away_from_edges() {
        let mut rng = Mix(7);
        for _ in 0..20_000 {
            let (slices, k, phi, pointer, plan) = random_case(&mut rng);
            let end = plan.theta(plan.duration);
            assert_eq!(slice_at(&slices, pointer, end), k);
            let local = (pointer - end).rem_euclid(TAU);
            let err = (local - phi).rem_euclid(TAU);
            assert!(err.min(TAU - err) < 1e-9, "landed {local}, wanted {phi}");
            let u = (local - slices[k].start).rem_euclid(TAU) / slices[k].sweep;
            assert!((LANDING_MARGIN - 1e-9..=1.0 - LANDING_MARGIN + 1e-9).contains(&u));
            assert!((plan.rest() - end.rem_euclid(TAU)).abs() < 1e-9);
        }
    }

    #[test]
    fn exponent_stays_in_range_and_matches_the_initial_velocity() {
        let mut rng = Mix(11);
        for _ in 0..20_000 {
            let (_, _, _, _, plan) = random_case(&mut rng);
            assert!((N_MIN..=N_MAX).contains(&plan.n), "n = {}", plan.n);
            assert!(plan.delta > 0.0);
            assert!((plan.omega(0.0) - plan.omega0).abs() < 1e-9 * plan.omega0.max(1.0));
            assert!(plan.omega(plan.duration).abs() < 1e-12);
        }
    }

    #[test]
    fn feasible_targets_keep_their_velocity() {
        // ω₀·T ≥ 2π / (1/N_MIN − 1/N_MAX) ≈ 26.9 rad always admits a whole number of turns.
        let mut rng = Mix(3);
        for _ in 0..5_000 {
            let duration = rng.range(1.0, 30.0);
            let omega = rng.range(27.0 / duration, 80.0);
            let plan = SpinPlan::new(rng.range(0.0, TAU), rng.range(0.0, TAU), 0.0, duration, omega);
            assert!((plan.omega0 - omega).abs() < 1e-9 * omega, "{} vs {omega}", plan.omega0);
        }
    }

    #[test]
    fn time_at_rotation_inverts_theta() {
        let mut rng = Mix(5);
        for _ in 0..2_000 {
            let (_, _, _, _, plan) = random_case(&mut rng);
            let b = rng.range(0.0, plan.delta);
            let t = plan.time_at_rotation(b);
            assert!((plan.theta(t) - plan.theta0 - b).abs() < 1e-6 * plan.delta.max(1.0));
        }
    }

    #[test]
    fn ticks_are_ordered_and_each_one_changes_the_slice_by_one() {
        let mut rng = Mix(13);
        for _ in 0..500 {
            let (slices, _, _, pointer, plan) = random_case(&mut rng);
            let ticks = plan.ticks(&slices, pointer);
            assert!(ticks.windows(2).all(|w| w[0].t <= w[1].t));
            assert!(ticks.iter().all(|t| t.t >= 0.0 && t.t <= plan.duration));
            // Crossings = edges passed: every slice start met within Δ.
            let a0 = (pointer - plan.theta0).rem_euclid(TAU);
            let first_crossing = |s: &Slice| {
                let b = (a0 - s.start).rem_euclid(TAU);
                if b == 0.0 { TAU } else { b }
            };
            let expected: usize = slices
                .iter()
                .map(|s| {
                    let b = first_crossing(s);
                    if b > plan.delta { 0 } else { 1 + ((plan.delta - b) / TAU).floor() as usize }
                })
                .sum();
            assert_eq!(ticks.len(), expected);
            // Each tick maps back to its crossing, and just past it the pointer is over
            // the slice before the crossed edge.
            let n = slices.len();
            let mut seen = vec![0u32; n];
            for tick in ticks.iter().take(300) {
                let b = first_crossing(&slices[tick.edge]) + TAU * f64::from(seen[tick.edge]);
                seen[tick.edge] += 1;
                assert!((plan.theta(tick.t) - plan.theta0 - b).abs() < 1e-6 * plan.delta.max(1.0));
                let before = (tick.edge + n - 1) % n;
                let eps = 1e-3 * slices[tick.edge].sweep.min(slices[before].sweep);
                assert_eq!(slice_at(&slices, pointer, plan.theta0 + b + eps), before, "edge {}", tick.edge);
            }
        }
    }

    #[test]
    fn omega_for_level_is_increasing() {
        let w: Vec<f64> = (1..=10).map(omega_for_level).collect();
        assert!(w.windows(2).all(|p| p[0] < p[1]));
    }
}
