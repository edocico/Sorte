//! The suite checked against itself: a small conforming picker passes it in both outcome kinds,
//! and each way of breaking the contract fails it, on the matching check.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kurbo::{Affine, Point, Rect, Shape};
use peniko::Color;
use sorte_anim::Seconds;
use sorte_core::{EntryId, Outcome, RankingRule};

use super::{Check, LISTS, LISTS_PER_PICKER, Violation, assert_conforms, check};
use crate::{
    Assets, BoolField, Choice, ChoiceField, Choreography, CosmeticRng, DisplayList, EventKind, Frame, Gesture,
    IntField, ListView, LocalizedStr, OutcomeKind, Picker, PickerError, PickerMeta, PickerSettings, Picture,
    PointerEvent, PointerPhase, SettingsSchema, Shown, SpinRequest, TextEngine, TextStyle, TimedEvent, Viewport,
};

/// How a test picker breaks the contract, if it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    BadMeta,
    BadSchema,
    RefusesLongLists,
    /// A single picker naming a ranking rule.
    RuleOnSingle,
    /// A ranking picker naming none.
    NoRule,
    FlingsOnPress,
    /// A drag answers a rest that is not a number.
    DragsToNan,
    /// Keeps the drag itself and draws it, instead of handing the controller the moved rest.
    DragKept,
    /// `entry_at` names an entry from no list.
    HoversElsewhere,
    /// `idle_rest` is off the rest state from the start.
    IdleJumps,
    /// The glow pulses at rest while `idle_animates` says nothing moves.
    AnimatesSilently,
    /// `idle_rest` drifts while the idle frames stand still and `idle_animates` says nothing
    /// moves.
    DriftsSilently,
    /// `prepare` takes its bar colours from a process-wide counter, not from its arguments.
    HiddenPrepareState,
    /// `prepare` takes the laps from a process-wide counter: nothing shows at rest, only in
    /// the draws.
    HiddenLaps,
    WrongWinner,
    OverlongDuration,
    IgnoresReduceMotion,
    UnsortedEvents,
    MissingArrive,
    NanRest,
    NanFrame,
    NondeterministicRender,
    StatefulRender,
    /// `render` records a new picture every frame.
    PicturePerFrame,
    /// The easing is not clamped: the strip keeps moving past the end.
    MovesPastTheEnd,
    LooksFromElsewhere,
    StrayReplan,
    /// A manual stop ends after the draw would have.
    SlowStop,
    /// A manual stop starts a third of the strip away from where the draw was.
    JumpingReplan,
    /// `render` indexes past its entries on lists over 30 entries.
    PanicsOnLongLists,
}

/// Width of an entry's bar, and height of the strip, in logical pixels.
const BAR: f64 = 10.0;
const BAND: f64 = 40.0;
/// How fast the strip drifts at rest, with the drift setting on, in bars per second.
const DRIFT_SPEED: f64 = 0.25;

const GLOW: BoolField = BoolField { key: "glow", label: LocalizedStr::new("Bagliore", "Glow"), default: true };
const DRIFT: BoolField = BoolField { key: "drift", label: LocalizedStr::new("Deriva", "Drift"), default: false };
const LAPS: IntField = IntField { key: "laps", label: LocalizedStr::new("Giri", "Laps"), min: 0, max: 3, default: 1 };
const RULES: &[Choice] = &[
    Choice { key: "first", label: LocalizedStr::new("Vince il primo", "First wins") },
    Choice { key: "last", label: LocalizedStr::new("Vince l'ultimo", "Last wins") },
];
const RULE: ChoiceField =
    ChoiceField { key: "rule", label: LocalizedStr::new("Regola", "Rule"), options: RULES, default: "first" };

/// Entries as a strip of bars scrolling under a fixed spotlight in the middle of the scene,
/// which stops on the drawn entry; as a ranking picker, it also shows the entries' final order.
/// Dragging the strip moves it, a release flings it, and at rest it may glow and drift.
struct Spotlight {
    meta: PickerMeta,
    strip: Strip,
    glow: bool,
    drift: bool,
    laps: usize,
    rule: Option<RankingRule>,
    /// Where the pointer went down, and the rest then: the gesture's own bookkeeping.
    press: Option<Press>,
    /// The drag a faulty picker keeps for itself, in bars.
    drag: f64,
}

