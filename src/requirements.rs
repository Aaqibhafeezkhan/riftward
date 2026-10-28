//! Requirements: the small shared language for "who qualifies" and "when", wherever data says
//! `requires:` (backgrounds, skills, trainers, discovery) or `when:` (world events, gated
//! spawns), and, as it grows, quests (see `docs/facts.md`). Not to be confused with conditions,
//! which are what's on a body, like poisoned (`combat`).

use serde::{Deserialize, Serialize};

use crate::attributes::{Attribute, Attributes};
use crate::combat::Conditions;
use crate::facts::{Facts, Owner, Value};
use crate::skills::Skills;
use crate::time::unix_now;
use crate::world::{ItemKey, MobKey, RoomKey};

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
    /// A status condition or trait is on the character: `Has("poisoned")`, `Has("fey_mind")`.
    Has(String),
    /// The character carries at least this many of an item, or of items with a tag:
    /// `HasItem(Item("sewers:rat_tail"), 3)`, `HasItem(Tag("herb"), 5)`.
    HasItem(ItemMatch, u32),
    /// The character is in this room (`zone:room`, or `room` in the zone it's written in).
    InRoom(String),
    /// The character is in this zone.
    InZone(String),
    /// A world flag is set: `WorldFlag("rats_broke_out")` is `Flag("world.rats_broke_out")`.
    WorldFlag(String),
    /// Something just happened: only true while that happening is being handled, as world
    /// events are. Mob ids are `mob` or `zone:mob`, relative to where it's written.
    Event(Happening),
    All(Vec<Requirement>),
    Any(Vec<Requirement>),
    Not(Box<Requirement>),
}

/// Which items `HasItem` counts: one template (`zone:item`), or anything with a tag.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ItemMatch {
    Item(String),
    Tag(String),
}

/// Something that happens in the world, for `Requirement::Event`. Mob ids are `mob` or
/// `zone:mob`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Happening {
    /// A mob made from this template walked into a room in `zone`.
    Entered { mob: String, zone: String },
    /// A mob made from this template died.
    Died(String),
}

/// What actually happened, resolved to full ids.
#[derive(Debug, Clone, PartialEq)]
pub enum Occurred {
    Entered { mob: MobKey, zone: String },
    Died { mob: MobKey },
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

/// The character a requirement is tested against, and what's known of them.
pub struct Who<'a> {
    pub attributes: &'a Attributes,
    pub race: &'a str,
    /// Their skills, if they have any yet; a character being created has none.
    pub skills: Option<&'a Skills>,
    /// The guild rank they show, by its place among the ranks: a member wearing their tag.
    pub guild_rank: Option<usize>,
    /// Their saved character, whose `char.` facts are theirs.
    pub character: Option<i64>,
    /// What's on them, for `Has`.
    pub conditions: Option<&'a Conditions>,
    /// Where they are, for `InRoom` and `InZone`.
    pub room: Option<&'a RoomKey>,
    /// What they carry (each item's template and tags, from `items::carried`), for `HasItem`.
    pub carrying: Option<&'a [(ItemKey, Vec<String>)]>,
}

/// What requirements need to know about someone besides their sheet: where they are, what
/// they carry, what's on them, the rank they show, and the fact store.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Surroundings<'w, 's> {
    rooms: crate::output::RoomQuery<'w, 's>,
    places: bevy::prelude::Query<'w, 's, &'static crate::world::InRoom>,
    items: crate::items::ItemQuery<'w, 's>,
    templates: bevy::prelude::Res<'w, crate::items::ItemTemplates>,
    pub conditions: bevy::prelude::Query<'w, 's, &'static Conditions>,
    shown: bevy::prelude::Query<'w, 's, &'static crate::guild::ShownRank>,
    pub story: bevy::prelude::Res<'w, Facts>,
}

/// What `Surroundings` found out about someone, for a `Who` to borrow.
pub struct Known {
    pub room: Option<RoomKey>,
    pub carrying: Vec<(ItemKey, Vec<String>)>,
    pub guild_rank: Option<usize>,
}

