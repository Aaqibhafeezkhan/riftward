//! Making an attack: choosing it, rolling it, and what follows. Plain attacks, techniques and
//! ripostes all come through [`Round::strike`].

use std::collections::{HashMap, HashSet};

use bevy::ecs::query::QueryData;
use bevy::prelude::*;

use super::casting::MobMove;
use super::states::{ConditionDefs, Conditions};
use super::{
    ActionQueue, Aggressive, AutoAttack, BASIC_ATTACK, Dead, Fighting, HideNumbers, Tactics,
};
use crate::attributes::{Attribute, Attributes};
use crate::db::Database;
use crate::dice::Dice;
use crate::effects::{self, Effect, Who};
use crate::events::GameEvent;
use crate::items::{ItemLocation, ItemQuery, ItemTemplates, capitalized};
use crate::mobs::Mob;
use crate::output::PuppetedBy;
use crate::output::{Outbox, Outgoing};
use crate::skills::{
    Bonus, Boosts, Cost, Learner, Lines, Skill, SkillDefs, SkillKind, SkillText, Skills,
    StrikeKind, Technique, Trigger, boosts, count_use, fill, tough_fight_boost,
};
use crate::spells::{SpellDefs, Spells};
use crate::targeting::Keywords;
use crate::vitals::{BaseHealth, Maxima, Vitals};
use crate::world::{ArmourDef, InRoom, Player, WeaponDef};

/// The skill that helps avoid attacks.
const DODGE: &str = "athletics:dodge";
/// Levels of a weapon skill for each extra point of damage with that weapon.
/// Stamina a riposte takes.
const RIPOSTE_COST: f32 = 1.0;

/// Everything combat needs to know about a fighter, player or mob.
#[derive(QueryData)]
#[query_data(mutable)]
pub struct Fighter {
    pub entity: Entity,
    pub in_room: &'static InRoom,
    pub attributes: &'static mut Attributes,
    pub vitals: &'static mut Vitals,
    pub skills: &'static mut Skills,
    pub conditions: &'static mut Conditions,
    pub fighting: Option<&'static Fighting>,
    pub auto: Option<&'static AutoAttack>,
    pub base: Option<&'static BaseHealth>,
    pub tactics: Option<&'static Tactics>,
    pub queue: Option<&'static mut ActionQueue>,
    pub aggressive: Has<Aggressive>,
    pub dead: Has<Dead>,
    pub hide_numbers: Has<HideNumbers>,
    pub player: Option<&'static Player>,
    pub mob: Option<&'static Mob>,
    pub keywords: Option<&'static Keywords>,
    pub spells: Option<&'static Spells>,
    /// A mob a builder is playing: it acts on what they queue, not on its own.
    pub puppeted: Has<PuppetedBy>,
}

pub type Fighters<'w, 's> = Query<'w, 's, Fighter>;

/// A fighter's name, as others say it: "Grok", "the sewer rat".
pub fn name_of(player: Option<&Player>, mob: Option<&Mob>) -> String {
    match (player, mob) {
        (Some(player), _) => player.name.clone(),
        (_, Some(mob)) => super::the(&mob.name),
        _ => "someone".into(),
    }
}

/// How hurt someone looks, from their health and their most.
pub fn condition(health: f32, max: i32) -> &'static str {
    match health / max.max(1) as f32 {
        f if f >= 1.0 => "unhurt",
        f if f >= 0.75 => "scratched",
        f if f >= 0.5 => "hurt",
        f if f >= 0.25 => "badly wounded",
        _ => "barely standing",
    }
}

/// An attack's rating with `skill`: its level and attribute, plus auras and passive bonuses.
fn attack_rating(
    attributes: &Attributes,
    skills: &Skills,
    skill: &Skill,
    aura: Bonus,
    boosts: &Boosts,
) -> i32 {
    let level = skills.0.get(&skill.key).map_or(0, |s| s.level as i32);
    level + attributes.get(skill.attribute) + aura.attack + boosts.attack_with(&skill.key)
}

/// Defence before conditions: Reaction and Dodge, plus auras and passive bonuses.
pub(super) fn defence_rating(
    attributes: &Attributes,
    skills: &Skills,
    aura: Bonus,
    boosts: &Boosts,
    gear: &ArmourDef,
) -> Defence {
    let dodge = skills.0.get(DODGE).map_or(0, |s| s.level as i32);
    let penalty = (gear.penalty - boosts.armour_ease).max(0);
    Defence {
        evasion: attributes.get(Attribute::Reaction) + dodge + aura.defence + boosts.defence
            - penalty,
        block: if gear.block > 0 {
            gear.block + boosts.block
        } else {
            0
        },
    }
}

/// Defence before conditions, in its two parts: getting out of the way, and a shield in the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defence {
    /// Reaction, Dodge and the rest, less what armour takes off.
    pub evasion: i32,
    /// A held shield's block.
    pub block: i32,
}

impl Defence {
    pub fn total(self) -> i32 {
        self.evasion + self.block
    }
}

