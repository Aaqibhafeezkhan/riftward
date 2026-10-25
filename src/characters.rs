//! Races and backgrounds: the choices that decide where a new character starts.
//!
//! Both are data, in `assets/races.ron` and `assets/backgrounds.ron`, read at startup. A race shifts
//! the base attributes, Earthdawn-style, and grants racial skills; humans get extra Karma instead
//! of attribute shifts. A background grants a few starting skills, and can be gated by a
//! [`Condition`]. Skills don't exist yet, so the grants are recorded for when they do.

use std::collections::BTreeMap;

use bevy::asset::io::file::FileAssetReader;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attributes::{Attribute, Attributes, STARTING_KARMA};
use crate::conditions::{Condition, Facts};
use crate::world::ron_options;

pub struct CharactersPlugin;

impl Plugin for CharactersPlugin {
    fn build(&self, app: &mut App) {
        let dir = FileAssetReader::get_base_path().join("assets");
        let races = load::<RaceDef>(&dir.join("races.ron"));
        let backgrounds = load::<BackgroundDef>(&dir.join("backgrounds.ron"));
        info!(
            "loaded {} races and {} backgrounds",
            races.len(),
            backgrounds.len()
        );
        app.insert_resource(Races(races))
            .insert_resource(Backgrounds(backgrounds));
    }
}

fn load<T: for<'de> Deserialize<'de>>(path: &std::path::Path) -> Vec<T> {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("can't read {}: {err}", path.display()));
    ron_options()
        .from_str(&text)
        .unwrap_or_else(|err| panic!("can't parse {}: {err}", path.display()))
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct RaceDef {
    pub id: String,
    pub name: String,
    /// One line for the creation menu.
    pub summary: String,
    /// How it shifts the base attributes.
    #[serde(default)]
    pub attributes: BTreeMap<Attribute, i32>,
    /// Extra starting Karma.
    #[serde(default)]
    pub karma: i32,
    /// Racial skills, granted once skills exist.
    #[serde(default)]
    pub skills: Vec<String>,
}

impl RaceDef {
    /// A new character's attributes as this race: the base ratings shifted, never below 1 (or 0
    /// for Magic), with the starting Karma plus any racial bonus.
    pub fn starting_attributes(&self) -> Attributes {
        Attributes::starting(
            |attribute| {
                let floor = attribute.base_rating().min(1);
                let shift = self.attributes.get(&attribute).copied().unwrap_or(0);
                (attribute.base_rating() + shift).max(floor)
            },
            STARTING_KARMA + self.karma,
        )
    }

