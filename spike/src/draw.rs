//! The outcome draw, as sorte-core will do it (design doc, § Estrazione equa).

use rand::distr::{Distribution, Uniform};

/// Index of the drawn entry: an unbiased uniform r in [0, W), then the first
/// cumulative weight above r. Weights are integers, so the odds are exact.
pub fn draw_weighted<R: rand::Rng + ?Sized>(weights: &[u32], rng: &mut R) -> usize {
    let cumulative: Vec<u64> = weights
        .iter()
        .scan(0u64, |acc, &w| {
            *acc += u64::from(w);
            Some(*acc)
        })
        .collect();
    let total = *cumulative.last().expect("a non-empty list");
    let r = Uniform::new(0, total).expect("a positive total weight").sample(rng);
    cumulative.partition_point(|&c| c <= r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::Xoshiro256PlusPlus;

    #[test]
    fn frequencies_follow_weights() {
        let weights = [1, 2, 7];
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut counts = [0u32; 3];
        let draws = 200_000;
        for _ in 0..draws {
            counts[draw_weighted(&weights, &mut rng)] += 1;
        }
        // Chi-square with 2 degrees of freedom; 13.8 is the 0.1% critical value.
        let chi2: f64 = counts
            .iter()
            .zip(weights)
            .map(|(&c, w)| {
                let expected = f64::from(draws) * f64::from(w) / 10.0;
                (f64::from(c) - expected).powi(2) / expected
            })
            .sum();
        assert!(chi2 < 13.8, "chi2 = {chi2}, counts = {counts:?}");
    }
}