/// Everything `who` has on as armour, added up: worn pieces count where they're worn, and a
/// shield in hand.
pub fn gear(who: Entity, items: &ItemQuery, templates: &ItemTemplates) -> ArmourDef {
    let mut total = ArmourDef::default();
    for (_, item, location) in items {
        let Some(def) = templates.0.get(&item.key) else {
            continue;
        };
        let Some(armour) = &def.armour else {
            continue;
        };
        let on = match (*location, def.wear) {
            (ItemLocation::Worn(holder, _), Some(_)) => holder == who,
            (ItemLocation::Held(holder), None) => holder == who,
            _ => false,
        };
        // How well it's made moves what it stops.
        if on {
            total.reduction += item.quality.protection(armour.reduction);
            total.penalty += armour.penalty;
            total.block += item.quality.protection(armour.block);
        }
    }
    total
}

/// What `who`'s armour takes off a blow of type `kind`: every piece on them, worn where it
/// belongs or a shield in hand, that stops that type, for its quality.
pub fn armour_against(
    who: Entity,
    items: &ItemQuery,
    templates: &ItemTemplates,
    kind: &str,
) -> i32 {
    items
        .iter()
        .filter_map(|(_, item, location)| {
            let def = templates.0.get(&item.key)?;
            let armour = def.armour.as_ref().filter(|a| a.stops(kind))?;
            let on = match (*location, def.wear) {
                (ItemLocation::Worn(holder, _), Some(_)) => holder == who,
                (ItemLocation::Held(holder), None) => holder == who,
                _ => false,
            };
            on.then(|| item.quality.protection(armour.reduction))
        })
        .sum()
}

/// How far each player is outmatched by the foe they fight, by the ratio rule: the foe's attack
/// against their defence, and the foe's defence against their attack, averaged (a geometric
/// mean). 1 is an even fight; above it, the foe is stronger. Mobs don't learn, so only players
/// are counted.
pub fn outmatched(
    fighters: &Fighters,
    targets: &HashMap<Entity, Entity>,
    auras: &HashMap<Entity, Bonus>,
    items: &ItemQuery,
    templates: &ItemTemplates,
    defs: &SkillDefs,
) -> HashMap<Entity, f64> {
    let ratings = |entity: Entity| {
        let f = fighters.get(entity).ok()?;
        let aura = auras.get(&entity).copied().unwrap_or_default();
        let boosts = boosts(f.skills, defs);
        let strike = choose_attack(
            entity,
            f.auto.and_then(|auto| auto.0.as_deref()),
            f.player.is_some(),
            f.vitals.stamina,
            f.skills,
            items,
            templates,
            defs,
        )?;
        let skill = defs.skills.get(&strike.skill)?;
        let attack = attack_rating(f.attributes, f.skills, skill, aura, &boosts);
        let defence = defence_rating(
            f.attributes,
            f.skills,
            aura,
            &boosts,
            &gear(entity, items, templates),
        )
        .total();
        Some((f64::from(attack.max(1)), f64::from(defence.max(1))))
    };
    targets
        .iter()
        .filter(|(who, _)| fighters.get(**who).is_ok_and(|f| f.player.is_some()))
        .filter_map(|(who, foe)| {
            let ((attack, defence), (their_attack, their_defence)) =
                (ratings(*who)?, ratings(*foe)?);
            Some((
                *who,
                (their_attack / defence * their_defence / attack).sqrt(),
            ))
        })
        .collect()
}

/// The attack a fighter makes this round: which skill, how much damage, and what it looks like.
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    /// The skill it's made with, and practises.
    pub skill: String,
    pub damage: (u32, u32),
    /// What kind of harm it does: the weapon's, or the attack's own.
    pub damage_type: String,
    pub text: SkillText,
    /// Stamina it takes.
    pub cost: u32,
}

/// The attack a fighter makes this round: their chosen attack if they can make it now, otherwise
/// the weapon in their hand, otherwise the basic attack.
///
/// A weapon does its own damage in anyone's hands. Trained in its skill, a fighter attacks with
/// that skill, and their skill shows in how decisively their blows land (see [`Blow`]);
/// untrained, they swing it with only their Brawling to guide them, and learn nothing of the
/// weapon by it. Players must know an attack
/// to make it; mobs fight with whatever their template gives them. An attack they haven't the
/// stamina for falls back to the next, ending at the free basic attack.
#[allow(clippy::too_many_arguments)]
pub fn choose_attack(
    entity: Entity,
    chosen: Option<&str>,
    is_player: bool,
    stamina: f32,
    skills: &Skills,
    items: &ItemQuery,
    templates: &ItemTemplates,
    defs: &SkillDefs,
) -> Option<Strike> {
    // The weapon in hand, its damage moved by how well it's made.
    let weapon = items.iter().find_map(|(_, item, location)| {
        let w = (*location == ItemLocation::Held(entity))
            .then(|| templates.0.get(&item.key)?.weapon.clone())
            .flatten()?;
        Some(WeaponDef {
            damage: item.quality.damage(w.damage),
            ..w
        })
    });
    let weapon = weapon.as_ref();
    let strike = |key: &str| {
        let skill = defs.skills.get(key)?;
        let SkillKind::Attack {
            damage,
            cost,
            damage_type,
        } = &skill.def.kind
        else {
            return None;
        };
        let text = skill.def.text.clone().unwrap_or_else(untrained);
        if is_player && !skills.0.contains_key(key) {
            return None;
        }
        let cost = match cost {
            Cost::Free => 0,
            Cost::Stamina(n) => *n,
        };
        if stamina < cost as f32 {
            return None;
        }
        match weapon {
            Some(w) if w.skill == key => Some(Strike {
                skill: key.to_string(),
                damage: w.damage,
                damage_type: w.damage_type.clone(),
                text,
                cost,
            }),
            _ => damage.map(|damage| Strike {
                skill: key.to_string(),
                damage,
                damage_type: damage_type.clone(),
                text,
                cost,
            }),
        }
    };
    chosen
        .and_then(strike)
        .or_else(|| weapon.and_then(|w| strike(&w.skill)))
        .or_else(|| {
            let brawl = strike(BASIC_ATTACK)?;
            Some(match weapon {
                // Swung untrained: Brawling's aim, the weapon's damage.
                Some(w) => Strike {
                    damage: w.damage,
                    damage_type: w.damage_type.clone(),
                    text: untrained(),
                    ..brawl
                },
                None => brawl,
            })
        })
}