#[derive(Clone, Copy)]
struct Press {
    x: f64,
    t: Seconds,
    rest: f64,
}

/// What the suite had a picker do, counted: proof that its checks ran on real drags, flings,
/// hovers, idle drift, rankings under both rules and manual stops, not on defaults alone.
#[derive(Debug, Default)]
struct Coverage {
    drags: Cell<usize>,
    flings: Cell<usize>,
    /// The last fling, not yet staged: the rest it left and its velocity.
    pending_fling: Cell<Option<(f64, f64)>>,
    /// Draws staged with the fling and from the rest of the gesture just before them.
    flung_draws: Cell<usize>,
    hovers: Cell<usize>,
    drifting_lists: Cell<usize>,
    rules: Cell<[usize; 2]>,
    replans: Cell<usize>,
}

fn count(counter: &Cell<usize>) {
    counter.set(counter.get() + 1);
}

impl Spotlight {
    /// Counts what the suite has the picker do into `coverage`.
    fn counted(mut self, coverage: &Rc<Coverage>) -> Self {
        self.strip.coverage = coverage.clone();
        self
    }

    fn new(outcome: OutcomeKind, fault: Fault) -> Self {
        let entries = match outcome {
            OutcomeKind::Single => 1..=usize::MAX,
            OutcomeKind::Ranking => 2..=40,
        };
        let id = if fault == Fault::BadMeta { "Spot light" } else { "spotlight" };
        Self {
            meta: PickerMeta {
                id,
                name: LocalizedStr::new("Riflettore", "Spotlight"),
                outcome,
                entries,
                supports_images: false,
            },
            strip: Strip {
                fault,
                ids: Arc::new([]),
                bars: None,
                viewport: Viewport { width: 1, height: 1, scale: 1.0 },
                bar: BAR,
                coverage: Rc::default(),
            },
            glow: GLOW.default,
            drift: DRIFT.default,
            laps: 1,
            rule: None,
            press: None,
            drag: 0.0,
        }
    }

    fn fault(&self) -> Fault {
        self.strip.fault
    }
}

/// The strip's colour for entries without one, from a counter that moves on every `prepare`.
fn hidden_shade() -> u8 {
    static PREPARES: AtomicU64 = AtomicU64::new(0);
    (PREPARES.fetch_add(1, Ordering::Relaxed) % 200) as u8
}

/// Laps from a counter that moves on every `prepare`.
fn hidden_laps() -> usize {
    static PREPARES: AtomicU64 = AtomicU64::new(0);
    (PREPARES.fetch_add(1, Ordering::Relaxed) % 4) as usize
}

impl Picker for Spotlight {
    fn meta(&self) -> &PickerMeta {
        &self.meta
    }

    fn settings_schema(&self) -> SettingsSchema {
        let mut fields = vec![GLOW.into(), DRIFT.into(), LAPS.into()];
        if self.meta.outcome == OutcomeKind::Ranking {
            fields.push(RULE.into());
        }
        if self.fault() == Fault::BadSchema {
            fields.push(GLOW.into());
        }
        SettingsSchema::new(fields)
    }

