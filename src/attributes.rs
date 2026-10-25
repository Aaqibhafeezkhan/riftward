//! Character attributes and the Karma spent to raise them.
//!
//! Attributes are uncapped. Every character starts from the base ratings (3, Luck 1, Magic 0),
//! shifted by their race, with `STARTING_KARMA` to spend (humans get more). Magic can be raised like
//! anything else, but it stays latent (it does nothing) until the character knows a casting skill.
//! Karma spent is not stored: it is worked out from the current ratings and the starting ones, so
//! it can never drift out of sync with them.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::db::Database;

/// Karma every character starts with, to spend on attributes.
pub const STARTING_KARMA: i32 = 10;

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
    /// Karma earned in play: one per character level.
    earned: i32,
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
            earned: 0,
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
        (self.karma + self.earned - self.karma_spent()).max(0)
    }

    /// Set the Karma earned in play, which is the character's level.
    pub fn set_earned(&mut self, earned: i32) {
        self.earned = earned;
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

/// The `score` command. `who` is what the character is, like "Troll Street Urchin", and `current`
/// their current health and stamina.
pub fn score_text(name: &str, who: &str, attributes: &Attributes, current: (i32, i32)) -> String {
    let row = |label: &str, list: &[Attribute]| {
        let cells: Vec<_> = list
            .iter()
            .map(|a| format!("{:<10}{:>4}", a.name(), attributes.get(*a)))
            .collect();
        format!("  {label:<10}{}", cells.join("    "))
    };
    use Attribute::*;
    // Casting skills don't exist yet, so any Magic is still latent.
    let magic = if attributes.get(Magic) > 0 {
        "  (latent: no casting skill)"
    } else {
        ""
    };
    let karma = attributes.karma_unspent();
    let mut text = format!(
        "\x1b[1m{name}\x1b[0m, {who}\r\n{}\r\n{}\r\n{}{magic}\r\n\r\n  Health {}/{}    Stamina {}/{}    Initiative {}\r\n  Karma: {karma} unspent",
        row("Physical", &[Body, Agility, Reaction, Strength]),
        row("Mental", &[Willpower, Logic, Intuition, Charisma]),
        row("Special", &[Luck, Magic]),
        current.0,
        attributes.health(),
        current.1,
        attributes.stamina(),
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