/// What a hit did, by the share of its target's most health it took, from Diku's ladder: a blow
/// that "scratches" a veteran might "massacre" a rat. The words fill `{harm}` in an attack's text.
pub fn harm(dealt: i32, most: i32) -> &'static str {
    if dealt <= 0 {
        return "doesn't hurt";
    }
    match f64::from(dealt) / f64::from(most.max(1)) {
        f if f < 0.05 => "scratches",
        f if f < 0.10 => "grazes",
        f if f < 0.15 => "hits",
        f if f < 0.20 => "injures",
        f if f < 0.30 => "wounds",
        f if f < 0.40 => "mauls",
        f if f < 0.50 => "decimates",
        f if f < 0.65 => "devastates",
        f if f < 0.80 => "maims",
        f if f < 1.00 => "MUTILATES",
        f if f < 1.50 => "EVISCERATES",
        _ => "MASSACRES",
    }
}

/// How hard a blow lands, from how decisively its attack won (the opposed roll's margin): a near
/// thing glances, a decisive win crushes. Skill shows in damage this way, for every attack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blow {
    Glancing,
    Solid,
    Strong,
    Crushing,
}

impl Blow {
    /// The tier a margin earns: under 1.15 glancing, under 1.6 solid, under 2.5 strong, then
    /// crushing.
    pub fn of(margin: f64) -> Self {
        match margin {
            m if m < 1.15 => Blow::Glancing,
            m if m < 1.6 => Blow::Solid,
            m if m < 2.5 => Blow::Strong,
            _ => Blow::Crushing,
        }
    }

    /// What it does to the damage, before armour.
    pub fn multiplier(self) -> f64 {
        match self {
            Blow::Glancing => 0.5,
            Blow::Solid => 1.0,
            Blow::Strong => 1.5,
            Blow::Crushing => 2.0,
        }
    }
}

/// What a weapon swung untrained looks like: there's no skill of its own to describe it.
pub(super) fn untrained() -> SkillText {
    let lines = |actor: &str, target: &str, room: &str| Lines {
        actor: actor.into(),
        target: Some(target.into()),
        room: room.into(),
        beyond: None,
    };
    SkillText {
        hit: lines(
            "Your blow {harm} {target}.",
            "{actor}'s blow {harm} you.",
            "{actor}'s blow {harm} {target}.",
        ),
        miss: Some(lines(
            "You swing at {target}, but miss.",
            "{actor} swings at you, but misses.",
            "{actor} swings at {target}, but misses.",
        )),
    }
}

/// What a shield turning a blow looks like for someone with no skill to describe it.
fn shield_block() -> Lines {
    Lines {
        actor: "You get your shield in the way of {target}'s attack.".into(),
        target: Some("{actor} gets a shield in the way of your attack.".into()),
        room: "{actor} gets a shield in the way of {target}'s attack.".into(),
        beyond: None,
    }
}

/// `lines` filled in for `actor` using a skill on `target`: what the actor, the target and the
/// room see.
pub(super) fn told(lines: &Lines, actor: &str, target: &str) -> (String, String, String) {
    let with = [("actor", actor), ("target", target)];
    (
        fill(&lines.actor, &with),
        lines
            .target
            .as_ref()
            .map_or_else(String::new, |t| fill(t, &with)),
        fill(&lines.room, &with),
    )
}

/// A technique to make, and its skill.
pub struct Move<'a> {
    pub key: &'a str,
    pub technique: &'a Technique,
}