    fn prepare(
        &mut self,
        list: &ListView,
        settings: &PickerSettings,
        viewport: Viewport,
        assets: &mut Assets,
    ) -> Result<(), PickerError> {
        self.meta.accepts(list.len())?;
        let fault = self.fault();
        if fault == Fault::RefusesLongLists && list.len() > 3 {
            return Err(PickerError::EntryCount { count: list.len(), accepted: 1..=3 });
        }
        // Sizes in physical pixels, decided for the viewport.
        let (bar, band, text) = (BAR * viewport.scale, BAND * viewport.scale, 12.0 * viewport.scale as f32);
        let shade = if fault == Fault::HiddenPrepareState { hidden_shade() } else { 255 };
        let mut bars = DisplayList::default();
        for (i, entry) in list.entries().iter().enumerate() {
            let x = i as f64 * bar;
            let color = entry.color.unwrap_or(Color::from_rgb8(shade, shade, shade));
            bars.fill(Rect::new(x, 0.0, x + bar, band).to_path(0.1), Affine::IDENTITY, color);
            for run in &assets.text.line(&entry.text, text, TextStyle::Label).runs {
                bars.glyphs(run, Affine::translate((x, band - 4.0)), Color::BLACK);
            }
        }
        let ids = list.entries().iter().map(|entry| entry.id).collect();
        let coverage = self.strip.coverage.clone();
        self.strip = Strip { fault, ids, bars: Some(Picture::new(bars)), viewport, bar, coverage };
        self.glow = settings.bool(&GLOW);
        self.drift = settings.bool(&DRIFT);
        self.laps = if fault == Fault::HiddenLaps { hidden_laps() } else { settings.int(&LAPS) as usize };
        self.rule = (self.meta.outcome == OutcomeKind::Ranking).then(|| match settings.choice(&RULE) {
            "last" => RankingRule::LastWins,
            _ => RankingRule::FirstWins,
        });
        let coverage = &self.strip.coverage;
        if self.drift {
            count(&coverage.drifting_lists);
        }
        if let Some(rule) = self.rule {
            let mut rules = coverage.rules.get();
            rules[usize::from(rule == RankingRule::LastWins)] += 1;
            coverage.rules.set(rules);
        }
        Ok(())
    }

    fn ranking_rule(&self) -> Option<RankingRule> {
        match self.fault() {
            Fault::NoRule => None,
            Fault::RuleOnSingle => Some(RankingRule::FirstWins),
            _ => self.rule,
        }
    }

    fn render_idle(&self, frame: &mut Frame, rest: f64, t: Seconds) {
        if self.fault() == Fault::DriftsSilently {
            return self.strip.draw(frame, rest, 0.0, 1.0);
        }
        let glow = if self.glow { 0.6 + 0.4 * t.sin().abs() as f32 } else { 1.0 };
        self.strip.draw(frame, self.idle_rest(rest, t) + self.drag, 0.0, glow);
    }

    fn idle_animates(&self) -> bool {
        (self.glow || self.drift) && !matches!(self.fault(), Fault::AnimatesSilently | Fault::DriftsSilently)
    }

    fn idle_rest(&self, rest: f64, t: Seconds) -> f64 {
        let drift = if self.drift || self.fault() == Fault::DriftsSilently { DRIFT_SPEED * t } else { 0.0 };
        let jump = if self.fault() == Fault::IdleJumps { 0.5 } else { 0.0 };
        rest + drift + jump
    }

    fn on_pointer(&mut self, event: &PointerEvent, rest: f64) -> Gesture {
        let fault = self.fault();
        if event.phase == PointerPhase::Down {
            if fault == Fault::FlingsOnPress {
                return Gesture::Fling { rest, omega: 1.0 };
            }
            self.press = Some(Press { x: event.position.x, t: event.t, rest });
            return Gesture::None;
        }
        let Some(press) = self.press else { return Gesture::None };
        // Dragging right pulls the strip right: the spotlight falls on an earlier bar.
        let dragged = press.rest - (event.position.x - press.x) / self.strip.bar;
        if event.phase != PointerPhase::Move {
            self.press = None;
        }
        if fault == Fault::DragKept && event.phase != PointerPhase::Cancel {
            self.drag = dragged - press.rest;
            return Gesture::None;
        }
        let gesture = match event.phase {
            PointerPhase::Up => {
                // In bars per second; no time between press and release is no fling.
                let dt = event.t - press.t;
                let omega = (dragged - press.rest) / dt;
                if dt > 0.0 && omega.is_finite() && omega != 0.0 {
                    Gesture::Fling { rest: dragged, omega }
                } else {
                    Gesture::Drag { rest: dragged }
                }
            }
            // The strip goes back where the press found it.
            PointerPhase::Cancel => Gesture::Drag { rest: press.rest },
            _ if fault == Fault::DragsToNan => Gesture::Drag { rest: f64::NAN },
            _ => Gesture::Drag { rest: dragged },
        };
        let coverage = &self.strip.coverage;
        match gesture {
            Gesture::Drag { .. } => count(&coverage.drags),
            Gesture::Fling { rest, omega } => {
                count(&coverage.flings);
                coverage.pending_fling.set(Some((rest, omega)));
            }
            Gesture::None => {}
        }
        gesture
    }

