//! Every random roll in the game goes through `Dice`, which applies the roller's Luck: a lucky
//! roll is rolled a second time and the better result kept. However high Luck goes, the effect
//! never exceeds "best of two". Nothing else in the game should own a random number generator.

use bevy::prelude::*;
use rand::rngs::{SmallRng, SysRng};
use rand::{RngExt, SeedableRng};

/// Luck at which half of all rolls get a second chance. Higher = Luck matters more slowly.
pub const LUCK_SCALE: f64 = 50.0;

pub struct DicePlugin;

impl Plugin for DicePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Dice::from_os());
    }
}

/// The chance, from 0 to 1, that a roll by someone with this much Luck is rolled twice.
pub fn lucky_chance(luck: i32) -> f64 {
    let luck = f64::from(luck.max(0));
    luck / (luck + LUCK_SCALE)
}

#[derive(Resource)]
pub struct Dice {
    rng: SmallRng,
}

// Gameplay doesn't roll anything yet; skills will be the first user.
#[cfg_attr(not(test), allow(dead_code))]
impl Dice {
    pub fn from_os() -> Self {
        Self {
            rng: SmallRng::try_from_rng(&mut SysRng).expect("OS random number generator failed"),
        }
    }

    /// Deterministic dice, for tests and simulations.
    pub fn seeded(seed: u64) -> Self {
        Self {
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    fn lucky(&mut self, luck: i32) -> bool {
        let p = lucky_chance(luck);
        p > 0.0 && self.rng.random_bool(p)
    }

    /// A yes/no roll that succeeds with probability `p`; Luck gives a second try on a miss.
    pub fn chance(&mut self, luck: i32, p: f64) -> bool {
        let p = p.clamp(0.0, 1.0);
        self.rng.random_bool(p) || (self.lucky(luck) && self.rng.random_bool(p))
    }

    /// A value in `range`, where higher is better for the roller.
    pub fn range(&mut self, luck: i32, range: std::ops::RangeInclusive<i64>) -> i64 {
        let first = self.rng.random_range(range.clone());
        if self.lucky(luck) {
            first.max(self.rng.random_range(range))
        } else {
            first
        }
    }

    /// Pick from a weighted table ordered worst to best, e.g. loot tiers from Common to
    /// Legendary. Returns `None` if every weight is zero.
    pub fn pick<'a, T>(&mut self, luck: i32, table: &'a [(T, u32)]) -> Option<&'a T> {
        let first = self.weighted_index(table)?;
        let index = if self.lucky(luck) {
            first.max(self.weighted_index(table)?)
        } else {
            first
        };
        table.get(index).map(|(item, _)| item)
    }

    fn weighted_index<T>(&mut self, table: &[(T, u32)]) -> Option<usize> {
        let total: u64 = table.iter().map(|(_, w)| u64::from(*w)).sum();
        if total == 0 {
            return None;
        }
        let mut roll = self.rng.random_range(0..total);
        for (i, (_, weight)) in table.iter().enumerate() {
            let weight = u64::from(*weight);
            if roll < weight {
                return Some(i);
            }
            roll -= weight;
        }
        None
    }

    /// An opposed roll, `a` against `b`: each side rolls a performance around its rating, and
    /// the higher wins. The weaker side wins (weaker / stronger)^STEEPNESS as often as the
    /// stronger, whatever the scale, so 5 against 10 is the same contest as 50 against 100. A
    /// side with Luck rolls twice and keeps the better. A fixed difficulty rolls with no Luck.
    pub fn opposed(&mut self, a: f64, luck_a: i32, b: f64, luck_b: i32) -> Contest {
        let (a, b) = (self.perform(a, luck_a), self.perform(b, luck_b));
        Contest {
            won: a > b,
            margin: ((a - b).abs() / STEEPNESS).exp(),
        }
    }

    /// A performance: STEEPNESS times the log of the rating, plus a Gumbel draw. The difference
    /// of two Gumbel draws is logistic, which makes the odds exactly a^k / (a^k + b^k).
    fn perform(&mut self, rating: f64, luck: i32) -> f64 {
        let base = STEEPNESS * rating.max(0.1).ln();
        let first = self.gumbel();
        let roll = if self.lucky(luck) {
            first.max(self.gumbel())
        } else {
            first
        };
        base + roll
    }

    /// A standard Gumbel draw.
    fn gumbel(&mut self) -> f64 {
        -(-self.rng.random_range(f64::EPSILON..1.0).ln()).ln()
    }
}

/// How steep contests are: the weaker side wins (weaker / stronger)^STEEPNESS as often as the
/// stronger. See the ratio rule in `docs/attributes.md`.
pub const STEEPNESS: f64 = 3.0;