/// One round of fighting: the fighters, and what they need to make their attacks. The queries it
/// borrows each keep their own lifetimes (`'w`, `'s` for the fighters, `'iw`, `'is` for items, `'ew`
/// for events).
pub struct Round<'a, 'w, 's, 'iw, 'is, 'ew> {
    pub fighters: &'a mut Fighters<'w, 's>,
    pub items: &'a ItemQuery<'iw, 'is>,
    pub templates: &'a ItemTemplates,
    pub defs: &'a SkillDefs,
    pub conditions: &'a ConditionDefs,
    pub spells: &'a SpellDefs,
    pub dice: &'a mut Dice,
    pub db: &'a Database,
    pub events: &'a mut MessageWriter<'ew, GameEvent>,
    pub log: &'a mut Outbox,
    /// Players and the rooms they're in, who see fights there.
    pub watchers: &'a [(Entity, Entity)],
    /// Who each fighter was fighting before this round began, for openers.
    pub engaged: &'a HashMap<Entity, Entity>,
    /// Everyone fighting, and whom.
    pub targets: &'a HashMap<Entity, Entity>,
    /// The aura each mob fights under.
    pub auras: &'a HashMap<Entity, Bonus>,
    /// Who was resting as the round began.
    pub resting: &'a HashSet<Entity>,
    /// How far each player is outmatched, for the boost a tough fight gives their learning.
    pub outmatched: &'a HashMap<Entity, f64>,
    /// Attackers their targets haven't noticed, striking from hiding.
    pub ambushers: &'a HashSet<Entity>,
    /// Seconds since the server started, for conditions that last a while.
    pub now: f64,
}

/// Messages for a strike: the attacker, the target, and everyone else there.
#[derive(Default)]
pub(super) struct Telling {
    pub(super) lines: Vec<(Entity, String)>,
    pub(super) room: Vec<String>,
}

impl Round<'_, '_, '_, '_, '_, '_> {
    pub(super) fn say(&mut self, who: Entity, line: impl Into<String>) {
        self.log.0.push(Outgoing::line(who, line.into()));
    }

    /// Tell each their lines, and the room (but `a` and `b`) the rest, together.
    pub(super) fn tell(&mut self, tell: Telling, room: Entity, a: Entity, b: Entity) {
        for (who, line) in tell.lines {
            self.say(who, line);
        }
        if !tell.room.is_empty() {
            self.tell_room(room, a, b, &tell.room.join(" "));
        }
    }

    /// Tell everyone in `room` but `a` and `b`.
    pub(super) fn tell_room(&mut self, room: Entity, a: Entity, b: Entity, line: &str) {
        for &(player, at) in self.watchers {
            if at == room && player != a && player != b {
                self.log.0.push(Outgoing::line(player, line.to_string()));
            }
        }
    }

    pub(super) fn name(&self, entity: Entity) -> String {
        self.fighters
            .get(entity)
            .map(|f| name_of(f.player, f.mob))
            .unwrap_or_else(|_| "someone".into())
    }

    /// Spend a fighter's stamina, if they have it.
    pub(super) fn pay(&mut self, who: Entity, cost: f32) -> bool {
        let Ok(mut fighter) = self.fighters.get_mut(who) else {
            return false;
        };
        if fighter.vitals.stamina < cost {
            return false;
        }
        fighter.vitals.stamina -= cost;
        if cost > 0.0 {
            self.events
                .write(GameEvent::StaminaSpent { who, amount: cost });
        }
        true
    }

    /// Count a use of a skill by a player. Mobs don't improve.
    pub(super) fn practise(&mut self, who: Entity, key: &str) {
        let Ok(mut fighter) = self.fighters.get_mut(who) else {
            return;
        };
        let Some(player) = fighter.player else {
            return;
        };
        let learner = Learner {
            character_id: Some(player.character_id),
            race: &player.race,
            boost: tough_fight_boost(self.outmatched.get(&who).copied()),
        };
        let lines = count_use(
            self.defs,
            &mut fighter.skills,
            &mut fighter.attributes,
            self.dice,
            self.db,
            learner,
            key,
        );
        for line in lines {
            self.say(who, line);
        }
    }

    /// What a mob does this turn instead of its auto-attack, if anything: with its `Tactics`
    /// chance, a technique it can make now (its attack ready, the stamina for it) or a spell that
    /// would do some good, picked at random. Openers start fights, so a mob already in one never
    /// picks them.
    pub fn mob_move(&mut self, attacker: Entity) -> Option<MobMove> {
        let spells = self
            .targets
            .get(&attacker)
            .map_or_else(Vec::new, |foe| self.mob_spells(attacker, *foe));
        let f = self.fighters.get(attacker).ok()?;
        let chance = f.tactics?.0;
        let (stamina, luck) = (f.vitals.stamina, f.attributes.get(Attribute::Luck));
        let usable: Vec<MobMove> = f
            .skills
            .0
            .keys()
            .filter(|key| {
                let Some(SkillKind::Technique(t)) = self.defs.skills.get(*key).map(|s| &s.def.kind)
                else {
                    return false;
                };
                let needs = match t.cost {
                    Cost::Free => 0.0,
                    Cost::Stamina(n) => n as f32,
                };
                !t.opener
                    && stamina >= needs
                    && choose_attack(
                        attacker,
                        Some(&t.with),
                        false,
                        stamina,
                        f.skills,
                        self.items,
                        self.templates,
                        self.defs,
                    )
                    .is_some_and(|s| s.skill == t.with)
            })
            .map(|key| MobMove::Technique(key.clone()))
            .chain(spells)
            .collect();
        if usable.is_empty() || !self.dice.chance(luck, f64::from(chance) / 100.0) {
            return None;
        }
        let pick = self.dice.range(0, 0..=usable.len() as i64 - 1) as usize;
        usable.into_iter().nth(pick)
    }