    fn entry_at(&self, position: Point, viewport: Viewport, rest: f64, t: Seconds) -> Option<EntryId> {
        if self.fault() == Fault::HoversElsewhere {
            return Some(EntryId::random());
        }
        let (centre, top) = self.strip.origin(viewport, self.idle_rest(rest, t));
        let band = BAND * self.strip.viewport.scale;
        let index = ((position.x - centre) / self.strip.bar).floor();
        let over = (top..=top + band).contains(&position.y) && (0.0..self.strip.n()).contains(&index);
        let entry = over.then(|| self.strip.ids[index as usize]);
        if entry.is_some() {
            count(&self.strip.coverage.hovers);
        }
        entry
    }

    fn choreograph(&self, outcome: &Outcome, request: &SpinRequest, rng: &mut CosmeticRng) -> Box<dyn Choreography> {
        if let (Some(rule), Some(ranking)) = (self.rule, &outcome.ranking) {
            let place = if rule == RankingRule::FirstWins { ranking.first() } else { ranking.last() };
            assert_eq!(place, Some(&outcome.selected), "the ranking follows the picker's rule");
        }
        let coverage = &self.strip.coverage;
        if let Some((rest, omega)) = coverage.pending_fling.take() {
            // The controller stages the fling's draw right after the release, from its rest.
            assert_eq!((request.start(), request.fling()), (rest, Some(omega)), "a fling starts the next draw");
            count(&coverage.flung_draws);
        }
        let fault = self.fault();
        let n = self.strip.n();
        let target = self.strip.ids.iter().position(|id| *id == outcome.selected).unwrap_or(0);
        let slot = if fault == Fault::WrongWinner { target + 1 } else { target };
        let across = if fault == Fault::LooksFromElsewhere { elsewhere() } else { rng.uniform(0.2, 0.8) };
        let land = slot as f64 + across;
        let laps = if request.reduce_motion() { 0 } else { self.laps + 1 + rng.below(2) };
        let start = request.start() + self.drag;
        let travel = (land - start).rem_euclid(n) + (laps as f64) * n;
        let duration = if request.reduce_motion() && fault != Fault::IgnoresReduceMotion {
            0.5
        } else if fault == Fault::OverlongDuration {
            request.duration() + 1.0
        } else if let Some(fling) = request.fling() {
            // E(x) = 1 − (1 − x)³ starts at 3·travel/T bars per second.
            (3.0 * travel / fling.abs()).clamp(1.0, 30.0)
        } else {
            request.duration()
        };
        let mut lanes = outcome.ranking.clone();
        if let Some(lanes) = lanes.as_mut().filter(|_| fault == Fault::WrongWinner) {
            lanes.swap(0, 1);
        }
        Box::new(Slide::new(self.strip.clone(), start, travel, duration, lanes, true))
    }
}

/// The landing point from a counter shared by every choreography, not from the given generator.
fn elsewhere() -> f64 {
    static CALLS: AtomicU64 = AtomicU64::new(0);
    0.2 + 0.6 * (CALLS.fetch_add(1, Ordering::Relaxed) as f64 * 0.618_034).fract()
}

/// What the picker prepared, shared with its choreographies.
#[derive(Clone)]
struct Strip {
    fault: Fault,
    ids: Arc<[EntryId]>,
    bars: Option<Arc<Picture>>,
    /// What it was prepared for, and the width of a bar there, in physical pixels.
    viewport: Viewport,
    bar: f64,
    coverage: Rc<Coverage>,
}

impl Strip {
    fn n(&self) -> f64 {
        self.ids.len().max(1) as f64
    }

