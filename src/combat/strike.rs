//! Making an attack: choosing it, rolling it, and what follows. Plain attacks, techniques and
//! ripostes all come through [`Round::strike`].

use std::collections::{HashMap, HashSet};

use bevy::ecs::query::QueryData;
use bevy::prelude::*;

use super::casting::MobMove;
use super::resolve::{BlowDamage, Use};
use super::states::{ConditionDefs, Conditions, Imbued};
use super::{
    ActionQueue, Aggressive, AutoAttack, BASIC_ATTACK, Dead, Fighting, HideNumbers, Tactics,
};
use crate::attributes::{Attribute, Attributes};
use crate::db::Database;
use crate::dice::Dice;
use crate::effects::{self, Resist, Who};
use crate::events::GameEvent;
use crate::facts::Facts;
use crate::items::{ItemLocation, ItemQuery, ItemTemplates, capitalized};
use crate::mobs::{Mob, MobTemplates};
use crate::money::Purse;
use crate::output::PuppetedBy;
use crate::output::{Outbox, Outgoing};
use crate::skills::{
    Bonus, Boosts, Cost, Learner, Lines, Skill, SkillDefs, SkillKind, SkillText, Skills,
    StrikeKind, Technique, boosts, count_use, fill, tough_fight_boost,
};
use crate::spells::{SpellDefs, Spells};
use crate::targeting::Keywords;
use crate::vitals::{BaseHealth, Maxima, Vitals};
use crate::world::{ArmourDef, InRoom, Player, WeaponDef};

/// The skill that helps avoid attacks.
const DODGE: &str = "athletics:dodge";
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
    /// A thing, not a creature: it never acts in a fight.
    pub inanimate: Has<super::Inanimate>,
    /// The coins they carry: a player's, or a mob's for a pickpocket and its slayer.
    pub purse: Option<&'static mut Purse>,
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

/// How battered a thing looks (a training dummy), as `condition` says how hurt someone is.
fn battered(health: f32, max: i32) -> &'static str {
    match health / max.max(1) as f32 {
        f if f >= 1.0 => "untouched",
        f if f >= 0.75 => "scuffed",
        f if f >= 0.5 => "battered",
        f if f >= 0.25 => "coming apart",
        _ => "about to fall to pieces",
    }
}

/// An attack's rating with `skill`: its level and attribute, plus auras and passive bonuses.
fn attack_rating(
    attributes: &Attributes,
    skills: &Skills,
    skill: &Skill,
    aura: Bonus,
    boosts: &Boosts,
) -> f64 {
    let level = skills.0.get(&skill.key).map_or(0, |s| s.level as i32);
    f64::from(level + attributes.get(skill.attribute) + aura.attack)
        + boosts.attack_with(&skill.key)
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
    let penalty = (f64::from(gear.penalty) - boosts.armour_ease).max(0.0);
    Defence {
        evasion: f64::from(attributes.get(Attribute::Reaction) + dodge + aura.defence)
            + boosts.defence
            - penalty,
        block: if gear.block > 0 {
            f64::from(gear.block) + boosts.block
        } else {
            0.0
        },
    }
}

/// Defence before conditions, in its two parts: getting out of the way, and a shield in the way.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Defence {
    /// Reaction, Dodge and the rest, less what armour takes off.
    pub evasion: f64,
    /// A held shield's block.
    pub block: f64,
}