    /// Whether `target` is caught unawares by `attacker`: not fighting them before this round,
    /// or down, or resting.
    fn unaware(&self, attacker: Entity, target: Entity) -> bool {
        self.engaged.get(&target) != Some(&attacker)
            || self.resting.contains(&target)
            || self
                .fighters
                .get(target)
                .is_ok_and(|f| f.conditions.has("down"))
    }

    /// Bleeding and the like at the start of a fighter's turn. Returns whether they keep their
    /// turn, and whether they died.
    pub fn start_turn(&mut self, who: Entity) -> (bool, bool) {
        let name = capitalized(&self.name(who));
        let conditions = self.conditions;
        let Ok(mut fighter) = self.fighters.get_mut(who) else {
            return (false, false);
        };
        let start = fighter.conditions.start_turn(conditions);
        let room = fighter.in_room.0;
        let most = Maxima::of(
            &fighter.attributes,
            &fighter.skills,
            self.defs,
            fighter.base,
        )
        .health;
        let mut killed_by = None;
        let mut told = Vec::new();
        // What hurts or heals each turn, in each condition's words.
        for (key, amount) in &start.health {
            fighter.vitals.health = (fighter.vitals.health + *amount as f32).min(most as f32);
            let text = conditions.get(key).map(|d| &d.text);
            if let Some(each) = text.and_then(|t| t.each_turn.as_ref()) {
                let colour = if *amount < 0 { "\x1b[31m" } else { "\x1b[32m" };
                let (mine, theirs) = each.fill(&name, *amount);
                told.push((format!("{colour}{mine}\x1b[0m"), theirs));
            }
            if fighter.vitals.health <= 0.0 && killed_by.is_none() {
                killed_by = Some(key.clone());
            }
        }
        let mob = fighter.mob.map(|m| m.key.clone());
        for (mine, theirs) in told {
            self.say(who, mine);
            if let Some(theirs) = theirs {
                self.tell_room(room, who, who, &theirs);
            }
        }
        if let Some(key) = killed_by {
            self.events.write(GameEvent::Died {
                who,
                name: name.clone(),
                room,
                mob,
            });
            let kills = conditions.get(&key).and_then(|d| d.text.kills.as_ref());
            let (mine, theirs) = kills.map_or_else(
                || ("You die.".to_string(), Some(format!("{name} falls, dead."))),
                |told| told.fill(&name, 0),
            );
            self.say(who, format!("\x1b[1m{mine}\x1b[0m"));
            if let Some(theirs) = theirs {
                self.tell_room(room, who, who, &theirs);
            }
            return (false, true);
        }
        // A condition that costs the turn says so, in its own words.
        let Some(key) = start.lost_to else {
            return (true, false);
        };
        let lost = conditions.get(&key).and_then(|d| d.text.lost_turn.as_ref());
        if let Some(lost) = lost {
            let (mine, theirs) = lost.fill(&name, 0);
            self.say(who, format!("\x1b[1m{mine}\x1b[0m"));
            if let Some(theirs) = theirs {
                self.tell_room(room, who, who, &theirs);
            }
        }
        (false, false)
    }

