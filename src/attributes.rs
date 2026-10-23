//! Character attributes and the Karma spent to raise them.
//!
//! Attributes are uncapped. Every character starts at 1 (Magic at 0, until they Awaken) with
//! `STARTING_KARMA` to spend. Karma spent is not stored: it is worked out from the current
//! ratings, so it can never drift out of sync with them.

use bevy::prelude::*;

use crate::db::Database;

/// Karma every character starts with, to spend on attributes.
pub const STARTING_KARMA: i32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

    /// The rating every new character starts with.
    pub fn starting_rating(self) -> i32 {
        match self {
            Attribute::Magic => 0,
            _ => 1,
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
                many.iter().map(|a| a.name()).collect::<Vec<_>>().join(" or ")
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

/// A character's attribute ratings. Lives on the player entity.
#[derive(Component, Clone, Debug)]
pub struct Attributes([i32; 10]);

impl Default for Attributes {
    fn default() -> Self {
        Self(Attribute::ALL.map(Attribute::starting_rating))
    }
}

impl Attributes {
    /// Build from database rows; attributes with no row keep their starting rating.
    pub fn from_rows(rows: impl IntoIterator<Item = (String, i32)>) -> Self {
        let mut attributes = Self::default();
        for (name, rating) in rows {
            match Attribute::ALL.into_iter().find(|a| a.name() == name) {
                Some(attribute) => attributes.set(attribute, rating),
                None => warn!("ignoring unknown attribute '{name}' in the database"),
            }
        }
        attributes
    }

    pub fn get(&self, attribute: Attribute) -> i32 {
        self.0[attribute.index()]
    }

    pub fn set(&mut self, attribute: Attribute, rating: i32) {
        self.0[attribute.index()] = rating;
    }

    /// Total Karma spent to reach the current ratings. Magic counts from 1, since Awakening grants
    /// the first point.
    pub fn karma_spent(&self) -> i32 {
        Attribute::ALL
            .into_iter()
            .map(|attribute| {
                let from = attribute.starting_rating().max(1);
                (from..self.get(attribute)).map(raise_cost).sum::<i32>()
            })
            .sum()
    }

    /// Karma available to spend. Earned Karma will be added once skills grant character levels.
    pub fn karma_unspent(&self) -> i32 {
        (STARTING_KARMA - self.karma_spent()).max(0)
    }

    pub fn health(&self) -> i32 {
        8 + self.get(Attribute::Body) / 2
    }

    pub fn fatigue(&self) -> i32 {
        8 + self.get(Attribute::Willpower) / 2
    }

    pub fn initiative(&self) -> i32 {
        self.get(Attribute::Reaction) + self.get(Attribute::Intuition)
    }
}

/// The `score` command.
pub fn score_text(name: &str, attributes: &Attributes) -> String {
    let row = |label: &str, list: &[Attribute]| {
        let cells: Vec<_> = list
            .iter()
            .map(|a| format!("{:<10}{:>4}", a.name(), attributes.get(*a)))
            .collect();
        format!("  {label:<10}{}", cells.join("    "))
    };
    use Attribute::*;
    let magic = if attributes.get(Magic) > 0 {
        String::new()
    } else {
        "  (not Awakened)".into()
    };
    let karma = attributes.karma_unspent();
    let mut text = format!(
        "\x1b[1m{name}\x1b[0m\r\n{}\r\n{}\r\n{}{magic}\r\n\r\n  Health {}    Fatigue {}    Initiative {}\r\n  Karma: {karma} unspent",
        row("Physical", &[Body, Agility, Reaction, Strength]),
        row("Mental", &[Willpower, Logic, Intuition, Charisma]),
        row("Special", &[Luck, Magic]),
        attributes.health(),
        attributes.fatigue(),
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
    if attribute == Attribute::Magic && current == 0 {
        return "You have not Awakened. Magic cannot be raised until you do.".into();
    }

    let cost = raise_cost(current);
    let unspent = attributes.karma_unspent();
    if cost > unspent {
        return format!(
            "Raising {} to {} costs {cost} Karma, and you have {unspent}.",
            attribute.name(),
            current + 1
        );
    }

    if let Err(err) = db.set_attribute(character_id, attribute.name(), current + 1) {
        error!("failed to save attribute for character {character_id}: {err}");
        return "Something went wrong saving that. Please try again.".into();
    }
    attributes.set(attribute, current + 1);
    format!(
        "{} raised to {} for {cost} Karma. {} Karma left.",
        attribute.name(),
        current + 1,
        attributes.karma_unspent()
    )
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

        attributes.set(Attribute::Agility, 5); // 1 -> 5: four raises at 1 each
        assert_eq!(attributes.karma_spent(), 4);
        attributes.set(Attribute::Agility, 7); // 5 -> 7: two more at 2 each
        assert_eq!(attributes.karma_spent(), 8);

        attributes.set(Attribute::Magic, 1); // Awakening's free point costs nothing
        assert_eq!(attributes.karma_spent(), 8);
        attributes.set(Attribute::Magic, 2);
        assert_eq!(attributes.karma_spent(), 9);
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
        let attributes = Attributes::from_rows([("Body".to_string(), 4), ("Nonsense".into(), 9)]);
        assert_eq!(attributes.get(Attribute::Body), 4);
        assert_eq!(attributes.get(Attribute::Agility), 1);
        assert_eq!(attributes.get(Attribute::Magic), 0);
    }
}
