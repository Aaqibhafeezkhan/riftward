//! Mobs brought into being by an effect (`Spawn`): an ooze splitting, the Slime Lord shedding
//! slimes. They come once the round is done, where their maker was, and join the fight against
//! its foe if it had one there. The reaction's own text tells the room what happened; they
//! belong to no spawn, so once killed they're gone.

use bevy::prelude::*;

use super::{Dead, Fighting};
use crate::characters::Races;
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::items::ItemTemplates;
use crate::mobs::{MobTemplates, spawn_mob};
use crate::skills::SkillDefs;
use crate::world::InRoom;

/// Bring into being everything a `Spawn` asked for since the last.
#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_from_effects(
    mut events: MessageReader<GameEvent>,
    mut commands: Commands,
    standing: Query<&InRoom, Without<Dead>>,
    templates: Res<MobTemplates>,
    items: Res<ItemTemplates>,
    defs: Res<SkillDefs>,
    races: Res<Races>,
    mut dice: ResMut<Dice>,
) {
    for event in events.read() {
        let GameEvent::Spawns {
            who,
            foe,
            mob,
            count,
            room,
        } = event
        else {
            continue;
        };
        // A foe still standing there to fight; not the maker itself.
        let foe = (foe != who && standing.get(*foe).is_ok_and(|at| at.0 == *room)).then_some(*foe);
        for _ in 0..*count {
            let Some(made) = spawn_mob(
                &mut commands,
                &templates,
                &items,
                &defs,
                mob,
                *room,
                &races,
                &mut dice,
            ) else {
                break;
            };
            if let Some(foe) = foe {
                commands.entity(made).insert(Fighting(foe));
            }
        }
    }
}
