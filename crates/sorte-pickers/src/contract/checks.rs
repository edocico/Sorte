//! The contract's checks, one per promise a picker makes. Each returns the first [`Failure`] it
//! finds; the suite adds the picker, the seed and the case to it.

use std::collections::HashSet;

use rand::RngExt;
use rand::seq::SliceRandom;
use sorte_core::RankingRule;

use super::cases::{self, CaseRng};
use super::{Check, Violation};
use crate::frame::{Frame, Viewport};
use crate::list::ListView;
use crate::localized::LocalizedStr;
use crate::picker::{Choreography, OutcomeKind, Picker, PickerError, PickerMeta, Shown};
use crate::pointer::{Gesture, PointerPhase};
use crate::settings::{SettingField, SettingsSchema};
use crate::spin::{EventKind, SpinRequest, TimedEvent};

/// A failed check, before the suite says where it happened.
#[derive(Debug)]
pub(super) struct Failure {
    check: Check,
    detail: String,
}

impl Failure {
    fn new(check: Check, detail: impl Into<String>) -> Self {
        Self { check, detail: detail.into() }
    }

    pub(super) fn at(self, picker: &'static str, seed: u64, case: &str) -> Violation {
        Violation { picker, check: self.check, seed, case: case.to_owned(), detail: self.detail }
    }
}

/// Frames rendered at sampled times, kept to compare a twin choreography against.
pub(super) type Sampled = Vec<(f64, Frame)>;

/// Idle frames rendered at sampled times, for each rest state sampled.
pub(super) type IdleSamples = Vec<(f64, Sampled)>;

/// Where a choreography's events begin.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// A new draw: `Start` first.
    Start,
    /// A manual stop's replan, which continues a motion under way: no `Start`.
    Continuing,
}

/// Times idle rendering is sampled at, up to an hour at rest; 0 first.
const IDLE_TIMES: [f64; 4] = [0.0, 0.37, 61.0, 3600.0];

/// Relative slack between two frames that draw the same state computed two ways (a replan's
/// start, a state held past the end): far below a visible step, far above rounding.
const FRAME_TOLERANCE: f64 = 1e-6;

pub(super) fn meta(meta: &PickerMeta) -> Result<(), Failure> {
    let fail = |detail: String| Err(Failure::new(Check::Meta, detail));
    let word = |word: &str| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !meta.id.split('-').all(word) {
        return fail(format!("the id {:?} is not lowercase ASCII words joined by hyphens", meta.id));
    }
    if blank(meta.name) {
        return fail(format!("the name {:?} is blank in a language", meta.name));
    }
    if meta.entries.is_empty() || *meta.entries.start() == 0 {
        return fail(format!("the entry range {:?} must start at 1 or more and hold a count", meta.entries));
    }
    Ok(())
}

pub(super) fn schema(schema: &SettingsSchema) -> Result<(), Failure> {
    let fail = |detail: String| Err(Failure::new(Check::Schema, detail));
    let mut keys = HashSet::new();
    for field in schema.fields() {
        let key = field.key();
        if key.trim().is_empty() || !keys.insert(key) {
            return fail(format!("the key {key:?} is blank or used twice"));
        }
        if blank(field.label()) {
            return fail(format!("{key:?} has a blank label"));
        }
        match field {
            SettingField::Bool(_) => {}
            SettingField::Int(f) if !(f.min <= f.default && f.default <= f.max) => {
                return fail(format!("{key:?}: the default {} is outside {}..={}", f.default, f.min, f.max));
            }
            SettingField::Int(_) => {}
            SettingField::Float(f) => {
                if ![f.min, f.max, f.step, f.default].iter().all(|v| v.is_finite()) {
                    return fail(format!("{key:?} has a bound, step or default that is not finite"));
                }
                if !(f.min <= f.default && f.default <= f.max) {
                    return fail(format!("{key:?}: the default {} is outside {}..={}", f.default, f.min, f.max));
                }
                if f.step <= 0.0 {
                    return fail(format!("{key:?}: the step {} is not positive", f.step));
                }
            }
            SettingField::Choice(f) => {
                let mut options = HashSet::new();
                for option in f.options {
                    if option.key.trim().is_empty() || !options.insert(option.key) || blank(option.label) {
                        return fail(format!("{key:?}: the option {:?} is blank or used twice", option.key));
                    }
                }
                if !options.contains(f.default) {
                    return fail(format!("{key:?}: the default {:?} is not one of the options", f.default));
                }
            }
        }
    }
    Ok(())
}

