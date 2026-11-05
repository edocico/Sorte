//! What the controller asks of a draw's staging, and the moments a choreography announces.

use std::ops::RangeInclusive;

use sorte_anim::Seconds;

/// How a draw should be staged: the controller fills it from the list's settings, the
/// preferences, the state on screen and a fling, if the draw started with one.
///
/// It holds only what every picker honours. What one picker alone offers, such as the wheel's
/// speed level, is in its own settings ([`Picker::settings_schema`](crate::Picker::settings_schema)),
/// read in `prepare`: the controller never needs to know a picker's keys.
///
/// Every value is brought into its documented range on the way in, so a picker can rely on the
/// ranges without checking them again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpinRequest {
    duration: Seconds,
    reduce_motion: bool,
    start: f64,
    fling: Option<f64>,
}

impl SpinRequest {
    /// The durations a list may ask for, in seconds. A fling's staging stays within them too.
    pub const DURATIONS: RangeInclusive<Seconds> = 1.0..=30.0;
    /// With reduce motion on, a choreography lasts less than this, in seconds.
    pub const REDUCED_MOTION_LIMIT: Seconds = 1.0;

    /// A request lasting `duration` seconds, from a rest state of 0, with motion in full and no
    /// fling. A duration out of range takes the nearest bound; a NaN one the shortest.
    pub fn new(duration: Seconds) -> Self {
        let (shortest, longest) = (*Self::DURATIONS.start(), *Self::DURATIONS.end());
        let duration = if duration.is_nan() { shortest } else { duration.clamp(shortest, longest) };
        Self { duration, reduce_motion: false, start: 0.0, fling: None }
    }

    /// Starts from `rest`, the state on screen: the rest the previous choreography ended in, as
    /// gestures and idle motion moved it since ([`Picker::idle_rest`](crate::Picker::idle_rest)).
    /// A rest that is not a finite number counts as 0.
    pub fn with_start(self, rest: f64) -> Self {
        Self { start: if rest.is_finite() { rest } else { 0.0 }, ..self }
    }

    /// Starts with a fling of `omega`: the release velocity of a
    /// [`Gesture::Fling`](crate::Gesture::Fling). A velocity that is not a finite number is no
    /// fling.
    pub fn with_fling(self, omega: f64) -> Self {
        Self { fling: omega.is_finite().then_some(omega), ..self }
    }

    /// The preferences' "Riduci animazioni": a choreography under
    /// [`REDUCED_MOTION_LIMIT`](Self::REDUCED_MOTION_LIMIT) that fades rather than moves.
    pub fn with_reduce_motion(self, reduce_motion: bool) -> Self {
        Self { reduce_motion, ..self }
    }

    /// Within [`DURATIONS`](Self::DURATIONS), in seconds. A fling may stage the draw for a
    /// different time within the same range.
    pub fn duration(&self) -> Seconds {
        self.duration
    }

    pub fn reduce_motion(&self) -> bool {
        self.reduce_motion
    }

    /// The rest state to start from (for the wheel, its angle θ₀); always finite.
    pub fn start(&self) -> f64 {
        self.start
    }

    /// The release velocity of a fling, per second (for the wheel, rad/s); always finite.
    pub fn fling(&self) -> Option<f64> {
        self.fling
    }
}

/// A moment of a choreography known in advance, so the audio thread can schedule its sound on
/// the audio clock ahead of time instead of reacting to frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedEvent {
    /// Seconds from the start of the choreography.
    pub t: Seconds,
    pub kind: EventKind,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    /// The staging starts moving.
    Start,
    /// The pointer passes a slice edge, or the picker's equivalent. `strength`, in [0, 1], says
    /// how loud: a narrow slice's edge is a small peg.
    Tick { strength: f32 },
    /// The staging lands on the outcome: the last event, at the choreography's end.
    Arrive,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_in_range_are_kept() {
        let request = SpinRequest::new(12.5).with_start(-3.25).with_fling(-40.0).with_reduce_motion(true);
        assert_eq!(request.duration(), 12.5);
        assert_eq!(request.start(), -3.25);
        assert_eq!(request.fling(), Some(-40.0));
        assert!(request.reduce_motion());
    }

    #[test]
    fn a_new_request_starts_at_rest_in_full_motion() {
        let request = SpinRequest::new(8.0);
        assert_eq!((request.start(), request.fling(), request.reduce_motion()), (0.0, None, false));
    }

    #[test]
    fn durations_out_of_range_take_the_nearest_bound() {
        assert_eq!(SpinRequest::new(0.2).duration(), 1.0);
        assert_eq!(SpinRequest::new(-4.0).duration(), 1.0);
        assert_eq!(SpinRequest::new(1e9).duration(), 30.0);
        assert_eq!(SpinRequest::new(f64::INFINITY).duration(), 30.0);
        assert_eq!(SpinRequest::new(f64::NAN).duration(), 1.0);
    }

    #[test]
    fn values_that_are_not_finite_numbers_are_dropped() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let request = SpinRequest::new(8.0).with_start(bad).with_fling(bad);
            assert_eq!((request.start(), request.fling()), (0.0, None), "{bad}");
        }
    }
}