impl Surroundings<'_, '_> {
    pub fn of(&self, who: bevy::prelude::Entity) -> Known {
        Known {
            room: self
                .places
                .get(who)
                .ok()
                .and_then(|r| self.rooms.get(r.0).ok())
                .map(|(_, key, _)| key.clone()),
            carrying: crate::items::carried(who, &self.items, &self.templates),
            guild_rank: crate::guild::shown_rank(who, &self.shown),
        }
    }
}

/// What a requirement is tested against: someone, if anyone (a world event has no one, and
/// whatever asks about a character is false there); the fact store (without it, no fact is set);
/// the zone it was written in, for relative ids; and what just happened, if anything.
pub struct Subject<'a> {
    pub who: Option<Who<'a>>,
    pub story: Option<&'a Facts>,
    pub zone: &'a str,
    pub happened: Option<&'a Occurred>,
}

impl<'a> Subject<'a> {
    /// A character, with the fact store if there is one.
    pub fn of(who: Who<'a>, story: Option<&'a Facts>) -> Self {
        Self {
            who: Some(who),
            story,
            zone: "",
            happened: None,
        }
    }

    /// No one: the world, in zone `zone`, given the facts and what (if anything) just happened.
    pub fn world(story: &'a Facts, zone: &'a str, happened: Option<&'a Occurred>) -> Self {
        Self {
            who: None,
            story: Some(story),
            zone,
            happened,
        }
    }

