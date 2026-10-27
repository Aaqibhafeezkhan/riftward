//! Casting a spell, in a fight or out of one: its Drain, the contest against whatever resists it,
//! and what it does. Damage, deaths and conditions go the way a blow's do (see `strike.rs`).

use bevy::prelude::*;

use super::Round;
use super::resolve::Use;
use crate::attributes::Attribute;
use crate::effects::{self, Effect, When, Who};
use crate::items::capitalized;
use crate::skills::Skills;
use crate::spells::{
    Aim, can_cast, choose_target, drain_paid, drain_resistance, power, takes_hold,
};
use crate::stealth::can_see;
use crate::targeting::Keywords;
use crate::vitals::Maxima;

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
    skills: Skills,
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
        let room = c.in_room.0;
        let (c_attributes, c_skills) = (c.attributes.clone(), c.skills.clone());
        if let Err(refusal) = can_cast(
            spell,
            &c_skills,
            c.vitals.stamina,
            self.defs,
            (c.conditions, self.conditions),
        ) {
            self.say(caster, refusal);
            return cast;
        }
        let Ok(d) = self.fighters.get(target).map(|d| Struck {
            dead: d.dead,
            room: d.in_room.0,
            skills: d.skills.clone(),
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

        // Against whatever resists it, the caster's power, resolved like any blow.
        let outcome = self.resolve(Use {
            user: caster,
            target,
            rating: power,
            resist: spell.resist,
            against: None,
            tags: effects::tags(&spell.tags, &spell.effects, self.conditions),
            blow: None,
            effects: &spell.effects,
            power,
            text: &spell.text,
            opener: false,
            harmful: spell.aim == Aim::Foe,
            reacting: false,
        });
        cast.died = outcome.died;
        if outcome.calmed {
            cast.calmed = Some(target);
        }
        // What the caster's conditions and skills do as they cast.
        for died in self.react(caster, target, When::Casts) {
            if !cast.died.contains(&died) {
                cast.died.push(died);
            }
        }
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
            .filter(|f| f.in_room.0 == room && !f.dead && can_see(caster, f.entity, f.conditions))
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
                can_cast(
                    spell,
                    f.skills,
                    f.vitals.stamina,
                    self.defs,
                    (f.conditions, self.conditions),
                )
                .ok()?;
                let target = match spell.aim {
                    Aim::Foe | Aim::Other => foe,
                    Aim::Ally | Aim::Me => mob,
                    Aim::Lock | Aim::Room | Aim::Item => return None,
                };
                let target_skills = self.fighters.get(target).ok()?.skills;
                let useful = takes_hold(spell, target_skills)
                    && spell.effects.iter().all(|effect| match effect {
                        Effect::Heal { .. } => hurt,
                        // Not twice on itself; at a foe, not on one that already has it.
                        Effect::Apply {
                            condition,
                            on: Who::Target,
                            ..
                        } => self
                            .fighters
                            .get(target)
                            .is_ok_and(|t| !t.conditions.has(condition)),
                        _ => true,
                    });
                useful.then(|| MobMove::Spell(key.clone(), target))
            })
            .collect()
    }
}
