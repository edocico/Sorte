//! Random cases for the contract suite, all from its seeded generator: lists, outcomes, spin
//! requests, settings, viewports and gestures.

use std::fmt;
use std::ops::RangeInclusive;

use kurbo::Point;
use peniko::Color;
use rand::rngs::Xoshiro256PlusPlus;
use rand::{Rng, RngExt};
use serde_json::{Map, Value};
use sorte_core::{Entry, EntryId, ListDoc, Outcome, RankingRule, Rgba8, WEIGHT_RANGE, draw, draw_ranking};
use uuid::Uuid;

use crate::frame::{Frame, Viewport};
use crate::list::ListView;
use crate::picker::{OutcomeKind, Shown};
use crate::pointer::{PointerEvent, PointerPhase};
use crate::settings::{PickerSettings, SettingField, SettingsSchema};
use crate::spin::SpinRequest;

/// The suite's generator: portable, so a seed replays the same cases on every platform.
pub(super) type CaseRng = Xoshiro256PlusPlus;

/// The most visible entries a generated list has, whatever the picker accepts: enough for
/// slices narrower than a pixel, few enough to keep the suite quick.
const MAX_ENTRIES: usize = 1000;

/// Entry texts: names short and long, complex scripts, emoji, blanks, pasted control characters.
const TEXTS: &[&str] = &[
    "Alice",
    "Bruno",
    "Pierfrancesco Massimiliano di Montefeltro",
    "Zoë & Chloé",
    "مرحبا بالعالم",
    "שלום",
    "नमस्ते",
    "你好",
    "🍕 Pizza",
    "👩‍👩‍👧‍👦",
    "",
    "   ",
    "a\tb\r\nc",
    "42",
];

/// A list whose visible entries are as many as `accepted` allows (up to `MAX_ENTRIES`), with up
/// to three hidden ones among them.
pub(super) fn list(rng: &mut CaseRng, accepted: &RangeInclusive<usize>) -> ListDoc {
    let visible = entry_count(rng, accepted);
    let hidden = if rng.random_bool(0.4) { rng.random_range(1..=3) } else { 0 };
    let weights = Weights::pick(rng);
    let mut list = ListDoc::new("Contract");
    list.entries = (0..visible + hidden).map(|index| entry(rng, index, weights)).collect();
    for _ in 0..hidden {
        let enabled: Vec<usize> = (0..list.entries.len()).filter(|&i| list.entries[i].enabled).collect();
        list.entries[enabled[rng.random_range(0..enabled.len())]].enabled = false;
    }
    list
}

/// Each bound of the range one time in eight, a log-uniform count in between otherwise: small
/// lists are the common case, but large ones keep coming up.
fn entry_count(rng: &mut CaseRng, accepted: &RangeInclusive<usize>) -> usize {
    let low = (*accepted.start()).max(1);
    let high = (*accepted.end()).clamp(low, MAX_ENTRIES.max(low));
    match rng.random_range(0..8) {
        0 => low,
        1 => high,
        _ => {
            let count = low as f64 * (high as f64 / low as f64).powf(rng.random::<f64>());
            (count.round() as usize).clamp(low, high)
        }
    }
}

/// How a list's weights are spread.
#[derive(Clone, Copy)]
enum Weights {
    /// All 1, as when nobody sets weights.
    Equal,
    /// Small integers.
    Small,
    /// Anywhere in the range.
    Uniform,
    /// 1 or 1000: slivers next to wide slices.
    Extreme,
}

impl Weights {
    fn pick(rng: &mut CaseRng) -> Self {
        [Self::Equal, Self::Small, Self::Uniform, Self::Extreme][rng.random_range(0..4)]
    }

    fn draw(self, rng: &mut CaseRng) -> u32 {
        match self {
            Self::Equal => 1,
            Self::Small => rng.random_range(1..=5),
            Self::Uniform => rng.random_range(WEIGHT_RANGE),
            Self::Extreme if rng.random_bool(0.5) => *WEIGHT_RANGE.start(),
            Self::Extreme => *WEIGHT_RANGE.end(),
        }
    }
}

