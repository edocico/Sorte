//! The cosmetic generator: randomness for how a draw looks (where in the slice the wheel stops,
//! how the horses pace), never for what it decides.

use std::time::{SystemTime, UNIX_EPOCH};

use rand::rngs::{SysRng, Xoshiro256PlusPlus};
use rand::{Rng, SeedableRng, TryRng};

/// Random numbers for a choreography's looks, from their own seed.
///
/// Kept apart from the outcome's generator by type: it implements none of rand's traits, so it
/// cannot be passed to `sorte-core`'s draw, nor the draw's generator to a picker. The same seed
/// gives the same numbers on every platform and release (xoshiro256++), so a staged draw can be
/// replayed exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CosmeticRng(Xoshiro256PlusPlus);

impl CosmeticRng {
    pub fn seeded(seed: u64) -> Self {
        Self(Xoshiro256PlusPlus::seed_from_u64(seed))
    }

    /// Seeded by the operating system, or by the clock if the OS cannot provide a seed: looks
    /// need variety, not secrecy.
    pub fn from_os() -> Self {
        let seed = SysRng.try_next_u64().unwrap_or_else(|_| clock_seed());
        Self::seeded(seed)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        unit_of(self.0.next_u64())
    }

    /// Uniform between `low` (included) and `high` (excluded); `low` when they are equal.
    pub fn uniform(&mut self, low: f64, high: f64) -> f64 {
        between(low, high, self.unit())
    }

    /// Uniform in `0..n`, or 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        // Multiply-shift: unbiased enough for looks, and no division.
        ((u128::from(self.0.next_u64()) * n as u128) >> 64) as usize
    }

    /// True with probability `p` (clamped to [0, 1]).
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

/// The top 53 bits of `bits` as a number in [0, 1): every double of [0, 1) on a grid of 2⁻⁵³.
fn unit_of(bits: u64) -> f64 {
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// The point `unit` (in [0, 1)) of the way from `low` to `high`, below `high` when `low` is:
/// near the top of [0, 1) the sum can round up to `high` itself.
fn between(low: f64, high: f64, unit: f64) -> f64 {
    let value = low + (high - low) * unit;
    if low < high && value >= high { high.next_down() } else { value }
}

/// A seed from the wall clock's nanoseconds.
fn clock_seed() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_nanos() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_numbers() {
        let (mut a, mut b) = (CosmeticRng::seeded(42), CosmeticRng::seeded(42));
        let draws = |rng: &mut CosmeticRng| (rng.unit(), rng.uniform(0.15, 0.85), rng.below(7), rng.chance(0.5));
        for _ in 0..100 {
            assert_eq!(draws(&mut a), draws(&mut b));
        }
        assert_ne!(CosmeticRng::seeded(1).unit(), CosmeticRng::seeded(2).unit());
    }

    #[test]
    fn numbers_are_portable_across_platforms_and_releases() {
        // rand 0.10's own expected outputs for xoshiro256++ seeded with 0 through SplitMix64
        // (rngs/xoshiro256plusplus.rs, `stable_seed_from_u64_and_from_seed`): if the generator
        // or its seeding ever changed, replays of staged draws would change with it.
        let mut rng = CosmeticRng::seeded(0);
        assert_eq!([rng.0.next_u64(), rng.0.next_u64()], [5987356902031041503, 7051070477665621255]);
    }

    #[test]
    fn os_seeds_differ() {
        assert_ne!(CosmeticRng::from_os(), CosmeticRng::from_os());
    }

    #[test]
    fn numbers_stay_in_their_ranges() {
        let mut rng = CosmeticRng::seeded(7);
        for _ in 0..10_000 {
            let unit = rng.unit();
            assert!((0.0..1.0).contains(&unit));
            let u = rng.uniform(0.15, 0.85);
            assert!((0.15..0.85).contains(&u), "{u}");
            assert!(rng.below(3) < 3);
        }
        assert_eq!(rng.uniform(2.0, 2.0), 2.0);
        assert_eq!(rng.below(0), 0);
        assert_eq!(rng.below(1), 0);
        assert!(!rng.chance(0.0) && rng.chance(1.0));
    }

    #[test]
    fn uniform_stays_below_high_even_at_the_largest_unit() {
        let largest = unit_of(u64::MAX);
        assert_eq!(largest, 1.0 - f64::EPSILON / 2.0);
        let pairs = [(0.2, 0.8), (1.0, 3.0), (100.0, 100.5), (0.15, 0.85), (-1.0, 0.7)];
        // The plain formula rounds up to `high` itself for some of them.
        assert!(pairs.iter().any(|&(low, high)| low + (high - low) * largest >= high));
        for (low, high) in pairs {
            let value = between(low, high, largest);
            assert!(low <= value && value < high, "{low}..{high} gave {value}");
        }
        assert_eq!(between(0.2, 0.8, 0.0), 0.2);
        assert_eq!(between(2.0, 2.0, largest), 2.0);
    }

    #[test]
    fn below_covers_every_value_evenly() {
        let mut rng = CosmeticRng::seeded(3);
        let mut counts = [0u32; 6];
        for _ in 0..60_000 {
            counts[rng.below(6)] += 1;
        }
        assert!(counts.iter().all(|&c| (9_400..10_600).contains(&c)), "{counts:?}");
    }
}