/// `prepare` refused a list the picker's metadata accepts.
pub(super) fn refused(error: &PickerError) -> Failure {
    Failure::new(Check::Prepare, format!("prepare refused a list within the entry range: {error}"))
}

/// A ranking picker names the rule its ranking is drawn under; a single picker names none.
pub(super) fn rule(kind: OutcomeKind, rule: Option<RankingRule>) -> Result<(), Failure> {
    match (kind, rule) {
        (OutcomeKind::Single, None) | (OutcomeKind::Ranking, Some(_)) => Ok(()),
        (OutcomeKind::Single, Some(rule)) => {
            Err(Failure::new(Check::Rule, format!("a single picker names the ranking rule {rule:?}")))
        }
        (OutcomeKind::Ranking, None) => {
            Err(Failure::new(Check::Rule, "a ranking picker names no ranking rule, so its ranking cannot be drawn"))
        }
    }
}

/// Idle frames are finite and depend on the rest state and time alone; idle motion starts from
/// the rest state and is announced. Returns the frames, to compare a twin picker against.
pub(super) fn idle<P: Picker>(
    picker: &P,
    rest: f64,
    viewport: Viewport,
    rng: &mut CaseRng,
) -> Result<IdleSamples, Failure> {
    let animates = picker.idle_animates();
    let mut samples = Vec::new();
    for rest in [rest, 0.0, rng.random_range(-50.0..50.0)] {
        let what = format!("render_idle at rest {rest}");
        let frames = renders(&IDLE_TIMES, |frame, t| picker.render_idle(frame, rest, t), viewport, rng, &what)?;
        idle_motion(picker, rest, animates, &frames)?;
        samples.push((rest, frames));
    }
    Ok(samples)
}

/// `idle_rest` is finite and is `rest` at `t = 0`; without `idle_animates`, neither it nor the
/// idle frames change with time.
fn idle_motion<P: Picker>(picker: &P, rest: f64, animates: bool, frames: &Sampled) -> Result<(), Failure> {
    let fail = |detail: String| Err(Failure::new(Check::Idle, detail));
    for t in IDLE_TIMES {
        let moved = picker.idle_rest(rest, t);
        if !moved.is_finite() {
            return fail(format!("idle_rest({rest}, {t}) is {moved}"));
        }
        if t == 0.0 && (moved - rest).abs() > tolerance(rest) {
            return fail(format!("idle_rest({rest}, 0) is {moved}, not the rest state: coming to rest would jump"));
        }
        if !animates && moved != rest {
            return fail(format!(
                "idle_rest({rest}, {t}) moves to {moved}, but idle_animates() is false: the next draw would \
                 start elsewhere than the still frame on screen"
            ));
        }
    }
    let (_, still) = &frames[0];
    if !animates && let Some((t, _)) = frames.iter().find(|(_, frame)| frame != still) {
        return fail(format!(
            "render_idle at rest {rest} draws another frame at t = {t} s than at 0 s, but idle_animates() is \
             false: the frame loop would stop on a moving picture"
        ));
    }
    Ok(())
}

/// A picker prepared the same way, from a fresh one, draws the same idle frames: `prepare`
/// depends on its arguments alone.
pub(super) fn replayed_idle<P: Picker>(
    picker: &P,
    twin: &P,
    samples: &IdleSamples,
    viewport: Viewport,
) -> Result<(), Failure> {
    let fail = |what: String| Err(Failure::new(Check::Replay, format!("a picker prepared the same way {what}")));
    if twin.ranking_rule() != picker.ranking_rule() {
        return fail(format!("names the ranking rule {:?}, not {:?}", twin.ranking_rule(), picker.ranking_rule()));
    }
    if twin.idle_animates() != picker.idle_animates() {
        return fail(format!("says idle_animates() is {}", twin.idle_animates()));
    }
    for (rest, frames) in samples {
        for (t, frame) in frames {
            let mut again = cases::frame(viewport);
            twin.render_idle(&mut again, *rest, *t);
            if again != *frame {
                return fail(format!("drew another idle frame at rest {rest}, t = {t} s"));
            }
        }
    }
    Ok(())
}

