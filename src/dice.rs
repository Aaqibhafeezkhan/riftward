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

    /// A contested roll: `a` wins with base probability `p`. Each side gets its own lucky
    /// chance; if both are lucky they cancel and the plain roll stands.
    pub fn contest(&mut self, luck_a: i32, luck_b: i32, p: f64) -> bool {
        let p = p.clamp(0.0, 1.0);
        let (a, b) = (self.lucky(luck_a), self.lucky(luck_b));
        let first = self.rng.random_bool(p);
        match (a, b) {
            (true, false) => first || self.rng.random_bool(p),
            (false, true) => first && self.rng.random_bool(p),
            _ => first,
        }
    }
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

    #[test]
    fn equal_luck_cancels_in_contests() {
        let mut dice = Dice::seeded(7);
        assert_near(rate(|| dice.contest(30, 30, 0.6)), 0.6);
        assert!(rate(|| dice.contest(100, 0, 0.5)) > 0.55);
        assert!(rate(|| dice.contest(0, 100, 0.5)) < 0.45);
    }
}
