//! Character attributes and the Karma spent to raise them.
//!
//! Attributes are uncapped. Every character starts from the base ratings (3, Luck 1, Magic 0),
//! shifted by their race, with `STARTING_KARMA` to spend (humans get more). Magic can be raised like
//! anything else, but it stays latent (it does nothing) until the character knows a casting skill.
//! Karma spent is not stored: it is worked out from the current ratings and the starting ones, so
//! it can never drift out of sync with them.
//!
//! Karma is earned by character level, which comes from experience: deeds like victories and
//! discoveries, not skill levels (see `docs/progression.md`).

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::db::Database;

/// Karma every character starts with, to spend on attributes.
pub const STARTING_KARMA: i32 = 10;
/// Karma each character level earns.
pub const KARMA_PER_LEVEL: i32 = 2;
/// Experience for the first character level; each level after costs `LEVEL_GROWTH` times more.
const FIRST_LEVEL_EXPERIENCE: f64 = 100.0;
const LEVEL_GROWTH: f64 = 1.25;

/// Total experience to reach character `level`: 100 for the first, then 1.25 times more each.
pub fn experience_for(level: u32) -> u64 {
    (0..level)
        .map(|n| (FIRST_LEVEL_EXPERIENCE * LEVEL_GROWTH.powi(n as i32)).round() as u64)
        .sum()
}

/// The character level `experience` reaches.
pub fn level_for(experience: u64) -> u32 {
    let mut level = 0;
    while experience_for(level + 1) <= experience {
        level += 1;
    }
    level
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Attribute {
    Body,
    Agility,
    Reaction,
    Strength,
    Willpower,
    Logic,
    Intuition,
    Charisma,
    Luck,
    Magic,
}

impl Attribute {
    pub const ALL: [Attribute; 10] = [
        Attribute::Body,
        Attribute::Agility,
        Attribute::Reaction,
        Attribute::Strength,
        Attribute::Willpower,
        Attribute::Logic,
        Attribute::Intuition,
        Attribute::Charisma,
        Attribute::Luck,
        Attribute::Magic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Attribute::Body => "Body",
            Attribute::Agility => "Agility",
            Attribute::Reaction => "Reaction",
            Attribute::Strength => "Strength",
            Attribute::Willpower => "Willpower",
            Attribute::Logic => "Logic",
            Attribute::Intuition => "Intuition",
            Attribute::Charisma => "Charisma",
            Attribute::Luck => "Luck",
            Attribute::Magic => "Magic",
        }
    }

    /// The rating every character starts from, before their race shifts it.
    pub fn base_rating(self) -> i32 {
        match self {
            Attribute::Magic => 0,
            Attribute::Luck => 1,
            _ => 3,
        }
    }

    /// Parse a name or any unambiguous prefix of one: `str` is Strength, but `l` could be Logic
    /// or Luck, so it's rejected.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.to_ascii_lowercase();
        if s.is_empty() {
            return Err("Which attribute?".into());
        }
        let matches: Vec<_> = Self::ALL
            .into_iter()
            .filter(|a| a.name().to_ascii_lowercase().starts_with(&s))
            .collect();
        match matches.as_slice() {
            [one] => Ok(*one),
            [] => Err(format!("There is no attribute '{s}'.")),
            many => Err(format!(
                "'{s}' could mean {}.",
                many.iter()
                    .map(|a| a.name())
                    .collect::<Vec<_>>()
                    .join(" or ")
            )),
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|a| *a == self).unwrap()
    }
}

/// Karma it costs to raise an attribute from `rating` to `rating + 1`.
pub fn raise_cost(rating: i32) -> i32 {
    1 + rating.max(0) / 5
}

/// A character's attribute ratings, where they started, and the Karma they started with. Lives on
/// the player entity.
#[derive(Component, Clone, Debug)]
pub struct Attributes {
    ratings: [i32; 10],
    start: [i32; 10],
    karma: i32,
    /// Experience, from deeds; it sets the character's level and so the Karma they've earned.
    experience: u64,
}

