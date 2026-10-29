//! Effects carried out there and then, out of a fight: what eating or drinking something does. A
//! [`Round`] is built for the moment, as `cast` builds one, so it's resolved as every blow,
//! spell and active skill is, and told in the conditions' own words.

use std::collections::{HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::states::{ConditionDefs, Imbued, ItemConditions};
use super::strike::{Fighters, Round, Telling};
use crate::db::Database;
use crate::dice::Dice;
use crate::effects::{Effect, Who};
use crate::events::GameEvent;
use crate::facts::Facts;
use crate::items::{ItemQuery, ItemTemplates};
use crate::mobs::MobTemplates;
use crate::output::Outbox;
use crate::skills::SkillDefs;
use crate::spells::SpellDefs;
use crate::vitals::{Maxima, Resting};

/// What it takes to build a [`Round`] out of a fight.
#[derive(SystemParam)]
pub struct Rounds<'w, 's> {
    pub fighters: Fighters<'w, 's>,
    pub items: ItemQuery<'w, 's>,
    pub templates: Res<'w, ItemTemplates>,
    pub db: Res<'w, Database>,
    pub events: MessageWriter<'w, GameEvent>,
    pub log: ResMut<'w, Outbox>,
    mobs: Res<'w, MobTemplates>,
    defs: Res<'w, SkillDefs>,
    spells: Res<'w, SpellDefs>,
    conditions: Res<'w, ConditionDefs>,
    dice: ResMut<'w, Dice>,
    story: Res<'w, Facts>,
    resting: Query<'w, 's, Entity, With<Resting>>,
    time: Res<'w, Time>,
    item_conditions: Query<'w, 's, (Entity, &'static mut ItemConditions)>,
}

impl<'w, 's> Rounds<'w, 's> {
    /// Build a round for now, and run `f` in it.
    pub fn now<R>(&mut self, f: impl FnOnce(&mut Round<'_, 'w, 's, 'w, 's, 'w>) -> R) -> R {
        let watchers: Vec<(Entity, Entity)> = self
            .fighters
            .iter()
            .filter(|f| f.player.is_some())
            .map(|f| (f.entity, f.in_room.0))
            .collect();
        let targets: HashMap<Entity, Entity> = self
            .fighters
            .iter()
            .filter_map(|f| Some((f.entity, f.fighting?.0)))
            .collect();
        let resting: HashSet<Entity> = self.resting.iter().collect();
        let none = HashMap::new();
        let mut imbued = Imbued::gather(self.item_conditions.iter());
        let made = {
            let mut round = Round {
                fighters: &mut self.fighters,
                items: &self.items,
                templates: &self.templates,
                mobs: &self.mobs,
                defs: &self.defs,
                spells: &self.spells,
                conditions: &self.conditions,
                dice: &mut self.dice,
                db: &self.db,
                story: Some(&self.story),
                events: &mut self.events,
                log: &mut self.log,
                watchers: &watchers,
                engaged: &targets,
                targets: &targets,
                auras: &HashMap::new(),
                resting: &resting,
                outmatched: &none,
                ambushers: &HashSet::new(),
                now: self.time.elapsed_secs_f64(),
                imbued: &mut imbued,
            };
            f(&mut round)
        };
        imbued.write_back(&mut self.item_conditions);
        made
    }
}

impl Round<'_, '_, '_, '_, '_, '_> {
    /// `who` takes in something that does `effects`, every one on them: a heal, then conditions
    /// put on or taken off, told in their own words after `first`'s line (given how much health
    /// it gave back).
    pub fn take_in(&mut self, who: Entity, effects: &[Effect], first: impl FnOnce(i32) -> String) {
        let inputs = self.inputs(who, who, 0, 1.0);
        let Ok(f) = self.fighters.get(who) else {
            return;
        };
        let (luck, room) = (
            f.attributes.get(crate::attributes::Attribute::Luck),
            f.in_room.0,
        );
        let mut healed = 0;
        for effect in effects {
            if let Effect::Heal { amount } = effect {
                let rolled = amount.amount(&inputs, self.dice, luck).max(0);
                let Ok(mut f) = self.fighters.get_mut(who) else {
                    continue;
                };
                let most = Maxima::of(&f.attributes, &f.skills, self.defs, f.base).health;
                let before = f.vitals.health;
                f.vitals.health = (before + rolled as f32).min(most as f32);
                healed += (f.vitals.health - before).round() as i32;
            }
        }
        self.say(who, first(healed));
        let mut tell = Telling::default();
        for on in [Who::User, Who::Target] {
            self.affect(who, who, effects, on, 0, 1.0, false, &mut tell);
        }
        self.tell(tell, room, who, who);
    }
}

/// Whether `effect` can be what eating or drinking does (checked by the zone files' test): something done to whoever takes it in,
/// with no one to aim at or contest it.
#[cfg(test)]
pub fn takes_in(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Heal { .. } | Effect::Apply { .. } | Effect::Remove { .. } | Effect::Cure { .. }
    )
}
