//! Monotone cubic interpolation (Fritsch–Carlson), for the horse race (design doc,
//! § Gara di cavalli).
//!
//! Each horse's progress is a curve through random knots of (time, distance). An ordinary
//! cubic spline overshoots between knots, which would make a horse step back; the
//! Fritsch–Carlson method limits the tangents so that the curve never decreases where the
//! knots don't. Its slope, the horse's speed, is continuous and never negative, so the
//! gallop's cadence can follow it.
//!
//! F. N. Fritsch and R. E. Carlson, "Monotone Piecewise Cubic Interpolation", SIAM Journal
//! on Numerical Analysis 17(2), 1980.

use std::error::Error;
use std::fmt;

/// Why a list of knots cannot make a [`MonotoneSpline`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplineError {
    /// Fewer than two knots: a curve needs a start and an end.
    TooFewKnots {
        /// How many knots there were.
        count: usize,
    },
    /// A coordinate of the knot at `index` is NaN or infinite, or its distance or slope from
    /// the previous knot overflows.
    NotFinite {
        /// Position of the knot in the list.
        index: usize,
    },
    /// The knot at `index` is not strictly to the right of the previous one.
    XNotIncreasing {
        /// Position of the knot in the list.
        index: usize,
    },
    /// The knot at `index` is lower than the previous one: the curve would have to go back.
    YDecreasing {
        /// Position of the knot in the list.
        index: usize,
    },
}

impl fmt::Display for SplineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewKnots { count } => write!(f, "a monotone spline needs at least 2 knots, got {count}"),
            Self::NotFinite { index } => {
                write!(f, "knot {index}: a coordinate, or the step from the previous knot, is not finite")
            }
            Self::XNotIncreasing { index } => write!(f, "knot {index}: x is not greater than the previous knot's"),
            Self::YDecreasing { index } => write!(f, "knot {index}: y is lower than the previous knot's"),
        }
    }
}

impl Error for SplineError {}

/// A smooth curve through knots (x, y), with x strictly increasing and y non-decreasing,
/// that does not decrease between them either.
///
/// It is a piecewise cubic with a continuous slope (no jumps in speed), whose tangents at
/// the knots are limited so that no piece overshoots its knots. Outside the knots it holds
/// the first or the last y, with zero slope.
///
/// In exact arithmetic it never decreases; in floating point, [`value`](Self::value) returns
/// each knot's y exactly and stays between the y of the two knots around x, but rounding
/// inside a piece can make it fall by a few units in the last place of the largest |y|
/// between x values only a few units in the last place apart. Samples a frame apart do not
/// fall: check monotonicity on such samples, or with a tolerance. And rounding can make the
/// curve reach a knot's y just before the knot's x: take a horse's arrival from its last
/// knot, not from the first x at which the curve reaches the finish.
#[derive(Clone, Debug, PartialEq)]
pub struct MonotoneSpline {
    /// One piece per pair of consecutive knots, in order.
    segments: Vec<Segment>,
    /// The first knot.
    start: (f64, f64),
    /// The last knot.
    end: (f64, f64),
}

impl MonotoneSpline {
    /// The curve through `knots`, given as (x, y): at least two, all finite, x strictly
    /// increasing and y non-decreasing.
    pub fn new(knots: &[(f64, f64)]) -> Result<Self, SplineError> {
        let secants = secants(knots)?;
        let mut tangents = initial_tangents(knots, &secants);
        limit_tangents(&secants, &mut tangents);
        let segments = knots
            .windows(2)
            .zip(&secants)
            .zip(tangents.windows(2))
            .map(|((pair, &secant), ends)| Segment::new(pair[0], pair[1], secant, ends[0], ends[1]))
            .collect();
        Ok(Self { segments, start: knots[0], end: knots[knots.len() - 1] })
    }

