//! Casting a spell, in a fight or out of one: its Drain, the contest against whatever resists it,
//! and what it does. Damage, deaths and conditions go the way a blow's do (see `strike.rs`).

use bevy::prelude::*;

use super::Round;
use super::states::{Conditions, State};
use super::strike::{Blow, Telling, defence_rating, gear, harm, name_of, told};
use crate::attributes::{Attribute, Attributes};
use crate::events::GameEvent;
use crate::items::capitalized;
use crate::skills::{Skills, boosts, fill};
use crate::spells::{
    Aim, Resist, SpellEffect, can_cast, choose_target, drain_paid, drain_resistance, power,
    takes_hold,
};
use crate::stealth::{Hidden, sees};
use crate::targeting::Keywords;
use crate::vitals::Maxima;
use crate::world::MobKey;

/// What came of a cast.
#[derive(Debug, Default)]
pub struct Cast {
    /// Who died of it.
    pub died: Vec<Entity>,
    /// An animal it calmed, to be let out of its fights.
    pub calmed: Option<Entity>,
}

/// What a spell's target is like as it's cast.
struct Struck {
    dead: bool,
    room: Entity,
    attributes: Attributes,
    skills: Skills,
    conditions: Conditions,
    /// Whether they see damage numbers.
    numbers: bool,
    mob: Option<MobKey>,
    /// Their most health.
    most: i32,
}

/// What a mob does instead of its auto-attack.
#[derive(Debug, Clone, PartialEq)]
pub enum MobMove {
    Technique(String),
    /// A spell, and at whom.
    Spell(String, Entity),
}

