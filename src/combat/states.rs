//! Conditions: states on a character, made of data. Each condition is defined in
//! `assets/conditions/*.conditions.ron` (see `docs/effects.md#conditions`): what it does, as a
//! list of modifiers, how it stacks, and what it says. Blessed, Warded, Held and Bleeding are data,
//! and so is every new condition a designer adds.
//!
//! Every character has a `Conditions` component (it comes with `Vitals`): the conditions on them,
//! each with a strength and how long it lasts. Additive modifiers count once per point of
//! strength (Blessed at strength 2 attacks 2 better); multipliers don't. A technique's conditions
//! last some of the bearer's turns; a spell's last a while, counted in time, in a fight or out of
//! one; an item's last while it's worn, held or carried.
//!
//! At the start of each of their turns in a fight, conditions that hurt or heal each turn do, one
//! that costs the turn costs it, and those counted in turns count down. Those counted in time end
//! when their time is up (`expire`).

use std::collections::{BTreeMap, HashMap, HashSet};

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Permission;
use crate::effects::{Effect, Lasting, Reaction};
use crate::events::GameEvent;
use crate::items::capitalized;
use crate::mobs::{self, MobQuery};
use crate::output::{
    Actor, Characters, Outbox, Outgoing, Reply, deliver, reply_line, targets_in_room,
    targets_online,
};
use crate::skills::fill;
use crate::targeting::{Keywords, Target};
use crate::time::span;
use crate::world::ron_options;

/// Asset folder holding the condition files.
const CONDITIONS_PATH: &str = "conditions";
/// The extension condition files use.
const CONDITIONS_EXTENSION: &str = "conditions.ron";
/// Where trait files live, under `assets/`: the same shape as conditions.
const TRAITS_PATH: &str = "traits";
/// The extension trait files use.
const TRAITS_EXTENSION: &str = "traits.ron";

pub struct ConditionsPlugin;

impl Plugin for ConditionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ConditionBook>()
            .register_asset_loader(ConditionBookLoader)
            .init_resource::<ConditionDefs>()
            .add_systems(Startup, load_condition_books)
            .add_systems(Update, sync_condition_defs)
            .add_command(
                CommandSpec::new("condition", Category::Admin)
                    .usage("condition <who> <condition> [<strength>] [<minutes> | <turns>t] | condition <who> remove <condition>")
                    .summary("put a condition on someone here or online, or take one off, for testing")
                    .permission(Permission::Events),
                condition_cmd,
            );
    }
}

/// The strength and duration words after a condition's name: the first number is its strength
/// (1 without), the next game minutes (10 without), and `5t` is five turns instead.
fn strength_and_lasting(words: &[&str]) -> Option<(i32, Lasting)> {
    let (mut strength, mut lasting) = (None, None);
    for word in words {
        if let Some(turns) = word.strip_suffix('t') {
            lasting = Some(Lasting::Turns(turns.parse().ok()?));
        } else if strength.is_none() {
            strength = Some(word.parse().ok()?);
        } else if lasting.is_none() {
            lasting = Some(Lasting::Minutes(word.parse().ok()?));
        } else {
            return None;
        }
    }
    Some((
        strength.unwrap_or(1),
        lasting.unwrap_or(Lasting::Minutes(10)),
    ))
}

/// `condition <who> <condition> [<strength>] [<minutes> | <turns>t]`: put a condition on someone
/// here (a mob too) or a player online, for 10 game minutes unless told otherwise;
/// `condition <who> remove <condition>` takes it off.
#[allow(clippy::too_many_arguments)]
fn condition_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    mut bearers: Query<&mut Conditions>,
    defs: Res<ConditionDefs>,
    time: Res<Time>,
    mut outbox: ResMut<Outbox>,
    mut events: MessageWriter<GameEvent>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| -> Reply {
        let words: Vec<&str> = inv.args.split_whitespace().collect();
        let (who, rest) = words.split_first().ok_or_else(|| inv.usage_error())?;
        let target = Target::parse(who).ok_or_else(|| inv.usage_error())?;
        // Someone here first, mob or player, then anyone online.
        let mut candidates = targets_in_room(actor.room, &players, &keywords);
        candidates.extend(mobs::targets_in(actor.room, &mobs, &keywords));
        let bearer = target
            .find_one(inv.entity, candidates)
            .or_else(|| target.find_one(inv.entity, targets_online(&players, &keywords)))
            .and_then(|e| Actor::get(&players, e))
            .ok_or_else(|| {
                format!(
                    "There's no one matching {} here or online.",
                    target.describe()
                )
            })?;
        let mut conditions = bearers
            .get_mut(bearer.entity)
            .map_err(|_| format!("{} can't have conditions.", capitalized(&bearer.name)))?;
        if let ["remove", key] = rest {
            return if conditions.remove(key) {
                Ok(format!(
                    "{} no longer has {key}.",
                    capitalized(&bearer.name)
                ))
            } else {
                Err(format!("{} doesn't have {key}.", capitalized(&bearer.name)))
            };
        }
        let (key, rest) = rest.split_first().ok_or_else(|| inv.usage_error())?;
        let def = defs.condition(key).ok_or_else(|| {
            let mut known: Vec<&str> = defs
                .0
                .iter()
                .filter(|(_, d)| !d.is_trait)
                .map(|(k, _)| k.as_str())
                .collect();
            known.sort();
            format!(
                "There's no condition '{key}'. There are: {}.",
                known.join(", ")
            )
        })?;
        let (strength, lasting) = strength_and_lasting(rest).ok_or_else(|| inv.usage_error())?;
        if !conditions.apply(&defs, key, strength, lasting.lasts(time.elapsed_secs_f64())) {
            return Err(format!("{} is immune to {key}.", capitalized(&bearer.name)));
        }
        // As if it came on by itself: its reactions to taking hold are at the bearer.
        events.write(GameEvent::ConditionApplied {
            who: bearer.entity,
            by: bearer.entity,
            condition: key.to_string(),
            strength,
            reacting: false,
        });
        // Told in the condition's own words, as when anything puts it on.
        if let Some(applied) = &def.text.applied {
            let (theirs, seen) = applied.fill(&capitalized(&bearer.name), strength);
            out.push(Outgoing::line(
                bearer.entity,
                format!("\x1b[1m{theirs}\x1b[0m"),
            ));
            if let Some(seen) = seen {
                out.push(Outgoing::Room {
                    room: bearer.room,
                    except: bearer.entity,
                    text: seen + "\r\n",
                });
            }
        }
        let how_long = match lasting {
            Lasting::Turns(n) => format!("{n} of their turns"),
            time => span(time.real_secs()),
        };
        Ok(format!(
            "{} now has {key} at strength {strength}, for {how_long}.",
            capitalized(&bearer.name)
        ))
    })();
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut outbox);
}