    /// Make an attack: a plain one, or `technique`. `counter` marks a riposte, which can't itself
    /// be riposted. Returns everyone who died of it.
    pub fn strike(
        &mut self,
        attacker: Entity,
        target: Entity,
        technique: Option<Move>,
        counter: bool,
    ) -> Vec<Entity> {
        let mut died = Vec::new();
        let Ok(a) = self.fighters.get(attacker) else {
            return died;
        };
        let (room, a_name, is_player) = (a.in_room.0, name_of(a.player, a.mob), a.player.is_some());
        let chosen = a.auto.and_then(|auto| auto.0.clone());
        let a_numbers = !a.hide_numbers;
        let blessing = a.conditions.attack(self.conditions);
        let a_damage_times = a.conditions.damage_dealt(self.conditions);
        let stamina = a.vitals.stamina;
        let a_attributes = a.attributes.clone();
        let a_skills = a.skills.clone();
        let d_name = self.name(target);

        // A technique needs its attack ready and its stamina; without them, a plain attack.
        let mut technique = technique;
        if let Some(Move { key, technique: t }) = &technique {
            let tech_name = self
                .defs
                .skills
                .get(*key)
                .map_or(key.to_string(), |s| s.def.name.clone());
            let needs = match t.cost {
                Cost::Free => 0.0,
                Cost::Stamina(n) => n as f32,
            };
            let ready = choose_attack(
                attacker,
                Some(&t.with),
                is_player,
                stamina,
                &a_skills,
                self.items,
                self.templates,
                self.defs,
            )
            .is_some_and(|s| s.skill == t.with);
            let refusal = if !ready {
                let with = self
                    .defs
                    .skills
                    .get(&t.with)
                    .map_or(t.with.clone(), |s| s.def.name.clone());
                Some(format!(
                    "You need to be fighting with {with} to {tech_name}."
                ))
            } else if stamina < needs {
                Some(format!("You're too exhausted to {tech_name}."))
            } else if t.opener && !self.unaware(attacker, target) {
                Some(format!(
                    "{} sees you coming; there's no opening.",
                    capitalized(&d_name)
                ))
            } else {
                None
            };
            if let Some(refusal) = refusal {
                self.say(attacker, refusal);
                technique = None;
            } else {
                self.pay(attacker, needs);
            }
        }

        // The user's own effects happen whether or not the blow lands.
        if let Some(Move { technique: t, .. }) = &technique {
            let mut own = Telling::default();
            self.affect(attacker, target, &t.effects, Who::User, 0, &mut own);
            self.tell(own, room, attacker, target);
            if t.strike == StrikeKind::None {
                let key = technique.as_ref().map_or("", |m| m.key);
                if let Some(text) = self.defs.skills.get(key).and_then(|s| s.def.text.clone()) {
                    let (mine, _, theirs) = told(&text.hit, &a_name, &d_name);
                    self.say(attacker, mine);
                    self.tell_room(room, attacker, attacker, &theirs);
                }
                self.practise(attacker, key);
                return died;
            }
        }

        let with = technique
            .as_ref()
            .map(|m| m.technique.with.clone())
            .or(chosen);
        let Some(Strike {
            skill: key,
            damage,
            damage_type,
            text,
            cost,
        }) = choose_attack(
            attacker,
            with.as_deref(),
            is_player,
            stamina,
            &a_skills,
            self.items,
            self.templates,
            self.defs,
        )
        else {
            return died;
        };
        let Some(skill) = self.defs.skills.get(&key) else {
            return died;
        };
        if technique.is_none() && cost > 0 {
            self.pay(attacker, cost as f32);
        }
        // A technique looks like itself; a plain attack like its skill.
        let text = technique
            .as_ref()
            .and_then(|m| self.defs.skills.get(m.key)?.def.text.clone())
            .unwrap_or(text);
        let (accuracy, extra) = technique
            .as_ref()
            .map_or((0, 0), |m| (m.technique.accuracy, m.technique.damage));
        let a_aura = self.auras.get(&attacker).copied().unwrap_or_default();
        let a_boosts = boosts(&a_skills, self.defs);
        let attack_rating =
            attack_rating(&a_attributes, &a_skills, skill, a_aura, &a_boosts) + accuracy + blessing;

        let Ok(d) = self.fighters.get(target) else {
            return died;
        };
        let d_attributes = d.attributes.clone();
        let d_skills = d.skills.clone();
        let d_mob = d.mob.map(|m| m.key.clone());
        let d_numbers = !d.hide_numbers;
        let d_most = Maxima::of(&d_attributes, &d_skills, self.defs, d.base).health;
        let d_aura = self.auras.get(&target).copied().unwrap_or_default();
        let d_boosts = boosts(&d_skills, self.defs);
        let d_gear = gear(target, self.items, self.templates);
        let parts = defence_rating(&d_attributes, &d_skills, d_aura, &d_boosts, &d_gear);
        let mut defence = d.conditions.defence(parts.total(), self.conditions);
        // Someone sitting down, or caught unawares by an opener, is much easier to hit.
        let opener = technique.as_ref().is_some_and(|m| m.technique.opener);
        if self.resting.contains(&target) || opener {
            defence /= 2;
        }
        let (a_luck, d_luck) = (
            a_attributes.get(Attribute::Luck),
            d_attributes.get(Attribute::Luck),
        );
        // The attacker's skill against the defender's evasion, by opposed roll; how decisively
        // it wins is how hard the blow lands.
        let contest = self.dice.opposed(
            f64::from(attack_rating.max(1)),
            a_luck,
            f64::from(defence.max(1)),
            d_luck,
        );
        // An opener from hiding can't be dodged or blocked: Backstab from the shadows lands.
        let ambush = self.ambushers.contains(&attacker)
            && technique.as_ref().is_some_and(|m| m.technique.opener);
        let (hit, blow) = match (ambush, contest.won) {
            (true, false) => (true, Blow::Solid),
            (_, won) => (won, Blow::of(contest.margin)),
        };

        let mut tell = Telling {
            lines: Vec::new(),
            room: Vec::new(),
        };
        let harmless = technique
            .as_ref()
            .is_some_and(|m| m.technique.strike == StrikeKind::Harmless);
        let ignores_armour = technique
            .as_ref()
            .is_some_and(|m| m.technique.effects.contains(&Effect::IgnoreArmour));
        let mut dealt = 0;
        let mut blocked = false;
        if hit {
            if harmless {
                let (mine, theirs, seen) = told(&text.hit, &a_name, &d_name);
                tell.lines.push((attacker, mine));
                tell.lines
                    .push((target, format!("\x1b[31m{theirs}\x1b[0m")));
                tell.room.push(seen);
            } else {
                let (low, high) = (damage.0.min(damage.1), damage.0.max(damage.1));
                let rolled = self.dice.range(a_luck, i64::from(low)..=i64::from(high)) as i32;
                // Armour takes its share off, even all of it; a point can find the gaps.
                let pierce = a_boosts.pierce_with(&key)
                    + technique
                        .as_ref()
                        .map_or(0, |m| effects::pierce(&m.technique.effects));
                let force = rolled
                    + a_attributes.get(Attribute::Strength) / 2
                    + a_boosts.damage_with(&key)
                    + extra;
                // Conditions on either side move it, weakened, strengthened and the like, and
                // the target's armour and resistances have their say.
                let times = blow.multiplier() * a_damage_times;
                dealt = self.mitigate(
                    target,
                    f64::from(force) * times,
                    &damage_type,
                    pierce,
                    ignores_armour,
                );
                // A ward takes what it can, told after the blow it stopped.
                let mut ward = Telling::default();
                let warded = self.ward_off(target, attacker, dealt, &d_name, &mut ward);
                dealt -= warded;
                // Told in the attack's words, with how much of them it took: "Your slash
                // decimates the rat." A crushing blow ends with a bang.
                let with = [
                    ("actor", a_name.as_str()),
                    ("target", d_name.as_str()),
                    ("harm", harm(dealt, d_most)),
                ];
                let say = |template: &str| {
                    let line = fill(template, &with);
                    match (blow, line.strip_suffix('.')) {
                        (Blow::Crushing, Some(line)) => format!("{line}!"),
                        _ => line,
                    }
                };
                let count = |shown: bool| {
                    if shown {
                        format!(" ({dealt})")
                    } else {
                        String::new()
                    }
                };
                let (mine, seen) = (say(&text.hit.actor), say(&text.hit.room));
                let theirs = text.hit.target.as_deref().map(say).unwrap_or_default();
                tell.lines
                    .push((attacker, format!("{mine}{}", count(a_numbers))));
                tell.lines.push((
                    target,
                    format!("\x1b[31m{theirs}{}\x1b[0m", count(d_numbers)),
                ));
                tell.room.push(seen);
                tell.lines.append(&mut ward.lines);
                tell.room.append(&mut ward.room);
                if dealt == 0 && warded == 0 {
                    let line = format!("The blow glances off {d_name}'s armour.");
                    tell.lines.push((attacker, line.clone()));
                    tell.lines
                        .push((target, "The blow glances off your armour.".into()));
                    tell.room.push(line);
                }
                if d_gear.reduction > 0 {
                    self.events.write(GameEvent::HitInArmour { who: target });
                }
                if dealt > 0
                    && self.wound(
                        target,
                        dealt,
                        &mut tell,
                        attacker,
                        &d_name,
                        room,
                        d_mob.clone(),
                    )
                {
                    died.push(target);
                }
            }
            // The technique's effects on the target, if it's still standing.
            if let Some(Move { technique: t, .. }) = &technique {
                // Told in the conditions' own words: "The rat is staggered!".
                if !died.contains(&target) {
                    self.affect(attacker, target, &t.effects, Who::Target, 0, &mut tell);
                }
                // Cleave: the blow carries on into another foe fighting the attacker, even
                // when it fells the first.
                if t.effects.contains(&Effect::Cleave) && dealt > 0 {
                    let second = self.targets.iter().find_map(|(who, whom)| {
                        (*whom == attacker && *who != target && !died.contains(who))
                            .then_some(*who)
                            .filter(|who| {
                                self.fighters
                                    .get(*who)
                                    .is_ok_and(|f| f.in_room.0 == room && !f.dead)
                            })
                    });
                    if let Some(second) = second {
                        let second_name = self.name(second);
                        let second_mob = self
                            .fighters
                            .get(second)
                            .ok()
                            .and_then(|f| f.mob.map(|m| m.key.clone()));
                        tell.lines.push((
                            attacker,
                            format!("Your blow carries on into {second_name}! ({dealt})"),
                        ));
                        tell.lines.push((
                            second,
                            format!(
                                "\x1b[31m{}'s blow carries on into you! ({dealt})\x1b[0m",
                                capitalized(&a_name)
                            ),
                        ));
                        tell.room.push(format!(
                            "{}'s blow carries on into {second_name}.",
                            capitalized(&a_name)
                        ));
                        if self.wound(
                            second,
                            dealt,
                            &mut tell,
                            attacker,
                            &second_name,
                            room,
                            second_mob,
                        ) {
                            died.push(second);
                        }
                    }
                }
            }
        } else {
            // The miss is the shield's in proportion to its share of the defence, and otherwise
            // a dodge. Either is told in the words of the defender's skill for it, if they have
            // one: a shield untrained says so plainly, and a miss nobody dodged is just a miss.
            let share = f64::from(parts.block) / f64::from(parts.total().max(1));
            blocked = parts.block > 0 && self.dice.chance(0, share);
            let words = |trigger: Trigger| {
                d_skills.0.keys().find_map(|key| {
                    let skill = self.defs.skills.get(key)?;
                    match &skill.def.kind {
                        SkillKind::Passive { triggers, .. } if triggers.contains(&trigger) => {
                            skill.def.text.as_ref().map(|t| t.hit.clone())
                        }
                        _ => None,
                    }
                })
            };
            let avoided = if blocked {
                Some(words(Trigger::Blocked).unwrap_or_else(shield_block))
            } else {
                words(Trigger::Dodged)
            };
            if let Some(avoided) = avoided {
                // The defender is the one using the skill.
                let (theirs, mine, seen) = told(&avoided, &d_name, &a_name);
                tell.lines.push((attacker, mine));
                tell.lines.push((target, theirs));
                tell.room.push(seen);
            } else {
                let miss = text
                    .miss
                    .clone()
                    .or_else(|| untrained().miss)
                    .unwrap_or(text.hit);
                let (mine, theirs, seen) = told(&miss, &a_name, &d_name);
                tell.lines.push((attacker, mine));
                tell.lines.push((target, theirs));
                tell.room.push(seen);
            }
            self.events.write(if blocked {
                GameEvent::Blocked { who: target }
            } else {
                GameEvent::Dodged { who: target }
            });
        }

        for (who, line) in tell.lines {
            self.say(who, line);
        }
        let room_line = tell.room.join(" ");
        self.tell_room(room, attacker, target, &room_line);

        // Players practise what they used.
        self.practise(attacker, &key);
        if let Some(Move { key, .. }) = technique {
            self.practise(attacker, key);
        }

        // A dodge can be answered at once.
        if !hit && !blocked && !counter && !died.contains(&target) {
            died.extend(self.riposte(target, attacker));
        }
        died
    }

