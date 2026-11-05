//! The picker contract: what a picker declares, the two traits it implements, and what it is
//! given (design doc, § Modello a plugin dei picker).
//!
//! A picker never chooses. `sorte-core` decides the outcome; the picker turns it into a
//! choreography, a pure function of time that stages it. Adding a picker therefore cannot make
//! the app less fair, and the contract suite ([`crate::contract`]) checks that every staging
//! lands on the outcome it was given.

use std::ops::RangeInclusive;

use kurbo::Point;
use sorte_anim::Seconds;
use sorte_core::{EntryId, Outcome, RankingRule};

use crate::frame::{Frame, Viewport};
use crate::list::ListView;
use crate::localized::LocalizedStr;
use crate::pointer::{Gesture, PointerEvent};
use crate::rng::CosmeticRng;
use crate::settings::{PickerSettings, SettingsSchema};
use crate::spin::{SpinRequest, TimedEvent};
use crate::text::TextEngine;

/// What a picker can stage and how the app presents it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerMeta {
    /// Stored in each list to name its picker: `"wheel"`, `"horse-race"`. Never changes.
    pub id: &'static str,
    pub name: LocalizedStr,
    pub outcome: OutcomeKind,
    /// How many visible entries the picker can stage. For a list outside the range the picker
    /// stays listed but disabled, with the reason.
    pub entries: RangeInclusive<usize>,
    pub supports_images: bool,
}

impl PickerMeta {
    /// Whether a list of `count` visible entries suits the picker, and the reason when not.
    pub fn accepts(&self, count: usize) -> Result<(), PickerError> {
        if self.entries.contains(&count) {
            Ok(())
        } else {
            Err(PickerError::EntryCount { count, accepted: self.entries.clone() })
        }
    }
}

/// The kind of result a picker stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OutcomeKind {
    /// One entry is chosen (the wheel).
    Single,
    /// Every entry gets a place, the chosen one first or last (the race). The result overlay
    /// shows the ranking.
    Ranking,
}

/// What a choreography's final frame shows as the result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Single(EntryId),
    Ranking(Vec<EntryId>),
}

impl Shown {
    /// What a picker of `kind` must show for `outcome`: its selected entry, or its ranking.
    /// `None` for a ranking picker given an outcome without a ranking.
    pub fn expected(kind: OutcomeKind, outcome: &Outcome) -> Option<Self> {
        match kind {
            OutcomeKind::Single => Some(Self::Single(outcome.selected)),
            OutcomeKind::Ranking => outcome.ranking.clone().map(Self::Ranking),
        }
    }
}

/// Why a picker cannot stage a list.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PickerError {
    #[error("the list has {count} visible entries, the picker takes {} to {}", accepted.start(), accepted.end())]
    EntryCount { count: usize, accepted: RangeInclusive<usize> },
}

/// What pickers share to build their scenes. Images join in v1.
#[derive(Debug)]
pub struct Assets {
    pub text: TextEngine,
}

/// A way of staging a draw, such as the wheel or the horse race.
///
/// Pickers are compiled into the app and listed in its registry; core, store and UI only ever
/// see this trait.
///
/// The controller, not the picker, keeps the state on screen at rest (for the wheel, its
/// angle): a choreography ends in it ([`Choreography::rest`]), gestures move it
/// ([`on_pointer`](Self::on_pointer)), idle motion carries it along
/// ([`idle_rest`](Self::idle_rest)), and the next draw starts from it
/// ([`SpinRequest::start`]). A picker keeps only what [`prepare`](Self::prepare) built from its
/// arguments, and draws the same frames as any other picker prepared the same way.
pub trait Picker {
    /// The same value for the picker's whole life.
    fn meta(&self) -> &PickerMeta;

    /// The settings the picker offers: the Aspetto panel is generated from it, and the list's
    /// `picker_settings` JSON is checked against it before reaching [`prepare`](Self::prepare).
    /// What only this picker stages by, such as the wheel's speed level, belongs here.
    fn settings_schema(&self) -> SettingsSchema;