    /// The curve's y at `x`: exactly a knot's y at its x, the first or last y beyond the
    /// knots. NaN gives NaN. Non-decreasing in `x` up to rounding, as the type's
    /// documentation details.
    pub fn value(&self, x: f64) -> f64 {
        if x <= self.start.0 {
            return self.start.1;
        }
        if x >= self.end.0 {
            return self.end.1;
        }
        self.segment_at(x).value(x)
    }

    /// The curve's slope dy/dx at `x`, never negative: 0 beyond the knots, and at the first
    /// and last knots the slope from inside. NaN gives NaN.
    pub fn derivative(&self, x: f64) -> f64 {
        // Not a range check: NaN must fail both comparisons and reach the piece.
        if x < self.start.0 || x > self.end.0 {
            return 0.0;
        }
        self.segment_at(x).derivative(x)
    }

    /// The piece that starts at or before `x` (the first one for NaN).
    fn segment_at(&self, x: f64) -> &Segment {
        let after = self.segments.partition_point(|s| s.x0 <= x);
        &self.segments[after.saturating_sub(1)]
    }
}

/// Validates the knots and returns the slope of each step between consecutive ones.
fn secants(knots: &[(f64, f64)]) -> Result<Vec<f64>, SplineError> {
    if knots.len() < 2 {
        return Err(SplineError::TooFewKnots { count: knots.len() });
    }
    let mut secants = Vec::with_capacity(knots.len() - 1);
    for (index, &(x, y)) in knots.iter().enumerate() {
        if !x.is_finite() || !y.is_finite() {
            return Err(SplineError::NotFinite { index });
        }
        if index == 0 {
            continue;
        }
        let (x0, y0) = knots[index - 1];
        if x <= x0 {
            return Err(SplineError::XNotIncreasing { index });
        }
        if y < y0 {
            return Err(SplineError::YDecreasing { index });
        }
        let (width, rise) = (x - x0, y - y0);
        let secant = rise / width;
        if !width.is_finite() || !rise.is_finite() || !secant.is_finite() {
            return Err(SplineError::NotFinite { index });
        }
        secants.push(secant);
    }
    Ok(secants)
}

/// Fritsch–Carlson step 1: the tangent at each knot before limiting. Interior knots take the
/// slope of the parabola through them and their neighbours, and the end knots the slope of
/// their only step.
fn initial_tangents(knots: &[(f64, f64)], secants: &[f64]) -> Vec<f64> {
    let mut tangents = Vec::with_capacity(knots.len());
    tangents.push(secants[0]);
    for (k, pair) in secants.windows(2).enumerate() {
        // Each step's slope weighs as much as the other step is wide.
        let (width_before, width_after) = (knots[k + 1].0 - knots[k].0, knots[k + 2].0 - knots[k + 1].0);
        let weight = width_after / (width_before + width_after);
        tangents.push(weight * pair[0] + (1.0 - weight) * pair[1]);
    }
    tangents.push(secants[secants.len() - 1]);
    tangents
}

/// Fritsch–Carlson step 2: on each step, scales both end tangents down until, measured in
/// units of the step's slope, they lie within the circle of radius 3, where a cubic piece is
/// sure to be monotone. A flat step's circle has radius 0, so its tangents become 0: it stays
/// flat, and the speed reaches 0 smoothly at its ends. Scaling a tangent down never takes the
/// previous step out of its circle.
fn limit_tangents(secants: &[f64], tangents: &mut [f64]) {
    for (k, &secant) in secants.iter().enumerate() {
        let size = tangents[k].hypot(tangents[k + 1]);
        let limit = 3.0 * secant;
        if size > limit {
            let scale = limit / size;
            tangents[k] *= scale;
            tangents[k + 1] *= scale;
        }
    }
}

/// The curve between two knots: y0 + (y1 − y0)·s(u) with u = (x − x0) / width, where
/// s(u) = c₁u + c₂u² + c₃u³ rises from s(0) = 0 to s(1) = 1. Written this way a flat step is
/// exactly flat and each knot's y is returned exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    x0: f64,
    width: f64,
    y0: f64,
    y1: f64,
    secant: f64,
    /// c₁, c₂, c₃.
    shape: [f64; 3],
}

