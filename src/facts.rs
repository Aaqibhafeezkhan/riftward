//! Facts: what the game remembers about its story, in one store (see `docs/facts.md`).
//!
//! A fact is a named value belonging to the world, a character, one character's quest, or a mob
//! template. Values are a yes, a whole number or a word; a fact never set is false, zero or
//! empty, so setting one back to that clears it, and nothing needs declaring. A fact can lapse
//! at a time. Facts are kept in memory and written through to the `facts` table as they change,
//! so they survive restarts.

use std::collections::HashMap;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::time::{heartbeat, unix_now};

pub struct FactsPlugin;

impl Plugin for FactsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Facts>()
            .add_systems(Startup, load_facts)
            .add_systems(Update, lapse_facts.run_if(heartbeat()));
    }
}

/// Whose a fact is: the scope it's in, and within it, whom it belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    /// `world.`: the whole game.
    World,
    /// `char.`: one character, by id.
    Character(i64),
    /// `quest.`: one character's one quest (`zone:quest`).
    Quest(i64, String),
    /// `npc.`: a mob template (`zone:mob`), wherever its mobs are.
    Npc(String),
}

impl Owner {
    /// The scope's name, as keys are written: `world`, `char`, `quest`, `npc`.
    pub fn scope(&self) -> &'static str {
        match self {
            Owner::World => "world",
            Owner::Character(_) => "char",
            Owner::Quest(..) => "quest",
            Owner::Npc(_) => "npc",
        }
    }

    /// As saved: the scope, whom within it, and the character it goes with, if any.
    fn columns(&self) -> (&'static str, String, Option<i64>) {
        let scope = self.scope();
        match self {
            Owner::World => (scope, String::new(), None),
            Owner::Character(id) => (scope, id.to_string(), Some(*id)),
            Owner::Quest(id, quest) => (scope, format!("{id}:{quest}"), Some(*id)),
            Owner::Npc(mob) => (scope, mob.clone(), None),
        }
    }

    /// Back from what was saved.
    fn from_columns(scope: &str, owner: &str) -> Option<Self> {
        Some(match scope {
            "world" => Owner::World,
            "char" => Owner::Character(owner.parse().ok()?),
            "quest" => {
                let (id, quest) = owner.split_once(':')?;
                Owner::Quest(id.parse().ok()?, quest.to_string())
            }
            "npc" => Owner::Npc(owner.to_string()),
            _ => return None,
        })
    }
}

/// A fact's value. In data: `true`, `3`, `"sold"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Word(String),
}

impl Value {
    /// False, zero or empty: what a fact never set is.
    pub fn is_unset(&self) -> bool {
        match self {
            Value::Bool(b) => !b,
            Value::Int(n) => *n == 0,
            Value::Word(w) => w.is_empty(),
        }
    }

    /// As saved: its kind, and itself as text.
    fn columns(&self) -> (&'static str, String) {
        match self {
            Value::Bool(b) => ("bool", b.to_string()),
            Value::Int(n) => ("int", n.to_string()),
            Value::Word(w) => ("word", w.clone()),
        }
    }

    fn from_columns(kind: &str, text: &str) -> Option<Self> {
        Some(match kind {
            "bool" => Value::Bool(text.parse().ok()?),
            "int" => Value::Int(text.parse().ok()?),
            "word" => Value::Word(text.to_string()),
            _ => return None,
        })
    }
}

/// A fact's value, and when it lapses (unix seconds), if it does.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    value: Value,
    until: Option<i64>,
}

impl Entry {
    fn live(&self, now: i64) -> bool {
        self.until.is_none_or(|until| until > now)
    }
}

/// Every fact, by owner and key (the key without its scope: `rats_broke_out`, `trust.marin`).
/// Changed only through these methods, which save as they go.
#[derive(Resource, Default)]
pub struct Facts {
    facts: HashMap<(Owner, String), Entry>,
}

impl Facts {
    /// A fact's value, if it's set and hasn't lapsed.
    pub fn get(&self, owner: &Owner, key: &str, now: i64) -> Option<&Value> {
        self.facts
            .get(&(owner.clone(), key.to_string()))
            .filter(|entry| entry.live(now))
            .map(|entry| &entry.value)
    }

    /// Whether it's set: true, a number but zero, a word.
    pub fn is_set(&self, owner: &Owner, key: &str, now: i64) -> bool {
        self.get(owner, key, now).is_some()
    }

    // Counted by quests' objectives, once there are any.
    #[allow(dead_code)]
    /// Its number: zero if it isn't set or isn't a number.
    pub fn number(&self, owner: &Owner, key: &str, now: i64) -> i64 {
        match self.get(owner, key, now) {
            Some(Value::Int(n)) => *n,
            _ => 0,
        }
    }

    /// Set a fact, until `until` (unix seconds) or for good. Setting it to false, zero or empty
    /// clears it.
    pub fn set(
        &mut self,
        owner: &Owner,
        key: &str,
        value: Value,
        until: Option<i64>,
        db: &Database,
    ) {
        if value.is_unset() {
            self.clear(owner, key, db);
            return;
        }
        let (scope, whom, character) = owner.columns();
        let (kind, text) = value.columns();
        if let Err(err) = db.save_fact(scope, &whom, character, key, kind, &text, until) {
            error!("failed to save the fact {scope}.{key} ({whom}): {err}");
        }
        self.facts
            .insert((owner.clone(), key.to_string()), Entry { value, until });
    }