fn entry(rng: &mut CaseRng, index: usize, weights: Weights) -> Entry {
    let base = TEXTS[rng.random_range(0..TEXTS.len())];
    let text = if rng.random_bool(0.5) { base.to_owned() } else { format!("{base} {index}") };
    let label = rng.random_bool(0.2).then(|| TEXTS[rng.random_range(0..TEXTS.len())].to_owned());
    let color = rng.random_bool(0.2).then(|| {
        let [r, g, b, a] = rng.next_u32().to_le_bytes();
        Rgba8 { r, g, b, a }
    });
    // Ids from the seed too, so a replay matches a picker that orders entries by id.
    let id = EntryId(Uuid::from_u64_pair(rng.next_u64(), rng.next_u64()));
    Entry { id, label, color, weight: weights.draw(rng), ..Entry::new(text) }
}

/// An outcome drawn by `sorte-core`, as the controller draws it: for a ranking picker, ranked
/// under the rule the picker names (the suite checked it names one).
pub(super) fn outcome(list: &ListDoc, kind: OutcomeKind, rule: Option<RankingRule>, rng: &mut CaseRng) -> Outcome {
    let drawn = match (kind, rule) {
        (OutcomeKind::Ranking, Some(rule)) => draw_ranking(list, rule, rng),
        _ => draw(list, rng),
    };
    drawn.expect("generated lists have visible entries with valid weights")
}

/// A request often at the bounds of the durations, from `rest` most of the time, sometimes
/// with reduce motion; with the `fling` a gesture ended in, or else sometimes a fling either
/// way.
pub(super) fn request(rng: &mut CaseRng, rest: f64, fling: Option<f64>) -> SpinRequest {
    let (shortest, longest) = (*SpinRequest::DURATIONS.start(), *SpinRequest::DURATIONS.end());
    let duration = match rng.random_range(0..6) {
        0 => shortest,
        1 => longest,
        _ => rng.random_range(shortest..=longest),
    };
    let start = if fling.is_some() || rng.random_bool(0.75) { rest } else { rng.random_range(-100.0..100.0) };
    let request = SpinRequest::new(duration).with_start(start).with_reduce_motion(rng.random_bool(0.25));
    let fling = fling.or_else(|| {
        rng.random_bool(0.25).then(|| {
            let omega = rng.random_range(0.5..60.0);
            if rng.random_bool(0.5) { omega } else { -omega }
        })
    });
    match fling {
        Some(omega) => request.with_fling(omega),
        None => request,
    }
}

/// Settings with a random valid value for every field of `schema`, which the suite checked.
pub(super) fn settings(rng: &mut CaseRng, schema: &SettingsSchema) -> PickerSettings {
    let mut values = Map::new();
    for field in schema.fields() {
        let value = match field {
            SettingField::Bool(_) => Value::Bool(rng.random_bool(0.5)),
            SettingField::Int(f) => Value::from(rng.random_range(f.min..=f.max)),
            SettingField::Float(f) => Value::from(if f.min < f.max { rng.random_range(f.min..=f.max) } else { f.min }),
            SettingField::Choice(f) => Value::from(f.options[rng.random_range(0..f.options.len())].key),
        };
        values.insert(field.key().to_owned(), value);
    }
    schema.validate(&Value::Object(values))
}

/// A laptop panel, a HiDPI one at 167 %, a portrait window, and a window too small to read.
const VIEWPORTS: [Viewport; 4] = [
    Viewport { width: 1280, height: 800, scale: 1.0 },
    Viewport { width: 2560, height: 1600, scale: 1.667 },
    Viewport { width: 720, height: 1280, scale: 2.0 },
    Viewport { width: 48, height: 32, scale: 1.0 },
];

/// The viewport a list is prepared for and rendered at.
pub(super) fn viewport(rng: &mut CaseRng) -> Viewport {
    VIEWPORTS[rng.random_range(0..VIEWPORTS.len())]
}