    /// Where the strip's first bar starts with the spotlight on `position` (in bars), and the
    /// strip's top edge, in a frame of `viewport`.
    fn origin(&self, viewport: Viewport, position: f64) -> (f64, f64) {
        let size = viewport.size();
        let centre = size.width / 2.0 - position.rem_euclid(self.n()) * self.bar;
        (centre, size.height / 2.0 - BAND * self.viewport.scale / 2.0)
    }

    /// Draws the strip with the spotlight on `position` (in bars), moving at `speed` bars per
    /// second.
    fn draw(&self, frame: &mut Frame, position: f64, speed: f64, glow: f32) {
        // The suite draws at the viewport it prepared for. In the app a frame may come at
        // another size before the next prepare, and a picker scales what it prepared instead.
        assert_eq!(frame.viewport(), self.viewport, "frames come at the prepared viewport");
        let (left, top) = self.origin(frame.viewport(), position);
        if let Some(bars) = &self.bars {
            frame.moving.picture(bars, Affine::translate((left, top)));
        }
        let size = frame.viewport().size();
        let spot = Rect::new(-2.0, -10.0, 2.0, 50.0).to_path(0.1);
        frame.top.fill(spot, Affine::translate((size.width / 2.0, top)), Color::WHITE.with_alpha(glow));
        if speed != 0.0 {
            let centre = Point::new(size.width / 2.0, size.height / 2.0);
            frame.request_motion_blur(centre, 30.0, speed * self.bar / 30.0);
        }
    }
}

/// The spotlight sliding `travel` bars from `start` with E(x) = 1 − (1 − x)³.
struct Slide {
    strip: Strip,
    start: f64,
    travel: f64,
    duration: f64,
    events: Vec<TimedEvent>,
    /// The entries' final order, for a ranking.
    lanes: Option<Vec<EntryId>>,
    renders: Cell<u64>,
    /// The time and position of the last render, for a render that integrates.
    last: Cell<(f64, f64)>,
}

impl Slide {
    fn new(strip: Strip, start: f64, travel: f64, duration: f64, lanes: Option<Vec<EntryId>>, opens: bool) -> Self {
        let mut slide = Self {
            strip,
            start,
            travel,
            duration,
            events: Vec::new(),
            lanes,
            renders: Cell::new(0),
            last: Cell::new((0.0, start)),
        };
        slide.events = slide.plan_events(opens);
        slide
    }

    /// Start, a tick at every bar edge the spotlight crosses (in closed form), then Arrive.
    fn plan_events(&self, opens: bool) -> Vec<TimedEvent> {
        let mut ticks = Vec::new();
        let mut edge = self.start.floor() + 1.0;
        while edge <= self.start + self.travel {
            let left = (1.0 - (edge - self.start) / self.travel).max(0.0);
            ticks.push(TimedEvent { t: self.duration * (1.0 - left.cbrt()), kind: EventKind::Tick { strength: 1.0 } });
            edge += 1.0;
        }
        if self.strip.fault == Fault::UnsortedEvents {
            ticks.reverse();
        }
        let start = opens.then_some(TimedEvent { t: 0.0, kind: EventKind::Start });
        // Missing its Arrive, the staging still ends on an event at the end: a last tick.
        let last = if self.strip.fault == Fault::MissingArrive {
            EventKind::Tick { strength: 1.0 }
        } else {
            EventKind::Arrive
        };
        start.into_iter().chain(ticks).chain([TimedEvent { t: self.duration, kind: last }]).collect()
    }

    fn progress(&self, t: Seconds) -> f64 {
        let x = t / self.duration;
        if self.strip.fault == Fault::MovesPastTheEnd { x.max(0.0) } else { x.clamp(0.0, 1.0) }
    }

    fn position(&self, t: Seconds) -> f64 {
        self.start + self.travel * (1.0 - (1.0 - self.progress(t)).powi(3))
    }

    fn speed(&self, t: Seconds) -> f64 {
        let x = (t / self.duration).clamp(0.0, 1.0);
        3.0 * self.travel * (1.0 - x).powi(2) / self.duration
    }
}

impl Choreography for Slide {
    fn duration(&self) -> Seconds {
        self.duration
    }