/// A condition, as a designer writes it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ConditionDef {
    /// What it's called: "blessed".
    pub name: String,
    /// What it does, for `affects`, with `{strength}` filled in: "attacking {strength} better".
    pub description: String,
    /// Kinds, for cures, immunities and dispels later: `magic`, `poison`, `physical`.
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub stacking: Stacking,
    pub modifiers: Vec<Modifier>,
    /// On an item, what lands on whatever it strikes, using a charge: a venom coating's
    /// `[Apply(condition: "poisoned", ...)]`. Nothing on a character.
    #[serde(default)]
    pub on_hit: Vec<Effect>,
    /// What it does by itself when its bearer hits, is struck, dodges, kills or is hurt: thorns
    /// tearing at an attacker. Effects' formulas see the condition's `strength`.
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub text: ConditionText,
    /// Whether it's a trait (`assets/traits/`): what its bearer is, not what's happening to them.
    /// Set by the file it comes from.
    #[serde(skip)]
    pub is_trait: bool,
}

/// What happens when a condition is put on someone who already has it.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Stacking {
    /// The new one replaces the old.
    #[default]
    Replace,
    /// The stronger stays, and the longer.
    Strongest,
    /// Their strengths add up, to at most this many times the new one's: doses of poison.
    Stack(u32),
}

/// What a condition does. Amounts on additive modifiers count once per point of strength.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Modifier {
    /// Attacks better, or worse.
    Attack(i32),
    /// Defends better, or worse.
    Defence(i32),
    /// Defence multiplied: half for someone down, held or exposed.
    DefenceTimes(f64),
    /// Takes this much more off every blow, like armour: only damage armour stops (physical).
    Armour(i32),
    /// The share of damage of this type (`assets/damage_types.ron`) they shrug off: `Resist("fire",
    /// 0.8)` stops 80% of fire, and 1 stops it all.
    Resist(String, f64),
    /// Extra damage of this type they take, added up: `Weakness("fire", 0.5)` takes 150% of fire,
    /// and 1 takes double.
    Weakness(String, f64),
    /// Conditions with this tag don't take hold of them: `Immune("poison")`.
    Immune(String),
    /// Resists whatever is tagged so this much better in the contest, per point of strength:
    /// `Withstand("sleep", 4)`, an elf's fey mind. Several tags on one spell count the best once.
    Withstand(String, f64),
    /// Damage they deal, multiplied.
    DamageDealt(f64),
    /// Damage they take, multiplied.
    DamageTaken(f64),
    /// Health at the start of each of their turns in a fight: lost if negative (bleeding,
    /// poison), gained if positive (regenerating).
    HealthEachTurn(i32),
    /// Takes damage in their place, up to its strength, which wears down as it does: a ward.
    Absorb,
    /// Loses their next turn.
    LosesTurn,
    /// Can't run from a fight.
    CantFlee,
    /// Can't walk anywhere.
    CantMove,
    /// Can't cast spells.
    CantCast,
}

/// What a condition says. Bearer lines are the bearer's own; room lines are what everyone else
/// there sees, with `{bearer}` and `{amount}` filled in.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ConditionText {
    /// When it takes hold, from a blow or a technique: "You are staggered!".
    #[serde(default)]
    pub applied: Option<Told>,
    /// When it hurts or heals at the start of a turn: "You bleed. ({amount})".
    #[serde(default)]
    pub each_turn: Option<Told>,
    /// When it costs them their turn: "You reel, staggered, and can't act!".
    #[serde(default)]
    pub lost_turn: Option<Told>,
    /// When its hurt kills them: "You bleed out.".
    #[serde(default)]
    pub kills: Option<Told>,
    /// When its time runs out: "The warm light of your blessing fades.".
    #[serde(default)]
    pub fades: Option<Told>,
    /// When it takes damage in its bearer's place: "Your ward takes the blow ({amount}).".
    #[serde(default)]
    pub absorbs: Option<Told>,
    /// When it has taken all it can: "Your ward fails!".
    #[serde(default)]
    pub breaks: Option<Told>,
    /// What others see when they look at the bearer: one hint, or several from strongest to
    /// weakest, chosen by how much of its strength is left. "A faint shimmer surrounds {bearer}."
    #[serde(default)]
    pub looks: Vec<String>,
}

/// One moment, for its bearer and for the room.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Told {
    pub bearer: String,
    #[serde(default)]
    pub room: Option<String>,
}

impl Told {
    /// Its lines filled in for a bearer called `name`, with `amount`.
    pub fn fill(&self, name: &str, amount: i32) -> (String, Option<String>) {
        let amount = amount.abs().to_string();
        let with = [("bearer", name), ("amount", amount.as_str())];
        (
            fill(&self.bearer, &with),
            self.room.as_ref().map(|room| fill(room, &with)),
        )
    }
}

/// Every condition from every loaded file, by key.
#[derive(Resource, Default)]
pub struct ConditionDefs(pub HashMap<String, ConditionDef>);

impl ConditionDefs {
    /// A condition or a trait.
    pub fn get(&self, key: &str) -> Option<&ConditionDef> {
        self.0.get(key)
    }

    /// A condition, not a trait: what effects put on and take off.
    pub fn condition(&self, key: &str) -> Option<&ConditionDef> {
        self.get(key).filter(|d| !d.is_trait)
    }

    /// A trait, not a condition.
    pub fn trait_def(&self, key: &str) -> Option<&ConditionDef> {
        self.get(key).filter(|d| d.is_trait)
    }
}

