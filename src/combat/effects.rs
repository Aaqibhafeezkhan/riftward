//! Carrying out effects (`crate::effects`) in the combat round, for spells and techniques alike:
//! the armour and resistances any damage goes through, and the conditions effects put on and
//! take off.

use bevy::prelude::*;

use super::states::ItemCondition;
use super::strike::{Round, Telling};
use crate::attributes::Attribute;
use crate::effects::{Effect, SPAWN_MOST, Who};
use crate::events::GameEvent;
use crate::formula::{Inputs, Side};
use crate::items::ItemLocation;
use crate::items::capitalized;
use crate::skills::boosts;
use crate::vitals::Maxima;
use crate::world::{MobKey, armour_stops};

use super::strike::armour_against;

impl Round<'_, '_, '_, '_, '_, '_> {
    /// What `force` of `damage_type` does to `target`, once their armour and what they resist
    /// have had their say: worn pieces stop what their `against` says, natural armour and
    /// conditions only what armour stops, less what `pierce` gets past (or all of it, if
    /// `ignore_armour`). Never less than nothing.
    pub(super) fn mitigate(
        &self,
        target: Entity,
        force: f64,
        damage_type: &str,
        pierce: f64,
        ignore_armour: bool,
    ) -> i32 {
        let Ok(d) = self.fighters.get(target) else {
            return 0;
        };
        let armour = if ignore_armour {
            0.0
        } else {
            let natural = if armour_stops(damage_type) {
                f64::from(self.auras.get(&target).map_or(0, |a| a.armour))
                    + boosts(d.skills, self.defs).armour
            } else {
                0.0
            };
            let worn = armour_against(target, self.items, self.templates, damage_type);
            let conditions = d.conditions.armour(self.conditions, damage_type);
            (natural + f64::from(worn + conditions) - pierce).max(0.0)
        };
        let times = d.conditions.damage_taken(self.conditions)
            * d.conditions.resist(self.conditions, damage_type);
        (force * times - armour).round().max(0.0) as i32
    }

    /// What formulas see when `user` does something to `target`: their attributes and health,
    /// the user's level, a caster's `power`, and the contest's `margin`.
    pub(super) fn inputs(&self, user: Entity, target: Entity, power: i32, margin: f64) -> Inputs {
        let side = |who: Entity| {
            self.fighters
                .get(who)
                .map(|f| Side {
                    attributes: Attribute::ALL
                        .iter()
                        .map(|a| (*a, f64::from(f.attributes.get(*a))))
                        .collect(),
                    health: f64::from(f.vitals.health),
                    most_health: f64::from(
                        Maxima::of(f.attributes, f.skills, self.defs, f.base).health,
                    ),
                    coins: f.purse.as_ref().map_or(0.0, |p| p.0 as f64),
                    ..Side::default()
                })
                .unwrap_or_default()
        };
        Inputs {
            power: f64::from(power),
            strength: None,
            margin,
            level: self
                .fighters
                .get(user)
                .map_or(0.0, |f| f64::from(f.attributes.level())),
            allies: self.allies_fighting(user) as f64,
            user: side(user),
            target: side(target),
            ..Inputs::default()
        }
    }

    /// How many of `who`'s allies are fighting in the room with them, alive: as `allied` counts
    /// them, a mob's own kind and those it names, or for a player, other players.
    pub(super) fn allies_fighting(&self, who: Entity) -> usize {
        let Ok(me) = self.fighters.get(who) else {
            return 0;
        };
        let room = me.in_room.0;
        let others: Vec<Entity> = self
            .fighters
            .iter()
            .filter(|f| f.entity != who && f.in_room.0 == room && !f.dead && f.fighting.is_some())
            .map(|f| f.entity)
            .collect();
        others.into_iter().filter(|o| self.allied(who, *o)).count()
    }

    /// The weapon `who` holds, if any.
    pub(super) fn weapon_of(&self, who: Entity) -> Option<Entity> {
        self.items.iter().find_map(|(entity, item, location)| {
            (*location == ItemLocation::Held(who)
                && self
                    .templates
                    .0
                    .get(&item.key)
                    .is_some_and(|d| d.weapon.is_some()))
            .then_some(entity)
        })
    }