    fn render(&self, frame: &mut Frame, t: Seconds) {
        let mut position = self.position(t);
        match self.strip.fault {
            Fault::NondeterministicRender => {
                self.renders.set(self.renders.get() + 1);
                position += self.renders.get() as f64 * 1e-3;
            }
            Fault::StatefulRender => {
                let (last_t, last_position) = self.last.get();
                position = last_position + self.speed(last_t) * (t - last_t);
                self.last.set((t, position));
            }
            Fault::NanFrame if t >= self.duration => position = f64::NAN,
            Fault::PanicsOnLongLists if self.strip.ids.len() > 30 => {
                let lane = self.strip.ids[self.strip.ids.len()];
                position += lane.0.as_u128() as f64;
            }
            _ => {}
        }
        self.strip.draw(frame, position, self.speed(t), 1.0);
        if self.strip.fault == Fault::PicturePerFrame {
            let mut badge = DisplayList::default();
            badge.fill(Rect::new(0.0, 0.0, 4.0, 4.0).to_path(0.1), Affine::IDENTITY, Color::WHITE);
            frame.top.picture(&Picture::new(badge), Affine::IDENTITY);
        }
    }

    fn events(&self) -> &[TimedEvent] {
        &self.events
    }

    fn rest(&self) -> f64 {
        if self.strip.fault == Fault::NanRest { f64::NAN } else { self.start + self.travel }
    }

    fn shown(&self) -> Shown {
        match &self.lanes {
            Some(lanes) => Shown::Ranking(lanes.clone()),
            None => {
                let n = self.strip.ids.len().max(1);
                let slot = (self.rest().rem_euclid(n as f64).floor() as usize) % n;
                self.strip.ids.get(slot).map_or(Shown::Ranking(Vec::new()), |id| Shown::Single(*id))
            }
        }
    }

    /// Skips the laps left and lands on the same point of the same bar, at the speed the strip
    /// had when stopped: sooner, without a jump.
    fn replan_stop(&self, at: Seconds) -> Option<Box<dyn Choreography>> {
        let fault = self.strip.fault;
        let n = self.strip.n();
        let (here, speed) = (self.position(at), self.speed(at));
        let left = self.start + self.travel - here;
        if !(at < self.duration && speed > 0.0 && left >= n) {
            return None;
        }
        let travel = left.rem_euclid(n);
        // E(x) = 1 − (1 − x)³ starts at 3·travel/T: matching the speed fixes T.
        let duration = match fault {
            Fault::SlowStop => self.duration - at + 1.0,
            _ => 3.0 * travel / speed,
        };
        if duration.is_nan() || duration <= 0.0 {
            return None;
        }
        let (here, travel) = match fault {
            Fault::StrayReplan => (here, travel + 1.0),
            Fault::JumpingReplan => (here + n / 3.0, (travel - n / 3.0).rem_euclid(n)),
            _ => (here, travel),
        };
        count(&self.strip.coverage.replans);
        Some(Box::new(Slide::new(self.strip.clone(), here, travel, duration, self.lanes.clone(), false)))
    }
}

fn assets() -> Assets {
    Assets { text: TextEngine::new([]).expect("a text engine needs no font file") }
}

fn run(outcome: OutcomeKind, fault: Fault, seed: u64) -> Result<(), Violation> {
    check(|| Spotlight::new(outcome, fault), &mut assets(), seed)
}

#[test]
fn a_conforming_picker_passes_in_both_outcome_kinds() {
    // The ranking picker asserts its rankings follow its own rule (a random setting): the suite
    // draws them under the rule it names, as the controller does.
    for outcome in [OutcomeKind::Single, OutcomeKind::Ranking] {
        for seed in 0..3 {
            if let Err(violation) = run(outcome, Fault::None, seed) {
                panic!("{violation}");
            }
        }
    }
}