    /// Rebuilds the static scene for `list` (its visible entries, in order), `settings` and
    /// `viewport`, the size of the frames to come: what is measured in pixels (label sizes,
    /// which slices are too narrow to label, stroke widths) is decided here. The controller
    /// calls it again whenever any of them changes, the viewport once a resize settles; until
    /// then [`render_idle`](Self::render_idle) and [`Choreography::render`] keep drawing into
    /// the frames they get, scaling what was prepared to the frame's size, which is also how an
    /// offscreen export at twice the size can be drawn.
    ///
    /// The result depends on the arguments alone (no clock, no counter, no randomness): two
    /// pickers prepared the same way draw the same frames, so a draw can be replayed. An error
    /// leaves the picker unusable until a later `prepare` succeeds: the app keeps it disabled
    /// and shows the reason.
    fn prepare(
        &mut self,
        list: &ListView,
        settings: &PickerSettings,
        viewport: Viewport,
        assets: &mut Assets,
    ) -> Result<(), PickerError>;

    /// Where a ranking picker puts the drawn entry, from the prepared settings (the race's
    /// "vince chi arriva primo" or "ultimo"): the controller passes it to
    /// `sorte_core::draw_ranking`. `Some` for a ranking picker, `None` for a single one, the
    /// default.
    fn ranking_rule(&self) -> Option<RankingRule> {
        None
    }

    /// Draws the picker at rest: `rest` is the state on screen, `t` the seconds since the
    /// controller last set it (a draw ended, a gesture moved it). A function of its arguments
    /// and of what `prepare` built alone, like [`Choreography::render`]: a gesture reaches it
    /// only through `rest`. It may change with `t` (the wheel's idle rotation) only while
    /// [`idle_animates`](Self::idle_animates) says so, since otherwise nothing draws at rest.
    fn render_idle(&self, frame: &mut Frame, rest: f64, t: Seconds);

    /// Whether [`render_idle`](Self::render_idle) changes with `t`: the frame loop keeps
    /// drawing at rest only while this is true, so CPU and GPU stay near zero when nothing
    /// moves. By default nothing moves at rest.
    fn idle_animates(&self) -> bool {
        false
    }

    /// The state [`render_idle`](Self::render_idle) shows `t` seconds after `rest` was set:
    /// where idle motion has carried it. It is `rest` itself at `t = 0`, so coming to rest
    /// does not jump, and at every `t` when nothing moves at rest (the default). The controller
    /// starts the next draw, or a gesture, from it.
    fn idle_rest(&self, rest: f64, _t: Seconds) -> f64 {
        rest
    }

    /// Follows a gesture over the scene (dragging the wheel): `rest` is the state on screen, as
    /// [`idle_rest`](Self::idle_rest) gives it, and the answer says what the event does to it.
    /// The controller keeps the moved rest and draws it; the picker keeps only the gesture's
    /// own bookkeeping, such as where the press was, never a state it draws. Only a release
    /// ([`PointerPhase::Up`](crate::PointerPhase::Up)) may fling, and every number returned
    /// is finite. By default pickers take no gestures.
    fn on_pointer(&mut self, _event: &PointerEvent, _rest: f64) -> Gesture {
        Gesture::None
    }

    /// The entry drawn under `position` (in physical pixels) when
    /// [`render_idle`](Self::render_idle) draws `rest` at `t` into a frame of `viewport`: the
    /// tooltip names the entries whose labels are too small to draw. `None` over no entry,
    /// the default.
    fn entry_at(&self, _position: Point, _viewport: Viewport, _rest: f64, _t: Seconds) -> Option<EntryId> {
        None
    }

    /// Builds the choreography that lands on `outcome`, which `sorte-core` drew from the list
    /// last given to [`prepare`](Self::prepare); for a ranking picker it carries the ranking,
    /// ordered by [`ranking_rule`](Self::ranking_rule). Its looks may vary only through `rng`:
    /// the same outcome, request and generator state give the same choreography. It owns what
    /// it draws (prepared pictures shared through `Arc`), so a later `prepare` cannot change a
    /// draw under way.
    fn choreograph(&self, outcome: &Outcome, request: &SpinRequest, rng: &mut CosmeticRng) -> Box<dyn Choreography>;
}