    /// The value of `key` (`scope.rest`) for them: unset if there's no store, no such scope, or
    /// no character for a `char.` fact. Quests' own facts come with quests.
    fn fact(&self, key: &str) -> Option<&Value> {
        let story = self.story?;
        let (scope, rest) = key.split_once('.')?;
        let (owner, key) = match scope {
            "world" => (Owner::World, rest),
            "char" => (Owner::Character(self.who.as_ref()?.character?), rest),
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
        let who = facts.who.as_ref();
        match self {
            Requirement::Stat(attribute, min) => {
                who.is_some_and(|w| w.attributes.get(*attribute) >= *min)
            }
            Requirement::Race(race) => who.is_some_and(|w| w.race == race),
            Requirement::Skill(skill, level) => who
                .and_then(|w| w.skills)
                .and_then(|skills| skills.0.get(skill))
                .is_some_and(|state| state.level >= *level),
            Requirement::GuildRank(rank) => who
                .and_then(|w| w.guild_rank)
                .zip(crate::guild::guild().rank(rank))
                .is_some_and(|(shown, needed)| shown >= needed),
            Requirement::Fact(key, op, value) => compare(facts.fact(key), *op, value),
            Requirement::Flag(key) => facts.fact(key).is_some(),
            Requirement::Has(condition) => who
                .and_then(|w| w.conditions)
                .is_some_and(|c| c.has(condition)),
            Requirement::HasItem(which, n) => {
                let count = who.and_then(|w| w.carrying).map_or(0, |carrying| {
                    carrying
                        .iter()
                        .filter(|(key, tags)| match which {
                            ItemMatch::Item(id) => ItemKey::parse(id, facts.zone) == *key,
                            ItemMatch::Tag(tag) => tags.contains(tag),
                        })
                        .count()
                });
                count >= *n as usize
            }
            Requirement::InRoom(room) => who
                .and_then(|w| w.room)
                .is_some_and(|at| RoomKey::parse(room, facts.zone) == *at),
            Requirement::InZone(zone) => {
                who.and_then(|w| w.room).is_some_and(|at| at.zone == *zone)
            }
            Requirement::WorldFlag(flag) => facts.fact(&format!("world.{flag}")).is_some(),
            Requirement::Event(happening) => match (happening, facts.happened) {
                (
                    Happening::Entered { mob, zone },
                    Some(Occurred::Entered { mob: m, zone: at }),
                ) => MobKey::parse(mob, facts.zone) == *m && zone == at,
                (Happening::Died(mob), Some(Occurred::Died { mob: m })) => {
                    MobKey::parse(mob, facts.zone) == *m
                }
                _ => false,
            },
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
            Requirement::WorldFlag(flag) | Requirement::Has(flag) => flag.replace('_', " "),
            Requirement::HasItem(which, n) => {
                let (ItemMatch::Item(name) | ItemMatch::Tag(name)) = which;
                let name = name.rsplit(':').next().unwrap_or(name).replace('_', " ");
                format!("{n} {name}")
            }
            Requirement::InRoom(place) | Requirement::InZone(place) => {
                format!(
                    "being in {}",
                    place.rsplit(':').next().unwrap_or(place).replace('_', " ")
                )
            }
            Requirement::Event(Happening::Entered { mob, zone }) => {
                format!("{mob} coming into {zone}")
            }
            Requirement::Event(Happening::Died(mob)) => format!("{mob} dying"),
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
        let facts = Subject::of(
            Who {
                attributes: &attributes,
                race: "elf",
                skills: None,
                guild_rank: None,
                character: None,
                conditions: None,
                room: None,
                carrying: None,
            },
            None,
        );
        let acolyte = Requirement::All(vec![
            Requirement::Stat(Attribute::Magic, 1),
            Requirement::Stat(Attribute::Willpower, 4),
        ]);
        assert!(!acolyte.met(&facts));
        attributes.set(Attribute::Willpower, 4);
        let facts = Subject::of(
            Who {
                attributes: &attributes,
                race: "elf",
                skills: None,
                guild_rank: None,
                character: None,
                conditions: None,
                room: None,
                carrying: None,
            },
            None,
        );
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
        let who = |character: Option<i64>| {
            Subject::of(
                Who {
                    attributes: &attributes,
                    race: "human",
                    skills: None,
                    guild_rank: None,
                    character,
                    conditions: None,
                    room: None,
                    carrying: None,
                },
                Some(&story),
            )
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
    fn what_they_carry_where_they_are_and_whats_on_them() {
        let attributes = Attributes::default();
        let conditions = Conditions::default().with_traits(&["fey_mind".to_string()]);
        let room = RoomKey::parse("sewers:nest", "");
        let carrying = vec![
            (
                ItemKey::parse("sewers:rat_tail", ""),
                vec!["trophy".to_string()],
            ),
            (
                ItemKey::parse("sewers:rat_tail", ""),
                vec!["trophy".to_string()],
            ),
            (
                ItemKey::parse("forest:yarrow", ""),
                vec!["herb".to_string()],
            ),
        ];
        let subject = Subject::of(
            Who {
                attributes: &attributes,
                race: "elf",
                skills: None,
                guild_rank: None,
                character: None,
                conditions: Some(&conditions),
                room: Some(&room),
                carrying: Some(&carrying),
            },
            None,
        );
        let met = |text: &str| {
            crate::world::ron_options()
                .from_str::<Requirement>(text)
                .unwrap()
                .met(&subject)
        };
        assert!(met(r#"Has("fey_mind")"#));
        assert!(!met(r#"Has("poisoned")"#));
        assert!(met(r#"HasItem(Item("sewers:rat_tail"), 2)"#));
        assert!(!met(r#"HasItem(Item("sewers:rat_tail"), 3)"#));
        assert!(met(r#"HasItem(Tag("herb"), 1)"#));
        assert!(met(r#"InRoom("sewers:nest")"#));
        assert!(!met(r#"InRoom("riftwatch:market")"#));
        assert!(met(r#"InZone("sewers")"#));
        assert_eq!(
            Requirement::HasItem(ItemMatch::Item("sewers:rat_tail".into()), 3)
                .describe(&|id| id.into()),
            "3 rat tail"
        );
        // A world event has no one carrying anything, or anywhere.
        let story = Facts::default();
        assert!(!Requirement::InZone("sewers".into()).met(&Subject::world(&story, "", None)));
    }

    #[test]
    fn a_guild_rank_is_met_by_that_rank_or_higher_shown() {
        let attributes = Attributes::default();
        let shows = |rank: Option<usize>| {
            Subject::of(
                Who {
                    attributes: &attributes,
                    race: "human",
                    skills: None,
                    guild_rank: rank,
                    character: None,
                    conditions: None,
                    room: None,
                    carrying: None,
                },
                None,
            )
        };
        let silver = Requirement::GuildRank("silver".into());
        assert!(!silver.met(&shows(None)), "no tag, no rank");
        assert!(!silver.met(&shows(Some(2))), "Iron");
        assert!(silver.met(&shows(Some(3))));
        assert!(silver.met(&shows(Some(5))), "Platinum");
        assert_eq!(silver.describe(&|id| id.to_string()), "a Silver guild tag");
    }
}
