//! Making an attack: choosing it, rolling it, and what follows. Plain attacks, techniques and
//! ripostes all come through [`Round::strike`].

use std::collections::{HashMap, HashSet};

use bevy::ecs::query::QueryData;
use bevy::prelude::*;

use super::states::{Conditions, State};
use super::{
    ActionQueue, Aggressive, AutoAttack, BASIC_ATTACK, CombatLog, Dead, Fighting, MaxHealth,
};
use crate::attributes::{Attribute, Attributes};
use crate::db::Database;
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::items::{ItemLocation, ItemQuery, ItemTemplates, capitalized};
use crate::mobs::Mob;
use crate::output::Outgoing;
use crate::skills::{
    Bonus, Cost, Effect, SkillDefs, SkillKind, Skills, StrikeKind, Technique, boosts, count_use,
};
use crate::vitals::{Maxima, Vitals};
use crate::world::{InRoom, Player};

/// The skill that helps avoid attacks.
const DODGE: &str = "athletics:dodge";
/// Levels of a weapon skill for each extra point of damage with that weapon.
const LEVELS_PER_DAMAGE: i32 = 10;
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
    pub most: Option<&'static MaxHealth>,
    pub queue: Option<&'static mut ActionQueue>,
    pub aggressive: Has<Aggressive>,
    pub dead: Has<Dead>,
    pub player: Option<&'static Player>,
    pub mob: Option<&'static Mob>,
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

/// The most health a fighter can have.
pub fn max_health(
    attributes: &Attributes,
    skills: &Skills,
    defs: &SkillDefs,
    override_: Option<&MaxHealth>,
) -> i32 {
    override_.map_or_else(|| Maxima::of(attributes, skills, defs).health, |m| m.0)
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

/// "punches" for "punch", "slashes" for "slash".
pub fn third_person(verb: &str) -> String {
    // A phrasal verb changes its first word: "lunge at" is "lunges at".
    let (first, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    let first = if ["s", "x", "z", "sh", "ch"]
        .iter()
        .any(|end| first.ends_with(end))
    {
        format!("{first}es")
    } else {
        format!("{first}s")
    };
    if rest.is_empty() {
        first
    } else {
        format!("{first} {rest}")
    }
}

/// The chance an attack hits: the attacker's skill against the defender's evasion, as a ratio.
pub fn hit_chance(attack: i32, defence: i32) -> f64 {
    let (attack, defence) = (f64::from(attack.max(1)), f64::from(defence.max(1)));
    attack / (attack + defence)
}

/// The attack a fighter makes this round: which skill, how much damage, and the verb.
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    /// The skill it's made with, and practises.
    pub skill: String,
    pub damage: (u32, u32),
    pub verb: String,
    /// Extra damage from training in the weapon.
    pub bonus: i32,
    /// Stamina it takes.
    pub cost: u32,
}

/// The attack a fighter makes this round: their chosen attack if they can make it now, otherwise
/// the weapon in their hand, otherwise the basic attack.
///
/// A weapon does its own damage in anyone's hands. Trained in its skill, a fighter attacks with
/// that skill and does a point more for every ten levels; untrained, they swing it with only
/// their Brawling to guide them, and learn nothing of the weapon by it. Players must know an attack
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
    let weapon = items.iter().find_map(|(_, item, location)| {
        (*location == ItemLocation::Held(entity))
            .then(|| templates.0.get(&item.key)?.weapon.as_ref())
            .flatten()
    });
    let strike = |key: &str| {
        let skill = defs.skills.get(key)?;
        let SkillKind::Attack { verb, damage, cost } = &skill.def.kind else {
            return None;
        };
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
        let level = skills.0.get(key).map_or(0, |s| s.level as i32);
        match weapon {
            Some(w) if w.skill == key => Some(Strike {
                skill: key.to_string(),
                damage: w.damage,
                verb: w.verb.clone().unwrap_or_else(|| verb.clone()),
                bonus: level / LEVELS_PER_DAMAGE,
                cost,
            }),
            _ => damage.map(|damage| Strike {
                skill: key.to_string(),
                damage,
                verb: verb.clone(),
                bonus: 0,
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
                    verb: "hit".into(),
                    ..brawl
                },
                None => brawl,
            })
        })
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
    pub dice: &'a mut Dice,
    pub db: &'a Database,
    pub events: &'a mut MessageWriter<'ew, GameEvent>,
    pub log: &'a mut CombatLog,
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
}

/// Messages for a strike: the attacker, the target, and everyone else there.
struct Telling {
    lines: Vec<(Entity, String)>,
    room: Vec<String>,
}

impl Round<'_, '_, '_, '_, '_, '_> {
    fn say(&mut self, who: Entity, line: impl Into<String>) {
        self.log.0.push(Outgoing::line(who, line.into()));
    }

    /// Tell everyone in `room` but `a` and `b`.
    fn tell_room(&mut self, room: Entity, a: Entity, b: Entity, line: &str) {
        for &(player, at) in self.watchers {
            if at == room && player != a && player != b {
                self.log.0.push(Outgoing::line(player, line.to_string()));
            }
        }
    }

