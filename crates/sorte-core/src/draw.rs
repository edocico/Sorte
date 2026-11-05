//! The fair draw (design doc, § Estrazione equa). The outcome is decided here, from the
//! list alone, before any picker stages it: duration, speed, picker type, a fling or a
//! manual stop cannot change it.
//!
//! An entry's chance is exactly `weight / W`, with `W` the total weight of the visible
//! entries: the draw takes an unbiased uniform integer `r` in `[0, W)` and picks the first
//! entry whose cumulative weight exceeds `r`, so entry `i` wins for exactly `weight_i` of
//! the `W` values of `r`.

use rand::Rng;
use rand::distr::{Distribution, Uniform};
use rand::rand_core::UnwrapErr;
use rand::rngs::SysRng;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::{EntryId, ListDoc, WEIGHT_RANGE};

/// What a draw decided. Pickers only stage it: they never choose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// The entry chosen in proportion to its weight.
    pub selected: EntryId,
    /// The full arrival order, for ranking pickers only (M2).
    pub ranking: Option<Vec<EntryId>>,
}

/// Where a ranking picker puts the drawn entry. Either way the entry is drawn by weight
/// first, so both rules give every entry the same chance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RankingRule {
    /// The drawn entry arrives first ("vince chi arriva primo").
    FirstWins,
    /// The drawn entry arrives last ("vince chi arriva ultimo").
    LastWins,
}

/// Why a list cannot be drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DrawError {
    /// The list is empty or every entry is hidden.
    #[error("there is nothing to draw: the list is empty or every entry is hidden")]
    NoVisibleEntries,
    /// A visible entry's weight is outside [`WEIGHT_RANGE`], as [`ListDoc::validate`] would
    /// also report.
    #[error("entry {entry} has weight {weight}, outside {}..={}", WEIGHT_RANGE.start(), WEIGHT_RANGE.end())]
    WeightOutOfRange {
        /// The entry.
        entry: EntryId,
        /// Its weight.
        weight: u32,
    },
    /// Two entries share this id, hidden ones included, as [`ListDoc::validate`] would also
    /// report. Everything after the draw finds the drawn entry by its id, so it would stage,
    /// record and act on whichever entry comes first rather than the one drawn.
    #[error("two entries have id {0}, so the drawn one could not be told apart")]
    DuplicateEntryId(EntryId),
}

/// The generator for real draws: the operating system's. It panics if the OS cannot
/// provide randomness, which leaves no fair way to draw. Tests pass a seeded generator
/// to [`draw`] instead.
pub fn os_rng() -> UnwrapErr<SysRng> {
    UnwrapErr(SysRng)
}

/// Draws one visible entry, each with probability `weight / W`. Refuses a list with no
/// visible entry, with a visible weight outside [`WEIGHT_RANGE`], or where two entries
/// share an id (see [`DrawError`]).
pub fn draw(list: &ListDoc, rng: &mut dyn Rng) -> Result<Outcome, DrawError> {
    let (visible, winner) = draw_visible(list, rng)?;
    Ok(Outcome { selected: visible[winner], ranking: None })
}

/// Draws one visible entry as [`draw`] does, then ranks all the visible entries for a
/// ranking picker: the drawn entry first or last according to `rule`, the others in a
/// uniformly random (Fisher–Yates) order.
pub fn draw_ranking(list: &ListDoc, rule: RankingRule, rng: &mut dyn Rng) -> Result<Outcome, DrawError> {
    let (mut ranking, winner) = draw_visible(list, rng)?;
    let selected = ranking.remove(winner);
    ranking.shuffle(rng);
    match rule {
        RankingRule::FirstWins => ranking.insert(0, selected),
        RankingRule::LastWins => ranking.push(selected),
    }
    Ok(Outcome { selected, ranking: Some(ranking) })
}

/// Draws an index `i` with probability `weights[i] / sum(weights)`, so a weight of 0 is
/// never drawn. `None` when the weights sum to 0, as when there are none.
pub fn draw_index(weights: &[u32], rng: &mut dyn Rng) -> Option<usize> {
    let cumulative = cumulative_sums(weights);
    let total = cumulative.last().copied()?;
    // Uniform samples integers without bias (unlike `random_range`); it refuses total = 0.
    let r = Uniform::new(0, total).ok()?.sample(rng);
    Some(index_for(&cumulative, r))
}