    // Counted by quests' objectives, once there are any.
    #[allow(dead_code)]
    /// Add `n` to a number (a fact not set, or not a number, counts as zero), keeping when it
    /// lapses. Returns the new number.
    pub fn add(&mut self, owner: &Owner, key: &str, n: i64, now: i64, db: &Database) -> i64 {
        let total = self.number(owner, key, now) + n;
        let until = self
            .facts
            .get(&(owner.clone(), key.to_string()))
            .filter(|entry| entry.live(now))
            .and_then(|entry| entry.until);
        self.set(owner, key, Value::Int(total), until, db);
        total
    }

    /// Clear a fact.
    pub fn clear(&mut self, owner: &Owner, key: &str, db: &Database) {
        if self
            .facts
            .remove(&(owner.clone(), key.to_string()))
            .is_some()
        {
            let (scope, whom, _) = owner.columns();
            if let Err(err) = db.delete_fact(scope, &whom, key) {
                error!("failed to clear the fact {scope}.{key} ({whom}): {err}");
            }
        }
    }

    /// Every live fact `owner` has, by key, sorted.
    pub fn of(&self, owner: &Owner, now: i64) -> Vec<(&str, &Value)> {
        let mut facts: Vec<_> = self
            .facts
            .iter()
            .filter(|((whose, _), entry)| whose == owner && entry.live(now))
            .map(|((_, key), entry)| (key.as_str(), &entry.value))
            .collect();
        facts.sort_by_key(|(key, _)| *key);
        facts
    }
}

/// Every fact, as saved.
pub fn load_facts(db: Res<Database>, mut facts: ResMut<Facts>) {
    let rows = match db.facts() {
        Ok(rows) => rows,
        Err(err) => {
            error!("failed to load facts: {err}");
            return;
        }
    };
    for row in rows {
        let (Some(owner), Some(value)) = (
            Owner::from_columns(&row.scope, &row.owner),
            Value::from_columns(&row.kind, &row.value),
        ) else {
            warn!("a saved fact can't be read: {row:?}");
            continue;
        };
        facts.facts.insert(
            (owner, row.key),
            Entry {
                value,
                until: row.until,
            },
        );
    }
}

/// Lapsed facts are forgotten.
fn lapse_facts(mut facts: ResMut<Facts>, db: Res<Database>) {
    let now = unix_now();
    let before = facts.facts.len();
    facts.facts.retain(|_, entry| entry.live(now));
    if facts.facts.len() < before
        && let Err(err) = db.delete_lapsed_facts(now)
    {
        error!("failed to forget lapsed facts: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open(":memory:").unwrap()
    }

    #[test]
    fn facts_are_set_counted_and_cleared() {
        let db = db();
        let mut facts = Facts::default();
        let (world, marin) = (Owner::World, Owner::Npc("riftwatch:contract_clerk".into()));
        assert!(!facts.is_set(&world, "rats_broke_out", 0), "never set");
        facts.set(&world, "rats_broke_out", Value::Bool(true), None, &db);
        assert!(facts.is_set(&world, "rats_broke_out", 0));
        assert!(
            !facts.is_set(&marin, "rats_broke_out", 0),
            "each owner's own"
        );
        assert_eq!(facts.add(&marin, "mood", 2, 0, &db), 2);
        assert_eq!(facts.add(&marin, "mood", -2, 0, &db), 0);
        assert!(!facts.is_set(&marin, "mood", 0), "zero is cleared");
        facts.set(&world, "rats_broke_out", Value::Bool(false), None, &db);
        assert!(facts.of(&world, 0).is_empty(), "false is cleared");
    }

    #[test]
    fn facts_lapse_and_survive_a_restart() {
        let db = db();
        // A character's facts are tied to the character, who has to exist.
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut facts = Facts::default();
        let ada = Owner::Character(id);
        let quest = Owner::Quest(id, "riftwatch:crystals_gone_missing".into());
        facts.set(&ada, "heard_rumour", Value::Bool(true), Some(100), &db);
        facts.set(&quest, "ending", Value::Word("sold".into()), None, &db);
        assert!(facts.is_set(&ada, "heard_rumour", 99));
        assert!(!facts.is_set(&ada, "heard_rumour", 100), "lapsed");

        // What a restart loads.
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        use bevy::ecs::system::RunSystemOnce;
        world.run_system_once(load_facts).unwrap();
        let loaded = world.resource::<Facts>();
        assert_eq!(
            loaded.get(&quest, "ending", 0),
            Some(&Value::Word("sold".into()))
        );
        assert!(loaded.is_set(&ada, "heard_rumour", 99));
    }

    #[test]
    fn values_read_from_data_as_written() {
        let read = |text: &str| crate::world::ron_options().from_str::<Value>(text).unwrap();
        assert_eq!(read("true"), Value::Bool(true));
        assert_eq!(read("3"), Value::Int(3));
        assert_eq!(read("\"sold\""), Value::Word("sold".into()));
    }
}