/// A condition on one item, not its template: put on by `Imbue`, saved with the item
/// (`item_conditions`). Items take no turns, so it lasts until a time, or for so many hits.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemCondition {
    pub key: String,
    pub strength: i32,
    /// When it ends, in unix seconds; never, without.
    pub ends_at: Option<i64>,
    /// Hits left, each using one as its `on_hit` effects land; unlimited, without.
    pub charges: Option<u32>,
}

/// The conditions on one item. Every item has them, most none.
#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct ItemConditions(pub Vec<ItemCondition>);

impl ItemConditions {
    /// Put one on, replacing any of the same kind.
    pub fn imbue(&mut self, condition: ItemCondition) {
        self.0.retain(|c| c.key != condition.key);
        self.0.push(condition);
    }

    /// Take off those whose time is up at `unix`, returning them.
    pub fn expire(&mut self, unix: i64) -> Vec<ItemCondition> {
        let (gone, kept) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(|c| c.ends_at.is_some_and(|end| end <= unix));
        self.0 = kept;
        gone
    }

    /// The `on_hit` effects of every condition with a charge left, each at its strength, and use
    /// a charge of each.
    /// Returns the effects, and the conditions it spent the last charge of.
    pub fn strike(&mut self, defs: &ConditionDefs) -> (Vec<Effect>, Vec<String>) {
        let mut effects = Vec::new();
        for condition in &mut self.0 {
            let Some(def) = defs.get(&condition.key).filter(|d| !d.on_hit.is_empty()) else {
                continue;
            };
            if condition.charges == Some(0) {
                continue;
            }
            // At its strength: flaming at 2 burns twice as hot.
            effects.extend(
                def.on_hit
                    .iter()
                    .map(|e| e.with_strength(condition.strength)),
            );
            if let Some(charges) = condition.charges.as_mut() {
                *charges -= 1;
            }
        }
        let spent = self
            .0
            .iter()
            .filter(|c| c.charges == Some(0))
            .map(|c| c.key.clone())
            .collect();
        self.0.retain(|c| c.charges != Some(0));
        (effects, spent)
    }
}

/// Item conditions as a combat round sees them: copied out before it, changed as weapons strike
/// and are imbued, and written back after.
#[derive(Debug, Default)]
pub struct Imbued {
    pub items: HashMap<Entity, ItemConditions>,
    /// The items whose conditions changed, to write back.
    pub changed: std::collections::HashSet<Entity>,
}

impl Imbued {
    /// Every item with conditions on it.
    pub fn gather<'a>(items: impl Iterator<Item = (Entity, &'a ItemConditions)>) -> Self {
        Self {
            items: items
                .filter(|(_, c)| !c.0.is_empty())
                .map(|(e, c)| (e, c.clone()))
                .collect(),
            changed: Default::default(),
        }
    }

    /// Write back what changed.
    pub fn write_back(self, items: &mut Query<(Entity, &mut ItemConditions)>) {
        for entity in self.changed {
            if let (Ok((_, mut conditions)), Some(now)) =
                (items.get_mut(entity), self.items.get(&entity))
            {
                *conditions = now.clone();
            }
        }
    }
}

/// How long a condition lasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lasts {
    /// This many more of the bearer's turns in a fight.
    Turns(u32),
    /// Until this time, in seconds since the server started.
    Until(f64),
    /// While this item is worn, held or carried, as its template says.
    Granted(Entity),
    /// Until something ends it: hiding, until the hider acts openly or is found.
    Indefinitely,
    /// For good: a trait, what its bearer is. Cures, removal and death leave it.
    Trait,
}

impl Lasts {
    /// Whether nothing that ends conditions ends it: what an item grants, while the item stays,
    /// and traits.
    fn kept(self) -> bool {
        matches!(self, Lasts::Granted(_) | Lasts::Trait)
    }
}

/// One condition on someone: which, how strong, how strong it started, and for how long.
#[derive(Clone, Debug, PartialEq)]
pub struct Active {
    pub key: String,
    pub strength: i32,
    /// Its strength when it began, for hints that follow what's left (a ward's shimmer).
    pub most: i32,
    pub lasts: Lasts,
    /// Those who've seen through it: for `hidden`, everyone who noticed the hider anyway.
    pub seen_by: HashSet<Entity>,
}

/// The trait of being a thing, not a creature: a training dummy. It never acts in a fight, never
/// wanders or picks one, is worth no experience and leaves no corpse (`combat::Inanimate`, put
/// on mobs whose template has it).
pub const INANIMATE: &str = "inanimate";

/// The trait of having no body to leave behind: a slime. It leaves no corpse when it dies, and
/// what it held and leaves (its loot) falls where it died.
pub const FORMLESS: &str = "formless";

/// The condition of being out of sight: its strength is how well they hid (Stealth plus Agility),
/// and those who noticed them anyway are its `seen_by`. See `stealth.rs`.
pub const HIDDEN: &str = "hidden";

/// Someone's current conditions.
#[derive(Component, Clone, Debug, Default)]
pub struct Conditions(pub Vec<Active>);

/// A condition that took damage in its bearer's place.
#[derive(Debug, Clone, PartialEq)]
pub struct Absorbed {
    pub key: String,
    pub taken: i32,
    /// Whether it took all it could, and is gone.
    pub spent: bool,
}

/// What the start of someone's turn brings, condition by condition.
#[derive(Debug, Default, PartialEq)]
pub struct TurnStart {
    /// Health lost (negative) or gained, and which conditions did it, with how much.
    pub health: Vec<(String, i32)>,
    /// The condition that costs them this turn, if any.
    pub lost_to: Option<String>,
    /// Conditions counted in turns that ran out.
    pub ended: Vec<Active>,
}