/// The running totals of the weights.
fn cumulative_sums(weights: &[u32]) -> Vec<u64> {
    // u64 cannot overflow: that would take 2^32 weights of u32::MAX.
    weights
        .iter()
        .scan(0u64, |sum, &weight| {
            *sum += u64::from(weight);
            Some(*sum)
        })
        .collect()
}

/// The index whose cumulative weight is the first above `r`, by binary search.
fn index_for(cumulative: &[u64], r: u64) -> usize {
    cumulative.partition_point(|&sum| sum <= r)
}

/// The visible entries' ids and the index of the drawn one among them.
fn draw_visible(list: &ListDoc, rng: &mut dyn Rng) -> Result<(Vec<EntryId>, usize), DrawError> {
    if let Some(id) = list.shared_entry_id() {
        return Err(DrawError::DuplicateEntryId(id));
    }
    let mut ids = Vec::new();
    let mut weights = Vec::new();
    for entry in list.visible_entries() {
        if !WEIGHT_RANGE.contains(&entry.weight) {
            return Err(DrawError::WeightOutOfRange { entry: entry.id, weight: entry.weight });
        }
        ids.push(entry.id);
        weights.push(entry.weight);
    }
    let winner = draw_index(&weights, rng).ok_or(DrawError::NoVisibleEntries)?;
    Ok((ids, winner))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use proptest::prelude::*;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    use super::*;
    use crate::{Entry, testing};

    fn seeded(seed: u64) -> Xoshiro256PlusPlus {
        Xoshiro256PlusPlus::seed_from_u64(seed)
    }

    fn list_of(weights: &[u32]) -> ListDoc {
        let mut list = ListDoc::new("Pesi");
        list.entries = weights.iter().map(|&weight| Entry { weight, ..Entry::new("x") }).collect();
        list
    }

    #[test]
    fn an_empty_or_fully_hidden_list_cannot_be_drawn() {
        let mut list = list_of(&[]);
        assert_eq!(draw(&list, &mut seeded(1)), Err(DrawError::NoVisibleEntries));
        list = list_of(&[3, 5]);
        list.entries.iter_mut().for_each(|entry| entry.enabled = false);
        assert_eq!(draw(&list, &mut seeded(1)), Err(DrawError::NoVisibleEntries));
        assert_eq!(draw_ranking(&list, RankingRule::FirstWins, &mut seeded(1)), Err(DrawError::NoVisibleEntries));
    }

    #[test]
    fn the_only_visible_entry_always_wins() {
        let mut list = list_of(&[1000, 1, 1000]);
        list.entries[0].enabled = false;
        list.entries[2].enabled = false;
        let mut rng = seeded(2);
        for _ in 0..100 {
            assert_eq!(draw(&list, &mut rng).unwrap().selected, list.entries[1].id);
        }
    }

    #[test]
    fn visible_weights_outside_the_range_are_refused() {
        for weight in [0, 1001] {
            let mut list = list_of(&[5, weight, 5]);
            let entry = list.entries[1].id;
            assert_eq!(draw(&list, &mut seeded(3)), Err(DrawError::WeightOutOfRange { entry, weight }));
            // A hidden entry takes no part in the draw, whatever its weight.
            list.entries[1].enabled = false;
            assert!(draw(&list, &mut seeded(3)).is_ok());
        }
    }

    #[test]
    fn a_list_whose_entries_share_an_id_cannot_be_drawn() {
        // A block copied in a hand-edited file: the outcome's id would name the first copy,
        // which would then win with the odds of both.
        let mut list = list_of(&[1, 1000]);
        let id = list.entries[0].id;
        list.entries[1].id = id;
        assert_eq!(draw(&list, &mut seeded(6)), Err(DrawError::DuplicateEntryId(id)));
        assert_eq!(draw_ranking(&list, RankingRule::LastWins, &mut seeded(6)), Err(DrawError::DuplicateEntryId(id)));
        // A hidden copy counts too: the result action would find it before the drawn entry.
        let mut list = list_of(&[5, 5, 5]);
        list.entries[0].enabled = false;
        let id = list.entries[1].id;
        list.entries[0].id = id;
        assert_eq!(draw(&list, &mut seeded(6)), Err(DrawError::DuplicateEntryId(id)));
        assert_eq!(draw_ranking(&list, RankingRule::FirstWins, &mut seeded(6)), Err(DrawError::DuplicateEntryId(id)));
    }

    #[test]
    fn weights_summing_to_zero_cannot_be_drawn() {
        assert_eq!(draw_index(&[], &mut seeded(4)), None);
        assert_eq!(draw_index(&[0, 0], &mut seeded(4)), None);
        let mut rng = seeded(4);
        for _ in 0..100 {
            assert_eq!(draw_index(&[0, 7, 0], &mut rng), Some(1));
        }
    }

    #[test]
    fn a_seed_fixes_the_outcome() {
        let list = list_of(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let outcomes = |seed| (0..20).map(|_| draw(&list, &mut seeded(seed)).unwrap()).collect::<Vec<_>>();
        assert_eq!(outcomes(5), outcomes(5));
        let first: HashSet<EntryId> = (0..50).map(|seed| draw(&list, &mut seeded(seed)).unwrap().selected).collect();
        assert!(first.len() > 1, "fifty seeds all drew the same entry");
    }

    #[test]
    fn the_os_generator_draws() {
        let mut rng = os_rng();
        assert_ne!(rng.next_u64(), rng.next_u64());
        assert!(draw(&list_of(&[1, 1]), &mut rng).is_ok());
    }

    #[test]
    fn ranking_draws_the_same_winner_as_a_single_draw() {
        let list = list_of(&[1, 10, 100, 1000]);
        for seed in 0..50 {
            let single = draw(&list, &mut seeded(seed)).unwrap();
            for rule in [RankingRule::FirstWins, RankingRule::LastWins] {
                assert_eq!(draw_ranking(&list, rule, &mut seeded(seed)).unwrap().selected, single.selected);
            }
        }
    }

    proptest! {
        /// The heart of fairness: each index is the answer for exactly `weight` of the `W`
        /// possible values of `r`, so a uniform `r` gives probability `weight / W`.
        #[test]
        fn each_index_wins_for_exactly_its_weight_in_values_of_r(
            weights in prop::collection::vec(0u32..=40, 0..12),
        ) {
            let cumulative = cumulative_sums(&weights);
            let mut wins = vec![0u32; weights.len()];
            for r in 0..cumulative.last().copied().unwrap_or(0) {
                wins[index_for(&cumulative, r)] += 1;
            }
            prop_assert_eq!(wins, weights);
        }

        /// Wherever a second entry with an existing id sits, hidden or not, neither draw
        /// goes ahead.
        #[test]
        fn lists_where_two_entries_share_an_id_are_never_drawn(
            list in testing::list_with(1..=12),
            copy in testing::entry(),
            original in any::<prop::sample::Index>(),
            at in any::<prop::sample::Index>(),
            seed in any::<u64>(),
        ) {
            let mut list = list;
            let id = list.entries[original.index(list.entries.len())].id;
            list.entries.insert(at.index(list.entries.len() + 1), Entry { id, ..copy });
            prop_assert_eq!(draw(&list, &mut seeded(seed)), Err(DrawError::DuplicateEntryId(id)));
            let ranking = draw_ranking(&list, RankingRule::FirstWins, &mut seeded(seed));
            prop_assert_eq!(ranking, Err(DrawError::DuplicateEntryId(id)));
        }

        #[test]
        fn draws_return_a_visible_entry(list in testing::list(), seed in any::<u64>()) {
            match draw(&list, &mut seeded(seed)) {
                Ok(outcome) => {
                    prop_assert!(list.entry(outcome.selected).is_some_and(|entry| entry.enabled));
                    prop_assert_eq!(outcome.ranking, None);
                }
                Err(error) => {
                    prop_assert_eq!(error, DrawError::NoVisibleEntries);
                    prop_assert!(list.entries.iter().all(|entry| !entry.enabled));
                }
            }
        }

        #[test]
        fn rankings_order_every_visible_entry_with_the_winner_at_the_rule_end(
            list in testing::list(),
            first_wins in any::<bool>(),
            seed in any::<u64>(),
        ) {
            let rule = if first_wins { RankingRule::FirstWins } else { RankingRule::LastWins };
            let Ok(outcome) = draw_ranking(&list, rule, &mut seeded(seed)) else {
                prop_assert!(list.entries.iter().all(|entry| !entry.enabled));
                return Ok(());
            };
            let ranking = outcome.ranking.unwrap();
            let mut ranked = ranking.clone();
            ranked.sort();
            let mut visible: Vec<EntryId> =
                list.entries.iter().filter(|entry| entry.enabled).map(|entry| entry.id).collect();
            visible.sort();
            prop_assert_eq!(ranked, visible);
            let winner_slot = if first_wins { ranking.first() } else { ranking.last() };
            prop_assert_eq!(winner_slot, Some(&outcome.selected));
        }
    }
}
