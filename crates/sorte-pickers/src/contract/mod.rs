//! The contract suite every picker crate runs from its own tests (design doc, § Modello a plugin
//! dei picker, "Aggiungere un picker"):
//!
//! ```ignore
//! #[test]
//! fn keeps_the_picker_contract() {
//!     sorte_pickers::contract::assert_conforms(Wheel::new, 1);
//! }
//! ```
//!
//! Over random lists within the picker's entry range (random weights, labels and colours, some
//! entries hidden), viewports and settings, gestures, outcomes drawn by `sorte-core` under the
//! picker's own ranking rule, and random spin requests, it checks that each choreography shows
//! its outcome, keeps to its duration, announces its events in order, draws the same frame
//! whenever it is asked for the same time and holds its last frame past the end, that a manual
//! stop ends sooner from where the draw was, that idle motion is announced, and that what a
//! picker draws depends on what it was prepared with, the rest state it is given and the
//! cosmetic generator alone: never on gestures it saw or on anything else. A failure, a panic
//! included, names the seed and the case: the same seed replays it.

mod cases;
mod checks;

use std::any::Any;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};

use rand::{Rng, SeedableRng};
use serde_json::Value;

use self::cases::CaseRng;
use crate::list::ListView;
use crate::picker::{Assets, Picker, Shown};
use crate::rng::CosmeticRng;
use crate::text::TextEngine;

/// Random lists per run.
const LISTS: usize = 24;
/// Lists one picker is prepared with in turn before the suite starts over with a fresh one, so
/// both fresh and re-prepared pickers are checked.
const LISTS_PER_PICKER: usize = 4;
/// Draws staged per list, each from the rest state the previous one left.
const DRAWS_PER_LIST: usize = 4;

/// Runs [`check`] over the system's fonts and panics with the violation, if any: the one call a
/// picker crate's test makes.
pub fn assert_conforms<P: Picker>(make: impl FnMut() -> P, seed: u64) {
    let text = TextEngine::new([]).expect("a text engine needs no font file");
    if let Err(violation) = check(make, &mut Assets { text }, seed) {
        panic!("{violation}");
    }
}

/// Checks the pickers `make` builds against the contract, over cases generated from `seed`,
/// and returns the first violation. A picker that panics is a violation too, reported with the
/// case it panicked in.
pub fn check<P: Picker>(mut make: impl FnMut() -> P, assets: &mut Assets, seed: u64) -> Result<(), Violation> {
    let mut at = Progress { picker: "an unnamed picker", case: "making the picker".to_owned() };
    // A panic may leave the picker or the assets half-updated: the suite stops there and uses
    // neither again.
    match panic::catch_unwind(AssertUnwindSafe(|| suite(&mut make, assets, seed, &mut at))) {
        Ok(result) => result,
        Err(payload) => Err(Violation {
            picker: at.picker,
            check: Check::Panic,
            seed,
            case: at.case,
            detail: format!("the picker panicked: {}", panic_message(&*payload)),
        }),
    }
}

/// Where the suite is, for a picker that panics.
struct Progress {
    picker: &'static str,
    case: String,
}