/// An empty frame of `viewport`, at 144 Hz.
pub(super) fn frame(viewport: Viewport) -> Frame {
    Frame::new(viewport.width, viewport.height, viewport.scale, Color::BLACK, 1.0 / 144.0)
}

/// A point anywhere over the scene, edges included.
pub(super) fn position(rng: &mut CaseRng, viewport: Viewport) -> Point {
    let size = viewport.size();
    Point::new(rng.random_range(0.0..=size.width), rng.random_range(0.0..=size.height))
}

/// A press, a few moves and a release (or a cancel) anywhere over the scene. Some events share
/// their timestamp: a velocity over no time is the classic division by zero.
pub(super) fn gesture(rng: &mut CaseRng, viewport: Viewport) -> Vec<PointerEvent> {
    let moves = rng.random_range(0..6);
    let mut t = rng.random_range(0.0..100.0);
    (0..moves + 2)
        .map(|i| {
            let phase = match i {
                0 => PointerPhase::Down,
                _ if i <= moves => PointerPhase::Move,
                _ if rng.random_bool(0.2) => PointerPhase::Cancel,
                _ => PointerPhase::Up,
            };
            if i > 0 && rng.random_bool(0.7) {
                t += rng.random_range(0.001..0.1);
            }
            PointerEvent { phase, position: position(rng, viewport), viewport, t }
        })
        .collect()
}

/// "1280×800 px at scale 1".
pub(super) fn describe_viewport(viewport: Viewport) -> String {
    format!("{}×{} px at scale {}", viewport.width, viewport.height, viewport.scale)
}

/// "12 visible entries and 2 hidden, weights [...]", the weights cut short on long lists.
pub(super) fn describe_list(list: &ListDoc) -> String {
    let weights: Vec<u32> = list.visible_entries().map(|entry| entry.weight).collect();
    let hidden = list.entries.len() - weights.len();
    format!("{} visible entries and {hidden} hidden, weights {}", weights.len(), abbreviated(&weights))
}

/// What `shown` names, as positions among the visible entries.
pub(super) fn describe_shown(shown: &Shown, view: &ListView) -> String {
    let place = |id: &EntryId| view.position(*id).map_or_else(|| "?".to_owned(), |i| format!("#{i}"));
    match shown {
        Shown::Single(id) if view.position(*id).is_none() => "an entry not in the list".to_owned(),
        Shown::Single(id) => format!("entry {}", place(id)),
        Shown::Ranking(ids) => format!("ranking {}", abbreviated(&ids.iter().map(place).collect::<Vec<_>>())),
    }
}

pub(super) fn describe_request(request: &SpinRequest) -> String {
    let fling = request.fling().map_or_else(|| "no fling".to_owned(), |omega| format!("fling {omega}/s"));
    let motion = if request.reduce_motion() { "reduced motion" } else { "full motion" };
    format!("{} s, start {}, {fling}, {motion}", request.duration(), request.start())
}