#[test]
fn the_suite_drives_drags_flings_hovers_drift_rules_and_stops() {
    let coverage = Rc::new(Coverage::default());
    for outcome in [OutcomeKind::Single, OutcomeKind::Ranking] {
        for seed in 0..2 {
            let result = check(|| Spotlight::new(outcome, Fault::None).counted(&coverage), &mut assets(), seed);
            assert_eq!(result, Ok(()), "{outcome:?}, seed {seed}");
        }
    }
    let Coverage { drags, flings, pending_fling, flung_draws, hovers, drifting_lists, rules, replans } = &*coverage;
    let counts = [drags, flings, flung_draws, hovers, drifting_lists, replans].map(|count| count.get());
    assert!(counts.iter().all(|&count| count > 0), "drags, flings, flung draws, hovers, drift, replans: {counts:?}");
    // Every fling started the very next draw, from where it left the strip.
    assert_eq!((flung_draws.get(), pending_fling.get()), (flings.get(), None));
    assert!(rules.get().iter().all(|&count| count > 0), "lists of each ranking rule: {:?}", rules.get());
}

#[test]
fn assert_conforms_passes_a_conforming_picker() {
    assert_conforms(|| Spotlight::new(OutcomeKind::Single, Fault::None), 11);
}

#[test]
#[should_panic(expected = "breaks the contract (Result) with seed 11")]
fn assert_conforms_panics_with_the_violation() {
    assert_conforms(|| Spotlight::new(OutcomeKind::Single, Fault::WrongWinner), 11);
}

#[test]
fn each_broken_picker_fails_on_its_check() {
    // The check, and words of the detail that tell its sub-checks apart.
    let cases = [
        (Fault::BadMeta, Check::Meta, "the id \"Spot light\""),
        (Fault::BadSchema, Check::Schema, "\"glow\" is blank or used twice"),
        (Fault::RefusesLongLists, Check::Prepare, "prepare refused"),
        (Fault::RuleOnSingle, Check::Rule, "a single picker names the ranking rule"),
        (Fault::FlingsOnPress, Check::Pointer, "Down flung"),
        (Fault::DragKept, Check::Pointer, "drew another frame than a picker that saw no gesture"),
        (Fault::HoversElsewhere, Check::Pointer, "names an entry that is not in the list"),
        (Fault::IdleJumps, Check::Idle, "coming to rest would jump"),
        (Fault::DragsToNan, Check::Pointer, "Move answered Drag { rest: NaN }, with a number that is not finite"),
        (Fault::AnimatesSilently, Check::Idle, "draws another frame at t = 0.37 s than at 0 s"),
        (Fault::DriftsSilently, Check::Idle, "the next draw would start elsewhere than the still frame"),
        (Fault::HiddenPrepareState, Check::Replay, "a picker prepared the same way drew another idle frame"),
        (Fault::HiddenLaps, Check::Replay, "a picker prepared the same way, with the same cosmetic seed, gave"),
        (Fault::WrongWinner, Check::Result, "the staging shows entry #"),
        (Fault::OverlongDuration, Check::Duration, "overruns"),
        (Fault::IgnoresReduceMotion, Check::Duration, "with reduce motion on"),
        (Fault::UnsortedEvents, Check::Events, "out of order"),
        (Fault::MissingArrive, Check::Events, "not Arrive"),
        (Fault::NanRest, Check::Rest, "the rest state is NaN"),
        (Fault::NanFrame, Check::Frame, "not finite in a picture transform"),
        (Fault::NondeterministicRender, Check::Determinism, "drew another frame"),
        (Fault::StatefulRender, Check::Determinism, "drew another frame"),
        (Fault::PicturePerFrame, Check::Determinism, "recorded its pictures anew"),
        (Fault::MovesPastTheEnd, Check::End, "past the end it holds the final state"),
        (Fault::LooksFromElsewhere, Check::Looks, "the same cosmetic seed gave"),
        (Fault::StrayReplan, Check::Replan, ": Result: the staging shows"),
        (Fault::SlowStop, Check::Replan, "after the draw would have"),
        (Fault::JumpingReplan, Check::Replan, "its first frame is not the draw's frame"),
        (Fault::PanicsOnLongLists, Check::Panic, "index out of bounds"),
    ];
    for (fault, check, detail) in cases {
        let violation = run(OutcomeKind::Single, fault, 5).expect_err(&format!("{fault:?} passed"));
        assert_eq!((violation.check, violation.seed), (check, 5), "{fault:?}: {violation}");
        assert!(violation.detail.contains(detail), "{fault:?}: {violation}");
    }
}