impl Round<'_, '_, '_, '_, '_, '_> {
    /// Cast the spell `key` at `target`: pay its Drain, roll it against what resists it, and
    /// apply what it does. Casting practises its school.
    pub fn cast(&mut self, caster: Entity, target: Entity, key: &str) -> Cast {
        let mut cast = Cast::default();
        let spells = self.spells;
        let Some(spell) = spells.0.get(key).map(|s| &s.def) else {
            return cast;
        };
        let d_name = self.name(target);
        let Ok(c) = self.fighters.get(caster) else {
            return cast;
        };
        let (room, c_name) = (c.in_room.0, name_of(c.player, c.mob));
        let (c_attributes, c_skills) = (c.attributes.clone(), c.skills.clone());
        let c_numbers = !c.hide_numbers;
        if let Err(refusal) = can_cast(spell, &c_skills, c.vitals.stamina, self.defs) {
            self.say(caster, refusal);
            return cast;
        }
        let defs = self.defs;
        let Ok(d) = self.fighters.get(target).map(|d| Struck {
            dead: d.dead,
            room: d.in_room.0,
            attributes: d.attributes.clone(),
            skills: d.skills.clone(),
            conditions: d.conditions.clone(),
            numbers: !d.hide_numbers,
            mob: d.mob.map(|m| m.key.clone()),
            most: Maxima::of(d.attributes, d.skills, defs, d.base).health,
        }) else {
            return cast;
        };
        if d.dead || d.room != room {
            self.say(caster, format!("{} isn't here.", capitalized(&d_name)));
            return cast;
        }
        if !takes_hold(spell, &d.skills) {
            self.say(caster, format!("{} isn't an animal.", capitalized(&d_name)));
            return cast;
        }

        // Drain first: resisted well, it costs half.
        let luck = c_attributes.get(Attribute::Luck);
        let resistance = drain_resistance(spell, &c_attributes, self.defs);
        let paid = drain_paid(self.dice, spell, resistance, luck);
        self.pay(caster, paid as f32);
        if paid > spell.drain.div_ceil(2) {
            self.say(
                caster,
                "\x1b[2mThe casting takes more out of you than it should.\x1b[0m",
            );
        }
        let power = power(spell, &c_skills, &c_attributes, self.defs).max(1);

        // Against whatever resists it, the caster's power by opposed roll; how decisively it
        // wins is how hard a harmful spell lands.
        let (hit, blow) = match spell.resist {
            None => (true, Blow::Solid),
            Some(resist) => {
                let rating = match resist {
                    Resist::Dodge => {
                        let aura = self.auras.get(&target).copied().unwrap_or_default();
                        let parts = defence_rating(
                            &d.attributes,
                            &d.skills,
                            aura,
                            &boosts(&d.skills, self.defs),
                            gear(target, self.items, self.templates),
                        );
                        let evasion = d.conditions.defence(parts.evasion);
                        if self.resting.contains(&target) {
                            evasion / 2
                        } else {
                            evasion
                        }
                    }
                    Resist::Attribute(attribute) => d.attributes.get(attribute),
                };
                let contest = self.dice.opposed(
                    f64::from(power),
                    luck,
                    f64::from(rating.max(1)),
                    d.attributes.get(Attribute::Luck),
                );
                (contest.won, Blow::of(contest.margin))
            }
        };

        // Cast on themselves, the caster is "yourself" to them and "themselves" to the room.
        let own = caster == target;
        let (to_me, to_room) = if own {
            ("yourself".to_string(), "themselves".to_string())
        } else {
            (d_name.clone(), d_name.clone())
        };
        let mut tell = Telling::default();
        let mut after = Telling::default();
        if hit {
            let mut dealt = None;
            let mut healed = None;
            for effect in &spell.effects {
                let Ok(mut f) = self.fighters.get_mut(target) else {
                    continue;
                };
                match *effect {
                    SpellEffect::Harm {
                        damage: (low, high),
                    } => {
                        let (low, high) = (low.min(high), low.max(high));
                        let rolled = self.dice.range(luck, i64::from(low)..=i64::from(high)) as i32;
                        let force = rolled + power / 3;
                        let armour = (self.auras.get(&target).map_or(0, |a| a.armour)
                            + boosts(&d.skills, self.defs).armour
                            + gear(target, self.items, self.templates).reduction)
                            .max(0);
                        let mut hurt =
                            ((f64::from(force) * blow.multiplier()).round() as i32 - armour).max(0);
                        hurt -= self.ward_off(target, caster, hurt, &d_name, &mut after);
                        dealt = Some(hurt);
                    }
                    SpellEffect::Heal { heal: (low, high) } => {
                        let (low, high) = (low.min(high), low.max(high));
                        let rolled = self.dice.range(luck, i64::from(low)..=i64::from(high)) as i32;
                        let most = Maxima::of(&f.attributes, &f.skills, self.defs, f.base).health;
                        let before = f.vitals.health;
                        f.vitals.health = (before + (rolled + power / 3) as f32).min(most as f32);
                        healed = Some((f.vitals.health - before).round() as i32);
                    }
                    SpellEffect::Bless { attack, lasts } => {
                        let until = self.now + lasts.real_secs();
                        f.conditions.add_until(State::Blessed(attack), until);
                    }
                    SpellEffect::Ward { absorb, lasts } => {
                        let strength = absorb + (power / 3).max(0) as u32;
                        let until = self.now + lasts.real_secs();
                        f.conditions.add_until(State::Warded(strength), until);
                    }
                    SpellEffect::Hold { rounds } => f.conditions.add(State::Held, rounds),
                    SpellEffect::Calm => cast.calmed = Some(target),
                }
            }
            let most = d.most;
            let with = [
                ("actor", c_name.as_str()),
                ("harm", dealt.map_or("", |n| harm(n, most))),
            ];
            let say = |template: &str, target: &str| {
                let mut with = with.to_vec();
                with.push(("target", target));
                let line = fill(template, &with);
                match (dealt.is_some(), blow, line.strip_suffix('.')) {
                    (true, Blow::Crushing, Some(line)) => format!("{line}!"),
                    _ => line,
                }
            };
            let count = |shown: bool| match (shown, dealt, healed) {
                (true, Some(n), _) => format!(" ({n})"),
                (true, _, Some(n)) => format!(" (+{n})"),
                _ => String::new(),
            };
            let text = &spell.text.hit;
            tell.lines.push((
                caster,
                format!("{}{}", say(&text.actor, &to_me), count(c_numbers)),
            ));
            if !own && let Some(theirs) = &text.target {
                let theirs = format!("{}{}", say(theirs, &d_name), count(d.numbers));
                let theirs = if spell.aim == Aim::Foe {
                    format!("\x1b[31m{theirs}\x1b[0m")
                } else {
                    theirs
                };
                tell.lines.push((target, theirs));
            }
            tell.room.push(say(&text.room, &to_room));
            tell.lines.append(&mut after.lines);
            tell.room.append(&mut after.room);
            if let Some(hurt) = dealt.filter(|n| *n > 0) {
                if gear(target, self.items, self.templates).reduction > 0 {
                    self.events.write(GameEvent::HitInArmour { who: target });
                }
                if self.wound(
                    target,
                    hurt,
                    &mut tell,
                    caster,
                    &d_name,
                    room,
                    d.mob.clone(),
                ) {
                    cast.died.push(target);
                }
            }
        } else {
            let miss = spell.text.miss.as_ref().unwrap_or(&spell.text.hit);
            let (mine, theirs, seen) = told(miss, &c_name, &d_name);
            tell.lines.push((caster, mine));
            tell.lines.push((target, theirs));
            tell.room.push(seen);
            if spell.resist == Some(Resist::Dodge) {
                self.events.write(GameEvent::Dodged { who: target });
            }
        }

        for (who, line) in tell.lines {
            self.say(who, line);
        }
        let room_line = tell.room.join(" ");
        self.tell_room(room, caster, target, &room_line);
        self.practise(caster, &spell.school);
        cast
    }

