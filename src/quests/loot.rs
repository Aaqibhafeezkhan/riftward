//! What a holder still needs for a quest, so a boss that can drop it leaves one for them
//! (`combat::WantedLoot`): there's only one Rat King, so his tooth isn't left to luck for someone
//! on his quest. An ordinary mob's drops are left to chance.

use bevy::prelude::*;

use super::def::{Objective, Quests};
use super::journal::{HELD, QuestLog, stage_number};
use crate::combat::{Dead, WantedLoot};
use crate::facts::{Facts, Owner};
use crate::items::{ItemQuery, ItemTemplates};
use crate::requirements::ItemMatch;
use crate::time::unix_now;
use crate::world::{ItemKey, Player};

/// Whether anything died this tick, so there's loot to roll.
pub(super) fn deaths(dead: Query<(), With<Dead>>) -> bool {
    !dead.is_empty()
}

/// Each player's items still wanted by the stage they're on of each quest they hold: those it
/// asks them to bring (and they haven't enough of) or to find (and they haven't yet), by item.
/// Any herb (a tag) isn't one thing to drop, so only items named are.
pub(super) fn want_loot(
    holders: Query<(Entity, &QuestLog, &Player)>,
    quests: Res<Quests>,
    story: Res<Facts>,
    (items, templates): (ItemQuery, Res<ItemTemplates>),
    mut wanted: ResMut<WantedLoot>,
) {
    let now = unix_now();
    wanted.0.clear();
    for (who, log, player) in &holders {
        let mut carried: Option<Vec<ItemKey>> = None;
        let mut theirs = Vec::new();
        for record in log.0.iter().filter(|r| r.state == HELD) {
            let Some(quest) = quests.0.get(&record.quest) else {
                continue;
            };
            let Some(n) = stage_number(record) else {
                continue;
            };
            let Some(stage) = quest.stages.get(n - 1) else {
                continue;
            };
            let owner = Owner::Quest(player.character_id, quest.id.clone());
            for objective in &stage.objectives {
                let still = match objective {
                    Objective::Bring(ItemMatch::Item(id), need) => {
                        let key = ItemKey::parse(id, quest.zone());
                        let carried = carried.get_or_insert_with(|| {
                            crate::items::carried(who, &items, &templates)
                                .into_iter()
                                .map(|(key, _)| key)
                                .collect()
                        });
                        let have = carried.iter().filter(|k| **k == key).count();
                        (have < *need as usize).then_some(key)
                    }
                    Objective::Obtain(ItemMatch::Item(id), need) => objective
                        .key(n, quest.zone())
                        .filter(|key| story.number(&owner, key, now) < i64::from(*need))
                        .map(|_| ItemKey::parse(id, quest.zone())),
                    _ => None,
                };
                theirs.extend(still);
            }
        }
        if !theirs.is_empty() {
            wanted.0.insert(who, theirs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QuestRecord;
    use crate::items::{Item, ItemLocation, Quality};
    use crate::quests::def::Stage;
    use crate::quests::testing::rats;
    use crate::world::{DoorState, ItemDef};
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashMap;

    #[test]
    fn only_what_a_holder_still_lacks_is_wanted() {
        let mut world = World::new();
        world.init_resource::<Facts>();
        world.init_resource::<WantedLoot>();
        let tail = ItemKey::parse("sewers:rat_tail", "");
        world.insert_resource(ItemTemplates(HashMap::from([(
            tail.clone(),
            ItemDef::named("a rat's tail"),
        )])));
        let quest = crate::quests::def::QuestDef {
            stages: vec![Stage {
                text: "Bring two tails.".into(),
                objectives: vec![Objective::Bring(
                    ItemMatch::Item("sewers:rat_tail".into()),
                    2,
                )],
            }],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let ada = world
            .spawn((
                Player {
                    name: "Ada".into(),
                    character_id: 1,
                    account: "ada".into(),
                    role: crate::db::Role::Player,
                    race: "human".into(),
                    background: String::new(),
                    description: String::new(),
                },
                QuestLog(vec![QuestRecord {
                    quest: "riftwatch:rats".into(),
                    state: HELD.into(),
                    stage: "1".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }]),
            ))
            .id();
        let give_tail = |world: &mut World| {
            world.spawn((
                Item {
                    key: tail.clone(),
                    name: "a rat's tail".into(),
                    id: None,
                    state: DoorState::Open,
                    quality: Quality::Standard,
                },
                ItemLocation::Held(ada),
            ));
        };
        let wanted = |world: &mut World| {
            world.run_system_once(want_loot).unwrap();
            world.resource::<WantedLoot>().0.get(&ada).cloned()
        };
        assert_eq!(wanted(&mut world), Some(vec![tail.clone()]));
        give_tail(&mut world);
        assert_eq!(wanted(&mut world), Some(vec![tail.clone()]), "one of two");
        give_tail(&mut world);
        assert_eq!(wanted(&mut world), None, "she has both");
    }
}
