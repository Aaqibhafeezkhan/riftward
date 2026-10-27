//! Every random roll in the game goes through `Dice`, which applies the roller's Luck: each point
//! is a percent better (`luck_scale` in `progression.ron` is the Luck that doubles a roll). A
//! chance is that much likelier, an amount that much bigger, a contest rating that much stronger,
//! and a pick from a table that much further toward its best end. Every point counts, and Luck,
//! like every attribute, has no cap. Nothing else in the game should own a random number
//! generator.

use bevy::prelude::*;
use rand::rngs::{SmallRng, SysRng};
use rand::{RngExt, SeedableRng};

pub struct DicePlugin;

impl Plugin for DicePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Dice::from_os());
    }
}

/// How much better than plain a roll by someone with this much Luck is: 1 with none, 1.1 with
/// Luck 10 (at the default `luck_scale` of 100), 2 with Luck 100.
pub fn luck_boost(luck: i32) -> f64 {
    let scale = crate::attributes::progression().luck_scale.max(1.0);
    1.0 + f64::from(luck.max(0)) / scale
}

#[derive(Resource)]
pub struct Dice {
    rng: SmallRng,
}

impl Dice {
    pub fn from_os() -> Self {
        Self {
            rng: SmallRng::try_from_rng(&mut SysRng).expect("OS random number generator failed"),
        }
    }

    /// Deterministic dice, for tests and simulations.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn seeded(seed: u64) -> Self {
        Self {
            rng: SmallRng::seed_from_u64(seed),
        }
    }

    /// A yes/no roll that succeeds with probability `p`, Luck percent likelier.
    pub fn chance(&mut self, luck: i32, p: f64) -> bool {
        let p = (p.clamp(0.0, 1.0) * luck_boost(luck)).min(1.0);
        self.rng.random_bool(p)
    }

    /// An amount in `range`, where higher is better for the roller, then Luck percent more. It
    /// can go past the top: luck hits harder. Whole numbers round by chance, so on average a
    /// roll is worth exactly its Luck: 6 with Luck 1 is 6.06, which is 7 one time in 16.
    pub fn range(&mut self, luck: i32, range: std::ops::RangeInclusive<i64>) -> i64 {
        let rolled = self.rng.random_range(range);
        if luck <= 0 {
            return rolled;
        }
        let lucky = rolled as f64 * luck_boost(luck);
        let whole = lucky.floor();
        let up = self.rng.random_bool((lucky - whole).clamp(0.0, 1.0));
        whole as i64 + i64::from(up)
    }

    /// Pick from a weighted table ordered worst to best, e.g. loot tiers from Common to
    /// Legendary: where the roll lands along the table, pushed Luck percent toward its best end.
    /// Returns `None` if every weight is zero.
    pub fn pick<'a, T>(&mut self, luck: i32, table: &'a [(T, u32)]) -> Option<&'a T> {
        let total: u64 = table.iter().map(|(_, w)| u64::from(*w)).sum();
        if total == 0 {
            return None;
        }
        let landed = self.rng.random_range(0.0..total as f64) * luck_boost(luck);
        let mut at = landed.min(total as f64 - 1.0) as u64;
        for (item, weight) in table {
            let weight = u64::from(*weight);
            if at < weight {
                return Some(item);
            }
            at -= weight;
        }
        None
    }

    /// An opposed roll, `a` against `b`: each side rolls a performance around its rating, made
    /// Luck percent stronger, and the higher wins. The weaker side wins (weaker /
    /// stronger)^STEEPNESS as often as the stronger, whatever the scale, so 5 against 10 is the
    /// same contest as 50 against 100. A fixed difficulty rolls with no Luck.
    pub fn opposed(&mut self, a: f64, luck_a: i32, b: f64, luck_b: i32) -> Contest {
        let (a, b) = (
            self.perform(a * luck_boost(luck_a)),
            self.perform(b * luck_boost(luck_b)),
        );
        Contest {
            won: a > b,
            margin: ((a - b).abs() / STEEPNESS).exp(),
        }
    }

    /// A performance: STEEPNESS times the log of the rating, plus a Gumbel draw. The difference
    /// of two Gumbel draws is logistic, which makes the odds exactly a^k / (a^k + b^k).
    fn perform(&mut self, rating: f64) -> f64 {
        STEEPNESS * rating.max(0.1).ln() + self.gumbel()
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
    fn each_point_of_luck_is_a_percent() {
        assert_eq!(luck_boost(0), 1.0);
        assert_eq!(luck_boost(-5), 1.0);
        assert!((luck_boost(10) - 1.1).abs() < 1e-9);
        assert!((luck_boost(100) - 2.0).abs() < 1e-9);
        assert!((luck_boost(200) - 3.0).abs() < 1e-9, "no cap");
    }

    #[test]
    fn no_luck_means_plain_odds() {
        let mut dice = Dice::seeded(1);
        assert_near(rate(|| dice.chance(0, 0.3)), 0.3);
    }

    #[test]
    fn luck_makes_a_chance_that_much_likelier() {
        let mut dice = Dice::seeded(2);
        assert_near(rate(|| dice.chance(50, 0.2)), 0.3);
        assert_near(rate(|| dice.chance(1, 0.5)), 0.505);
        assert!(
            (0..1000).all(|_| dice.chance(1000, 0.5)),
            "never past certain"
        );
    }

    #[test]
    fn certain_and_impossible_rolls_stay_so() {
        let mut dice = Dice::seeded(4);
        assert!((0..1000).all(|_| dice.chance(1000, 1.0)));
        assert!((0..1000).all(|_| !dice.chance(1000, 0.0)));
    }

    #[test]
    fn luck_shifts_picks_toward_the_best_entry() {
        let table = [("common", 90), ("rare", 9), ("legendary", 1)];
        let mut dice = Dice::seeded(5);
        assert_near(rate(|| dice.pick(0, &table) == Some(&"legendary")), 0.01);
        // Half again as far along: anything landing past two thirds of the way reaches the end.
        let lucky = rate(|| dice.pick(50, &table) == Some(&"legendary"));
        assert!((0.32..0.35).contains(&lucky), "got {lucky}");
        assert_eq!(dice.pick(10, &[("nothing", 0)]), None);
    }

    #[test]
    fn luck_makes_amounts_that_much_bigger_on_average() {
        let mut dice = Dice::seeded(6);
        let mean = |dice: &mut Dice, luck| {
            (0..TRIALS).map(|_| dice.range(luck, 1..=10)).sum::<i64>() as f64 / f64::from(TRIALS)
        };
        assert!((mean(&mut dice, 0) - 5.5).abs() < 0.05);
        assert!(
            (mean(&mut dice, 1) - 5.555).abs() < 0.05,
            "even a point counts"
        );
        assert!((mean(&mut dice, 50) - 8.25).abs() < 0.05);
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
    fn luck_strengthens_a_side_and_equal_luck_cancels_in_contests() {
        let mut dice = Dice::seeded(8);
        assert_near(rate(|| dice.opposed(10.0, 30, 10.0, 30).won), 0.5);
        // Luck 100 doubles a rating: 20 against 10.
        assert_near(
            rate(|| dice.opposed(10.0, 100, 10.0, 0).won),
            curve(20.0, 10.0),
        );
        assert_near(
            rate(|| dice.opposed(10.0, 0, 10.0, 100).won),
            curve(10.0, 20.0),
        );
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