impl Conditions {
    /// Put a condition on them, by its stacking rule. Returns whether it took hold: one with a tag
    /// they're immune to doesn't.
    pub fn apply(&mut self, defs: &ConditionDefs, key: &str, strength: i32, lasts: Lasts) -> bool {
        // A trait isn't put on like a condition.
        if defs.trait_def(key).is_some() {
            return false;
        }
        let tags = defs.get(key).map(|d| d.tags.as_slice()).unwrap_or_default();
        let immune = self
            .modifiers(defs)
            .any(|(m, _)| matches!(m, Modifier::Immune(tag) if tags.contains(tag)));
        if immune {
            return false;
        }
        let stacking = defs.get(key).map_or(Stacking::Replace, |d| d.stacking);
        let mine = |a: &Active| a.key == key && !a.lasts.kept();
        let new = Active {
            key: key.to_string(),
            strength,
            most: strength,
            lasts,
            seen_by: HashSet::new(),
        };
        match (stacking, self.0.iter_mut().find(|a| mine(a))) {
            (Stacking::Strongest, Some(old)) => {
                old.strength = old.strength.max(strength);
                old.most = old.most.max(strength);
                old.lasts = longer(old.lasts, lasts);
            }
            (Stacking::Stack(most), Some(old)) => {
                let cap = strength.saturating_mul(most as i32);
                old.strength = (old.strength + strength).min(cap);
                old.most = old.most.max(old.strength);
                old.lasts = longer(old.lasts, lasts);
            }
            _ => {
                self.0.retain(|a| !mine(a));
                self.0.push(new);
            }
        }
        true
    }

    pub fn has(&self, key: &str) -> bool {
        self.0.iter().any(|a| a.key == key)
    }

    /// Conditions someone is born with, (condition, strength), lasting until something ends them.
    pub fn born_with(conditions: impl IntoIterator<Item = (String, i32)>) -> Self {
        Self(
            conditions
                .into_iter()
                .map(|(key, strength)| Active {
                    key,
                    strength,
                    most: strength,
                    lasts: Lasts::Indefinitely,
                    seen_by: HashSet::new(),
                })
                .collect(),
        )
    }

