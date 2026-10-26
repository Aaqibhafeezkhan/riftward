//! Carrying out effects (`crate::effects`) in the combat round, for spells and techniques alike:
//! the armour and resistances any damage goes through, and the conditions effects put on and
//! take off.

use bevy::prelude::*;

use super::strike::{Round, Telling};
use crate::effects::{Effect, Who};
use crate::items::capitalized;
use crate::skills::boosts;
use crate::world::armour_stops;

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
        pierce: i32,
        ignore_armour: bool,
    ) -> i32 {
        let Ok(d) = self.fighters.get(target) else {
            return 0;
        };
        let armour = if ignore_armour {
            0
        } else {
            let natural = if armour_stops(damage_type) {
                self.auras.get(&target).map_or(0, |a| a.armour) + boosts(d.skills, self.defs).armour
            } else {
                0
            };
            let worn = armour_against(target, self.items, self.templates, damage_type);
            let conditions = d.conditions.armour(self.conditions, damage_type);
            (natural + worn + conditions - pierce).max(0)
        };
        let times = d.conditions.damage_taken(self.conditions)
            * d.conditions.resist(self.conditions, damage_type);
        ((force * times).round() as i32 - armour).max(0)
    }

    /// Carry out the `effects` that are on `who`: conditions put on or taken off `user` or
    /// `target`, told in the conditions' own words. `power` is a caster's, for `empowered`
    /// conditions; nothing for a technique. Returns whether it calmed the target.
    pub(super) fn affect(
        &mut self,
        user: Entity,
        target: Entity,
        effects: &[Effect],
        who: Who,
        power: i32,
        tell: &mut Telling,
    ) -> bool {
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
                    empowered,
                    ..
                } => {
                    let strength = strength + if *empowered { (power / 3).max(0) } else { 0 };
                    let lasts = lasts.lasts(self.now);
                    if f.conditions.apply(defs, condition, strength, lasts) {
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
                Effect::Calm => calmed = true,
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