impl Segment {
    /// The cubic Hermite piece with slopes `m0` and `m1` at its ends.
    fn new((x0, y0): (f64, f64), (x1, y1): (f64, f64), secant: f64, m0: f64, m1: f64) -> Self {
        let shape = if secant > 0.0 {
            // The end slopes in units of the step's own slope.
            let (a, b) = (m0 / secant, m1 / secant);
            [a, 3.0 - 2.0 * a - b, a + b - 2.0]
        } else {
            [0.0; 3]
        };
        Self { x0, width: x1 - x0, y0, y1, secant, shape }
    }

    fn value(&self, x: f64) -> f64 {
        let u = (x - self.x0) / self.width;
        let [c1, c2, c3] = self.shape;
        let s = u * (c1 + u * (c2 + u * c3));
        // Exactly, a monotone piece stays between its knots' y. Clamping removes only
        // rounding, which could carry the curve past the next knot's y just before reaching
        // it, and then step back.
        (self.y0 + (self.y1 - self.y0) * s).clamp(self.y0, self.y1)
    }

    fn derivative(&self, x: f64) -> f64 {
        let u = (x - self.x0) / self.width;
        let [c1, c2, c3] = self.shape;
        let slope = self.secant * (c1 + u * (2.0 * c2 + 3.0 * c3 * u));
        // Exactly never negative, but rounding dips below 0 next to a zero tangent. The
        // comparison keeps NaN, which max() would drop.
        if slope < 0.0 { 0.0 } else { slope }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn spline(knots: &[(f64, f64)]) -> MonotoneSpline {
        MonotoneSpline::new(knots).unwrap()
    }

    #[test]
    fn rejects_fewer_than_two_knots() {
        assert_eq!(MonotoneSpline::new(&[]), Err(SplineError::TooFewKnots { count: 0 }));
        assert_eq!(MonotoneSpline::new(&[(0.0, 0.0)]), Err(SplineError::TooFewKnots { count: 1 }));
    }

    #[test]
    fn rejects_coordinates_that_are_not_finite() {
        assert_eq!(MonotoneSpline::new(&[(f64::NAN, 0.0), (1.0, 1.0)]), Err(SplineError::NotFinite { index: 0 }));
        assert_eq!(
            MonotoneSpline::new(&[(0.0, 0.0), (1.0, 1.0), (2.0, f64::INFINITY)]),
            Err(SplineError::NotFinite { index: 2 })
        );
    }

    #[test]
    fn rejects_steps_that_overflow() {
        // Each coordinate is finite, but the distance between them is not.
        assert_eq!(MonotoneSpline::new(&[(-1e308, 0.0), (1e308, 1.0)]), Err(SplineError::NotFinite { index: 1 }));
        assert_eq!(MonotoneSpline::new(&[(0.0, -1e308), (1.0, 1e308)]), Err(SplineError::NotFinite { index: 1 }));
        // A rise over a vanishing width has an infinite slope.
        assert_eq!(MonotoneSpline::new(&[(0.0, 0.0), (1e-310, 1.0)]), Err(SplineError::NotFinite { index: 1 }));
    }

    #[test]
    fn rejects_x_that_does_not_increase() {
        assert_eq!(
            MonotoneSpline::new(&[(0.0, 0.0), (1.0, 1.0), (1.0, 2.0)]),
            Err(SplineError::XNotIncreasing { index: 2 })
        );
        assert_eq!(MonotoneSpline::new(&[(1.0, 0.0), (0.0, 1.0)]), Err(SplineError::XNotIncreasing { index: 1 }));
    }

    #[test]
    fn rejects_y_that_decreases() {
        assert_eq!(
            MonotoneSpline::new(&[(0.0, 0.0), (1.0, 2.0), (2.0, 1.0)]),
            Err(SplineError::YDecreasing { index: 2 })
        );
    }

    #[test]
    fn reports_the_first_bad_knot() {
        assert_eq!(
            MonotoneSpline::new(&[(0.0, 0.0), (0.0, 1.0), (f64::NAN, 2.0)]),
            Err(SplineError::XNotIncreasing { index: 1 })
        );
    }

    #[test]
    fn errors_name_the_knot() {
        assert_eq!(
            SplineError::TooFewKnots { count: 1 }.to_string(),
            "a monotone spline needs at least 2 knots, got 1"
        );
        assert!(SplineError::NotFinite { index: 3 }.to_string().starts_with("knot 3: "));
        assert!(SplineError::XNotIncreasing { index: 4 }.to_string().starts_with("knot 4: "));
        assert!(SplineError::YDecreasing { index: 5 }.to_string().starts_with("knot 5: "));
    }

    #[test]
    fn two_knots_make_a_straight_line() {
        let s = spline(&[(1.0, 2.0), (3.0, 6.0)]);
        assert_eq!(s.value(2.0), 4.0);
        assert_eq!(s.derivative(1.5), 2.0);
    }

    #[test]
    fn collinear_knots_make_a_straight_line_even_unevenly_spaced() {
        let s = spline(&[(0.0, 1.0), (0.5, 2.0), (3.0, 7.0), (3.25, 7.5), (10.0, 21.0)]);
        for i in 0..=100 {
            let x = f64::from(i) / 10.0;
            assert!((s.value(x) - (1.0 + 2.0 * x)).abs() < 1e-12, "x {x}");
            assert!((s.derivative(x) - 2.0).abs() < 1e-12, "x {x}");
        }
    }

    #[test]
    fn a_parabola_keeps_its_slope_at_an_interior_knot() {
        // y = x² through unevenly spaced knots: at x = 1 the interior tangent is the slope of
        // the parabola through the knot and its neighbours, 2, exactly. The mean of the two
        // steps' slopes (1 and 4) would give 2.5, their harmonic mean 1.6; the limiter is
        // inactive here (√5 ≤ 3·1 on the first step, √20 ≤ 3·4 on the second).
        let s = spline(&[(0.0, 0.0), (1.0, 1.0), (3.0, 9.0)]);
        assert_eq!(s.derivative(1.0), 2.0);
        // The piece on the left arrives with the same slope.
        assert!((s.derivative(1.0f64.next_down()) - 2.0).abs() < 1e-12);
    }

    #[test]
    fn holds_the_end_values_beyond_the_knots() {
        let s = spline(&[(0.0, 1.0), (1.0, 3.0), (2.0, 4.0)]);
        assert_eq!(s.value(-5.0), 1.0);
        assert_eq!(s.value(f64::NEG_INFINITY), 1.0);
        assert_eq!(s.value(7.0), 4.0);
        assert_eq!(s.value(f64::INFINITY), 4.0);
        assert_eq!(s.derivative(-1e-9), 0.0);
        assert_eq!(s.derivative(2.0 + 1e-9), 0.0);
    }

    #[test]
    fn at_the_ends_the_slope_comes_from_inside() {
        // The end tangents are the end steps' slopes: 2 and 1 here, already within the limit.
        let s = spline(&[(0.0, 1.0), (1.0, 3.0), (2.0, 4.0)]);
        assert_eq!(s.derivative(0.0), 2.0);
        assert!((s.derivative(2.0) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn a_flat_step_stays_exactly_flat() {
        let s = spline(&[(0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (3.0, 2.0)]);
        for i in 0..=100 {
            let x = 1.0 + f64::from(i) / 100.0;
            assert_eq!(s.value(x), 1.0, "x {x}");
            assert_eq!(s.derivative(x), 0.0, "x {x}");
        }
    }

    #[test]
    fn a_steep_step_between_gentle_ones_does_not_overshoot() {
        // With unlimited tangents the first and last pieces would swing back here: the
        // tangent the steep step gives their inner knot is 50 times their own slope.
        let s = spline(&[(0.0, 0.0), (1.0, 0.01), (2.0, 1.0), (3.0, 1.01)]);
        let samples: Vec<f64> = (0..=3000).map(|i| s.value(f64::from(i) / 1000.0)).collect();
        assert!(samples.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn a_slope_that_rounds_below_zero_reads_as_zero() {
        // Found by search. After the flat step the gentle piece starts with tangent 0 and
        // ends with the largest tangent the limit allows, because the steep step pulls it up;
        // one unit in the last place past its first knot its slope rounds to −1.8·10⁻³¹.
        let s = spline(&[
            (-1.6298216249109059, 0.0),
            (3.2800244461893593, 0.0),
            (6.403729890426083, 8.342716134057804),
            (9.297858959005465, 242.35267402351442),
        ]);
        assert!(s.derivative(3.2800244461893593f64.next_up()) >= 0.0);
    }

    #[test]
    fn rounding_can_dip_by_a_unit_in_the_last_place_between_adjacent_x() {
        // A horse's curve (found by the review): one unit in the last place further in time,
        // the computed distance is one unit in the last place shorter, the kind of dip the
        // type's documentation allows. A frame later the curve is well ahead.
        let s = spline(&[
            (0.0, 0.0),
            (4.330795136550807, 166.70773847017966),
            (9.001908487411367, 326.4310201131911),
            (13.08504511011893, 506.94183511639363),
            (17.283936811085006, 691.1798329525545),
            (21.974956398669907, 835.2886808296072),
            (26.213641005473647, 1000.0),
        ]);
        let x = 3.391104456621716f64;
        let (here, next) = (s.value(x), s.value(x.next_up()));
        assert!(next >= here.next_down(), "{here} then {next}");
        assert!(s.value(x + 1.0 / 240.0) > here);
    }

    #[test]
    fn nan_propagates() {
        let s = spline(&[(0.0, 0.0), (1.0, 1.0), (2.0, 3.0)]);
        assert!(s.value(f64::NAN).is_nan());
        assert!(s.derivative(f64::NAN).is_nan());
    }

    /// Knots like a horse's: x strictly increasing, y non-decreasing, with flat steps,
    /// tiny ones and steep jumps next to gentle ones, where tangents most need limiting.
    fn knots() -> impl Strategy<Value = Vec<(f64, f64)>> {
        let step =
            (0.01..10.0f64, prop_oneof![2 => Just(0.0), 1 => 0.0..1e-3f64, 6 => 0.0..10.0f64, 1 => 50.0..500.0f64]);
        (-50.0..50.0f64, -50.0..50.0f64, prop::collection::vec(step, 1..12)).prop_map(|(x0, y0, steps)| {
            let mut knots = vec![(x0, y0)];
            for (dx, dy) in steps {
                let (x, y) = knots[knots.len() - 1];
                knots.push((x + dx, y + dy));
            }
            knots
        })
    }

    /// The largest slope between consecutive knots: the scale for slope tolerances.
    fn steepest(knots: &[(f64, f64)]) -> f64 {
        knots.windows(2).fold(0.0, |m, w| m.max((w[1].1 - w[0].1) / (w[1].0 - w[0].0)))
    }

    proptest! {
        #[test]
        fn passes_exactly_through_the_knots(knots in knots()) {
            let s = spline(&knots);
            for &(x, y) in &knots {
                prop_assert_eq!(s.value(x), y);
            }
        }

        /// No horse ever goes backwards. Rounding inside a piece may cost the last digit,
        /// hence the tolerance of a few units in the last place of the largest y.
        #[test]
        fn never_decreases(knots in knots(), extra in prop::collection::vec(0.0..1.0f64, 0..64)) {
            let s = spline(&knots);
            let (first, last) = (knots[0].0, knots[knots.len() - 1].0);
            let span = last - first;
            let mut xs: Vec<f64> = (0..=1000).map(|i| first - 0.1 * span + 1.2 * span * f64::from(i) / 1000.0).collect();
            xs.extend(extra.iter().map(|u| first + u * span));
            for &(x, _) in &knots {
                xs.extend([x - 1e-9, x, x + 1e-9]);
            }
            xs.sort_by(f64::total_cmp);
            let scale = knots.iter().fold(1.0f64, |m, &(_, y)| m.max(y.abs()));
            let tolerance = 8.0 * f64::EPSILON * scale;
            for pair in xs.windows(2) {
                let (a, b) = (s.value(pair[0]), s.value(pair[1]));
                prop_assert!(b >= a - tolerance, "y {a} at x {} but {b} at x {}", pair[0], pair[1]);
            }
        }

        /// Sampled a frame apart (at 240 Hz, or every millisecond, x in seconds), the curve
        /// never decreases, without any tolerance: rounding only matters between x values a
        /// few units in the last place apart.
        #[test]
        fn never_decreases_between_samples_a_frame_apart(knots in knots(), phase in 0.0..1.0f64) {
            let s = spline(&knots);
            let (first, last) = (knots[0].0, knots[knots.len() - 1].0);
            for step in [1.0 / 240.0, 0.001] {
                let mut previous = s.value(first - step);
                let mut i = 0.0;
                loop {
                    let x = first + (i + phase) * step;
                    let y = s.value(x);
                    prop_assert!(y >= previous, "y {previous} then {y} at x {x}, {step} later");
                    if x > last {
                        break;
                    }
                    previous = y;
                    i += 1.0;
                }
            }
        }

        /// The slope is the derivative of the curve: central differences inside each piece.
        #[test]
        fn the_slope_matches_finite_differences(knots in knots(), u in 0.01..0.99f64) {
            let s = spline(&knots);
            let tolerance = 1e-6 * (1.0 + steepest(&knots));
            for pair in knots.windows(2) {
                let ((x0, _), (x1, _)) = (pair[0], pair[1]);
                let (x, h) = (x0 + u * (x1 - x0), 1e-4 * (x1 - x0));
                let estimate = (s.value(x + h) - s.value(x - h)) / (2.0 * h);
                let slope = s.derivative(x);
                prop_assert!((estimate - slope).abs() <= tolerance, "x {x}: {slope} against {estimate}");
            }
        }

        /// Between two knots the curve stays between their y, even one unit in the last place
        /// from a knot, where rounding bites: a horse never runs past its finish (though rounding
        /// can make it reach the finish just before its arrival).
        #[test]
        fn stays_between_its_knots(knots in knots(), u in 0.0..1.0f64) {
            let s = spline(&knots);
            for pair in knots.windows(2) {
                let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
                for x in [x0.next_up(), x0 + u * (x1 - x0), x1.next_down()] {
                    let y = s.value(x);
                    prop_assert!(y0 <= y && y <= y1, "y {y} at x {x}, between knots at {y0} and {y1}");
                }
            }
        }

        /// The speed is never negative, even one unit in the last place from a knot.
        #[test]
        fn the_slope_is_never_negative(knots in knots(), u in 0.0..1.0f64) {
            let s = spline(&knots);
            for pair in knots.windows(2) {
                let ((x0, _), (x1, _)) = (pair[0], pair[1]);
                for x in [x0, x0.next_up(), x0 + u * (x1 - x0), x1.next_down(), x1] {
                    let slope = s.derivative(x);
                    prop_assert!(slope >= 0.0, "slope {slope} at x {x}");
                }
            }
        }

        /// Speed never jumps: at each knot the slope arriving from the left meets the slope
        /// leaving to the right, next to a flat step too, where both must be 0.
        #[test]
        fn the_slope_is_continuous_at_the_knots(knots in knots()) {
            let s = spline(&knots);
            let tolerance = 1e-6 * (1.0 + steepest(&knots));
            for pair in knots.windows(2) {
                let ((x0, _), (x1, _)) = (pair[0], pair[1]);
                let (left, right) = (s.derivative(x1 - 1e-9 * (x1 - x0)), s.derivative(x1));
                prop_assert!((left - right).abs() <= tolerance, "x {x1}: {left} against {right}");
            }
        }
    }
}