    /// Add traits (`assets/traits/`), each at strength 1: what a mob's template or a race says its
    /// bearer is.
    pub fn with_traits<'a>(mut self, traits: impl IntoIterator<Item = &'a String>) -> Self {
        for key in traits {
            if !self
                .0
                .iter()
                .any(|a| a.key == *key && a.lasts == Lasts::Trait)
            {
                self.0.push(Active {
                    key: key.clone(),
                    strength: 1,
                    most: 1,
                    lasts: Lasts::Trait,
                    seen_by: HashSet::new(),
                });
            }
        }
        self
    }

    /// Their hiding, if they're hidden: its strength is how well they hid.
    pub fn hidden(&self) -> Option<&Active> {
        self.0.iter().find(|a| a.key == HIDDEN)
    }

    pub fn hidden_mut(&mut self) -> Option<&mut Active> {
        self.0.iter_mut().find(|a| a.key == HIDDEN)
    }

    /// Whether `viewer` can see them: unless they're hidden and `viewer` hasn't noticed.
    pub fn seen_by(&self, viewer: Entity) -> bool {
        self.hidden().is_none_or(|h| h.seen_by.contains(&viewer))
    }

    /// End a condition early, whatever its turns or strength: bandaged bleeding stops. Item
    /// grants stay while the item does, and traits stay. Returns whether they had it.
    pub fn remove(&mut self, key: &str) -> bool {
        let before = self.0.len();
        self.0.retain(|a| a.key != key || a.lasts.kept());
        self.0.len() < before
    }

    /// End every condition with `tag`, but those items grant and traits: a cure for poison.
    /// Returns the conditions it ended.
    pub fn cure(&mut self, defs: &ConditionDefs, tag: &str) -> Vec<String> {
        let tagged = |a: &Active| {
            !a.lasts.kept()
                && defs
                    .get(&a.key)
                    .is_some_and(|d| d.tags.iter().any(|t| t == tag))
        };
        let ended = self.0.iter().filter(|a| tagged(a)).map(|a| a.key.clone());
        let ended: Vec<String> = ended.collect();
        self.0.retain(|a| !tagged(a));
        ended
    }

    /// End every condition but those items grant, and traits: death clears them.
    pub fn clear(&mut self) {
        self.0.retain(|a| a.lasts.kept());
    }

    /// Each of their modifiers, with the strength it counts at.
    fn modifiers<'a>(
        &'a self,
        defs: &'a ConditionDefs,
    ) -> impl Iterator<Item = (&'a Modifier, i32)> + 'a {
        self.0.iter().flat_map(move |active| {
            defs.get(&active.key)
                .into_iter()
                .flat_map(|def| def.modifiers.iter())
                .map(move |m| (m, active.strength))
        })
    }

    /// The sum of an additive modifier, each counted per point of strength.
    fn total(&self, defs: &ConditionDefs, pick: impl Fn(&Modifier) -> Option<i32>) -> i32 {
        self.modifiers(defs)
            .filter_map(|(m, strength)| pick(m).map(|n| n * strength))
            .sum()
    }

    /// The product of a multiplier.
    fn product(&self, defs: &ConditionDefs, pick: impl Fn(&Modifier) -> Option<f64>) -> f64 {
        self.modifiers(defs).filter_map(|(m, _)| pick(m)).product()
    }

    fn any(&self, defs: &ConditionDefs, flag: Modifier) -> bool {
        self.modifiers(defs).any(|(m, _)| *m == flag)
    }

    /// How much better they attack.
    pub fn attack(&self, defs: &ConditionDefs) -> i32 {
        self.total(defs, |m| match m {
            Modifier::Attack(n) => Some(*n),
            _ => None,
        })
    }

    /// Their defence from `base`, once conditions are counted: added to, then multiplied.
    pub fn defence(&self, base: f64, defs: &ConditionDefs) -> f64 {
        let added = base
            + f64::from(self.total(defs, |m| match m {
                Modifier::Defence(n) => Some(*n),
                _ => None,
            }));
        let times = self.product(defs, |m| match m {
            Modifier::DefenceTimes(x) => Some(*x),
            _ => None,
        });
        added * times
    }

    /// How much more they take off a blow of type `kind`: only what armour stops.
    pub fn armour(&self, defs: &ConditionDefs, kind: &str) -> i32 {
        if !crate::world::armour_stops(kind) {
            return 0;
        }
        self.total(defs, |m| match m {
            Modifier::Armour(n) => Some(*n),
            _ => None,
        })
    }

    /// What damage of type `kind` they take is multiplied by: what each resistance lets through,
    /// together, times one plus their weaknesses to it.
    pub fn resist(&self, defs: &ConditionDefs, kind: &str) -> f64 {
        let through = self.product(defs, |m| match m {
            Modifier::Resist(k, x) if k == kind => Some((1.0 - x).clamp(0.0, 1.0)),
            _ => None,
        });
        let weakness: f64 = self
            .modifiers(defs)
            .filter_map(|(m, _)| match m {
                Modifier::Weakness(k, x) if k == kind => Some(*x),
                _ => None,
            })
            .sum();
        through * (1.0 + weakness).max(0.0)
    }

    /// How much better they resist something with these `tags` in its contest: the best of
    /// their `Withstand`s for any one tag.
    pub fn withstanding(&self, defs: &ConditionDefs, tags: &[String]) -> f64 {
        tags.iter()
            .map(|tag| {
                self.modifiers(defs)
                    .filter_map(|(m, strength)| match m {
                        Modifier::Withstand(t, n) if t == tag => Some(n * f64::from(strength)),
                        _ => None,
                    })
                    .sum::<f64>()
            })
            .fold(0.0, f64::max)
    }

    /// What the damage they deal is multiplied by.
    pub fn damage_dealt(&self, defs: &ConditionDefs) -> f64 {
        self.product(defs, |m| match m {
            Modifier::DamageDealt(x) => Some(*x),
            _ => None,
        })
    }

    /// What the damage they take is multiplied by.
    pub fn damage_taken(&self, defs: &ConditionDefs) -> f64 {
        self.product(defs, |m| match m {
            Modifier::DamageTaken(x) => Some(*x),
            _ => None,
        })
    }

    /// Whether they can run from a fight.
    pub fn can_flee(&self, defs: &ConditionDefs) -> bool {
        !self.any(defs, Modifier::CantFlee)
    }

    /// Whether they can walk anywhere.
    pub fn can_move(&self, defs: &ConditionDefs) -> bool {
        !self.any(defs, Modifier::CantMove)
    }

    /// Whether they can cast.
    pub fn can_cast(&self, defs: &ConditionDefs) -> bool {
        !self.any(defs, Modifier::CantCast)
    }

    /// Let what absorbs damage take what it can of `damage`. Returns how much it took.
    #[cfg(test)]
    pub fn absorb(&mut self, damage: i32, defs: &ConditionDefs) -> i32 {
        self.absorb_by(damage, defs).iter().map(|a| a.taken).sum()
    }

    /// Let what absorbs damage take what it can of `damage`, and say which did: each condition,
    /// how much it took, and whether it's spent. One that's taken all it can is gone.
    pub fn absorb_by(&mut self, damage: i32, defs: &ConditionDefs) -> Vec<Absorbed> {
        let absorbs = |key: &str| {
            defs.get(key)
                .is_some_and(|d| d.modifiers.contains(&Modifier::Absorb))
        };
        let mut left = damage.max(0);
        let mut by = Vec::new();
        for active in &mut self.0 {
            if absorbs(&active.key) && left > 0 {
                let taken = left.min(active.strength.max(0));
                active.strength -= taken;
                left -= taken;
                by.push(Absorbed {
                    key: active.key.clone(),
                    taken,
                    spent: active.strength <= 0,
                });
            }
        }
        self.0
            .retain(|active| active.strength > 0 || !absorbs(&active.key));
        by
    }

    /// The start of their turn in a fight: what hurts or heals does, a condition that costs the
    /// turn costs it, and everything counted in turns counts down.
    pub fn start_turn(&mut self, defs: &ConditionDefs) -> TurnStart {
        let mut start = TurnStart::default();
        for active in &mut self.0 {
            let Some(def) = defs.get(&active.key) else {
                continue;
            };
            for modifier in &def.modifiers {
                match *modifier {
                    Modifier::HealthEachTurn(n) => {
                        start.health.push((active.key.clone(), n * active.strength));
                    }
                    Modifier::LosesTurn if start.lost_to.is_none() => {
                        start.lost_to = Some(active.key.clone());
                    }
                    _ => {}
                }
            }
            if let Lasts::Turns(turns) = &mut active.lasts {
                *turns = turns.saturating_sub(1);
            }
        }
        let (ended, left) = self
            .0
            .drain(..)
            .partition(|active| active.lasts == Lasts::Turns(0));
        self.0 = left;
        start.ended = ended;
        start
    }

    /// End the conditions whose time is up at `now`. Returns them.
    pub fn expire(&mut self, now: f64) -> Vec<Active> {
        let (over, left) = self
            .0
            .drain(..)
            .partition(|a| matches!(a.lasts, Lasts::Until(until) if until <= now));
        self.0 = left;
        over
    }

    /// Each condition, what it does and how long it has left at `now`, for `affects`:
    /// "blessed: attacking 2 better, for 1 hour 20 minutes (10 real minutes)". `item` names the
    /// item behind a granted one.
    pub fn listed(
        &self,
        now: f64,
        defs: &ConditionDefs,
        item: impl Fn(Entity) -> String,
    ) -> Vec<String> {
        self.0
            .iter()
            .filter(|active| active.lasts != Lasts::Trait)
            .filter_map(|active| {
                let def = defs.get(&active.key)?;
                let strength = active.strength.to_string();
                let what = fill(&def.description, &[("strength", strength.as_str())]);
                let left = match active.lasts {
                    Lasts::Until(until) => format!("for {}", span((until - now).max(0.0))),
                    Lasts::Turns(1) => "for 1 more turn".into(),
                    Lasts::Turns(n) => format!("for {n} more turns"),
                    Lasts::Granted(by) => format!("from {}", item(by)),
                    Lasts::Indefinitely => "for now".into(),
                    Lasts::Trait => return None,
                };
                // `fill` capitalises; a description reads mid-sentence here.
                Some(format!("{}: {}, {left}", def.name, lower_first(&what)))
            })
            .collect()
    }

    /// Their conditions' names, for `score`; or with `traits`, their traits'.
    pub fn names(&self, defs: &ConditionDefs, traits: bool) -> Vec<String> {
        let mut names: Vec<String> = self
            .0
            .iter()
            .filter(|a| (a.lasts == Lasts::Trait) == traits)
            .filter_map(|a| defs.get(&a.key).map(|d| d.name.clone()))
            .collect();
        names.dedup();
        names
    }

    /// What others see of them: each condition's hint, by how much of its strength is left.
    pub fn hints(&self, defs: &ConditionDefs, name: &str) -> Vec<String> {
        self.0
            .iter()
            .filter_map(|active| {
                let looks = &defs.get(&active.key)?.text.looks;
                if looks.is_empty() {
                    return None;
                }
                let spent = 1.0 - f64::from(active.strength) / f64::from(active.most.max(1));
                let at = ((spent * looks.len() as f64) as usize).min(looks.len() - 1);
                Some(fill(&looks[at], &[("bearer", name)]))
            })
            .collect()
    }

    /// The conditions worth saving: those lasting in time, as the real time (unix seconds) each
    /// ends. `now` is seconds since the server started, and `unix_now` the time it is.
    pub fn to_saved(&self, now: f64, unix_now: i64) -> Vec<crate::db::SavedCondition> {
        self.0
            .iter()
            .filter_map(|active| match active.lasts {
                Lasts::Until(until) if until > now => Some((
                    active.key.clone(),
                    active.strength,
                    active.most,
                    unix_now + (until - now).ceil() as i64,
                )),
                _ => None,
            })
            .collect()
    }

    /// Conditions from saved ones, as they stand at `now`: those already over are gone.
    pub fn from_saved(saved: Vec<crate::db::SavedCondition>, now: f64, unix_now: i64) -> Self {
        Self(
            saved
                .into_iter()
                .filter(|(_, _, _, ends_at)| *ends_at > unix_now)
                .map(|(key, strength, most, ends_at)| Active {
                    key,
                    strength,
                    most,
                    lasts: Lasts::Until(now + (ends_at - unix_now) as f64),
                    seen_by: HashSet::new(),
                })
                .collect(),
        )
    }

    /// Replace what items grant them with `granted`: (condition, strength, item).
    pub fn set_grants(&mut self, granted: Vec<(String, i32, Entity)>) {
        self.0
            .retain(|active| !matches!(active.lasts, Lasts::Granted(_)));
        self.0
            .extend(granted.into_iter().map(|(key, strength, item)| Active {
                key,
                strength,
                most: strength,
                lasts: Lasts::Granted(item),
                seen_by: HashSet::new(),
            }));
    }

    /// What items grant them now, to compare with what they should.
    pub fn grants(&self) -> Vec<(String, i32, Entity)> {
        self.0
            .iter()
            .filter_map(|active| match active.lasts {
                Lasts::Granted(item) => Some((active.key.clone(), active.strength, item)),
                _ => None,
            })
            .collect()
    }
}

