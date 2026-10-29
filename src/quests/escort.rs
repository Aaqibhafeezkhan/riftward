//! Escorts: a mob that follows a holder on a quest's say-so (`Escort(mob: ..., to: ...)`), from
//! where they find it to where it's to go, by following as anyone follows a leader. Its walking
//! into that room counts for them; once it's no longer needed (done, failed, given up, its
//! holder gone) it stops following and goes home.

use bevy::prelude::*;

use super::def::{Objective, QuestDef, Quests};
use super::journal::{HELD, QuestLog};
use crate::facts::{Facts, Owner};
use crate::groups::Following;
use crate::mobs::Mob;
use crate::output::{Outbox, Outgoing};
use crate::time::unix_now;
use crate::world::{InRoom, MobKey, Player};

/// A mob escorting someone for a quest: whom, for which quest, and the room to go home to.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Escort {
    pub holder: Entity,
    pub quest: String,
    pub home: Entity,
}

/// The mobs `quest` still wants escorted by the holder with id `id` on the stage `place`
/// they're on: each `Escort` objective there not yet done.
fn wanted(quest: &QuestDef, place: &str, id: i64, story: &Facts, now: i64) -> Vec<MobKey> {
    let Some((i, stage)) = quest.stage_at(place) else {
        return Vec::new();
    };
    let at = quest.stage_key(i);
    let owner = Owner::Quest(id, quest.id.clone());
    stage
        .parts(&at)
        .into_iter()
        .flat_map(|(_, at, objectives)| objectives.iter().map(move |o| (at.clone(), o)))
        .filter_map(|(at, objective)| {
            let Objective::Escort { mob, .. } = objective else {
                return None;
            };
            let key = objective.key(&at, quest.zone())?;
            (story.number(&owner, &key, now) == 0).then(|| MobKey::parse(mob, quest.zone()))
        })
        .collect()
}

/// Mobs free to take up an escort: escorting no one, and not fighting.
type Free<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Mob, &'static InRoom),
    (Without<Escort>, Without<crate::combat::Fighting>),
>;

/// A holder in the game on a stage that wants a mob escorted, and that mob in the room with
/// them and escorting no one: it falls in behind them.
pub(super) fn start_escorts(
    mut commands: Commands,
    holders: Query<(Entity, &Player, &QuestLog, &InRoom)>,
    mobs: Free,
    quests: Res<Quests>,
    story: Res<Facts>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let mut taken = Vec::new();
    for (holder, player, log, at) in &holders {
        for record in log.0.iter().filter(|r| r.state == HELD) {
            let Some(quest) = quests.0.get(&record.quest) else {
                continue;
            };
            for key in wanted(quest, &record.stage, player.character_id, &story, now) {
                let found = mobs.iter().find(|(entity, mob, there)| {
                    mob.key == key && there.0 == at.0 && !taken.contains(entity)
                });
                let Some((entity, mob, there)) = found else {
                    continue;
                };
                taken.push(entity);
                commands.entity(entity).insert((
                    Following(holder),
                    Escort {
                        holder,
                        quest: quest.id.clone(),
                        home: there.0,
                    },
                ));
                outbox.0.push(Outgoing::line(
                    holder,
                    format!(
                        "{} falls in behind you.",
                        crate::items::capitalized(&crate::combat::the(&mob.name))
                    ),
                ));
            }
        }
    }
}

/// An escort no longer wanted (the quest done, failed or given up, its stage past it, or its
/// holder gone) stops following and goes home.
pub(super) fn end_escorts(
    mut commands: Commands,
    escorts: Query<(Entity, &Escort, &Mob, &InRoom)>,
    holders: Query<(&Player, &QuestLog)>,
    quests: Res<Quests>,
    story: Res<Facts>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    for (entity, escort, mob, at) in &escorts {
        let still = holders.get(escort.holder).is_ok_and(|(player, log)| {
            let Some(quest) = quests.0.get(&escort.quest) else {
                return false;
            };
            log.get(&escort.quest).is_some_and(|record| {
                record.state == HELD
                    && wanted(quest, &record.stage, player.character_id, &story, now)
                        .contains(&mob.key)
            })
        });
        if still {
            continue;
        }
        commands
            .entity(entity)
            .remove::<(Following, Escort)>()
            .insert(InRoom(escort.home));
        if at.0 != escort.home {
            let name = crate::items::capitalized(&crate::combat::the(&mob.name));
            outbox.0.push(Outgoing::Room {
                room: at.0,
                except: entity,
                text: format!("{name} heads for home.\r\n"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QuestRecord;
    use crate::quests::def::Stage;
    use crate::quests::journal::DONE;
    use crate::quests::testing::rats;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashMap;

    #[test]
    fn an_escort_falls_in_behind_and_goes_home_when_its_done() {
        let mut world = World::new();
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        let quest = QuestDef {
            stages: vec![Stage {
                text: "Take Narina to the ravine.".into(),
                objectives: vec![Objective::Escort {
                    mob: "herbalist".into(),
                    to: "forest:ravine".into(),
                }],
                ..Default::default()
            }],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let (stall, road) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mob = |world: &mut World, id: &str, name: &str| {
            world
                .spawn((
                    Mob {
                        key: MobKey::parse(id, ""),
                        name: name.into(),
                        here: String::new(),
                        description: String::new(),
                        race: None,
                    },
                    InRoom(stall),
                ))
                .id()
        };
        let narina = mob(&mut world, "riftwatch:herbalist", "Narina Greenthorn");
        let other = mob(&mut world, "riftwatch:hunter", "the hunter");
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
                InRoom(stall),
            ))
            .id();
        let run = |world: &mut World| {
            world.run_system_once(end_escorts).unwrap();
            world.run_system_once(start_escorts).unwrap();
            world.flush();
        };
        run(&mut world);
        assert_eq!(world.get::<Following>(narina), Some(&Following(ada)));
        assert!(world.get::<Following>(other).is_none(), "not who it wants");
        assert!(world.resource::<Outbox>().0.iter().any(|o| matches!(o,
            Outgoing::To(who, text) if *who == ada && text.contains("Narina Greenthorn falls in behind you."))));
        // Out on the road with her, it's still wanted.
        world.get_mut::<InRoom>(narina).unwrap().0 = road;
        run(&mut world);
        assert!(world.get::<Escort>(narina).is_some());
        // The quest done, she heads home.
        world.get_mut::<QuestLog>(ada).unwrap().0[0].state = DONE.into();
        run(&mut world);
        assert!(world.get::<Escort>(narina).is_none() && world.get::<Following>(narina).is_none());
        assert_eq!(world.get::<InRoom>(narina).unwrap().0, stall, "home");
    }
}