#[test]
fn a_ranking_picker_showing_another_order_fails() {
    let violation = run(OutcomeKind::Ranking, Fault::WrongWinner, 6).expect_err("a swapped ranking passed");
    assert_eq!(violation.check, Check::Result, "{violation}");
}

#[test]
fn a_ranking_picker_naming_no_rule_fails() {
    let violation = run(OutcomeKind::Ranking, Fault::NoRule, 6).expect_err("a ranking picker without a rule passed");
    assert_eq!(violation.check, Check::Rule, "{violation}");
    assert!(violation.detail.contains("names no ranking rule"), "{violation}");
}

#[test]
fn violations_name_the_picker_seed_and_case_and_replay() {
    let violation = run(OutcomeKind::Single, Fault::WrongWinner, 9).expect_err("a wrong winner passed");
    let text = violation.to_string();
    assert!(text.starts_with("picker \"spotlight\" breaks the contract (Result) with seed 9, at list "), "{text}");
    for part in ["visible entries", " px at scale ", ", draw ", "looks seed", "the staging shows entry #"] {
        assert!(text.contains(part), "{part:?} missing from {text}");
    }
    assert_eq!(run(OutcomeKind::Single, Fault::WrongWinner, 9), Err(violation));
}

#[test]
fn a_panicking_picker_is_reported_with_its_case_and_replays() {
    let violation = run(OutcomeKind::Single, Fault::PanicsOnLongLists, 3).expect_err("a panic passed");
    assert_eq!((violation.picker, violation.check, violation.seed), ("spotlight", Check::Panic, 3));
    for part in ["list ", "visible entries", ", draw ", "looks seed"] {
        assert!(violation.case.contains(part), "{part:?} missing from {violation}");
    }
    assert!(violation.detail.starts_with("the picker panicked: index out of bounds"), "{violation}");
    assert_eq!(run(OutcomeKind::Single, Fault::PanicsOnLongLists, 3), Err(violation));
}

#[test]
fn a_panic_before_the_metadata_is_reported_too() {
    let violation = check(|| -> Spotlight { panic!("no picker today") }, &mut assets(), 1).expect_err("a panic");
    assert_eq!((violation.picker, violation.check), ("an unnamed picker", Check::Panic));
    assert_eq!(
        (violation.case.as_str(), violation.detail.as_str()),
        ("making the picker", "the picker panicked: no picker today")
    );
}

#[test]
fn the_suite_checks_fresh_and_reprepared_pickers() {
    let made = Cell::new(0);
    let result = check(
        || {
            made.set(made.get() + 1);
            Spotlight::new(OutcomeKind::Single, Fault::None)
        },
        &mut assets(),
        3,
    );
    assert_eq!(result, Ok(()));
    // One picker for every few lists, and a fresh twin for each list to compare it against.
    assert_eq!(made.get(), LISTS / LISTS_PER_PICKER + LISTS);
}

#[test]
fn hovering_names_the_entry_under_the_spotlight() {
    let mut list = sorte_core::ListDoc::new("Pranzo");
    list.entries = ["Pizza", "Ramen", "Sushi"].map(sorte_core::Entry::new).to_vec();
    let view = ListView::new(&list);
    let viewport = Viewport { width: 400, height: 300, scale: 2.0 };
    let mut picker = Spotlight::new(OutcomeKind::Single, Fault::None);
    let settings = picker.settings_schema().validate(&serde_json::Value::Null);
    picker.prepare(&view, &settings, viewport, &mut assets()).expect("three entries");
    let centre = Point::new(200.0, 150.0);
    for (rest, entry) in [(0.5, 0), (1.25, 1), (2.9, 2), (3.5, 0)] {
        assert_eq!(picker.entry_at(centre, viewport, rest, 0.0), Some(list.entries[entry].id), "rest {rest}");
    }
    assert_eq!(picker.entry_at(Point::new(200.0, 10.0), viewport, 0.5, 0.0), None, "above the strip");
}
