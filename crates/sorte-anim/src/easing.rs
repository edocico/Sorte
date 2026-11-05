//! The deceleration curve of a spin (design doc, § Spin della ruota): E(x) = 1 − (1 − x)^n,
//! its slope and its exact inverse.
//!
//! The wheel turns by θ(τ) = θ₀ + Δ·E(τ/T). The slope gives its angular velocity, n·Δ/T at
//! the start, for the motion blur and for replanning a manual stop at matching speed. The
//! inverse gives the instant at which it has turned by a given angle, so every pointer tick
//! is known in closed form before the spin starts, rather than noticed by a frame.

/// E(x) = 1 − (1 − x)^n: leaves 0 with slope n and comes to rest at 1.
///
/// `x` is the share of the motion's duration elapsed, clamped to [0, 1], so the curve holds
/// 0 before the start and 1 after the end; NaN gives NaN. `n` must be positive and finite:
/// the wheel keeps it in [2.5, 6], and any n > 1 arrives with zero slope.
pub fn ease_out_pow(x: f64, n: f64) -> f64 {
    debug_assert!(n > 0.0 && n.is_finite(), "exponent {n} must be positive and finite");
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powf(n)
}

/// dE/dx = n·(1 − x)^(n − 1), the slope of [`ease_out_pow`].
///
/// It is the slope of the clamped curve: 0 outside [0, 1], and at either end the slope
/// from inside, so a motion starts at n times its average velocity. NaN gives NaN.
pub fn ease_out_pow_derivative(x: f64, n: f64) -> f64 {
    debug_assert!(n > 0.0 && n.is_finite(), "exponent {n} must be positive and finite");
    // Not a range check: NaN must fail both comparisons and reach the formula.
    if x < 0.0 || x > 1.0 {
        return 0.0;
    }
    n * (1.0 - x).powf(n - 1.0)
}

/// The x at which [`ease_out_pow`] reaches `y`: x = 1 − (1 − y)^(1/n).
///
/// `y` is clamped to [0, 1]; NaN gives NaN. A wheel that turns by Δ in T has turned by b at
/// τ = T·inverse(b/Δ): that is how its ticks are computed. Near x = 1 the curve is nearly
/// flat, so there a change of y in its last digits moves x much more: the instant at which
/// a creeping wheel crosses an edge is sensitive to the edge's angle, whatever the formula.
pub fn ease_out_pow_inverse(y: f64, n: f64) -> f64 {
    debug_assert!(n > 0.0 && n.is_finite(), "exponent {n} must be positive and finite");
    1.0 - (1.0 - y.clamp(0.0, 1.0)).powf(n.recip())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_curve_runs_from_0_to_1() {
        for n in [1.0, 2.5, 3.5, 6.0] {
            assert_eq!(ease_out_pow(0.0, n), 0.0);
            assert_eq!(ease_out_pow(1.0, n), 1.0);
        }
        assert_eq!(ease_out_pow(0.5, 2.0), 0.75);
        assert_eq!(ease_out_pow(0.5, 3.0), 0.875);
    }

    #[test]
    fn the_curve_holds_its_ends_outside_the_motion() {
        assert_eq!(ease_out_pow(-0.5, 3.0), 0.0);
        assert_eq!(ease_out_pow(1.5, 3.0), 1.0);
        assert_eq!(ease_out_pow(f64::NEG_INFINITY, 3.0), 0.0);
        assert_eq!(ease_out_pow(f64::INFINITY, 3.0), 1.0);
    }

    #[test]
    fn the_slope_starts_at_n_and_ends_at_rest() {
        assert_eq!(ease_out_pow_derivative(0.0, 3.5), 3.5);
        assert_eq!(ease_out_pow_derivative(0.5, 2.0), 1.0);
        assert_eq!(ease_out_pow_derivative(1.0, 3.5), 0.0);
        // n = 1 is a straight line.
        assert_eq!(ease_out_pow_derivative(0.3, 1.0), 1.0);
    }

    #[test]
    fn the_slope_is_zero_outside_the_motion() {
        assert_eq!(ease_out_pow_derivative(-1e-9, 3.0), 0.0);
        assert_eq!(ease_out_pow_derivative(1.0 + 1e-9, 3.0), 0.0);
        assert_eq!(ease_out_pow_derivative(f64::INFINITY, 3.0), 0.0);
    }

    #[test]
    fn the_inverse_maps_known_points_and_clamps() {
        assert_eq!(ease_out_pow_inverse(0.0, 3.0), 0.0);
        assert_eq!(ease_out_pow_inverse(1.0, 3.0), 1.0);
        assert_eq!(ease_out_pow_inverse(0.75, 2.0), 0.5);
        assert_eq!(ease_out_pow_inverse(-0.2, 3.0), 0.0);
        assert_eq!(ease_out_pow_inverse(1.2, 3.0), 1.0);
    }

    #[test]
    fn nan_propagates() {
        assert!(ease_out_pow(f64::NAN, 3.0).is_nan());
        assert!(ease_out_pow_derivative(f64::NAN, 3.0).is_nan());
        assert!(ease_out_pow_inverse(f64::NAN, 3.0).is_nan());
    }

    proptest! {
        /// inverse(E(x)) == x, within what the curve's flatness allows: where E′(x) is tiny,
        /// rounding E(x) in its last digit is worth a change of about ε / E′(x) in x.
        #[test]
        fn the_inverse_undoes_the_curve(x in 0.0..=1.0f64, n in 2.5..=6.0f64) {
            let back = ease_out_pow_inverse(ease_out_pow(x, n), n);
            let tolerance = 1e-14 + 8.0 * f64::EPSILON / ease_out_pow_derivative(x, n);
            prop_assert!((back - x).abs() <= tolerance, "x {x}, n {n}: back {back}");
        }

        /// The other way round the problem is well conditioned everywhere.
        #[test]
        fn the_curve_undoes_the_inverse(y in 0.0..=1.0f64, n in 2.5..=6.0f64) {
            let x = ease_out_pow_inverse(y, n);
            prop_assert!((ease_out_pow(x, n) - y).abs() <= 1e-14, "y {y}, n {n}: x {x}");
        }

        #[test]
        fn the_slope_matches_finite_differences(x in 1e-3..(1.0 - 1e-3), n in 2.5..=6.0f64) {
            let h = 1e-6;
            let estimate = (ease_out_pow(x + h, n) - ease_out_pow(x - h, n)) / (2.0 * h);
            let slope = ease_out_pow_derivative(x, n);
            prop_assert!((estimate - slope).abs() < 1e-7, "x {x}, n {n}: {slope} against {estimate}");
        }
    }
}
