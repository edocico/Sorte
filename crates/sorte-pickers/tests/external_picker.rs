//! A picker written outside this crate, as the wheel and the race are, implements the contract
//! and runs its suite through the public API alone.

use std::sync::Arc;

use sorte_anim::Seconds;
use sorte_core::{EntryId, Outcome};
use sorte_pickers::kurbo::{Affine, Circle, Shape};
use sorte_pickers::peniko::Color;
use sorte_pickers::{
    Assets, Choreography, CosmeticRng, DisplayList, EventKind, Frame, ListView, LocalizedStr, OutcomeKind, Picker,
    PickerError, PickerMeta, PickerSettings, Picture, SettingsSchema, Shown, SpinRequest, TimedEvent, Viewport,
    contract,
};

/// Dots in a row, lit one after another ever more slowly until the drawn one stays lit.
struct Chaser {
    meta: PickerMeta,
    ids: Arc<[EntryId]>,
    dots: Arc<Picture>,
}

impl Chaser {
    fn new() -> Self {
        let meta = PickerMeta {
            id: "chaser",
            name: LocalizedStr::new("Luci", "Chaser"),
            outcome: OutcomeKind::Single,
            entries: 1..=500,
            supports_images: false,
        };
        Self { meta, ids: Arc::new([]), dots: Picture::new(DisplayList::default()) }
    }
}

fn dot(index: f64) -> Affine {
    Affine::translate((8.0 + 6.0 * index, 8.0))
}

impl Picker for Chaser {
    fn meta(&self) -> &PickerMeta {
        &self.meta
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema::default()
    }

    fn prepare(
        &mut self,
        list: &ListView,
        _settings: &PickerSettings,
        _viewport: Viewport,
        _assets: &mut Assets,
    ) -> Result<(), PickerError> {
        self.meta.accepts(list.len())?;
        let mut dots = DisplayList::default();
        for (i, entry) in list.entries().iter().enumerate() {
            dots.fill(Circle::new((0.0, 0.0), 2.0).to_path(0.1), dot(i as f64), entry.color.unwrap_or(Color::WHITE));
        }
        self.ids = list.entries().iter().map(|entry| entry.id).collect();
        self.dots = Picture::new(dots);
        Ok(())
    }

    fn render_idle(&self, frame: &mut Frame, rest: f64, _t: Seconds) {
        light(frame, &self.dots, self.ids.len(), rest);
    }

    fn choreograph(&self, outcome: &Outcome, request: &SpinRequest, rng: &mut CosmeticRng) -> Box<dyn Choreography> {
        let n = self.ids.len().max(1) as f64;
        let target = self.ids.iter().position(|id| *id == outcome.selected).unwrap_or(0) as f64;
        let start = request.start().rem_euclid(n).floor();
        let rounds = if request.reduce_motion() { 0.0 } else { 1.0 + rng.below(2) as f64 };
        let steps = (target - start).rem_euclid(n) + rounds * n;
        let duration = if request.reduce_motion() { 0.4 } else { request.duration() };
        // Step k lights at T·(1 − √(1 − k/steps)): E(x) = 1 − (1 − x)² inverted, in closed form.
        let ticks = (1..=steps as u64).map(|k| TimedEvent {
            t: duration * (1.0 - (1.0 - k as f64 / steps).max(0.0).sqrt()),
            kind: EventKind::Tick { strength: 0.5 },
        });
        let events = [TimedEvent { t: 0.0, kind: EventKind::Start }]
            .into_iter()
            .chain(ticks)
            .chain([TimedEvent { t: duration, kind: EventKind::Arrive }])
            .collect();
        Box::new(Chase { ids: self.ids.clone(), dots: self.dots.clone(), start, steps, duration, events })
    }
}

fn light(frame: &mut Frame, dots: &Arc<Picture>, n: usize, lit: f64) {
    frame.moving.picture(dots, Affine::IDENTITY);
    let index = lit.rem_euclid(n.max(1) as f64).floor();
    frame.top.fill(Circle::new((0.0, 0.0), 3.0).to_path(0.1), dot(index), Color::WHITE);
}

struct Chase {
    ids: Arc<[EntryId]>,
    dots: Arc<Picture>,
    start: f64,
    steps: f64,
    duration: f64,
    events: Vec<TimedEvent>,
}

impl Choreography for Chase {
    fn duration(&self) -> Seconds {
        self.duration
    }

    fn render(&self, frame: &mut Frame, t: Seconds) {
        let x = (t / self.duration).clamp(0.0, 1.0);
        let lit = self.start + (self.steps * (1.0 - (1.0 - x).powi(2))).floor();
        light(frame, &self.dots, self.ids.len(), lit);
    }

    fn events(&self) -> &[TimedEvent] {
        &self.events
    }

    fn rest(&self) -> f64 {
        self.start + self.steps
    }

    fn shown(&self) -> Shown {
        let n = self.ids.len().max(1);
        let lit = (self.rest().rem_euclid(n as f64) as usize) % n;
        Shown::Single(self.ids[lit])
    }
}

#[test]
fn a_picker_from_another_crate_keeps_the_contract() {
    contract::assert_conforms(Chaser::new, 2);
}
