//! A damped spring solved in closed form: the pointer's settle at the end of a spin and the
//! race camera (design doc, § Qualità delle animazioni and § Gara di cavalli).
//!
//! A choreography computes its state from t alone (CLAUDE.md, "Time and choreography"), so
//! the spring is never stepped frame by frame: x″ + 2ζω·x′ + ω²·x = 0 has a closed-form
//! solution in each damping regime, and the same t gives the same state at any refresh rate.

use std::error::Error;
use std::fmt;

use crate::Seconds;

/// Where a spring is and how fast it moves.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpringState {
    /// Displacement from the rest point, which is 0.
    pub position: f64,
    /// Rate of change of `position`, per second.
    pub velocity: f64,
}

/// Why spring parameters were rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpringError {
    /// The angular frequency is not positive and finite.
    AngularFrequency,
    /// The damping ratio is negative, NaN or infinite.
    DampingRatio,
}

impl fmt::Display for SpringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AngularFrequency => "a spring's angular frequency must be positive and finite",
            Self::DampingRatio => "a spring's damping ratio must be finite and at least 0",
        })
    }
}

impl Error for SpringError {}

/// A spring pulling a value towards 0: x″ + 2ζω·x′ + ω²·x = 0.
///
/// ω is the angular frequency in radians per second (2π times the frequency, in hertz, at
/// which it would oscillate undamped) and ζ the damping ratio: below 1 the value overshoots
/// and oscillates around 0 (under-damped), at 1 it returns as fast as it can without
/// overshooting (critically damped), above 1 it creeps back more slowly (over-damped).
///
/// The equation is linear, so responses add up: a pointer kicked at every tick moves by the
/// sum of the responses to each kick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    omega: f64,
    zeta: f64,
}

impl Spring {
    /// A spring with angular frequency ω in rad/s (positive and finite) and damping ratio ζ
    /// (finite and at least 0).
    pub const fn new(angular_frequency: f64, damping_ratio: f64) -> Result<Self, SpringError> {
        if !(angular_frequency > 0.0 && angular_frequency.is_finite()) {
            return Err(SpringError::AngularFrequency);
        }
        if !(damping_ratio >= 0.0 && damping_ratio.is_finite()) {
            return Err(SpringError::DampingRatio);
        }
        Ok(Self { omega: angular_frequency, zeta: damping_ratio })
    }

    /// ω, in rad/s.
    pub const fn angular_frequency(&self) -> f64 {
        self.omega
    }

    /// ζ.
    pub const fn damping_ratio(&self) -> f64 {
        self.zeta
    }

    /// The state `t` seconds after the spring was in `start`.
    ///
    /// For t ≤ 0 it is `start`: the spring does not run backwards. NaN gives NaN.
    pub fn state_at(&self, start: SpringState, t: Seconds) -> SpringState {
        if t <= 0.0 {
            return start;
        }
        let (c, s) = self.basis(t);
        let (omega, a) = (self.omega, self.zeta * self.omega);
        let SpringState { position: x0, velocity: v0 } = start;
        SpringState { position: x0 * c + (v0 + a * x0) * s, velocity: v0 * c - (a * v0 + omega * omega * x0) * s }
    }

    /// An upper bound on |position| at every t ≥ 0 after `start`, so that a settle can be
    /// kept within a margin before it is drawn.
    ///
    /// The spring's energy, ½v² + ½ω²x², never grows, hence |x| ≤ √(x₀² + (v₀/ω)²); without
    /// damping that is the amplitude of the oscillation.
    pub fn displacement_bound(&self, start: SpringState) -> f64 {
        start.position.hypot(start.velocity / self.omega)
    }

