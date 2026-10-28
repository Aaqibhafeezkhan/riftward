//! Requirements: the small shared language for "who qualifies", wherever data says `requires:`
//! (backgrounds, skills, trainers, discovery) and, as it grows, quests (see `docs/facts.md`).
//! Not to be confused with conditions, which are what's on a body, like poisoned (`combat`).

use serde::{Deserialize, Serialize};

use crate::attributes::{Attribute, Attributes};
use crate::facts::{Facts, Owner, Value};
use crate::skills::Skills;
use crate::time::unix_now;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Requirement {
    /// An attribute is at least this.
    Stat(Attribute, i32),
    /// The character is of this race.
    Race(String),
    /// The character knows this skill (`tree:skill`) at this level or higher.
    Skill(String, u32),
    /// The character shows this guild rank (`bone`, `silver`...) or higher: a member, wearing
    /// their tag (see `guild.rs`).
    GuildRank(String),
    /// A fact compares: `Fact("char.trust.marin", Gte, 3)`. The key starts with its scope:
    /// `world.`, `char.` (the character's own), or `npc.<zone:mob>.` (see `facts.rs`). A fact
    /// never set is false, zero or empty.
    Fact(String, Cmp, Value),
    /// A fact is set at all (true, a number but zero, a word): `Flag("world.rats_broke_out")`.
    Flag(String),
    All(Vec<Requirement>),
    Any(Vec<Requirement>),
    Not(Box<Requirement>),
}

/// How a fact is compared.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

/// Who a requirement is tested against, and what's known of them.
pub struct Subject<'a> {
    pub attributes: &'a Attributes,
    pub race: &'a str,
    /// Their skills, if they have any yet; a character being created has none.
    pub skills: Option<&'a Skills>,
    /// The guild rank they show, by its place among the ranks: a member wearing their tag.
    pub guild_rank: Option<usize>,
    /// The fact store, for `Fact` and `Flag`, and whose `char.` facts are theirs. Without the
    /// store, no fact is set.
    pub story: Option<&'a Facts>,
    pub character: Option<i64>,
}

impl Subject<'_> {
    /// The value of `key` (`scope.rest`) for them: unset if there's no store, no such scope, or
    /// no character for a `char.` fact. Quests' own facts come with quests.
    fn fact(&self, key: &str) -> Option<&Value> {
        let story = self.story?;
        let (scope, rest) = key.split_once('.')?;
        let (owner, key) = match scope {
            "world" => (Owner::World, rest),
            "char" => (Owner::Character(self.character?), rest),
            "npc" => {
                let (mob, key) = rest.split_once('.')?;
                (Owner::Npc(mob.to_string()), key)
            }
            _ => return None,
        };
        story.get(&owner, key, unix_now())
    }
}

/// Whether `have` compares to `want` by `op`. Numbers compare as numbers, and anything else only
/// for being equal or not; a fact never set is false, zero or empty, as `want` is.
fn compare(have: Option<&Value>, op: Cmp, want: &Value) -> bool {
    let unset = match want {
        Value::Bool(_) => Value::Bool(false),
        Value::Int(_) => Value::Int(0),
        Value::Word(_) => Value::Word(String::new()),
    };
    let have = have.unwrap_or(&unset);
    match (have, want) {
        (Value::Int(a), Value::Int(b)) => match op {
            Cmp::Eq => a == b,
            Cmp::Ne => a != b,
            Cmp::Gt => a > b,
            Cmp::Gte => a >= b,
            Cmp::Lt => a < b,
            Cmp::Lte => a <= b,
        },
        _ => match op {
            Cmp::Eq => have == want,
            Cmp::Ne => have != want,
            _ => false,
        },
    }
}

impl Requirement {
    pub fn met(&self, facts: &Subject) -> bool {
        match self {
            Requirement::Stat(attribute, min) => facts.attributes.get(*attribute) >= *min,
            Requirement::Race(race) => facts.race == race,
            Requirement::Skill(skill, level) => facts
                .skills
                .and_then(|skills| skills.0.get(skill))
                .is_some_and(|state| state.level >= *level),
            Requirement::GuildRank(rank) => facts
                .guild_rank
                .zip(crate::guild::guild().rank(rank))
                .is_some_and(|(shown, needed)| shown >= needed),
            Requirement::Fact(key, op, value) => compare(facts.fact(key), *op, value),
            Requirement::Flag(key) => facts.fact(key).is_some(),
            Requirement::All(all) => all.iter().all(|c| c.met(facts)),
            Requirement::Any(any) => any.iter().any(|c| c.met(facts)),
            Requirement::Not(not) => !not.met(facts),
        }
    }