    /// "+3 Strength, +2 Body, -1 Agility", or the Karma bonus for races without shifts.
    pub fn shifts(&self) -> String {
        let mut shifts: Vec<(&Attribute, &i32)> =
            self.attributes.iter().filter(|(_, n)| **n != 0).collect();
        shifts.sort_by_key(|(attribute, n)| (-**n, **attribute));
        let mut parts: Vec<String> = shifts
            .into_iter()
            .map(|(attribute, n)| format!("{n:+} {}", attribute.name()))
            .collect();
        if self.karma != 0 {
            parts.push(format!("{:+} Karma", self.karma));
        }
        parts.join(", ")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct BackgroundDef {
    pub id: String,
    pub name: String,
    /// One line for the creation menu.
    pub summary: String,
    /// Who can choose it.
    #[serde(default)]
    pub requires: Option<Condition>,
    /// Starting skills, granted once skills exist.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Every race, in menu order.
#[derive(Resource, Default)]
pub struct Races(pub Vec<RaceDef>);

impl Races {
    pub fn get(&self, id: &str) -> Option<&RaceDef> {
        self.0.iter().find(|race| race.id == id)
    }

    /// A race's name from its id, for conditions and menus.
    pub fn name(&self, id: &str) -> String {
        self.get(id)
            .map_or_else(|| id.to_string(), |r| r.name.clone())
    }

    /// The race chosen by menu number, id or name.
    pub fn choose(&self, input: &str) -> Option<&RaceDef> {
        choose(&self.0, input, |r| (&r.id, &r.name))
    }
}

/// Every background, in menu order.
#[derive(Resource, Default)]
pub struct Backgrounds(pub Vec<BackgroundDef>);

impl Backgrounds {
    pub fn get(&self, id: &str) -> Option<&BackgroundDef> {
        self.0.iter().find(|background| background.id == id)
    }

    /// The background chosen by menu number, id or name.
    pub fn choose(&self, input: &str) -> Option<&BackgroundDef> {
        choose(&self.0, input, |b| (&b.id, &b.name))
    }
}

/// An entry picked by its number in the menu, its id, or the start of its name.
fn choose<'a, T>(
    list: &'a [T],
    input: &str,
    keys: impl Fn(&T) -> (&String, &String),
) -> Option<&'a T> {
    let input = input.trim().to_lowercase();
    if input.is_empty() {
        return None;
    }
    if let Ok(n) = input.parse::<usize>() {
        return n.checked_sub(1).and_then(|i| list.get(i));
    }
    let squash = |s: &str| s.to_lowercase().replace(['\'', ' ', '_'], "");
    list.iter().find(|item| keys(item).0 == &input).or_else(|| {
        let mut matches = list
            .iter()
            .filter(|item| squash(keys(item).1).starts_with(&squash(&input)));
        match (matches.next(), matches.next()) {
            (Some(one), None) => Some(one),
            _ => None,
        }
    })
}

/// The creation menu of races.
pub fn race_menu(races: &Races) -> String {
    let mut text = String::from("\r\nChoose your race:\r\n");
    for (i, race) in races.0.iter().enumerate() {
        text += &format!(
            "  {:>2}. \x1b[1m{:<10}\x1b[0m {}\r\n      \x1b[2m{}\x1b[0m\r\n",
            i + 1,
            race.name,
            race.summary,
            race.shifts()
        );
    }
    text + "Enter a number or name: "
}

/// The attribute sheet while spending starting Karma.
pub fn karma_sheet(race: &RaceDef, attributes: &Attributes) -> String {
    let mut text = format!(
        "\r\nSpend your starting Karma. You are {} {}.\r\n",
        article(&race.name),
        race.name
    );
    for row in [
        [
            Attribute::Body,
            Attribute::Agility,
            Attribute::Reaction,
            Attribute::Strength,
        ],
        [
            Attribute::Willpower,
            Attribute::Logic,
            Attribute::Intuition,
            Attribute::Charisma,
        ],
    ] {
        let cells: Vec<String> = row
            .iter()
            .map(|a| format!("{:<10}{:>3}", a.name(), attributes.get(*a)))
            .collect();
        text += &format!("  {}\r\n", cells.join("    "));
    }
    text += &format!(
        "  {:<10}{:>3}    {:<10}{:>3}  (latent until you learn to cast)\r\n",
        "Luck",
        attributes.get(Attribute::Luck),
        "Magic",
        attributes.get(Attribute::Magic)
    );
    text += &format!(
        "Karma: {} to spend. Raising costs 1 a point, 2 from 5, 3 from 10.\r\n\
         Type 'raise <attribute>', 'lower <attribute>', 'done' when finished, or 'back' to choose \
         another race.\r\n> ",
        attributes.karma_unspent()
    );
    text
}

/// The creation menu of backgrounds, showing which this character qualifies for.
pub fn background_menu(
    backgrounds: &Backgrounds,
    races: &Races,
    race: &str,
    attributes: &Attributes,
) -> String {
    let facts = Facts { attributes, race };
    let mut text = String::from("\r\nChoose your background, the life you led before:\r\n");
    for (i, background) in backgrounds.0.iter().enumerate() {
        let open = background.requires.as_ref().is_none_or(|c| c.met(&facts));
        let needs = background
            .requires
            .as_ref()
            .map(|c| format!(" (requires {})", c.describe(&|id| races.name(id))))
            .unwrap_or_default();
        let (on, off) = if open {
            ("\x1b[1m", "\x1b[0m")
        } else {
            ("\x1b[2m", "\x1b[0m")
        };
        text += &format!(
            "  {on}{:>2}. {:<23}{off} {}{}\r\n",
            i + 1,
            background.name,
            background.summary,
            if open { String::new() } else { needs }
        );
    }
    text + "Dimmed backgrounds need more than you have; type 'back' to change your attributes.\r\n\
            Enter a number or name: "
}

/// Why this character can't take the background, if they can't.
pub fn background_refusal(
    background: &BackgroundDef,
    races: &Races,
    race: &str,
    attributes: &Attributes,
) -> Option<String> {
    let facts = Facts { attributes, race };
    let requires = background.requires.as_ref()?;
    (!requires.met(&facts)).then(|| {
        format!(
            "{} requires {}. Type 'back' to change your attributes, or choose another: ",
            background.name,
            requires.describe(&|id| races.name(id))
        )
    })
}

/// "a" or "an".
pub fn article(word: &str) -> &'static str {
    match word.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read<T: for<'de> Deserialize<'de>>(file: &str) -> Vec<T> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(file);
        load(&path)
    }

    /// The data files parse, ids are unique, races come out even, and every background is open to
    /// someone who spends their starting Karma on its gate.
    #[test]
    fn race_and_background_files_are_consistent() {
        let races = Races(read::<RaceDef>("races.ron"));
        let backgrounds = Backgrounds(read::<BackgroundDef>("backgrounds.ron"));
        let mut problems = Vec::new();

        let mut ids: Vec<&str> = races.0.iter().map(|r| r.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != races.0.len() {
            problems.push("duplicate race ids".to_string());
        }
        for race in &races.0 {
            // Each race's shifts plus its extra Karma come to the same total.
            let total: i32 = race.attributes.values().sum::<i32>() + race.karma;
            if total != 3 {
                problems.push(format!(
                    "{}: shifts and Karma total {total}, not 3",
                    race.id
                ));
            }
        }

        let mut ids: Vec<&str> = backgrounds.0.iter().map(|b| b.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        if ids.len() != backgrounds.0.len() {
            problems.push("duplicate background ids".to_string());
        }
        for background in &backgrounds.0 {
            // Someone, of some race, can afford it from their starting Karma.
            let reachable = races.0.iter().any(|race| {
                let mut attributes = race.starting_attributes();
                let needs = background.requires.clone();
                let mut stats = Vec::new();
                collect_stats(needs.as_ref(), &mut stats);
                for (attribute, min) in stats {
                    while attributes.get(attribute) < min {
                        if attributes.raise(attribute).is_err() {
                            break;
                        }
                    }
                }
                let facts = Facts {
                    attributes: &attributes,
                    race: &race.id,
                };
                needs.is_none_or(|c| c.met(&facts))
            });
            if !reachable {
                problems.push(format!(
                    "{}: no starting character can take it",
                    background.id
                ));
            }
        }
        assert!(problems.is_empty(), "problems:\n{}", problems.join("\n"));
    }

    fn collect_stats(condition: Option<&Condition>, out: &mut Vec<(Attribute, i32)>) {
        match condition {
            Some(Condition::Stat(attribute, min)) => out.push((*attribute, *min)),
            Some(Condition::All(list) | Condition::Any(list)) => {
                list.iter().for_each(|c| collect_stats(Some(c), out))
            }
            _ => {}
        }
    }

    #[test]
    fn races_shift_attributes_but_not_below_one() {
        let windling = RaceDef {
            id: "windling".into(),
            name: "Windling".into(),
            summary: String::new(),
            attributes: BTreeMap::from([(Attribute::Strength, -5), (Attribute::Agility, 3)]),
            karma: 0,
            skills: Vec::new(),
        };
        let attributes = windling.starting_attributes();
        assert_eq!(attributes.get(Attribute::Strength), 1);
        assert_eq!(attributes.get(Attribute::Agility), 6);
        assert_eq!(attributes.get(Attribute::Body), 3);
        assert_eq!(attributes.karma_spent(), 0);
        assert_eq!(windling.shifts(), "+3 Agility, -5 Strength");
    }

    #[test]
    fn menu_choices_by_number_id_or_name() {
        let races = Races(read::<RaceDef>("races.ron"));
        assert_eq!(
            races.choose("1").map(|r| r.id.as_str()),
            Some(races.0[0].id.as_str())
        );
        assert_eq!(
            races.choose("tskrang").map(|r| r.id.as_str()),
            Some("tskrang")
        );
        assert_eq!(races.choose("T'sk").map(|r| r.id.as_str()), Some("tskrang"));
        assert_eq!(races.choose("tro").map(|r| r.id.as_str()), Some("troll"));
        assert_eq!(races.choose("99"), None);
        assert_eq!(races.choose(""), None);
    }
}