fn suite<P: Picker>(
    make: &mut impl FnMut() -> P,
    assets: &mut Assets,
    seed: u64,
    at: &mut Progress,
) -> Result<(), Violation> {
    let mut picker = make();
    at.case = "the metadata".to_owned();
    let meta = picker.meta().clone();
    at.picker = meta.id;
    let violation = |case: &str, failure: checks::Failure| failure.at(meta.id, seed, case);
    checks::meta(&meta).map_err(|f| violation("the metadata", f))?;
    at.case = "the settings schema".to_owned();
    let schema = picker.settings_schema();
    checks::schema(&schema).map_err(|f| violation("the settings schema", f))?;

    let mut rng = CaseRng::seed_from_u64(seed);
    let mut rest = 0.0;
    for list_index in 0..LISTS {
        if list_index > 0 && list_index % LISTS_PER_PICKER == 0 {
            picker = make();
            rest = 0.0;
        }
        let list = cases::list(&mut rng, &meta.entries);
        let view = ListView::new(&list);
        let viewport = cases::viewport(&mut rng);
        // The first list keeps every default, as a new list does.
        let settings = if list_index == 0 { schema.validate(&Value::Null) } else { cases::settings(&mut rng, &schema) };
        let list_case =
            format!("list {list_index} ({}, {})", cases::describe_list(&list), cases::describe_viewport(viewport));
        at.case.clone_from(&list_case);
        let in_list = |failure| violation(&list_case, failure);
        picker.prepare(&view, &settings, viewport, assets).map_err(|error| in_list(checks::refused(&error)))?;
        let rule = picker.ranking_rule();
        checks::rule(meta.outcome, rule).map_err(in_list)?;
        // A fresh picker prepared the same way, which sees no gesture: the reference for what
        // the picker may draw.
        let mut twin = make();
        twin.prepare(&view, &settings, viewport, assets).map_err(|error| in_list(checks::refused(&error)))?;
        let idle = checks::idle(&picker, rest, viewport, &mut rng).map_err(in_list)?;
        checks::replayed_idle(&picker, &twin, &idle, viewport).map_err(in_list)?;
        checks::hover(&picker, &view, viewport, rest, &mut rng).map_err(in_list)?;
        // The controller keeps the rest a gesture leaves, and draws next with its fling.
        let (moved, mut fling) = checks::gesture(&mut picker, &twin, viewport, rest, &mut rng).map_err(in_list)?;
        rest = moved;

        for draw_index in 0..DRAWS_PER_LIST {
            let outcome = cases::outcome(&list, meta.outcome, rule, &mut rng);
            let expected = Shown::expected(meta.outcome, &outcome).expect("ranking pickers get a drawn ranking");
            let request = cases::request(&mut rng, rest, fling.take());
            let looks = rng.next_u64();
            let case = format!(
                "{list_case}, draw {draw_index} (outcome {}, {}, looks seed {looks})",
                cases::describe_shown(&expected, &view),
                cases::describe_request(&request),
            );
            at.case.clone_from(&case);
            let in_draw = |failure| violation(&case, failure);
            let draw = checks::Draw { request: &request, expected: &expected, view: &view, viewport };
            let staged = picker.choreograph(&outcome, &request, &mut CosmeticRng::seeded(looks));
            let frames = checks::choreography(&*staged, &draw, &mut rng).map_err(in_draw)?;
            let again = picker.choreograph(&outcome, &request, &mut CosmeticRng::seeded(looks));
            checks::same_looks(&*staged, &*again, &frames, viewport).map_err(in_draw)?;
            let replayed = twin.choreograph(&outcome, &request, &mut CosmeticRng::seeded(looks));
            checks::replayed(&*staged, &*replayed, &frames, viewport).map_err(in_draw)?;
            checks::replans(&*staged, &draw, &mut rng).map_err(in_draw)?;
            rest = staged.rest();
        }
    }
    Ok(())
}

/// The message a panic carried, when it carried one.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    let text = payload.downcast_ref::<&str>().copied();
    text.or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("a panic without a message")
}

/// Which promise of the contract a picker broke.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Check {
    /// `meta()`: an id that is not lowercase ASCII words joined by hyphens, a blank name, or an
    /// entry range that is empty or admits an empty list.
    Meta,
    /// `settings_schema()`: a blank or repeated key or label, a default outside its bounds, a
    /// step that is not positive, or a default choice that is not an option.
    Schema,
    /// `prepare` refused a list within the picker's entry range.
    Prepare,
    /// `ranking_rule()` is `None` for a ranking picker, or `Some` for a single one.
    Rule,
    /// `shown()` is not the outcome.
    Result,
    /// The duration is not positive, overruns the request, or ignores reduce motion.
    Duration,
    /// Events out of order or outside the duration, a tick strength outside [0, 1], or the
    /// sequence not opening with `Start` and closing with `Arrive` at the end.
    Events,
    /// `rest()` is not finite.
    Rest,
    /// A frame holds a number that is not finite.
    Frame,
    /// Two renders of the same time drew different frames, or the same frame with its pictures
    /// recorded anew.
    Determinism,
    /// A choreography draws another frame past its end than at its end.
    End,
    /// The same outcome, request and cosmetic seed gave different choreographies.
    Looks,
    /// A fresh picker prepared with the same list, settings and viewport drew differently:
    /// `prepare` depended on something besides its arguments.
    Replay,
    /// Idle motion that `idle_animates()` does not announce, or `idle_rest` not finite or not
    /// starting from the rest state.
    Idle,
    /// A choreography from `replan_stop` broke one of the promises above, ends after the draw
    /// would have, or does not start from the frame the draw was at.
    Replan,
    /// `on_pointer` flung before the release, answered a number that is not finite, or left
    /// the picker drawing differently from one that saw no gesture; or `entry_at` named an
    /// entry not in the list.
    Pointer,
    /// The picker panicked.
    Panic,
}

/// A broken promise, with what it takes to replay it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    /// The picker's id.
    pub picker: &'static str,
    pub check: Check,
    /// The seed the suite ran with: the same seed generates the same cases.
    pub seed: u64,
    /// The generated case that failed.
    pub case: String,
    /// What went wrong.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { picker, check, seed, case, detail } = self;
        write!(f, "picker {picker:?} breaks the contract ({check:?}) with seed {seed}, at {case}: {detail}")
    }
}

impl std::error::Error for Violation {}

#[cfg(test)]
mod tests;