    /// Carry out the `effects` that are on `who`: conditions put on or taken off `user` or
    /// `target`, told in the conditions' own words. `power` is a caster's, for `empowered`
    /// conditions; nothing for a technique. `reacting` for a reaction's, whose conditions set off
    /// no more. Returns whether it calmed the target.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn affect(
        &mut self,
        user: Entity,
        target: Entity,
        effects: &[Effect],
        who: Who,
        power: i32,
        margin: f64,
        reacting: bool,
        tell: &mut Telling,
    ) -> bool {
        let inputs = self.inputs(user, target, power, margin);
        let luck = self
            .fighters
            .get(user)
            .map_or(0, |f| f.attributes.get(Attribute::Luck));
        let bearer = match who {
            Who::User => user,
            Who::Target => target,
        };
        // The one who sees it happen to the bearer: the target, if it's on the user, and the
        // other way about.
        let other = if bearer == user { target } else { user };
        let name = capitalized(&self.name(bearer));
        let defs = self.conditions;
        let mut calmed = false;
        for effect in effects.iter().filter(|e| e.on() == who) {
            let Ok(mut f) = self.fighters.get_mut(bearer) else {
                return calmed;
            };
            // What ended or began, to be told in its words.
            let mut told = Vec::new();
            match effect {
                Effect::Apply {
                    condition,
                    strength,
                    lasts,
                    ..
                } => {
                    let strength = strength.amount(&inputs, self.dice, luck);
                    let lasts = lasts.lasts(self.now);
                    if f.conditions
                        .apply_by(defs, condition, strength, lasts, Some(user))
                    {
                        self.events.write(GameEvent::ConditionApplied {
                            who: bearer,
                            by: user,
                            condition: condition.clone(),
                            strength,
                            reacting,
                        });
                        let applied = defs.get(condition).and_then(|d| d.text.applied.as_ref());
                        told.extend(applied.map(|t| t.fill(&name, strength)));
                    }
                }
                Effect::Remove { condition, .. } => {
                    if f.conditions.remove(condition) {
                        let fades = defs.get(condition).and_then(|d| d.text.fades.as_ref());
                        told.extend(fades.map(|t| t.fill(&name, 0)));
                    }
                }
                Effect::Cure { tag, .. } => {
                    for ended in f.conditions.cure(defs, tag) {
                        let fades = defs.get(&ended).and_then(|d| d.text.fades.as_ref());
                        told.extend(fades.map(|t| t.fill(&name, 0)));
                    }
                }
                Effect::Imbue {
                    condition,
                    strength,
                    lasts,
                    charges,
                    ..
                } => {
                    let strength = strength.amount(&inputs, self.dice, luck);
                    match self.weapon_of(bearer) {
                        Some(weapon) => {
                            let unix = crate::time::unix_now();
                            self.imbued
                                .items
                                .entry(weapon)
                                .or_default()
                                .imbue(ItemCondition {
                                    key: condition.clone(),
                                    strength,
                                    ends_at: lasts.map(|l| unix + l.real_secs().round() as i64),
                                    charges: *charges,
                                });
                            self.imbued.changed.insert(weapon);
                            let applied = defs.get(condition).and_then(|d| d.text.applied.as_ref());
                            told.extend(applied.map(|t| t.fill(&name, strength)));
                        }
                        None => tell
                            .lines
                            .push((user, format!("{name} has no weapon in hand to take it."))),
                    }
                }
                Effect::Calm => calmed = true,
                // Heard and answered once the round is done (`help.rs`).
                Effect::CallForHelp { rooms } => {
                    self.events.write(GameEvent::CalledForHelp {
                        who: user,
                        foe: target,
                        rooms: *rooms,
                    });
                }
                // Brought into being once the round is done (`spawning.rs`).
                Effect::Spawn { mob, count } => {
                    let count = count
                        .amount(&inputs, self.dice, luck)
                        .clamp(0, SPAWN_MOST as i32);
                    let mob = MobKey::parse(mob, f.mob.map_or("", |m| m.key.zone.as_str()));
                    let room = f.in_room.0;
                    self.events.write(GameEvent::Spawns {
                        who: user,
                        foe: target,
                        mob,
                        count: count as u32,
                        room,
                    });
                }
                // Run once the round is done (`fleeing.rs`).
                Effect::Flee => {
                    self.events.write(GameEvent::Flees { who: bearer });
                }
                // The rest are how damage lands, carried out with it.
                _ => {}
            }
            for (theirs, seen) in told {
                if let Some(seen) = seen {
                    if other != bearer {
                        tell.lines.push((other, seen.clone()));
                    }
                    tell.room.push(seen);
                }
                tell.lines.push((bearer, format!("\x1b[1m{theirs}\x1b[0m")));
            }
        }
        calmed
    }
}