    /// Let `who`'s ward, if they have one, take what it can of `damage`, and say so. Returns how
    /// much it took.
    pub(super) fn ward_off(
        &mut self,
        who: Entity,
        attacker: Entity,
        damage: i32,
        name: &str,
        tell: &mut Telling,
    ) -> i32 {
        let conditions = self.conditions;
        let Ok(mut f) = self.fighters.get_mut(who) else {
            return 0;
        };
        let hide_numbers = f.hide_numbers;
        let absorbed = f.conditions.absorb_by(damage, conditions);
        let name = capitalized(name);
        for by in &absorbed {
            // In the condition's own words: "Your ward takes the blow (3)." and, spent, "Your
            // ward fails!".
            let Some(text) = conditions.get(&by.key).map(|d| &d.text) else {
                continue;
            };
            let breaks = text.breaks.as_ref().filter(|_| by.spent);
            for told in text.absorbs.iter().chain(breaks) {
                let mut told = told.clone();
                if hide_numbers {
                    told.bearer = told.bearer.replace(" ({amount})", "");
                }
                let (mine, seen) = told.fill(&name, by.taken);
                tell.lines.push((who, mine));
                if let Some(seen) = seen {
                    tell.lines.push((attacker, seen.clone()));
                    tell.room.push(seen);
                }
            }
        }
        absorbed.iter().map(|by| by.taken).sum()
    }

