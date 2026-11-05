//! The statistical test that gates M1 (design doc, § Testing e verifica): over a million
//! draws, each visible entry comes up in proportion to its weight, and hidden entries
//! never do. Seed and threshold are fixed, so the result is the same on every run.

use std::collections::HashMap;

use rand::SeedableRng;
use rand::rngs::Xoshiro256PlusPlus;
use sorte_core::{Entry, EntryId, ListDoc, draw};

/// Weights of the visible entries, extremes of the range included.
const WEIGHTS: [u32; 8] = [1, 2, 3, 10, 1000, 7, 1, 500];

const DRAWS: u32 = 1_000_000;

const SEED: u64 = 20_261_105;

/// The critical value of the chi-square distribution with 7 degrees of freedom (one less
/// than the 8 visible entries) at significance 0.001, from the NIST/SEMATECH e-Handbook
/// of Statistical Methods, § 1.3.6.7.4, "Critical Values of the Chi-Square
/// Distribution": a fair draw exceeds it for one seed in a thousand.
const CHI_SQUARE_CRITICAL: f64 = 24.322;

#[test]
fn a_million_draws_follow_the_weights_and_never_pick_a_hidden_entry() {
    assert_eq!(WEIGHTS.len() - 1, 7, "CHI_SQUARE_CRITICAL is for 7 degrees of freedom");
    let mut list = ListDoc::new("Equità");
    list.entries =
        WEIGHTS.iter().enumerate().map(|(i, &weight)| Entry { weight, ..Entry::new(format!("{i}")) }).collect();
    // Hidden entries at the start, in the middle and at the end, heavy enough that a draw
    // counting them would pick them most of the time.
    for at in [0, 5, 10] {
        list.entries.insert(at, Entry { weight: 1000, enabled: false, ..Entry::new("hidden") });
    }

    let mut rng = Xoshiro256PlusPlus::seed_from_u64(SEED);
    let mut counts: HashMap<EntryId, u32> = HashMap::new();
    for _ in 0..DRAWS {
        *counts.entry(draw(&list, &mut rng).unwrap().selected).or_default() += 1;
    }

    let hidden_draws: u32 =
        list.entries.iter().filter(|entry| !entry.enabled).filter_map(|entry| counts.get(&entry.id)).sum();
    assert_eq!(hidden_draws, 0, "hidden entries were drawn");

    let total_weight: u32 = WEIGHTS.iter().sum();
    let mut chi_square = 0.0;
    let mut report = String::new();
    for entry in list.visible_entries() {
        let expected = f64::from(DRAWS) * f64::from(entry.weight) / f64::from(total_weight);
        let observed = f64::from(counts.get(&entry.id).copied().unwrap_or(0));
        chi_square += (observed - expected).powi(2) / expected;
        report += &format!("\n  weight {:>4}: drawn {observed:>6}, expected {expected:>9.1}", entry.weight);
    }
    assert!(chi_square < CHI_SQUARE_CRITICAL, "chi-square {chi_square:.2} is not below {CHI_SQUARE_CRITICAL}:{report}");
}