/// The longer of two durations: the later end, or the more turns.
fn longer(a: Lasts, b: Lasts) -> Lasts {
    match (a, b) {
        (Lasts::Turns(x), Lasts::Turns(y)) => Lasts::Turns(x.max(y)),
        (Lasts::Until(x), Lasts::Until(y)) => Lasts::Until(x.max(y)),
        (_, b) => b,
    }
}

fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

// Loading condition files, like skill trees.

/// A file of conditions, by key.
#[derive(Asset, TypePath, Serialize, Deserialize, Debug, Clone)]
#[serde(transparent)]
pub struct ConditionBook(pub BTreeMap<String, ConditionDef>);

#[derive(Resource)]
struct ConditionBooksHandle(#[allow(dead_code)] [Handle<LoadedFolder>; 2]);

fn load_condition_books(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(ConditionBooksHandle([
        assets.load_folder(CONDITIONS_PATH),
        assets.load_folder(TRAITS_PATH),
    ]));
}

/// Rebuild the condition table whenever a file loads, changes or goes away.
fn sync_condition_defs(
    mut events: MessageReader<AssetEvent<ConditionBook>>,
    books: Res<Assets<ConditionBook>>,
    mut defs: ResMut<ConditionDefs>,
) {
    if events.read().count() == 0 {
        return;
    }
    defs.0.clear();
    for (_, book) in books.iter() {
        for (key, def) in &book.0 {
            if defs.0.insert(key.clone(), def.clone()).is_some() {
                warn!("'{key}' is defined twice among conditions and traits");
            }
        }
    }
    let traits = defs.0.values().filter(|d| d.is_trait).count();
    info!(
        "loaded {} conditions and {traits} traits",
        defs.0.len() - traits
    );
}

#[derive(TypePath)]
struct ConditionBookLoader;

#[derive(Debug, Error)]
enum ConditionBookLoaderError {
    #[error("could not read conditions: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse conditions: {0}")]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for ConditionBookLoader {
    type Asset = ConditionBook;
    type Settings = ();
    type Error = ConditionBookLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<ConditionBook, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let is_trait = load_context
            .path()
            .path()
            .to_string_lossy()
            .ends_with(TRAITS_EXTENSION);
        Ok(read_book(&bytes, is_trait)?)
    }

    fn extensions(&self) -> &[&str] {
        &[CONDITIONS_EXTENSION, TRAITS_EXTENSION]
    }
}

/// A file of conditions, or of traits.
fn read_book(bytes: &[u8], is_trait: bool) -> Result<ConditionBook, ron::error::SpannedError> {
    let mut book: ConditionBook = ron_options().from_bytes(bytes)?;
    for def in book.0.values_mut() {
        def.is_trait = is_trait;
    }
    Ok(book)
}

