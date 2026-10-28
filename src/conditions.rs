//! Conditions: the small shared language for "who qualifies", used by background gates now, and by
//! skill requirements, hidden-skill unlocks, quests and trainers later (see `docs/skills.md`).

use serde::{Deserialize, Serialize};

use crate::attributes::{Attribute, Attributes};
use crate::skills::Skills;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Condition {
    /// An attribute is at least this.
    Stat(Attribute, i32),
    /// The character is of this race.
    Race(String),
    /// The character knows this skill (`tree:skill`) at this level or higher.
    Skill(String, u32),
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Not(Box<Condition>),
}

/// What conditions are tested against.
pub struct Facts<'a> {
    pub attributes: &'a Attributes,
    pub race: &'a str,
    /// Their skills, if they have any yet; a character being created has none.
    pub skills: Option<&'a Skills>,
}

impl Condition {
    pub fn met(&self, facts: &Facts) -> bool {
        match self {
            Condition::Stat(attribute, min) => facts.attributes.get(*attribute) >= *min,
            Condition::Race(race) => facts.race == race,
            Condition::Skill(skill, level) => facts
                .skills
                .and_then(|skills| skills.0.get(skill))
                .is_some_and(|state| state.level >= *level),
            Condition::All(all) => all.iter().all(|c| c.met(facts)),
            Condition::Any(any) => any.iter().any(|c| c.met(facts)),
            Condition::Not(not) => !not.met(facts),
        }
    }

    /// For players: "Body 4 and Logic 4", "Lizardfolk". `race_name` turns a race id into its name.
    pub fn describe(&self, race_name: &dyn Fn(&str) -> String) -> String {
        let join = |list: &[Condition], word: &str| {
            let parts: Vec<String> = list.iter().map(|c| c.describe(race_name)).collect();
            match parts.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [rest @ .., last] => format!("{} {word} {last}", rest.join(", ")),
            }
        };
        match self {
            Condition::Stat(attribute, min) => format!("{} {min}", attribute.name()),
            Condition::Race(race) => race_name(race),
            Condition::Skill(skill, level) => format!("{skill} {level}"),
            Condition::All(all) => join(all, "and"),
            Condition::Any(any) => join(any, "or"),
            Condition::Not(not) => format!("not {}", not.describe(race_name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_test_and_read_naturally() {
        let mut attributes = Attributes::default();
        attributes.set(Attribute::Magic, 1);
        let facts = Facts {
            attributes: &attributes,
            race: "elf",
            skills: None,
        };
        let acolyte = Condition::All(vec![
            Condition::Stat(Attribute::Magic, 1),
            Condition::Stat(Attribute::Willpower, 4),
        ]);
        assert!(!acolyte.met(&facts));
        attributes.set(Attribute::Willpower, 4);
        let facts = Facts {
            attributes: &attributes,
            race: "elf",
            skills: None,
        };
        assert!(acolyte.met(&facts));
        assert!(!Condition::Race("lizardfolk".into()).met(&facts));

        let name = |id: &str| match id {
            "lizardfolk" => "Lizardfolk".to_string(),
            other => other.to_string(),
        };
        assert_eq!(acolyte.describe(&name), "Magic 1 and Willpower 4");
        assert_eq!(
            Condition::Race("lizardfolk".into()).describe(&name),
            "Lizardfolk"
        );
    }
}