/// `items` in brackets, cut after the first 12.
fn abbreviated<T: fmt::Display>(items: &[T]) -> String {
    const SHOWN: usize = 12;
    let head: Vec<String> = items.iter().take(SHOWN).map(ToString::to_string).collect();
    let more = items.len().saturating_sub(SHOWN);
    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
    format!("[{}]{tail}", head.join(", "))
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;

    use super::*;

    #[test]
    fn lists_stay_within_the_accepted_counts_and_reach_both_bounds() {
        let mut rng = CaseRng::seed_from_u64(1);
        for accepted in [1..=1, 2..=12, 1..=usize::MAX, 1001..=2000] {
            let counts: Vec<usize> = (0..60).map(|_| ListView::new(&list(&mut rng, &accepted)).len()).collect();
            let high = (*accepted.end()).min(MAX_ENTRIES).max(*accepted.start());
            assert!(counts.iter().all(|&n| (*accepted.start()..=high).contains(&n)), "{accepted:?}");
            assert!(counts.contains(accepted.start()) && counts.contains(&high), "{accepted:?}");
        }
    }

    #[test]
    fn lists_hide_entries_and_spread_weights_every_way() {
        let mut rng = CaseRng::seed_from_u64(2);
        let lists: Vec<ListDoc> = (0..200).map(|_| list(&mut rng, &(1..=40))).collect();
        assert!(lists.iter().any(|l| l.entries.iter().any(|e| !e.enabled)));
        assert!(lists.iter().any(|l| l.entries.iter().all(|e| e.enabled)));
        let weights: Vec<u32> = lists.iter().flat_map(|l| l.entries.iter().map(|e| e.weight)).collect();
        assert!(weights.contains(&1) && weights.contains(&1000));
        assert!(weights.iter().any(|w| (6..1000).contains(w)));
        assert!(lists.iter().all(|l| l.validate().is_ok()));
    }

    #[test]
    fn a_seed_replays_the_same_lists() {
        let replay = |seed| {
            let mut rng = CaseRng::seed_from_u64(seed);
            (0..20).map(|_| list(&mut rng, &(1..=50)).entries).collect::<Vec<_>>()
        };
        assert_eq!(replay(3), replay(3));
        assert_ne!(replay(3), replay(4));
    }

    #[test]
    fn requests_cover_the_bounds_flings_and_reduce_motion() {
        let mut rng = CaseRng::seed_from_u64(5);
        let requests: Vec<SpinRequest> = (0..400).map(|_| request(&mut rng, 1.5, None)).collect();
        assert!(requests.iter().any(|r| r.duration() == 1.0) && requests.iter().any(|r| r.duration() == 30.0));
        assert!(requests.iter().any(|r| r.fling().is_some_and(|w| w > 0.0)));
        assert!(requests.iter().any(|r| r.fling().is_some_and(|w| w < 0.0)));
        assert!(requests.iter().any(|r| r.fling().is_none()));
        assert!(requests.iter().any(|r| r.reduce_motion()) && requests.iter().any(|r| r.start() == 1.5));
        assert!(requests.iter().any(|r| r.start() != 1.5));
    }

    #[test]
    fn a_gesture_fling_starts_the_next_draw_from_where_it_left_the_picker() {
        let mut rng = CaseRng::seed_from_u64(7);
        for _ in 0..100 {
            let request = request(&mut rng, -2.5, Some(17.0));
            assert_eq!((request.start(), request.fling()), (-2.5, Some(17.0)));
        }
    }

    #[test]
    fn ranking_outcomes_follow_the_rule_the_picker_names() {
        let mut rng = CaseRng::seed_from_u64(8);
        for _ in 0..50 {
            let list = list(&mut rng, &(2..=12));
            for rule in [RankingRule::FirstWins, RankingRule::LastWins] {
                let drawn = outcome(&list, OutcomeKind::Ranking, Some(rule), &mut rng);
                let ranking = drawn.ranking.expect("a ranking");
                let place = if rule == RankingRule::FirstWins { ranking.first() } else { ranking.last() };
                assert_eq!(place, Some(&drawn.selected), "{rule:?}");
            }
            assert_eq!(outcome(&list, OutcomeKind::Single, None, &mut rng).ranking, None);
        }
    }

    #[test]
    fn gestures_press_first_and_release_or_cancel_last() {
        let mut rng = CaseRng::seed_from_u64(6);
        for _ in 0..100 {
            let events = gesture(&mut rng, VIEWPORTS[0]);
            assert_eq!(events.first().map(|e| e.phase), Some(PointerPhase::Down));
            assert!(matches!(events.last().map(|e| e.phase), Some(PointerPhase::Up | PointerPhase::Cancel)));
            assert!(events.windows(2).all(|w| w[0].t <= w[1].t));
            assert!(events.iter().all(|e| e.viewport == VIEWPORTS[0]));
        }
    }

    #[test]
    fn long_lists_are_cut_short_in_reports() {
        assert_eq!(abbreviated(&[1, 2, 3]), "[1, 2, 3]");
        let long: Vec<u32> = (0..15).collect();
        assert_eq!(abbreviated(&long), "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] and 3 more");
    }
}