    /// Take damage. Returns whether it killed them.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn wound(
        &mut self,
        who: Entity,
        dealt: i32,
        tell: &mut Telling,
        attacker: Entity,
        name: &str,
        room: Entity,
        mob: Option<crate::world::MobKey>,
    ) -> bool {
        let defs = self.defs;
        let Ok(mut f) = self.fighters.get_mut(who) else {
            return false;
        };
        f.vitals.health -= dealt as f32;
        let health = f.vitals.health;
        let max = Maxima::of(&f.attributes, &f.skills, defs, f.base).health;
        self.events.write(GameEvent::Damaged {
            who,
            amount: dealt as f32,
        });
        if health <= 0.0 {
            if let Ok(mut f) = self.fighters.get_mut(who) {
                f.vitals.health = 0.0;
            }
            tell.lines
                .push((attacker, format!("\x1b[1mYou have killed {name}!\x1b[0m")));
            tell.room
                .push(format!("{} falls, dead.", capitalized(name)));
            self.events.write(GameEvent::Died {
                who,
                name: name.to_string(),
                room,
                mob,
            });
            true
        } else {
            tell.lines.push((
                attacker,
                format!("{} is {}.", capitalized(name), condition(health, max)),
            ));
            false
        }
    }

    /// After a dodge: a defender who knows a counter for the attack they fight with strikes back
    /// at once, for a little stamina.
    fn riposte(&mut self, defender: Entity, attacker: Entity) -> Vec<Entity> {
        let Ok(d) = self.fighters.get(defender) else {
            return Vec::new();
        };
        let chosen = d.auto.and_then(|auto| auto.0.clone());
        let Some(strike) = choose_attack(
            defender,
            chosen.as_deref(),
            d.player.is_some(),
            d.vitals.stamina,
            d.skills,
            self.items,
            self.templates,
            self.defs,
        ) else {
            return Vec::new();
        };
        let counter = d.skills.0.keys().find(|key| {
            matches!(
                self.defs.skills.get(*key).map(|s| &s.def.kind),
                Some(SkillKind::Passive { counter: Some(with), .. }) if *with == strike.skill
            )
        });
        let Some(counter) = counter.cloned() else {
            return Vec::new();
        };
        if !self.pay(defender, RIPOSTE_COST) {
            return Vec::new();
        }
        if let Some(text) = self
            .defs
            .skills
            .get(&counter)
            .and_then(|s| s.def.text.clone())
        {
            let room = self.fighters.get(defender).map(|f| f.in_room.0);
            let (mine, theirs, seen) = told(&text.hit, &self.name(defender), &self.name(attacker));
            self.say(defender, format!("\x1b[1m{mine}\x1b[0m"));
            self.say(attacker, format!("\x1b[1m{theirs}\x1b[0m"));
            if let Ok(room) = room {
                self.tell_room(room, defender, attacker, &seen);
            }
        }
        self.practise(defender, &counter);
        self.strike(defender, attacker, None, true)
    }
}