    /// The spell and target a queued `cast <spell> [<target>]` names, for a caster fighting
    /// `foe`, or why it can't be cast.
    pub(super) fn queued_spell(
        &self,
        caster: Entity,
        foe: Entity,
        args: &str,
        hidden: &Query<&Hidden>,
    ) -> Result<(String, Entity), String> {
        let f = self.fighters.get(caster).map_err(|_| String::new())?;
        let known = f.spells.cloned().unwrap_or_default();
        let (spell, words) = self
            .spells
            .find_known_in(&known, args)
            .ok_or_else(|| format!("You don't know a spell called '{}'.", args.trim()))?;
        let room = f.in_room.0;
        let candidates: Vec<(Entity, &Keywords)> = self
            .fighters
            .iter()
            .filter(|f| f.in_room.0 == room && !f.dead && sees(hidden, caster, f.entity))
            .filter_map(|f| Some((f.entity, f.keywords?)))
            .collect();
        let target = choose_target(&spell.def, caster, Some(foe), words, candidates, |e| {
            self.fighters.get(e).is_ok_and(|f| f.player.is_some())
        })?;
        Ok((spell.key.clone(), target))
    }

    /// The spells a mob could cast now that would do some good, and at whom: at its `foe`, or
    /// on itself, healing only when badly hurt, and not blessing or warding itself twice.
    pub(super) fn mob_spells(&self, mob: Entity, foe: Entity) -> Vec<MobMove> {
        let Ok(f) = self.fighters.get(mob) else {
            return Vec::new();
        };
        let Some(known) = f.spells else {
            return Vec::new();
        };
        let most = Maxima::of(f.attributes, f.skills, self.defs, f.base).health;
        let hurt = f.vitals.health < most as f32 / 2.0;
        known
            .0
            .iter()
            .filter_map(|key| {
                let spell = &self.spells.0.get(key)?.def;
                can_cast(spell, f.skills, f.vitals.stamina, self.defs).ok()?;
                let target = match spell.aim {
                    Aim::Foe | Aim::Other => foe,
                    Aim::Ally => mob,
                };
                let target_skills = self.fighters.get(target).ok()?.skills;
                let useful = takes_hold(spell, target_skills)
                    && spell.effects.iter().all(|effect| match effect {
                        SpellEffect::Heal { .. } => hurt,
                        SpellEffect::Bless { .. } => !f.conditions.has(State::Blessed(0)),
                        SpellEffect::Ward { .. } => !f.conditions.has(State::Warded(0)),
                        _ => true,
                    });
                useful.then(|| MobMove::Spell(key.clone(), target))
            })
            .collect()
    }
}