impl Default for Attributes {
    fn default() -> Self {
        Self::starting(Attribute::base_rating, STARTING_KARMA)
    }
}

impl Attributes {
    /// A new character's attributes: each at its starting rating, with `karma` to spend.
    pub fn starting(start: impl Fn(Attribute) -> i32, karma: i32) -> Self {
        let start = Attribute::ALL.map(start);
        Self {
            ratings: start,
            start,
            karma,
            experience: 0,
        }
    }

    /// Apply saved ratings from database rows; attributes with no row keep their starting rating.
    pub fn with_rows(mut self, rows: impl IntoIterator<Item = (String, i32)>) -> Self {
        for (name, rating) in rows {
            match Attribute::ALL.into_iter().find(|a| a.name() == name) {
                Some(attribute) => self.set(attribute, rating),
                None => warn!("ignoring unknown attribute '{name}' in the database"),
            }
        }
        self
    }

    pub fn get(&self, attribute: Attribute) -> i32 {
        self.ratings[attribute.index()]
    }

    pub fn set(&mut self, attribute: Attribute, rating: i32) {
        self.ratings[attribute.index()] = rating;
    }

    /// The rating it started at, which Karma can't take it below.
    pub fn start(&self, attribute: Attribute) -> i32 {
        self.start[attribute.index()]
    }

    /// Every rating, for saving.
    pub fn rows(&self) -> Vec<(&'static str, i32)> {
        Attribute::ALL
            .into_iter()
            .map(|attribute| (attribute.name(), self.get(attribute)))
            .collect()
    }

    /// Total Karma spent to reach the current ratings from the starting ones.
    pub fn karma_spent(&self) -> i32 {
        Attribute::ALL
            .into_iter()
            .map(|attribute| {
                (self.start(attribute)..self.get(attribute))
                    .map(raise_cost)
                    .sum::<i32>()
            })
            .sum()
    }

    /// Karma available to spend: starting and earned Karma, less what's been spent.
    pub fn karma_unspent(&self) -> i32 {
        (self.karma + self.earned() - self.karma_spent()).max(0)
    }

    /// Karma earned in play: `KARMA_PER_LEVEL` for each character level.
    fn earned(&self) -> i32 {
        self.level() as i32 * KARMA_PER_LEVEL
    }

    pub fn experience(&self) -> u64 {
        self.experience
    }

    /// The character's level, from their experience.
    pub fn level(&self) -> u32 {
        level_for(self.experience)
    }

    /// Set experience as loaded.
    pub fn set_experience(&mut self, experience: u64) {
        self.experience = experience;
    }

    /// Earn `points` of experience for `deed` ("for defeating the sewer rat"), saving it for
    /// character `character_id`. Returns what to tell them, including any character levels and
    /// the Karma they bring.
    pub fn earn(
        &mut self,
        points: u64,
        deed: &str,
        character_id: i64,
        db: &Database,
    ) -> Vec<String> {
        if points == 0 {
            return Vec::new();
        }
        let before = self.level();
        self.experience += points;
        if let Err(err) = db.save_experience(character_id, self.experience) {
            error!("failed to save experience for character {character_id}: {err}");
        }
        let mut lines = vec![format!(
            "\x1b[33mYou gain {points} experience {deed}.\x1b[0m"
        )];
        let after = self.level();
        if after > before {
            let karma = (after - before) as i32 * KARMA_PER_LEVEL;
            lines.push(format!(
                "\x1b[1;33mYou have reached character level {after}! You have earned {karma} \
                 Karma ({} to spend).\x1b[0m",
                self.karma_unspent()
            ));
        }
        lines
    }

    /// Raise an attribute by one if there's Karma for it, or say why not.
    pub fn raise(&mut self, attribute: Attribute) -> Result<i32, String> {
        let current = self.get(attribute);
        let (cost, unspent) = (raise_cost(current), self.karma_unspent());
        if cost > unspent {
            return Err(format!(
                "Raising {} to {} costs {cost} Karma, and you have {unspent}.",
                attribute.name(),
                current + 1
            ));
        }
        self.set(attribute, current + 1);
        Ok(cost)
    }