/// Hovering names an entry of the list, or none.
pub(super) fn hover<P: Picker>(
    picker: &P,
    view: &ListView,
    viewport: Viewport,
    rest: f64,
    rng: &mut CaseRng,
) -> Result<(), Failure> {
    let mut positions = vec![viewport.size().to_rect().center()];
    positions.extend((0..4).map(|_| cases::position(rng, viewport)));
    for position in positions {
        for t in [0.0, 61.0] {
            if let Some(id) = picker.entry_at(position, viewport, rest, t)
                && view.position(id).is_none()
            {
                return Err(Failure::new(
                    Check::Pointer,
                    format!(
                        "entry_at({}, {}) at rest {rest}, t = {t} s names an entry that is not in the list",
                        position.x, position.y
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// A gesture moves the picker only through the rest states it returns, with finite numbers,
/// and only a release flings: after every event the picker draws what a twin that saw no
/// gesture draws at the same rest. Returns where the gesture left the picker, and its fling.
pub(super) fn gesture<P: Picker>(
    picker: &mut P,
    twin: &P,
    viewport: Viewport,
    rest: f64,
    rng: &mut CaseRng,
) -> Result<(f64, Option<f64>), Failure> {
    let fail = |detail: String| Err(Failure::new(Check::Pointer, detail));
    let (mut rest, mut fling) = (rest, None);
    for event in cases::gesture(rng, viewport) {
        let phase = event.phase;
        match picker.on_pointer(&event, rest) {
            Gesture::None => {}
            Gesture::Drag { rest: dragged } if dragged.is_finite() => rest = dragged,
            Gesture::Fling { rest: from, omega }
                if phase == PointerPhase::Up && from.is_finite() && omega.is_finite() =>
            {
                (rest, fling) = (from, Some(omega));
            }
            Gesture::Fling { omega, .. } if phase != PointerPhase::Up => {
                return fail(format!("{phase:?} flung at {omega}/s: only a release flings"));
            }
            gesture => return fail(format!("{phase:?} answered {gesture:?}, with a number that is not finite")),
        }
        for t in [0.0, 0.37] {
            let (mut seen, mut unseen) = (cases::frame(viewport), cases::frame(viewport));
            picker.render_idle(&mut seen, rest, t);
            twin.render_idle(&mut unseen, rest, t);
            if seen != unseen {
                return fail(format!(
                    "after {phase:?}, render_idle at rest {rest}, t = {t} s drew another frame than a picker that \
                     saw no gesture: a gesture moves the picker only through the rest it returns"
                ));
            }
        }
    }
    Ok((rest, fling))
}

/// What a draw asked for, and what describing and rendering its staging takes.
pub(super) struct Draw<'a> {
    pub(super) request: &'a SpinRequest,
    pub(super) expected: &'a Shown,
    pub(super) view: &'a ListView,
    pub(super) viewport: Viewport,
}

/// Checks a new draw's choreography, and returns the frames it rendered.
pub(super) fn choreography(staged: &dyn Choreography, draw: &Draw<'_>, rng: &mut CaseRng) -> Result<Sampled, Failure> {
    // A fling sets its own duration, within the longest a list may ask for.
    let request = draw.request;
    let limit = if request.fling().is_some() { *SpinRequest::DURATIONS.end() } else { request.duration() };
    staging(staged, draw, limit, Opening::Start, rng)
}

/// The same outcome, request and cosmetic seed give the same choreography: looks come from the
/// cosmetic generator alone.
pub(super) fn same_looks(
    a: &dyn Choreography,
    b: &dyn Choreography,
    sampled: &Sampled,
    viewport: Viewport,
) -> Result<(), Failure> {
    same_staging(a, b, sampled, viewport)
        .map_err(|what| Failure::new(Check::Looks, format!("the same cosmetic seed gave {what}")))
}

/// A picker prepared the same way, from a fresh one, stages the same draw the same way.
pub(super) fn replayed(
    a: &dyn Choreography,
    b: &dyn Choreography,
    sampled: &Sampled,
    viewport: Viewport,
) -> Result<(), Failure> {
    same_staging(a, b, sampled, viewport).map_err(|what| {
        Failure::new(Check::Replay, format!("a picker prepared the same way, with the same cosmetic seed, gave {what}"))
    })
}

/// What differs between two choreographies of the same draw, if anything.
fn same_staging(
    a: &dyn Choreography,
    b: &dyn Choreography,
    sampled: &Sampled,
    viewport: Viewport,
) -> Result<(), String> {
    if a.duration() != b.duration() {
        return Err(format!("durations of {} s and {} s", a.duration(), b.duration()));
    }
    if a.events() != b.events() {
        return Err("different events".to_owned());
    }
    if a.rest() != b.rest() || a.shown() != b.shown() {
        return Err("different end states".to_owned());
    }
    for (t, frame) in sampled {
        let mut twin = cases::frame(viewport);
        b.render(&mut twin, *t);
        if twin != *frame {
            return Err(format!("different frames at t = {t} s"));
        }
    }
    Ok(())
}

/// Manual stops halfway and at a random time either decline or replan onto the same outcome,
/// ending no later than the draw would have and starting from the frame it was at.
pub(super) fn replans(staged: &dyn Choreography, draw: &Draw<'_>, rng: &mut CaseRng) -> Result<(), Failure> {
    let duration = staged.duration();
    for at in [0.5 * duration, rng.random_range(0.0..duration)] {
        let Some(replanned) = staged.replan_stop(at) else { continue };
        let fail = |detail: String| Err(Failure::new(Check::Replan, format!("stopped at {at} s: {detail}")));
        let left = duration - at;
        if replanned.duration() > left + tolerance(duration) {
            return fail(format!(
                "it ends at {} s, after the draw would have, at {duration} s: a stop brings the end forward",
                at + replanned.duration()
            ));
        }
        if let Err(inner) = staging(&*replanned, draw, left, Opening::Continuing, rng) {
            return fail(format!("{:?}: {}", inner.check, inner.detail));
        }
        let (mut was, mut starts) = (cases::frame(draw.viewport), cases::frame(draw.viewport));
        staged.render(&mut was, at);
        replanned.render(&mut starts, 0.0);
        if let Some(part) = starts.difference(&was, FRAME_TOLERANCE) {
            return fail(format!(
                "its first frame is not the draw's frame at {at} s ({part} differs): a stop changes where the \
                 motion goes, never where it is or how fast"
            ));
        }
    }
    Ok(())
}

/// The checks every staging passes, whether a new draw or a replan: duration within `limit`,
/// events, end state, and frames, held past the end.
fn staging(
    staged: &dyn Choreography,
    draw: &Draw<'_>,
    limit: f64,
    opening: Opening,
    rng: &mut CaseRng,
) -> Result<Sampled, Failure> {
    let duration = staged.duration();
    if !duration.is_finite() || duration <= 0.0 {
        return Err(Failure::new(Check::Duration, format!("the duration is {duration} s, not a positive number")));
    }
    if draw.request.reduce_motion() && duration >= SpinRequest::REDUCED_MOTION_LIMIT {
        let detail =
            format!("{duration} s with reduce motion on: it must stay under {} s", SpinRequest::REDUCED_MOTION_LIMIT);
        return Err(Failure::new(Check::Duration, detail));
    }
    if duration > limit + tolerance(limit) {
        return Err(Failure::new(Check::Duration, format!("{duration} s overruns the {limit} s allowed")));
    }
    let events = staged.events();
    self::events(events, duration, opening)?;
    let rest = staged.rest();
    if !rest.is_finite() {
        return Err(Failure::new(Check::Rest, format!("the rest state is {rest}")));
    }
    let shown = staged.shown();
    if shown != *draw.expected {
        let detail = format!(
            "the staging shows {} but the outcome is {}",
            cases::describe_shown(&shown, draw.view),
            cases::describe_shown(draw.expected, draw.view)
        );
        return Err(Failure::new(Check::Result, detail));
    }
    // Past the end: the frame loop's last frame often falls there, and render_idle follows.
    let past = [duration + 0.25, duration + 3600.0];
    let mut times = vec![0.0, duration];
    times.extend(past);
    times.extend((0..5).map(|_| rng.random_range(0.0..duration)));
    if !events.is_empty() {
        times.push(events[rng.random_range(0..events.len())].t);
    }
    let sampled = renders(&times, |frame, t| staged.render(frame, t), draw.viewport, rng, "render")?;
    let at = |time: f64| sampled.iter().find(|(t, _)| *t == time).map(|(_, frame)| frame);
    let end = at(duration).expect("the end is sampled");
    for t in past {
        if let Some(part) = at(t).and_then(|frame| frame.difference(end, FRAME_TOLERANCE)) {
            return Err(Failure::new(
                Check::End,
                format!(
                    "at t = {t} s it draws another frame than at its end, {duration} s ({part} differs): past the end \
                     it holds the final state"
                ),
            ));
        }
    }
    Ok(sampled)
}

fn events(events: &[TimedEvent], duration: f64, opening: Opening) -> Result<(), Failure> {
    let fail = |detail: String| Err(Failure::new(Check::Events, detail));
    let Some(last) = events.last() else {
        return fail("there are no events, not even Arrive".to_owned());
    };
    let mut previous = 0.0;
    for (i, event) in events.iter().enumerate() {
        if !(0.0..=duration + tolerance(duration)).contains(&event.t) {
            return fail(format!("event {i} ({:?}) at {} s falls outside 0..={duration} s", event.kind, event.t));
        }
        if event.t < previous {
            return fail(format!("event {i} at {} s is out of order, after one at {previous} s", event.t));
        }
        previous = event.t;
        match event.kind {
            EventKind::Start if i > 0 || opening == Opening::Continuing => {
                return fail(format!("event {i} is a Start, which only opens a new draw"));
            }
            EventKind::Arrive if i + 1 < events.len() => return fail(format!("event {i} is an Arrive before the end")),
            EventKind::Tick { strength } if !(0.0..=1.0).contains(&strength) => {
                return fail(format!("tick {i} has strength {strength}, outside [0, 1]"));
            }
            _ => {}
        }
    }
    if opening == Opening::Start && events[0].kind != EventKind::Start {
        return fail(format!("the first event is {:?}, not Start", events[0].kind));
    }
    if last.kind != EventKind::Arrive {
        return fail(format!("the last event is {:?}, not Arrive", last.kind));
    }
    if (last.t - duration).abs() > tolerance(duration) {
        return fail(format!("Arrive comes at {} s, not at the end, {duration} s", last.t));
    }
    Ok(())
}

/// Renders each time once, checks every frame is finite, then renders them all again in a
/// shuffled order: a render that reads anything but its arguments, earlier calls included,
/// shows up as a different frame, and one that records its pictures again as new pictures.
fn renders(
    times: &[f64],
    render: impl Fn(&mut Frame, f64),
    viewport: Viewport,
    rng: &mut CaseRng,
    what: &str,
) -> Result<Sampled, Failure> {
    let draw = |t: f64| {
        let mut frame = cases::frame(viewport);
        render(&mut frame, t);
        frame
    };
    let first: Sampled = times.iter().map(|&t| (t, draw(t))).collect();
    for (t, frame) in &first {
        if let Some(part) = frame.non_finite() {
            return Err(Failure::new(
                Check::Frame,
                format!("{what} at t = {t} s puts a number that is not finite in a {part}"),
            ));
        }
    }
    let mut order: Vec<usize> = (0..first.len()).collect();
    order.shuffle(rng);
    for i in order {
        let (t, frame) = &first[i];
        let again = draw(*t);
        if again != *frame {
            return Err(Failure::new(
                Check::Determinism,
                format!("{what} at t = {t} s drew another frame the second time"),
            ));
        }
        if !again.same_pictures(frame) {
            return Err(Failure::new(
                Check::Determinism,
                format!(
                    "{what} at t = {t} s recorded its pictures anew: record each picture once, in prepare or \
                     choreograph, so the stage reuses its encoding"
                ),
            ));
        }
    }
    Ok(first)
}

/// Rounding slack for comparing numbers (times, rest states) computed two ways.
fn tolerance(value: f64) -> f64 {
    1e-9 * value.abs().max(1.0)
}

fn blank(text: LocalizedStr) -> bool {
    text.it.trim().is_empty() || text.en.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{BoolField, Choice, ChoiceField, FloatField, IntField};

    const NAME: LocalizedStr = LocalizedStr::new("Ruota", "Wheel");

    fn meta_with(id: &'static str, name: LocalizedStr, entries: std::ops::RangeInclusive<usize>) -> PickerMeta {
        PickerMeta { id, name, outcome: OutcomeKind::Single, entries, supports_images: false }
    }

    fn check_of<T>(result: Result<T, Failure>) -> Option<Check> {
        result.err().map(|failure| failure.check)
    }

    #[test]
    fn meta_needs_a_hyphenated_lowercase_id_names_and_a_range_from_one() {
        for id in ["wheel", "horse-race", "d20"] {
            assert!(meta(&meta_with(id, NAME, 1..=10)).is_ok(), "{id}");
        }
        for id in ["", "Wheel", "horse race", "horse--race", "-wheel", "wheel-", "ruota_2", "ruotà"] {
            assert_eq!(check_of(meta(&meta_with(id, NAME, 1..=10))), Some(Check::Meta), "{id:?}");
        }
        for name in [LocalizedStr::new("", "Wheel"), LocalizedStr::new("Ruota", "  ")] {
            assert_eq!(check_of(meta(&meta_with("wheel", name, 1..=10))), Some(Check::Meta), "{name:?}");
        }
        #[allow(clippy::reversed_empty_ranges)] // The empty range is the point.
        let empty = 5..=4;
        for entries in [0..=10, empty] {
            assert_eq!(check_of(meta(&meta_with("wheel", NAME, entries.clone()))), Some(Check::Meta), "{entries:?}");
        }
    }

    const fn label(text: &'static str) -> LocalizedStr {
        LocalizedStr::new(text, text)
    }

    const SPEED: IntField = IntField { key: "speed", label: label("speed"), min: 1, max: 10, default: 5 };
    const ZOOM: FloatField =
        FloatField { key: "zoom", label: label("zoom"), min: 0.5, max: 2.0, step: 0.1, default: 1.0 };
    const THEMES: &[Choice] = &[Choice { key: "a", label: label("a") }, Choice { key: "b", label: label("b") }];
    const THEME: ChoiceField = ChoiceField { key: "theme", label: label("theme"), options: THEMES, default: "a" };
    const MYSTERY: BoolField = BoolField { key: "mystery", label: label("mystery"), default: false };
    const TWIN_OPTIONS: &[Choice] = &[Choice { key: "a", label: label("a") }, Choice { key: "a", label: label("b") }];
    const BLANK_OPTION: &[Choice] = &[Choice { key: "a", label: label("") }];
    const SOUND: BoolField = BoolField { key: "sound", label: label("sound"), default: true };

    #[test]
    fn a_well_formed_schema_passes() {
        let fields = vec![SPEED.into(), ZOOM.into(), THEME.into(), MYSTERY.into()];
        assert!(schema(&SettingsSchema::new(fields)).is_ok());
        assert!(schema(&SettingsSchema::default()).is_ok());
    }

    #[test]
    fn each_malformed_field_fails_the_schema() {
        // Each beside a sound field of its own key, so only its own flaw can fail it.
        let malformed: [(SettingField, &str); 12] = [
            (BoolField { key: " ", ..MYSTERY }.into(), "blank or used twice"),
            (BoolField { label: LocalizedStr::new("mistero", ""), ..MYSTERY }.into(), "blank label"),
            (IntField { default: 11, ..SPEED }.into(), "the default 11 is outside 1..=10"),
            (IntField { min: 6, ..SPEED }.into(), "the default 5 is outside 6..=10"),
            (FloatField { default: 2.5, ..ZOOM }.into(), "the default 2.5 is outside"),
            (FloatField { step: 0.0, ..ZOOM }.into(), "not positive"),
            (FloatField { max: f64::INFINITY, ..ZOOM }.into(), "not finite"),
            (FloatField { default: f64::NAN, ..ZOOM }.into(), "not finite"),
            (ChoiceField { options: &[], ..THEME }.into(), "not one of the options"),
            (ChoiceField { default: "c", ..THEME }.into(), "not one of the options"),
            (ChoiceField { options: TWIN_OPTIONS, ..THEME }.into(), "blank or used twice"),
            (ChoiceField { options: BLANK_OPTION, ..THEME }.into(), "blank or used twice"),
        ];
        for (field, detail) in malformed {
            let failure = schema(&SettingsSchema::new(vec![SOUND.into(), field])).expect_err(detail);
            assert_eq!(failure.check, Check::Schema, "{field:?}");
            assert!(failure.detail.contains(detail), "{field:?}: {}", failure.detail);
        }
        let repeated = SettingsSchema::new(vec![SPEED.into(), IntField { min: 0, ..SPEED }.into()]);
        let failure = schema(&repeated).expect_err("a key used twice");
        assert!(failure.detail.contains("\"speed\" is blank or used twice"), "{}", failure.detail);
    }

    fn event(t: f64, kind: EventKind) -> TimedEvent {
        TimedEvent { t, kind }
    }

    const TICK: EventKind = EventKind::Tick { strength: 0.5 };

    #[test]
    fn well_ordered_events_pass() {
        let draw = [event(0.0, EventKind::Start), event(1.0, TICK), event(1.0, TICK), event(2.0, EventKind::Arrive)];
        assert!(events(&draw, 2.0, Opening::Start).is_ok());
        let replan = [event(0.5, TICK), event(0.75, EventKind::Arrive)];
        assert!(events(&replan, 0.75, Opening::Continuing).is_ok());
        assert!(events(&[event(0.0, EventKind::Start), event(3.0, EventKind::Arrive)], 3.0, Opening::Start).is_ok());
    }

    #[test]
    fn each_misplaced_event_fails() {
        let (start, arrive) = (EventKind::Start, EventKind::Arrive);
        let cases: [(&[TimedEvent], Opening, &str); 11] = [
            (&[], Opening::Start, "no events"),
            (&[event(0.0, start)], Opening::Start, "not Arrive"),
            (&[event(0.0, TICK), event(2.0, arrive)], Opening::Start, "not Start"),
            (&[event(0.0, start), event(0.5, start), event(2.0, arrive)], Opening::Start, "only opens a new draw"),
            (&[event(0.0, start), event(2.0, arrive)], Opening::Continuing, "only opens a new draw"),
            (&[event(0.0, start), event(1.0, arrive), event(2.0, arrive)], Opening::Start, "before the end"),
            (
                &[event(0.0, start), event(1.5, TICK), event(1.0, TICK), event(2.0, arrive)],
                Opening::Start,
                "out of order",
            ),
            (&[event(0.0, start), event(-0.1, TICK), event(2.0, arrive)], Opening::Start, "outside"),
            (&[event(0.0, start), event(f64::NAN, TICK), event(2.0, arrive)], Opening::Start, "outside"),
            (&[event(0.0, start), event(1.9, arrive)], Opening::Start, "not at the end"),
            (
                &[event(0.0, start), event(1.0, EventKind::Tick { strength: 1.5 }), event(2.0, arrive)],
                Opening::Start,
                "outside [0, 1]",
            ),
        ];
        for (sequence, opening, detail) in cases {
            let failure = events(sequence, 2.0, opening).expect_err(detail);
            assert_eq!(failure.check, Check::Events, "{sequence:?}");
            assert!(failure.detail.contains(detail), "{sequence:?}: {}", failure.detail);
        }
        let quiet = [event(0.0, start), event(1.0, EventKind::Tick { strength: -0.0 }), event(2.0, arrive)];
        assert!(events(&quiet, 2.0, Opening::Start).is_ok());
        let silent = [event(0.0, start), event(1.0, EventKind::Tick { strength: f32::NAN }), event(2.0, arrive)];
        assert!(events(&silent, 2.0, Opening::Start).is_err());
    }

    #[test]
    fn ranking_pickers_and_only_they_name_a_rule() {
        for rule_given in [RankingRule::FirstWins, RankingRule::LastWins] {
            assert!(rule(OutcomeKind::Ranking, Some(rule_given)).is_ok());
            let failure = rule(OutcomeKind::Single, Some(rule_given)).expect_err("a single picker with a rule");
            assert_eq!(failure.check, Check::Rule);
        }
        assert!(rule(OutcomeKind::Single, None).is_ok());
        let failure = rule(OutcomeKind::Ranking, None).expect_err("a ranking picker without a rule");
        assert_eq!(failure.check, Check::Rule);
        assert!(failure.detail.contains("names no ranking rule"), "{}", failure.detail);
    }
}