    fn name(&self, entity: Entity) -> String {
        self.fighters
            .get(entity)
            .map(|f| name_of(f.player, f.mob))
            .unwrap_or_else(|_| "someone".into())
    }

    /// Spend a fighter's stamina, if they have it.
    fn pay(&mut self, who: Entity, cost: f32) -> bool {
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
    fn practise(&mut self, who: Entity, key: &str) {
        let Ok(mut fighter) = self.fighters.get_mut(who) else {
            return;
        };
        let Some(character_id) = fighter.player.map(|p| p.character_id) else {
            return;
        };
        let lines = count_use(
            self.defs,
            &mut fighter.skills,
            &mut fighter.attributes,
            self.dice,
            self.db,
            character_id,
            key,
        );
        for line in lines {
            self.say(who, line);
        }
    }

    /// Whether `target` is caught unawares by `attacker`: not fighting them before this round,
    /// or down, or resting.
    fn unaware(&self, attacker: Entity, target: Entity) -> bool {
        self.engaged.get(&target) != Some(&attacker)
            || self.resting.contains(&target)
            || self
                .fighters
                .get(target)
                .is_ok_and(|f| f.conditions.has(State::Down))
    }

    /// Bleeding and the like at the start of a fighter's turn. Returns whether they keep their
    /// turn, and whether they died.
    pub fn start_turn(&mut self, who: Entity) -> (bool, bool) {
        let name = self.name(who);
        let Ok(mut fighter) = self.fighters.get_mut(who) else {
            return (false, false);
        };
        let start = fighter.conditions.start_turn();
        let room = fighter.in_room.0;
        let mut died = false;
        if start.bleeding > 0 {
            fighter.vitals.health -= start.bleeding as f32;
            died = fighter.vitals.health <= 0.0;
            self.say(
                who,
                format!("\x1b[31mYou bleed. ({})\x1b[0m", start.bleeding),
            );
            self.tell_room(room, who, who, &format!("{} bleeds.", capitalized(&name)));
            if died {
                self.events.write(GameEvent::Died {
                    who,
                    name: name.clone(),
                    room,
                    mob: self
                        .fighters
                        .get(who)
                        .ok()
                        .and_then(|f| f.mob.map(|m| m.key.clone())),
                });
                self.say(who, "\x1b[1mYou bleed out.\x1b[0m");
                self.tell_room(
                    room,
                    who,
                    who,
                    &format!("{} bleeds out and falls, dead.", capitalized(&name)),
                );
                return (false, true);
            }
        }
        match start.lost_to {
            Some(State::Staggered) => {
                self.say(who, "\x1b[1mYou reel, staggered, and can't act!\x1b[0m");
                self.tell_room(
                    room,
                    who,
                    who,
                    &format!("{} reels, staggered.", capitalized(&name)),
                );
                (false, false)
            }
            Some(State::Down) => {
                self.say(who, "\x1b[1mYou struggle back to your feet.\x1b[0m");
                self.tell_room(
                    room,
                    who,
                    who,
                    &format!("{} struggles back up.", capitalized(&name)),
                );
                (false, false)
            }
            _ => (true, died),
        }
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
            for effect in &t.effects {
                let state = match *effect {
                    Effect::Expose => Some((State::Exposed, 1)),
                    Effect::Guard { defence, rounds } => Some((State::Guarding(defence), rounds)),
                    _ => None,
                };
                if let (Some((state, turns)), Ok(mut f)) = (state, self.fighters.get_mut(attacker))
                {
                    f.conditions.add(state, turns);
                }
            }
            if t.strike == StrikeKind::None {
                let verbs = third_person(&t.verb);
                self.say(attacker, format!("You {}.", t.verb));
                self.tell_room(
                    room,
                    attacker,
                    attacker,
                    &format!("{} {verbs}.", capitalized(&a_name)),
                );
                if let Some(Move { key, .. }) = technique {
                    self.practise(attacker, key);
                }
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
            verb,
            bonus,
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
        let verb = technique
            .as_ref()
            .map_or(verb, |m| m.technique.verb.clone());
        let (accuracy, extra) = technique
            .as_ref()
            .map_or((0, 0), |m| (m.technique.accuracy, m.technique.damage));
        let a_aura = self.auras.get(&attacker).copied().unwrap_or_default();
        let a_boosts = boosts(&a_skills, self.defs);
        let level = a_skills.0.get(&key).map_or(0, |s| s.level as i32);
        let attack_rating = level
            + a_attributes.get(skill.attribute)
            + a_aura.attack
            + a_boosts.attack_with(&key)
            + accuracy;

        let Ok(d) = self.fighters.get(target) else {
            return died;
        };
        let d_attributes = d.attributes.clone();
        let d_skills = d.skills.clone();
        let d_mob = d.mob.map(|m| m.key.clone());
        let dodge = d_skills.0.get(DODGE).map_or(0, |s| s.level as i32);
        let d_aura = self.auras.get(&target).copied().unwrap_or_default();
        let d_boosts = boosts(&d_skills, self.defs);
        let mut defence = d.conditions.defence(
            d_attributes.get(Attribute::Reaction) + dodge + d_aura.defence + d_boosts.defence,
        );
        // Someone sitting down, or caught unawares by an opener, is much easier to hit.
        let opener = technique.as_ref().is_some_and(|m| m.technique.opener);
        if self.resting.contains(&target) || opener {
            defence /= 2;
        }
        let (a_luck, d_luck) = (
            a_attributes.get(Attribute::Luck),
            d_attributes.get(Attribute::Luck),
        );
        let hit = self
            .dice
            .contest(a_luck, d_luck, hit_chance(attack_rating, defence));

        let verbs = third_person(&verb);
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
        if hit {
            if harmless {
                tell.lines.push((attacker, format!("You {verb} {d_name}.")));
                tell.lines.push((
                    target,
                    format!("\x1b[31m{} {verbs} you.\x1b[0m", capitalized(&a_name)),
                ));
                tell.room
                    .push(format!("{} {verbs} {d_name}.", capitalized(&a_name)));
            } else {
                let (low, high) = (damage.0.min(damage.1), damage.0.max(damage.1));
                let rolled = self.dice.range(a_luck, i64::from(low)..=i64::from(high)) as i32;
                let armour = if ignores_armour {
                    0
                } else {
                    d_aura.armour + d_boosts.armour
                };
                dealt = (rolled
                    + a_attributes.get(Attribute::Strength) / 2
                    + bonus
                    + a_boosts.damage_with(&key)
                    + extra
                    - armour)
                    .max(1);
                tell.lines
                    .push((attacker, format!("You {verb} {d_name}. ({dealt})")));
                tell.lines.push((
                    target,
                    format!(
                        "\x1b[31m{} {verbs} you. ({dealt})\x1b[0m",
                        capitalized(&a_name)
                    ),
                ));
                tell.room
                    .push(format!("{} {verbs} {d_name}.", capitalized(&a_name)));
                if self.wound(
                    target,
                    dealt,
                    &mut tell,
                    attacker,
                    &d_name,
                    room,
                    d_mob.clone(),
                ) {
                    died.push(target);
                }
            }
            // The technique's effects on the target, if it's still standing.
            if let Some(Move { technique: t, .. }) = &technique {
                let standing = !died.contains(&target);
                for effect in t.effects.iter().filter(|_| standing) {
                    let (state, turns, said) = match *effect {
                        Effect::Stagger => (State::Staggered, 1, "is staggered"),
                        Effect::KnockDown => (State::Down, 1, "goes down"),
                        Effect::Bleed { damage, rounds } => {
                            (State::Bleeding(damage), rounds, "is bleeding")
                        }
                        Effect::Hold { rounds } => (State::Held, rounds, "is caught in a hold"),
                        _ => continue,
                    };
                    if let Ok(mut f) = self.fighters.get_mut(target) {
                        f.conditions.add(state, turns);
                    }
                    let line = format!("{} {said}!", capitalized(&d_name));
                    tell.lines.push((attacker, line.clone()));
                    tell.room.push(line);
                    tell.lines.push((
                        target,
                        format!(
                            "\x1b[1mYou {}!\x1b[0m",
                            match state {
                                State::Staggered => "are staggered",
                                State::Down => "go down",
                                State::Bleeding(_) => "are bleeding",
                                State::Held => "are caught in a hold",
                                _ => "are hurt",
                            }
                        ),
                    ));
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
            let missed = if technique.is_some() {
                format!("You try to {verb} {d_name}, but miss.")
            } else {
                format!("You miss {d_name}.")
            };
            tell.lines.push((attacker, missed));
            tell.lines.push((
                target,
                format!("{} {verbs} at you, but misses.", capitalized(&a_name)),
            ));
            tell.room
                .push(format!("{} misses {d_name}.", capitalized(&a_name)));
            self.events.write(GameEvent::Dodged { who: target });
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
        if !hit && !counter && !died.contains(&target) {
            died.extend(self.riposte(target, attacker));
        }
        died
    }

    /// Take damage. Returns whether it killed them.
    #[allow(clippy::too_many_arguments)]
    fn wound(
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
        let max = max_health(&f.attributes, &f.skills, defs, f.most);
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
        self.say(defender, "\x1b[1mYou riposte!\x1b[0m");
        self.practise(defender, &counter);
        self.strike(defender, attacker, None, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbs_read_naturally() {
        assert_eq!(third_person("punch"), "punches");
        assert_eq!(third_person("bite"), "bites");
        assert_eq!(third_person("slash"), "slashes");
        assert_eq!(third_person("lunge at"), "lunges at");
        assert_eq!(
            third_person("take a guarding stance"),
            "takes a guarding stance"
        );
    }

    #[test]
    fn hits_follow_the_ratio_rule() {
        assert!((hit_chance(6, 3) - 2.0 / 3.0).abs() < 1e-9);
        assert!((hit_chance(20, 10) - hit_chance(40, 20)).abs() < 1e-9);
        assert!(hit_chance(0, 0) > 0.0, "nothing is impossible");
    }
}