impl Defence {
    pub fn total(self) -> f64 {
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

/// A fighter's attack and defence ratings this round, as the ratio rule weighs them.
pub fn ratings(
    entity: Entity,
    fighters: &Fighters,
    auras: &HashMap<Entity, Bonus>,
    items: &ItemQuery,
    templates: &ItemTemplates,
    defs: &SkillDefs,
) -> Option<(f64, f64)> {
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
    Some((attack.max(1.0), defence.max(1.0)))
}

/// How far a side is outmatched by its foes, by the ratio rule: their attack against its
/// defence, and their defence against its attack, averaged (a geometric mean). Each side's
/// attacks and defences add up, as `(attack, defence)` ratings: five against an ogre isn't a
/// tough fight even if it outclasses each of them, and ten rats are. 1 is an even fight; above
/// it, the foes are stronger.
pub fn side_odds(side: &[(f64, f64)], foes: &[(f64, f64)]) -> Option<f64> {
    let sum = |ratings: &[(f64, f64)]| {
        ratings
            .iter()
            .fold((0.0, 0.0), |(a, d), (attack, defence)| {
                (a + attack, d + defence)
            })
    };
    let ((attack, defence), (their_attack, their_defence)) = (sum(side), sum(foes));
    (attack > 0.0 && their_attack > 0.0)
        .then(|| (their_attack / defence * their_defence / attack).sqrt())
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
    /// The weapon it's made with, whose conditions ride on it: none for fists or fangs.
    pub weapon: Option<Entity>,
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
    let weapon = items.iter().find_map(|(held, item, location)| {
        let w = (*location == ItemLocation::Held(entity))
            .then(|| templates.0.get(&item.key)?.weapon.clone())
            .flatten()?;
        Some((
            held,
            WeaponDef {
                damage: item.quality.damage(w.damage),
                ..w
            },
        ))
    });
    let (held, weapon) = weapon.unzip();
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
                weapon: held,
            }),
            _ => damage.map(|damage| Strike {
                skill: key.to_string(),
                damage,
                damage_type: damage_type.clone(),
                text,
                cost,
                weapon: None,
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
                    weapon: held,
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
pub(super) fn shield_block() -> Lines {
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
    /// Mob templates, for who counts whom an ally.
    pub mobs: &'a MobTemplates,
    pub defs: &'a SkillDefs,
    pub conditions: &'a ConditionDefs,
    pub spells: &'a SpellDefs,
    pub dice: &'a mut Dice,
    pub db: &'a Database,
    /// What the game remembers, for the requirements of skills a fighter might discover.
    pub story: Option<&'a Facts>,
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
    /// Conditions on items, read as weapons strike and changed as they're imbued.
    pub imbued: &'a mut Imbued,
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

    /// Tell everyone in `room` but `a` and `b`; while `a` is hidden, only those who've noticed
    /// them, as for every room message about a hidden character (`output.rs`).
    pub(super) fn tell_room(&mut self, room: Entity, a: Entity, b: Entity, line: &str) {
        for &(player, at) in self.watchers {
            let sees = self
                .fighters
                .get(a)
                .map_or(true, |f| f.conditions.seen_by(player));
            if at == room && player != a && player != b && sees {
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
            self.story,
            learner,
            key,
        );
        for line in lines {
            self.say(who, line);
        }
    }

    /// What a mob does this turn instead of its auto-attack, if anything: with its `Tactics`
    /// chance, a technique it can make now (its attack ready, the stamina for it), or a spell or
    /// active skill that would do some good, picked at random. Openers start fights, so a mob already in one never
    /// picks them.
    pub fn mob_move(&mut self, attacker: Entity) -> Option<MobMove> {
        let (spells, abilities) = self.targets.get(&attacker).map_or_else(
            || (Vec::new(), Vec::new()),
            |foe| {
                (
                    self.mob_spells(attacker, *foe),
                    self.mob_abilities(attacker, *foe),
                )
            },
        );
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
            .chain(abilities)
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
        for over in &start.ended {
            self.events.write(GameEvent::ConditionFaded {
                who,
                condition: over.key.clone(),
                strength: over.strength,
            });
        }
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
            let before = fighter.vitals.health;
            fighter.vitals.health = (before + *amount as f32).min((most as f32).max(before));
            // Healing someone already whole does nothing, and says nothing; otherwise it tells
            // what it really did.
            let amount = if *amount > 0 {
                (fighter.vitals.health - before).round() as i32
            } else {
                *amount
            };
            if amount == 0 {
                continue;
            }
            let text = conditions.get(key).map(|d| &d.text);
            if let Some(each) = text.and_then(|t| t.each_turn.as_ref()) {
                let colour = if amount < 0 { "\x1b[31m" } else { "\x1b[32m" };
                let (mine, theirs) = each.fill(&name, amount);
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
        let blessing = a.conditions.attack(self.conditions);
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
            self.affect(
                attacker,
                target,
                &t.effects,
                Who::User,
                0,
                1.0,
                false,
                &mut own,
            );
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
            weapon,
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
        let t = technique.as_ref().map(|m| m.technique);
        let (accuracy, extra) = t.map_or((0, 0), |t| (t.accuracy, t.damage));
        let a_aura = self.auras.get(&attacker).copied().unwrap_or_default();
        let a_boosts = boosts(&a_skills, self.defs);
        let rating = attack_rating(&a_attributes, &a_skills, skill, a_aura, &a_boosts)
            + f64::from(accuracy + blessing);
        // A grab only applies its effects; anything else hurts.
        let harmless = t.is_some_and(|t| t.strike == StrikeKind::Harmless);
        let blow = (!harmless).then(|| BlowDamage {
            damage,
            bonus: f64::from(a_attributes.get(Attribute::Strength) / 2 + extra)
                + a_boosts.damage_with(&key),
            damage_type,
            pierce: a_boosts.pierce_with(&key),
        });
        // What's on the weapon rides on the blow: a venom coating, a flaming edge. Its charges
        // are spent only if the blow lands.
        let mut riding = weapon
            .filter(|_| blow.is_some())
            .and_then(|w| Some((w, self.imbued.items.get(&w)?.clone())));
        let (on_hit, spent) = riding
            .as_mut()
            .map(|(_, c)| c.strike(self.conditions))
            .unwrap_or_default();
        let mut all_effects = t.map_or_else(Vec::new, |t| t.effects.clone());
        all_effects.extend(on_hit);
        let outcome = self.resolve(Use {
            user: attacker,
            target,
            rating,
            resist: t.map_or(Some(Resist::Evasion), |t| t.resist),
            against: None,
            tags: effects::tags(
                t.map_or(&[][..], |t| &t.tags),
                &all_effects,
                self.conditions,
            ),
            blow,
            effects: &all_effects,
            power: 0,
            text: &text,
            opener: t.is_some_and(|t| t.opener),
            harmful: true,
            reacting: false,
        });
        died.extend(outcome.died);
        if outcome.hit
            && let Some((weapon, conditions)) = riding
        {
            self.imbued.items.insert(weapon, conditions);
            self.imbued.changed.insert(weapon);
            // Told in the condition's own words as it runs out: "The venom on your blade is gone."
            let name = capitalized(&a_name);
            for key in spent {
                if let Some(fades) = self
                    .conditions
                    .get(&key)
                    .and_then(|d| d.text.fades.as_ref())
                {
                    let (mine, seen) = fades.fill(&name, 0);
                    self.say(attacker, mine);
                    if let Some(seen) = seen {
                        self.tell_room(room, attacker, attacker, &seen);
                    }
                }
            }
        }

        // Players practise what they used.
        self.practise(attacker, &key);
        if let Some(Move { key, .. }) = technique {
            self.practise(attacker, key);
        }

        // A dodge can be answered at once.
        if outcome.dodged && !counter && !died.contains(&target) {
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
        // A thing is broken, not killed, and battered, not hurt.
        let thing = f.inanimate;
        self.events.write(GameEvent::Damaged {
            who,
            amount: dealt as f32,
        });
        if health <= 0.0 {
            if let Ok(mut f) = self.fighters.get_mut(who) {
                f.vitals.health = 0.0;
            }
            let (yours, seen) = if thing {
                (
                    format!("\x1b[1mYou knock {name} to pieces!\x1b[0m"),
                    format!("{} falls to pieces.", capitalized(name)),
                )
            } else {
                (
                    format!("\x1b[1mYou have killed {name}!\x1b[0m"),
                    format!("{} falls, dead.", capitalized(name)),
                )
            };
            tell.lines.push((attacker, yours));
            tell.room.push(seen);
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
                format!(
                    "{} is {}.",
                    capitalized(name),
                    if thing {
                        battered(health, max)
                    } else {
                        condition(health, max)
                    }
                ),
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