/// How an opposed roll came out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Contest {
    /// Whether the first side won.
    pub won: bool,
    /// How far ahead the winner finished, as a ratio of performances: 1.1 is a near thing, 2 a
    /// decisive win. Always at least 1.
    pub margin: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIALS: u32 = 200_000;

    fn rate(mut roll: impl FnMut() -> bool) -> f64 {
        (0..TRIALS).filter(|_| roll()).count() as f64 / f64::from(TRIALS)
    }

    fn assert_near(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.005,
            "expected about {expected:.4}, got {actual:.4}"
        );
    }

    #[test]
    fn lucky_chance_follows_the_curve() {
        assert_eq!(lucky_chance(0), 0.0);
        assert_eq!(lucky_chance(-5), 0.0);
        assert!((lucky_chance(10) - 1.0 / 6.0).abs() < 1e-9);
        assert_eq!(lucky_chance(50), 0.5);
        assert!(lucky_chance(1_000_000) < 1.0);
    }

    #[test]
    fn no_luck_means_plain_odds() {
        let mut dice = Dice::seeded(1);
        assert_near(rate(|| dice.chance(0, 0.3)), 0.3);
    }

    #[test]
    fn luck_gives_a_second_try() {
        let mut dice = Dice::seeded(2);
        let (p, l) = (0.2, lucky_chance(50));
        assert_near(rate(|| dice.chance(50, p)), p + (1.0 - p) * l * p);
    }

    #[test]
    fn luck_never_exceeds_best_of_two() {
        let mut dice = Dice::seeded(3);
        let p = 0.01;
        let rate = rate(|| dice.chance(i32::MAX, p));
        assert!(rate < 1.0 - (1.0 - p) * (1.0 - p) + 0.002, "got {rate}");
    }

    #[test]
    fn certain_and_impossible_rolls_ignore_luck() {
        let mut dice = Dice::seeded(4);
        assert!((0..1000).all(|_| dice.chance(1000, 1.0)));
        assert!((0..1000).all(|_| !dice.chance(1000, 0.0)));
    }

    #[test]
    fn luck_shifts_picks_toward_the_best_entry() {
        let table = [("common", 90), ("rare", 9), ("legendary", 1)];
        let mut dice = Dice::seeded(5);
        assert_near(rate(|| dice.pick(0, &table) == Some(&"legendary")), 0.01);
        let lucky = rate(|| dice.pick(50, &table) == Some(&"legendary"));
        assert!(lucky > 0.012 && lucky < 0.0199, "got {lucky}");
        assert_eq!(dice.pick(10, &[("nothing", 0)]), None);
    }

    #[test]
    fn luck_raises_ranges() {
        let mut dice = Dice::seeded(6);
        let mean = |dice: &mut Dice, luck| {
            (0..TRIALS).map(|_| dice.range(luck, 1..=10)).sum::<i64>() as f64 / f64::from(TRIALS)
        };
        assert!((mean(&mut dice, 0) - 5.5).abs() < 0.05);
        assert!(mean(&mut dice, 50) > 5.9);
    }

    /// The weaker side's share of wins: (a / b)^k against 1.
    fn curve(a: f64, b: f64) -> f64 {
        let (a, b) = (a.powf(STEEPNESS), b.powf(STEEPNESS));
        a / (a + b)
    }

    #[test]
    fn opposed_rolls_follow_the_curve_at_any_scale() {
        let mut dice = Dice::seeded(7);
        assert_near(rate(|| dice.opposed(10.0, 0, 10.0, 0).won), 0.5);
        assert_near(rate(|| dice.opposed(5.0, 0, 10.0, 0).won), curve(5.0, 10.0));
        assert_near(
            rate(|| dice.opposed(50.0, 0, 100.0, 0).won),
            curve(5.0, 10.0),
        );
        assert_near(
            rate(|| dice.opposed(48.0, 0, 53.0, 0).won),
            curve(48.0, 53.0),
        );
        // A huge gap is effectively never, but still rolled.
        assert!(rate(|| dice.opposed(3.0, 0, 25.0, 0).won) < 0.005);
    }

    #[test]
    fn luck_helps_and_equal_luck_cancels_in_contests() {
        let mut dice = Dice::seeded(8);
        assert_near(rate(|| dice.opposed(10.0, 30, 10.0, 30).won), 0.5);
        assert!(rate(|| dice.opposed(10.0, 100, 10.0, 0).won) > 0.55);
        assert!(rate(|| dice.opposed(10.0, 0, 10.0, 100).won) < 0.45);
    }

    #[test]
    fn margins_measure_how_decisive_a_win_was() {
        let mut dice = Dice::seeded(9);
        let margins: Vec<f64> = (0..10_000)
            .map(|_| dice.opposed(30.0, 0, 10.0, 0).margin)
            .collect();
        assert!(margins.iter().all(|m| *m >= 1.0));
        // Three times the rating wins by about three times, on average, in performance.
        let mean = margins.iter().sum::<f64>() / margins.len() as f64;
        assert!(mean > 2.0, "{mean}");
    }
}