/// One staged draw: a pure function of time from the start state to the outcome.
///
/// The frame loop calls [`render`](Self::render) until `t` reaches [`duration`](Self::duration);
/// the audio thread schedules [`events`](Self::events) ahead on its own clock; the controller
/// keeps [`rest`](Self::rest) for the next draw and shows the result.
pub trait Choreography {
    /// Positive, at most the request's duration (after a fling, at most the longest duration
    /// a list may ask for), and under [`SpinRequest::REDUCED_MOTION_LIMIT`] with reduce motion.
    fn duration(&self) -> Seconds;

    /// Draws the state at `t` seconds from the start, computed from `t` alone: never
    /// accumulate per-frame deltas, so the same `t` gives the same frame at any refresh rate.
    /// Times past the end draw the frame at the end, held: the last frame of a draw often
    /// falls past it. It records no picture: it draws again those `prepare` or `choreograph`
    /// recorded, so the stage reuses their encoding.
    fn render(&self, frame: &mut Frame, t: Seconds);

    /// Every event, in time order and within `0..=duration`: [`Start`](crate::EventKind::Start)
    /// first, [`Arrive`](crate::EventKind::Arrive) last at exactly the duration.
    fn events(&self) -> &[TimedEvent];

    /// The state at the end (for the wheel, its angle), always finite. The controller keeps it
    /// for [`Picker::render_idle`] and the next request's start.
    fn rest(&self) -> f64;

    /// What the final frame shows as the result, read from the choreography's own end state
    /// (for the wheel, the slice under the pointer at the rest angle) rather than copied from
    /// the outcome, so the contract suite can catch a staging that lands elsewhere.
    fn shown(&self) -> Shown;

    /// A manual stop at `at` seconds (F7, the stop brings the end forward): a choreography that
    /// continues from the state at `at` and lands on the same outcome, sooner.
    ///
    /// - It ends no later than this one would: `at + duration ≤ self.duration()`.
    /// - Its first frame is this one's frame at `at`, up to rounding, motion blur included: the
    ///   stop changes where the motion is going, never where it is or how fast (for the wheel,
    ///   the same angle at the same velocity).
    /// - Its events hold no `Start`, since the motion is already under way.
    ///
    /// `None` when the picker cannot stop sooner, the default.
    fn replan_stop(&self, _at: Seconds) -> Option<Box<dyn Choreography>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(entries: RangeInclusive<usize>) -> PickerMeta {
        PickerMeta {
            id: "test",
            name: LocalizedStr::new("Prova", "Test"),
            outcome: OutcomeKind::Single,
            entries,
            supports_images: false,
        }
    }

    #[test]
    fn entry_counts_outside_the_range_are_refused_with_the_range() {
        let meta = meta(2..=12);
        assert_eq!(meta.accepts(2), Ok(()));
        assert_eq!(meta.accepts(12), Ok(()));
        let error = meta.accepts(13).expect_err("too many entries");
        assert_eq!(error, PickerError::EntryCount { count: 13, accepted: 2..=12 });
        assert_eq!(error.to_string(), "the list has 13 visible entries, the picker takes 2 to 12");
        assert!(meta.accepts(1).is_err());
    }

    #[test]
    fn single_pickers_show_the_selected_entry_and_ranking_pickers_the_ranking() {
        let (a, b) = (EntryId::random(), EntryId::random());
        let single = Outcome { selected: a, ranking: None };
        let ranked = Outcome { selected: a, ranking: Some(vec![a, b]) };
        assert_eq!(Shown::expected(OutcomeKind::Single, &single), Some(Shown::Single(a)));
        assert_eq!(Shown::expected(OutcomeKind::Single, &ranked), Some(Shown::Single(a)));
        assert_eq!(Shown::expected(OutcomeKind::Ranking, &ranked), Some(Shown::Ranking(vec![a, b])));
        assert_eq!(Shown::expected(OutcomeKind::Ranking, &single), None);
    }
}