/// Every condition and trait in the files on disk, for tests: they must parse, and other files'
/// references to them must be to real ones.
#[cfg(test)]
pub fn defs_on_disk() -> ConditionDefs {
    let mut defs = ConditionDefs::default();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    for (folder, is_trait) in [(CONDITIONS_PATH, false), (TRAITS_PATH, true)] {
        for entry in std::fs::read_dir(root.join(folder)).unwrap() {
            let path = entry.unwrap().path();
            let book = read_book(&std::fs::read(&path).unwrap(), is_trait)
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            for key in book.0.keys() {
                assert!(!defs.0.contains_key(key), "{key} is defined twice");
            }
            defs.0.extend(book.0);
        }
    }
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_condition_files_parse_and_say_what_they_do() {
        let defs = defs_on_disk();
        for key in [
            "staggered",
            "down",
            "bleeding",
            "held",
            "guarding",
            "exposed",
            "blessed",
            "warded",
        ] {
            let def = defs
                .get(key)
                .unwrap_or_else(|| panic!("no condition {key}"));
            assert!(
                !def.description.trim().is_empty(),
                "{key} needs a description"
            );
        }
        for (key, def) in &defs.0 {
            assert!(
                !def.description.trim().is_empty(),
                "{key} needs a description"
            );
            for key in def.on_hit.iter().filter_map(Effect::condition) {
                assert!(
                    defs.condition(key).is_some(),
                    "{key}: on_hit puts on no condition {key}"
                );
            }
            for reaction in &def.reactions {
                reaction
                    .check()
                    .unwrap_or_else(|err| panic!("{key}: {err}"));
                for named in reaction.effects.iter().filter_map(Effect::condition) {
                    assert!(
                        defs.condition(named).is_some(),
                        "{key}: no condition {named}"
                    );
                }
            }
            for m in &def.modifiers {
                if let Modifier::Resist(kind, _) | Modifier::Weakness(kind, _) = m {
                    assert!(
                        crate::world::damage_types().contains_key(kind),
                        "{key}: no damage type {kind}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_condition_can_be_ended_early() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "bleeding", 2, Lasts::Turns(3));
        conditions.apply(&defs, "held", 1, Lasts::Turns(2));
        // However hard and long the bleeding, a bandage stops it.
        conditions.remove("bleeding");
        assert!(!conditions.has("bleeding"));
        assert!(conditions.has("held"));
        assert!(conditions.start_turn(&defs).health.is_empty());
    }

    #[test]
    fn conditions_count_down_in_turns() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "bleeding", 1, Lasts::Turns(2));
        conditions.apply(&defs, "staggered", 1, Lasts::Turns(1));
        conditions.apply(&defs, "held", 1, Lasts::Turns(2));
        assert!(!conditions.can_flee(&defs));
        let first = conditions.start_turn(&defs);
        assert_eq!(first.health, vec![("bleeding".to_string(), -1)]);
        assert_eq!(first.lost_to.as_deref(), Some("staggered"));
        let second = conditions.start_turn(&defs);
        assert_eq!(second.lost_to, None);
        assert!(conditions.can_flee(&defs), "the hold is over");
        assert_eq!(conditions.start_turn(&defs), TurnStart::default());
        assert!(conditions.0.is_empty());
    }

    #[test]
    fn defence_follows_conditions() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        assert_eq!(conditions.defence(6.0, &defs), 6.0);
        conditions.apply(&defs, "guarding", 4, Lasts::Turns(2));
        assert_eq!(conditions.defence(6.0, &defs), 10.0);
        conditions.apply(&defs, "down", 1, Lasts::Turns(1));
        assert_eq!(conditions.defence(6.0, &defs), 5.0);
        // A new guard replaces the old one.
        conditions.apply(&defs, "guarding", 2, Lasts::Turns(2));
        assert_eq!(conditions.defence(6.0, &defs), 4.0);
    }

    #[test]
    fn a_ward_takes_what_it_can_and_then_is_gone() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "warded", 5, Lasts::Until(600.0));
        assert_eq!(conditions.absorb(3, &defs), 3);
        assert_eq!(conditions.absorb(4, &defs), 2, "only 2 left in it");
        assert!(!conditions.has("warded"));
        assert_eq!(conditions.absorb(4, &defs), 0);
    }

    #[test]
    fn timed_conditions_last_their_time_whatever_the_turns() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "blessed", 2, Lasts::Until(600.0));
        conditions.apply(&defs, "held", 1, Lasts::Turns(1));
        // Fighting doesn't wear a blessing down...
        for _ in 0..10 {
            conditions.start_turn(&defs);
        }
        assert_eq!(conditions.attack(&defs), 2);
        assert!(!conditions.has("held"));
        // ...time does, in a fight or out.
        assert!(conditions.expire(599.0).is_empty());
        assert_eq!(conditions.expire(600.0)[0].key, "blessed");
        assert_eq!(conditions.attack(&defs), 0);
    }

    #[test]
    fn exposure_ends_at_the_next_turn() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "exposed", 1, Lasts::Turns(1));
        assert_eq!(conditions.defence(8.0, &defs), 4.0);
        conditions.start_turn(&defs);
        assert_eq!(conditions.defence(8.0, &defs), 8.0);
    }

    #[test]
    fn poison_stacks_in_doses_and_hints_follow_a_ward() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        for _ in 0..10 {
            conditions.apply(&defs, "poisoned", 1, Lasts::Turns(3));
        }
        let Stacking::Stack(most) = defs.get("poisoned").unwrap().stacking else {
            panic!("poison stacks");
        };
        let start = conditions.start_turn(&defs);
        assert!(start.health[0].1 < -1, "doses add up");
        assert_eq!(start.health[0].1, -(most as i32), "to a limit");

        let mut warded = Conditions::default();
        warded.apply(&defs, "warded", 10, Lasts::Until(60.0));
        let fresh = warded.hints(&defs, "Grok");
        warded.absorb(9, &defs);
        let spent = warded.hints(&defs, "Grok");
        assert_ne!(fresh, spent, "a nearly spent ward looks it");
    }

    #[test]
    fn hiding_is_a_condition_seen_through_by_those_who_noticed() {
        let defs = defs_on_disk();
        let (noticer, other) = (
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
        );
        let mut conditions = Conditions::default();
        assert!(conditions.seen_by(other), "in plain sight");
        conditions.apply(&defs, HIDDEN, 12, Lasts::Indefinitely);
        conditions.hidden_mut().unwrap().seen_by.insert(noticer);
        assert!(conditions.seen_by(noticer));
        assert!(!conditions.seen_by(other));
        // It shows in `affects`, it isn't saved over a logout, and it can be ended like any other.
        assert_eq!(
            conditions.listed(0.0, &defs, |_| String::new()),
            ["hidden: out of sight of anyone who hasn't noticed you, for now"]
        );
        assert!(conditions.to_saved(0.0, 0).is_empty());
        assert!(conditions.remove(HIDDEN));
        assert!(conditions.seen_by(other));
    }

    #[test]
    fn timed_conditions_save_as_when_they_end() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "blessed", 2, Lasts::Until(700.0));
        conditions.apply(&defs, "held", 1, Lasts::Turns(2));
        // At 100 seconds in, unix time 5000: the blessing has 600 seconds left.
        let saved = conditions.to_saved(100.0, 5000);
        assert_eq!(
            saved,
            vec![("blessed".to_string(), 2, 2, 5600)],
            "a fight's hold isn't kept"
        );
        // Back after a restart, 10 seconds in, 200 seconds later: 400 left.
        let back = Conditions::from_saved(saved.clone(), 10.0, 5200);
        assert_eq!(back.0[0].lasts, Lasts::Until(410.0));
        // Back after it would have ended: gone.
        assert!(Conditions::from_saved(saved, 10.0, 6000).0.is_empty());
    }

    #[test]
    fn a_hint_names_a_mob_in_lower_case_but_starts_with_a_capital() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "thorns", 1, Lasts::Turns(3));
        assert_eq!(
            conditions.hints(&defs, "the town guard"),
            ["Thorns bristle along the town guard's skin."]
        );
        let mut asleep = Conditions::default();
        asleep.apply(&defs, "asleep", 1, Lasts::Turns(3));
        assert_eq!(
            asleep.hints(&defs, "the town guard"),
            ["The town guard is fast asleep."]
        );
    }

    #[test]
    fn the_condition_command_reads_strength_then_time() {
        assert_eq!(strength_and_lasting(&[]), Some((1, Lasting::Minutes(10))));
        assert_eq!(
            strength_and_lasting(&["3"]),
            Some((3, Lasting::Minutes(10)))
        );
        assert_eq!(
            strength_and_lasting(&["3", "20"]),
            Some((3, Lasting::Minutes(20)))
        );
        assert_eq!(
            strength_and_lasting(&["2", "4t"]),
            Some((2, Lasting::Turns(4)))
        );
        assert_eq!(strength_and_lasting(&["4t"]), Some((1, Lasting::Turns(4))));
        assert_eq!(strength_and_lasting(&["lots"]), None);
        assert_eq!(strength_and_lasting(&["1", "2", "3"]), None);
    }

    #[test]
    fn an_items_conditions_strike_until_spent_or_ended() {
        let defs = defs_on_disk();
        let venom = |charges, ends_at| ItemCondition {
            key: "envenomed".into(),
            strength: 1,
            ends_at,
            charges,
        };
        let mut blade = ItemConditions::default();
        blade.imbue(venom(Some(2), None));
        let (effects, spent) = blade.strike(&defs);
        assert_eq!(effects.len(), 1, "the venom lands");
        assert!(spent.is_empty());
        let (_, spent) = blade.strike(&defs);
        assert_eq!(spent, ["envenomed"], "its last charge");
        assert!(blade.0.is_empty());
        // Imbued again, the new coating replaces the old; one that's timed runs out.
        blade.imbue(venom(None, Some(100)));
        blade.imbue(venom(None, Some(200)));
        assert_eq!(blade.0.len(), 1);
        assert!(blade.expire(150).is_empty());
        assert_eq!(blade.expire(200).len(), 1);
        assert!(blade.0.is_empty());
    }

    #[test]
    fn resistances_and_immunities_follow_types_and_tags() {
        let mut defs = defs_on_disk();
        let charm = |modifiers| ConditionDef {
            name: "warded against fire".into(),
            description: "test".into(),
            tags: vec!["magic".into()],
            stacking: Stacking::Replace,
            modifiers,
            on_hit: Vec::new(),
            reactions: Vec::new(),
            text: ConditionText::default(),
            is_trait: false,
        };
        defs.0.insert(
            "fireproof".into(),
            charm(vec![
                Modifier::Resist("fire".into(), 0.75),
                Modifier::Weakness("cold".into(), 0.5),
                Modifier::Immune("poison".into()),
            ]),
        );
        defs.0.insert(
            "scaled".into(),
            charm(vec![
                Modifier::Resist("fire".into(), 0.5),
                Modifier::Weakness("cold".into(), 1.0),
            ]),
        );
        let mut conditions = Conditions::default();
        conditions.apply(&defs, "fireproof", 1, Lasts::Turns(5));
        assert_eq!(conditions.resist(&defs, "fire"), 0.25, "75% resisted");
        assert_eq!(conditions.resist(&defs, "cold"), 1.5, "a weakness");
        assert_eq!(conditions.resist(&defs, "force"), 1.0);
        conditions.apply(&defs, "scaled", 1, Lasts::Turns(5));
        assert_eq!(
            conditions.resist(&defs, "fire"),
            0.125,
            "resistances let through a share of a share"
        );
        assert_eq!(conditions.resist(&defs, "cold"), 2.5, "weaknesses add up");
        assert!(
            !conditions.apply(&defs, "poisoned", 1, Lasts::Turns(3)),
            "immune"
        );
        assert!(!conditions.has("poisoned"));
        assert!(
            conditions.apply(&defs, "burning", 1, Lasts::Turns(3)),
            "not immune to fire"
        );
        // Conditions' armour stops only what armour stops.
        conditions.apply(&defs, "shielded", 2, Lasts::Turns(5));
        assert_eq!(conditions.armour(&defs, "physical"), 2);
        assert_eq!(conditions.armour(&defs, "force"), 0);
    }

    #[test]
    fn items_grant_conditions_beside_the_rest() {
        let defs = defs_on_disk();
        let mut conditions = Conditions::default();
        let ring = Entity::PLACEHOLDER;
        conditions.apply(&defs, "blessed", 1, Lasts::Until(60.0));
        conditions.set_grants(vec![("blessed".into(), 2, ring)]);
        assert_eq!(conditions.attack(&defs), 3, "both count");
        conditions.remove("blessed");
        assert_eq!(conditions.attack(&defs), 2, "the ring's stays");
        conditions.set_grants(Vec::new());
        assert_eq!(conditions.attack(&defs), 0);
    }
}