    /// Take back one point bought with Karma, during character creation.
    pub fn lower(&mut self, attribute: Attribute) -> Result<(), String> {
        let current = self.get(attribute);
        if current <= self.start(attribute) {
            return Err(format!(
                "{} is at its starting rating of {current}.",
                attribute.name()
            ));
        }
        self.set(attribute, current - 1);
        Ok(())
    }

    pub fn health(&self) -> i32 {
        8 + self.get(Attribute::Body) / 2
    }

    pub fn stamina(&self) -> i32 {
        8 + self.get(Attribute::Willpower) / 2
    }

    pub fn initiative(&self) -> i32 {
        self.get(Attribute::Reaction) + self.get(Attribute::Intuition)
    }
}

/// The `score` command. `who` is what the character is, like "Troll Street Urchin", `current`
/// and `max` their health and stamina, and `tradition` the magic they're Awakened to, if any.
pub fn score_text(
    name: &str,
    who: &str,
    attributes: &Attributes,
    current: (i32, i32),
    max: (i32, i32),
    tradition: Option<&str>,
) -> String {
    let row = |label: &str, list: &[Attribute]| {
        let cells: Vec<_> = list
            .iter()
            .map(|a| format!("{:<10}{:>4}", a.name(), attributes.get(*a)))
            .collect();
        format!("  {label:<10}{}", cells.join("    "))
    };
    use Attribute::*;
    // Magic does nothing until a tradition's teacher Awakens it.
    let magic = match (tradition, attributes.get(Magic) > 0) {
        (Some(tradition), _) => format!("  (Awakened: {tradition})"),
        (None, true) => "  (latent: not yet Awakened)".to_string(),
        (None, false) => String::new(),
    };
    let karma = attributes.karma_unspent();
    let level = attributes.level();
    let next = experience_for(level + 1);
    let mut text = format!(
        "\x1b[1m{name}\x1b[0m, {who}, level {level}  ({} of {next} experience for the next)\r\n{}\r\n{}\r\n{}{magic}\r\n\r\n  Health {}/{}    Stamina {}/{}    Initiative {}\r\n  Karma: {karma} unspent",
        attributes.experience(),
        row("Physical", &[Body, Agility, Reaction, Strength]),
        row("Mental", &[Willpower, Logic, Intuition, Charisma]),
        row("Special", &[Luck, Magic]),
        current.0,
        max.0,
        current.1,
        max.1,
        attributes.initiative(),
    );
    if karma > 0 {
        text += "  (raise <attribute> to spend it)";
    }
    text
}