    /// The pair (C, S) in which every regime's solution takes the same form:
    /// x(t) = x₀·C + (v₀ + ζω·x₀)·S and v(t) = v₀·C − (ζω·v₀ + ω²·x₀)·S.
    /// Once the decay underflows the spring is exactly at rest, which also keeps an infinite
    /// t from turning into NaN.
    fn basis(&self, t: f64) -> (f64, f64) {
        let (omega, zeta) = (self.omega, self.zeta);
        if zeta < 1.0 {
            // e^(−ζωt)·(cos ω_d·t, sin(ω_d·t) / ω_d), with ω_d = ω·√(1 − ζ²).
            let decay = (-zeta * omega * t).exp();
            if decay == 0.0 {
                return (0.0, 0.0);
            }
            let damped = omega * ((1.0 - zeta) * (1.0 + zeta)).sqrt();
            let (sin, cos) = (damped * t).sin_cos();
            let sin_over = if damped == 0.0 { t } else { sin / damped };
            (decay * cos, decay * sin_over)
        } else if zeta == 1.0 {
            let decay = (-omega * t).exp();
            if decay == 0.0 {
                return (0.0, 0.0);
            }
            (decay, decay * t)
        } else {
            // e^(−ζωt)·(cosh γt, sinh(γt) / γ), with γ = ω·√(ζ² − 1), written as the slow mode's
            // decay e^(−(ζω − γ)t) times (1 + ratio) / 2 and (1 − ratio) / 2γ, where
            // ratio = e^(−2γt) is the fast mode against the slow one. Nothing cancels: the slow
            // rate is taken as ω / (ζ + √(ζ² − 1)) rather than ζω − γ, which loses digits when ζ
            // is large, and 1 − ratio through expm1, which keeps them when γt is small.
            let root = ((zeta - 1.0) * (zeta + 1.0)).sqrt();
            let slow = (-omega / (zeta + root) * t).exp();
            if slow == 0.0 {
                return (0.0, 0.0);
            }
            let gamma = omega * root;
            let ratio = (-2.0 * gamma * t).exp();
            let spread = if gamma == 0.0 { t } else { -(-2.0 * gamma * t).exp_m1() / (2.0 * gamma) };
            (slow * 0.5 * (1.0 + ratio), slow * spread)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::f64::consts::TAU;

    fn spring(omega: f64, zeta: f64) -> Spring {
        Spring::new(omega, zeta).unwrap()
    }

    fn state(position: f64, velocity: f64) -> SpringState {
        SpringState { position, velocity }
    }

    #[test]
    fn rejects_invalid_parameters() {
        for omega in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(Spring::new(omega, 0.5), Err(SpringError::AngularFrequency), "ω {omega}");
        }
        for zeta in [-0.1, f64::NAN, f64::INFINITY] {
            assert_eq!(Spring::new(10.0, zeta), Err(SpringError::DampingRatio), "ζ {zeta}");
        }
        let undamped = spring(10.0, 0.0);
        assert_eq!((undamped.angular_frequency(), undamped.damping_ratio()), (10.0, 0.0));
        assert!(!SpringError::AngularFrequency.to_string().is_empty());
        assert!(!SpringError::DampingRatio.to_string().is_empty());
    }

    #[test]
    fn can_be_a_constant() {
        const POINTER: Spring = match Spring::new(TAU * 11.0, 0.32) {
            Ok(spring) => spring,
            Err(_) => panic!("invalid spring"),
        };
        assert_eq!(POINTER.damping_ratio(), 0.32);
    }

    #[test]
    fn starts_from_the_given_state_and_never_runs_backwards() {
        let start = state(0.7, -3.0);
        for zeta in [0.0, 0.3, 1.0, 2.5] {
            let s = spring(12.0, zeta);
            assert_eq!(s.state_at(start, 0.0), start, "ζ {zeta}");
            assert_eq!(s.state_at(start, -1.0), start, "ζ {zeta}");
        }
    }

    #[test]
    fn at_rest_it_stays_at_rest() {
        for zeta in [0.0, 0.3, 1.0, 2.5] {
            assert_eq!(spring(12.0, zeta).state_at(SpringState::default(), 0.8), SpringState::default());
        }
    }

    #[test]
    fn undamped_it_oscillates_forever() {
        let s = spring(5.0, 0.0);
        for t in [0.1, 1.0, 7.3] {
            let now = s.state_at(state(2.0, 0.0), t);
            assert!((now.position - 2.0 * (5.0 * t).cos()).abs() < 1e-12, "t {t}");
            assert!((now.velocity + 10.0 * (5.0 * t).sin()).abs() < 1e-12, "t {t}");
        }
    }

    #[test]
    fn under_damped_each_period_shrinks_the_state_by_the_decay() {
        let (omega, zeta) = (20.0, 0.2);
        let s = spring(omega, zeta);
        let period = TAU / (omega * (1.0 - zeta * zeta).sqrt());
        let start = state(1.5, -4.0);
        let after = s.state_at(start, period);
        let decay = (-zeta * omega * period).exp();
        assert!((after.position - decay * start.position).abs() < 1e-12);
        assert!((after.velocity - decay * start.velocity).abs() < 1e-12);
    }

    #[test]
    fn critically_damped_it_returns_without_overshooting() {
        let omega = 8.0;
        let s = spring(omega, 1.0);
        for i in 1..=200 {
            let t = f64::from(i) * 0.01;
            let x = s.state_at(state(1.0, 0.0), t).position;
            assert!((x - (1.0 + omega * t) * (-omega * t).exp()).abs() < 1e-14, "t {t}");
            assert!(x > 0.0);
        }
    }

    #[test]
    fn over_damped_it_creeps_back_without_crossing() {
        let s = spring(8.0, 3.0);
        let mut previous = 1.0;
        for i in 1..=500 {
            let x = s.state_at(state(1.0, 0.0), f64::from(i) * 0.01).position;
            assert!(x > 0.0 && x < previous, "step {i}: {x}");
            previous = x;
        }
    }

    #[test]
    fn the_regimes_meet_at_critical_damping() {
        let start = state(1.0, 5.0);
        for t in [0.01, 0.3, 2.0] {
            let critical = spring(6.0, 1.0).state_at(start, t);
            for zeta in [1.0 - 1e-9, 1.0 + 1e-9, 1.0 - 1e-15, 1.0 + 1e-15] {
                let near = spring(6.0, zeta).state_at(start, t);
                assert!((near.position - critical.position).abs() < 1e-7, "ζ {zeta}, t {t}");
                assert!((near.velocity - critical.velocity).abs() < 1e-6, "ζ {zeta}, t {t}");
            }
        }
    }

    #[test]
    fn heavily_damped_it_creeps_at_the_slow_rate() {
        // Started on the slow mode, the state decays at exactly the slow rate ω / (ζ + √(ζ² − 1)).
        // Computed as ζω − γ instead, the rate would lose 4 of its digits at ζ = 10⁶.
        let (omega, zeta): (f64, f64) = (10.0, 1e6);
        let rate = omega / (zeta + (zeta * zeta - 1.0).sqrt());
        let start = state(1.0, -rate);
        for t in [0.5 / rate, 1.0 / rate, 3.0 / rate] {
            let now = spring(omega, zeta).state_at(start, t);
            let expected = (-rate * t).exp();
            assert!((now.position / expected - 1.0).abs() < 1e-9, "t {t}: {} against {expected}", now.position);
        }
    }

    #[test]
    fn just_above_critical_damping_a_kick_keeps_its_precision() {
        // Here γt is about 4·10⁻¹⁴: sinh(γt) / γ written as a difference of exponentials
        // would keep 3 digits. The response to a unit kick is then t·e^(−ωt), as when critical.
        let s = spring(1.0, 1.0 + f64::EPSILON);
        let t = 1e-6;
        let x = s.state_at(state(0.0, 1.0), t).position;
        let expected = t * (-t).exp();
        assert!((x / expected - 1.0).abs() < 1e-12, "{x} against {expected}");
    }

    #[test]
    fn it_settles_exactly_once_the_decay_underflows() {
        for zeta in [0.3, 1.0, 2.5] {
            let s = spring(12.0, zeta);
            assert_eq!(s.state_at(state(1.0, 1.0), f64::INFINITY), SpringState::default(), "ζ {zeta}");
            assert_eq!(s.state_at(state(1.0, 1.0), 1e6), SpringState::default(), "ζ {zeta}");
        }
    }

    #[test]
    fn nan_propagates() {
        let now = spring(12.0, 0.3).state_at(state(1.0, 1.0), f64::NAN);
        assert!(now.position.is_nan() && now.velocity.is_nan());
    }

    #[test]
    fn undamped_the_bound_is_the_amplitude() {
        let s = spring(4.0, 0.0);
        let start = state(3.0, 16.0);
        let peak = (0..10_000).map(|i| s.state_at(start, f64::from(i) * 1e-4).position.abs()).fold(0.0, f64::max);
        assert!((s.displacement_bound(start) - 5.0).abs() < 1e-12);
        assert!((peak - 5.0).abs() < 1e-6, "peak {peak}");
    }

    /// x′ = v, v′ = −2ζω·v − ω²·x, integrated with classic Runge–Kutta in steps of 1 % of the
    /// fastest time constant.
    fn rk4(omega: f64, zeta: f64, start: SpringState, t: f64) -> SpringState {
        let fastest = omega * (zeta + (zeta * zeta - 1.0).max(0.0).sqrt()).max(1.0);
        let steps = (t * fastest / 0.01).ceil().max(1.0) as u32;
        let h = t / f64::from(steps);
        let f = |x: f64, v: f64| (v, -2.0 * zeta * omega * v - omega * omega * x);
        let (mut x, mut v) = (start.position, start.velocity);
        for _ in 0..steps {
            let (k1x, k1v) = f(x, v);
            let (k2x, k2v) = f(x + 0.5 * h * k1x, v + 0.5 * h * k1v);
            let (k3x, k3v) = f(x + 0.5 * h * k2x, v + 0.5 * h * k2v);
            let (k4x, k4v) = f(x + h * k3x, v + h * k3v);
            x += h / 6.0 * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
            v += h / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
        }
        state(x, v)
    }

    /// The closed form against the integration, relative to the scale of the motion.
    fn matches_rk4(omega: f64, zeta: f64, x0: f64, v0: f64, t: f64) -> Result<(), TestCaseError> {
        let start = state(x0, v0);
        let closed = spring(omega, zeta).state_at(start, t);
        let numeric = rk4(omega, zeta, start, t);
        let position_scale = x0.hypot(v0 / omega);
        let velocity_scale = (omega * x0).hypot(v0);
        prop_assert!(
            (closed.position - numeric.position).abs() <= 1e-6 * position_scale + 1e-12,
            "position {} against {}",
            closed.position,
            numeric.position
        );
        prop_assert!(
            (closed.velocity - numeric.velocity).abs() <= 1e-6 * velocity_scale + 1e-12,
            "velocity {} against {}",
            closed.velocity,
            numeric.velocity
        );
        Ok(())
    }

    proptest! {
        #[test]
        fn under_damped_matches_numerical_integration(
            omega in 0.5..80.0f64, zeta in 0.0..0.98f64, x0 in -10.0..10.0f64, v0 in -100.0..100.0f64, t in 0.0..1.5f64,
        ) {
            matches_rk4(omega, zeta, x0, v0, t)?;
        }

        #[test]
        fn critically_damped_matches_numerical_integration(
            omega in 0.5..80.0f64, x0 in -10.0..10.0f64, v0 in -100.0..100.0f64, t in 0.0..1.5f64,
        ) {
            matches_rk4(omega, 1.0, x0, v0, t)?;
        }

        #[test]
        fn over_damped_matches_numerical_integration(
            omega in 0.5..80.0f64, zeta in 1.02..4.0f64, x0 in -10.0..10.0f64, v0 in -100.0..100.0f64, t in 0.0..1.5f64,
        ) {
            matches_rk4(omega, zeta, x0, v0, t)?;
        }

        /// Next to ζ = 1 the closed forms divide by a vanishing ω_d or γ.
        #[test]
        fn nearly_critically_damped_matches_numerical_integration(
            omega in 0.5..80.0f64, exponent in -15.0..-3.0f64, above in any::<bool>(),
            x0 in -10.0..10.0f64, v0 in -100.0..100.0f64, t in 0.0..1.5f64,
        ) {
            let offset = 10f64.powf(exponent);
            let zeta = if above { 1.0 + offset } else { 1.0 - offset };
            matches_rk4(omega, zeta, x0, v0, t)?;
        }

        #[test]
        fn never_strays_beyond_the_displacement_bound(
            omega in 0.5..80.0f64, zeta in 0.0..4.0f64, x0 in -10.0..10.0f64, v0 in -100.0..100.0f64, t in 0.0..3.0f64,
        ) {
            let s = spring(omega, zeta);
            let start = state(x0, v0);
            let bound = s.displacement_bound(start);
            let x = s.state_at(start, t).position;
            prop_assert!(x.abs() <= bound * (1.0 + 1e-12), "|{x}| > {bound}");
        }
    }
}