    /// For players: "Body 4 and Logic 4", "Lizardfolk". `race_name` turns a race id into its name.
    pub fn describe(&self, race_name: &dyn Fn(&str) -> String) -> String {
        let join = |list: &[Requirement], word: &str| {
            let parts: Vec<String> = list.iter().map(|c| c.describe(race_name)).collect();
            match parts.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [rest @ .., last] => format!("{} {word} {last}", rest.join(", ")),
            }
        };
        match self {
            Requirement::Stat(attribute, min) => format!("{} {min}", attribute.name()),
            Requirement::Race(race) => race_name(race),
            Requirement::Skill(skill, level) => format!("{skill} {level}"),
            Requirement::GuildRank(rank) => {
                let guild = crate::guild::guild();
                let name = guild
                    .rank(rank)
                    .map_or(rank.as_str(), |i| guild.ranks[i].name.as_str());
                format!("a {name} guild tag")
            }
            // What a fact is about, without its scope: "sold out the guild".
            Requirement::Fact(key, ..) | Requirement::Flag(key) => key
                .split_once('.')
                .map_or(key.as_str(), |(_, rest)| rest)
                .replace(['_', '.'], " "),
            Requirement::All(all) => join(all, "and"),
            Requirement::Any(any) => join(any, "or"),
            Requirement::Not(not) => format!("not {}", not.describe(race_name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_test_and_read_naturally() {
        let mut attributes = Attributes::default();
        attributes.set(Attribute::Magic, 1);
        let facts = Subject {
            attributes: &attributes,
            race: "elf",
            skills: None,
            guild_rank: None,
            story: None,
            character: None,
        };
        let acolyte = Requirement::All(vec![
            Requirement::Stat(Attribute::Magic, 1),
            Requirement::Stat(Attribute::Willpower, 4),
        ]);
        assert!(!acolyte.met(&facts));
        attributes.set(Attribute::Willpower, 4);
        let facts = Subject {
            attributes: &attributes,
            race: "elf",
            skills: None,
            guild_rank: None,
            story: None,
            character: None,
        };
        assert!(acolyte.met(&facts));
        assert!(!Requirement::Race("lizardfolk".into()).met(&facts));

        let name = |id: &str| match id {
            "lizardfolk" => "Lizardfolk".to_string(),
            other => other.to_string(),
        };
        assert_eq!(acolyte.describe(&name), "Magic 1 and Willpower 4");
        assert_eq!(
            Requirement::Race("lizardfolk".into()).describe(&name),
            "Lizardfolk"
        );
    }

    #[test]
    fn facts_are_tested_by_scope_and_compared() {
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let ada = db.create_character(account.id, "Ada").unwrap().id;
        let mut story = Facts::default();
        story.set(
            &Owner::World,
            "rats_broke_out",
            Value::Bool(true),
            None,
            &db,
        );
        story.set(
            &Owner::Character(ada),
            "trust.marin",
            Value::Int(3),
            None,
            &db,
        );
        story.set(
            &Owner::Npc("riftwatch:contract_clerk".into()),
            "mood",
            Value::Word("cold".into()),
            None,
            &db,
        );
        let attributes = Attributes::default();
        let who = |character: Option<i64>| Subject {
            attributes: &attributes,
            race: "human",
            skills: None,
            guild_rank: None,
            story: Some(&story),
            character,
        };
        let met = |text: &str, character: Option<i64>| {
            crate::world::ron_options()
                .from_str::<Requirement>(text)
                .unwrap()
                .met(&who(character))
        };
        assert!(met(r#"Flag("world.rats_broke_out")"#, None));
        assert!(!met(r#"Flag("world.slime_lord_slain")"#, None), "never set");
        assert!(met(r#"Fact("char.trust.marin", Gte, 3)"#, Some(ada)));
        assert!(!met(r#"Fact("char.trust.marin", Gt, 3)"#, Some(ada)));
        assert!(
            !met(r#"Fact("char.trust.marin", Gte, 3)"#, Some(ada + 1)),
            "someone else's"
        );
        assert!(
            met(r#"Fact("char.trust.bob", Eq, 0)"#, Some(ada)),
            "unset is zero"
        );
        assert!(met(
            r#"Fact("npc.riftwatch:contract_clerk.mood", Eq, "cold")"#,
            None
        ));
        assert!(!met(r#"Flag("nowhere.at_all")"#, None), "no such scope");
        assert_eq!(
            Requirement::Flag("char.sold_out_the_guild".into()).describe(&|id| id.into()),
            "sold out the guild"
        );
    }

    #[test]
    fn a_guild_rank_is_met_by_that_rank_or_higher_shown() {
        let attributes = Attributes::default();
        let shows = |rank: Option<usize>| Subject {
            attributes: &attributes,
            race: "human",
            skills: None,
            guild_rank: rank,
            story: None,
            character: None,
        };
        let silver = Requirement::GuildRank("silver".into());
        assert!(!silver.met(&shows(None)), "no tag, no rank");
        assert!(!silver.met(&shows(Some(2))), "Iron");
        assert!(silver.met(&shows(Some(3))));
        assert!(silver.met(&shows(Some(5))), "Platinum");
        assert_eq!(silver.describe(&|id| id.to_string()), "a Silver guild tag");
    }
}