/// The `raise <attribute>` command: spend Karma on one point.
pub fn raise_command(
    character_id: i64,
    attributes: &mut Attributes,
    args: &str,
    db: &Database,
) -> String {
    if args.is_empty() {
        return "Usage: raise <attribute>   (type 'score' to see your attributes)".into();
    }
    let attribute = match Attribute::parse(args) {
        Ok(attribute) => attribute,
        Err(err) => return err,
    };
    let current = attributes.get(attribute);
    let mut raised = attributes.clone();
    let cost = match raised.raise(attribute) {
        Ok(cost) => cost,
        Err(err) => return err,
    };
    if let Err(err) = db.set_attribute(character_id, attribute.name(), current + 1) {
        error!("failed to save attribute for character {character_id}: {err}");
        return "Something went wrong saving that. Please try again.".into();
    }
    *attributes = raised;
    let mut reply = format!(
        "{} raised to {} for {cost} Karma. {} Karma left.",
        attribute.name(),
        current + 1,
        attributes.karma_unspent()
    );
    if attribute == Attribute::Magic {
        reply += " Your magic stays latent until you learn to cast.";
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_level_costs_more_and_brings_karma() {
        assert_eq!(experience_for(0), 0);
        assert_eq!(experience_for(1), 100);
        assert_eq!(experience_for(2), 225);
        // The tenth level alone costs about 750.
        let tenth = experience_for(10) - experience_for(9);
        assert!((740..760).contains(&tenth), "{tenth}");
        assert_eq!(level_for(99), 0);
        assert_eq!(level_for(100), 1);
        assert_eq!(level_for(experience_for(12)), 12);
        assert_eq!(level_for(experience_for(12) - 1), 11);

        let db = Database::open(":memory:").unwrap();
        let mut attributes = Attributes::starting(Attribute::base_rating, STARTING_KARMA);
        let before = attributes.karma_unspent();
        let told = attributes.earn(150, "for testing", 1, &db);
        assert_eq!(attributes.level(), 1);
        assert_eq!(attributes.karma_unspent(), before + KARMA_PER_LEVEL);
        assert!(told.iter().any(|line| line.contains("character level 1")));
        assert!(attributes.earn(0, "for nothing", 1, &db).is_empty());
    }

    #[test]
    fn cost_rises_every_five_ratings() {
        assert_eq!((1..=4).map(raise_cost).collect::<Vec<_>>(), [1, 1, 1, 1]);
        assert_eq!(raise_cost(5), 2);
        assert_eq!(raise_cost(10), 3);
        assert_eq!(raise_cost(19), 4);
    }

    #[test]
    fn karma_spent_is_derived_from_ratings() {
        let mut attributes = Attributes::default();
        assert_eq!(attributes.karma_spent(), 0);
        assert_eq!(attributes.karma_unspent(), STARTING_KARMA);

        attributes.set(Attribute::Agility, 5); // 3 -> 5: two raises at 1 each
        assert_eq!(attributes.karma_spent(), 2);
        attributes.set(Attribute::Agility, 7); // 5 -> 7: two more at 2 each
        assert_eq!(attributes.karma_spent(), 6);

        attributes.set(Attribute::Magic, 2); // Magic starts at 0: two raises at 1 each
        assert_eq!(attributes.karma_spent(), 8);
    }

    #[test]
    fn karma_counts_from_where_a_race_starts() {
        // A troll starts strong: their first points of Strength weren't bought.
        let start = |a: Attribute| match a {
            Attribute::Strength => 6,
            other => other.base_rating(),
        };
        let mut troll = Attributes::starting(start, STARTING_KARMA);
        assert_eq!(troll.karma_spent(), 0);
        assert_eq!(troll.raise(Attribute::Strength), Ok(2));
        assert_eq!(troll.karma_unspent(), STARTING_KARMA - 2);
        assert!(troll.lower(Attribute::Strength).is_ok());
        assert!(
            troll.lower(Attribute::Strength).is_err(),
            "not below the start"
        );
        // Humans start with more Karma.
        assert_eq!(
            Attributes::starting(Attribute::base_rating, 13).karma_unspent(),
            13
        );
        // You can't spend Karma you don't have.
        let mut broke = Attributes::starting(Attribute::base_rating, 1);
        assert_eq!(broke.raise(Attribute::Body), Ok(1));
        assert!(broke.raise(Attribute::Body).is_err());
    }

    #[test]
    fn parses_unambiguous_prefixes() {
        assert_eq!(Attribute::parse("str"), Ok(Attribute::Strength));
        assert_eq!(Attribute::parse("LUCK"), Ok(Attribute::Luck));
        assert!(Attribute::parse("l").is_err());
        assert!(Attribute::parse("dexterity").is_err());
    }

    #[test]
    fn rows_override_defaults() {
        let attributes =
            Attributes::default().with_rows([("Body".to_string(), 4), ("Nonsense".into(), 9)]);
        assert_eq!(attributes.get(Attribute::Body), 4);
        assert_eq!(attributes.get(Attribute::Agility), 3);
        assert_eq!(attributes.get(Attribute::Magic), 0);
    }
}
