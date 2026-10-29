//! Counting what holders do toward their quests (kills, for now), as the quest's own facts, and
//! moving them from stage to stage.

use bevy::prelude::*;

use super::def::{Objective, QuestDef, Quests, Stage};
use super::journal::{HELD, QuestLog, stage_number};
use crate::db::{Database, QuestRecord};
use crate::events::GameEvent;
use crate::facts::{Facts, Owner};
use crate::mobs::MobTemplates;
use crate::output::{Outbox, Outgoing, deliver};
use crate::time::unix_now;
use crate::world::{MobDef, MobKey, Player, without_article};

/// Kills count for the quests their killer holds, for them and every member of their group in
/// the room who holds the same quest: each objective the dead mob answers on the stage that
/// holder is on goes up by one, to what it asks; a stage whose objectives are all counted and
/// done moves on to the next, and the last says it's time to report.
#[allow(clippy::too_many_arguments)]
pub(super) fn count_kills(
    mut events: MessageReader<GameEvent>,
    mut logs: Query<(&mut QuestLog, &Player, &crate::world::InRoom)>,
    groups: crate::groups::Groups,
    quests: Res<Quests>,
    mut story: ResMut<Facts>,
    mobs: Res<MobTemplates>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let mut out = Vec::new();
    for event in events.read() {
        let GameEvent::Died {
            mob: Some(mob),
            killer: Some(killer),
            name,
            room,
            ..
        } = event
        else {
            continue;
        };
        // A member who doesn't hold a quest doesn't move it.
        let Ok((log, ..)) = logs.get(*killer) else {
            continue;
        };
        let theirs: Vec<String> = log
            .0
            .iter()
            .filter(|r| r.state == HELD)
            .map(|r| r.quest.clone())
            .collect();
        let there: Vec<Entity> = crate::groups::members(*killer, &groups)
            .into_iter()
            .filter(|e| logs.get(*e).is_ok_and(|(.., at)| at.0 == *room))
            .collect();
        let def = mobs.0.get(mob);
        for quest_id in theirs {
            let Some(quest) = quests.0.get(&quest_id) else {
                continue;
            };
            for holder in &there {
                let Ok((mut log, player, _)) = logs.get_mut(*holder) else {
                    continue;
                };
                let Some(record) = log.get(&quest_id).filter(|r| r.state == HELD).cloned() else {
                    continue;
                };
                let character_id = player.character_id;
                let (lines, moved) = tally(
                    &record,
                    quest,
                    character_id,
                    (mob, def, name),
                    &mut story,
                    &mobs,
                    &db,
                    now,
                );
                if let Some(record) = moved {
                    log.put(record, character_id, &db);
                }
                out.extend(lines.into_iter().map(|line| Outgoing::line(*holder, line)));
            }
        }
    }
    deliver(out, &mut outbox);
}

/// Count a kill of `mob` (its template, and its name) toward one holder's `record`: what to tell
/// them, and the record moved on to the next stage if this finished the one they were on.
#[allow(clippy::too_many_arguments)]
fn tally(
    record: &QuestRecord,
    quest: &QuestDef,
    character_id: i64,
    (mob, def, name): (&MobKey, Option<&MobDef>, &str),
    story: &mut Facts,
    mobs: &MobTemplates,
    db: &Database,
    now: i64,
) -> (Vec<String>, Option<QuestRecord>) {
    let mut lines = Vec::new();
    let Some(n) = stage_number(record) else {
        return (lines, None);
    };
    let Some(stage) = quest.stages.get(n - 1) else {
        return (lines, None);
    };
    let owner = Owner::Quest(character_id, quest.id.clone());
    let mut counted = false;
    for objective in &stage.objectives {
        let (Objective::Kill(which, need), Some(key)) = (objective, objective.key(n, quest.zone()))
        else {
            continue;
        };
        if !which.matches(mob, def, quest.zone()) {
            continue;
        }
        if story.number(&owner, &key, now) >= i64::from(*need) {
            continue;
        }
        let have = story.add(&owner, &key, 1, now, db);
        counted = true;
        let done = if have >= i64::from(*need) {
            " Done."
        } else {
            ""
        };
        lines.push(format!(
            "\x1b[1;33m{}:\x1b[0m {} killed, {have} of {need}.{done}",
            quest.name,
            without_article(name)
        ));
    }
    if !counted || !stage_done(stage, n, quest, &owner, story, now) {
        return (lines, None);
    }
    // All counted and done: on to the next stage, or back to report.
    match quest.stages.get(n) {
        Some(next) => {
            lines.push(format!("\x1b[1;33m{}:\x1b[0m {}", quest.name, next.text));
            let mut record = record.clone();
            record.stage = (n + 1).to_string();
            (lines, Some(record))
        }
        None => {
            lines.push(format!(
                "\x1b[1;33m{}:\x1b[0m that's all it asks. Report to {}.",
                quest.name,
                quest.reported_to(mobs)
            ));
            (lines, None)
        }
    }
}

/// Whether every objective on stage `n` is counted as it happens, and done.
fn stage_done(
    stage: &Stage,
    n: usize,
    quest: &QuestDef,
    owner: &Owner,
    story: &Facts,
    now: i64,
) -> bool {
    stage.objectives.iter().all(|objective| {
        objective
            .key(n, quest.zone())
            .is_some_and(|key| story.number(owner, &key, now) >= i64::from(objective.count()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::ItemTemplates;
    use crate::quests::def::MobMatch;
    use crate::quests::testing::rats;
    use std::collections::HashMap;

    #[test]
    fn a_kill_counts_for_every_holder_in_the_group_who_is_there() {
        use crate::groups::InGroup;
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let mut world = World::new();
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        let rat = MobKey::parse("riftwatch:rat", "");
        world.insert_resource(MobTemplates(HashMap::from([(
            rat.clone(),
            MobDef::named("a rat"),
        )])));
        world.insert_resource(Quests(HashMap::from([("riftwatch:rats".into(), rats())])));
        let (cellar, street) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut ids = Vec::new();
        let mut spawn = |world: &mut World, name: &str, room: Entity, holds: bool| {
            let id = world
                .resource::<Database>()
                .create_character(account.id, name)
                .unwrap()
                .id;
            ids.push(id);
            let log = match holds {
                true => vec![QuestRecord {
                    quest: "riftwatch:rats".into(),
                    state: HELD.into(),
                    stage: "1".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }],
                false => Vec::new(),
            };
            world
                .spawn((
                    crate::world::InRoom(room),
                    Player {
                        name: name.into(),
                        character_id: id,
                        account: "ada".into(),
                        role: crate::db::Role::Player,
                        race: "human".into(),
                        background: String::new(),
                        description: String::new(),
                    },
                    QuestLog(log),
                ))
                .id()
        };
        world.insert_resource(db);
        let ada = spawn(&mut world, "Ada", cellar, true);
        let bob = spawn(&mut world, "Bob", cellar, true);
        spawn(&mut world, "Cara", cellar, true);
        let dan = spawn(&mut world, "Dan", street, true);
        let eve = spawn(&mut world, "Eve", cellar, false);
        // Ada leads Bob, Dan and Eve; Cara is on her own.
        for (n, who) in [ada, bob, dan, eve].into_iter().enumerate() {
            world.entity_mut(who).insert(InGroup {
                leader: ada,
                joined: n as u64,
            });
        }
        let kill = |world: &mut World, killer: Entity| {
            world.write_message(GameEvent::Died {
                who: cellar,
                name: "a rat".into(),
                room: cellar,
                mob: Some(rat.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_kills).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let count = |world: &World, id: i64| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "1.kill.riftwatch:rat",
                0,
            )
        };
        kill(&mut world, eve);
        assert_eq!(
            count(&world, ids[0]),
            0,
            "Eve doesn't hold it, so her kill moves nothing"
        );
        kill(&mut world, bob);
        assert_eq!(count(&world, ids[0]), 1, "Bob's kill is Ada's too");
        assert_eq!(count(&world, ids[1]), 1);
        assert_eq!(count(&world, ids[2]), 0, "Cara isn't in their group");
        assert_eq!(count(&world, ids[3]), 0, "Dan isn't there");
    }

    #[test]
    fn kills_count_stage_by_stage() {
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        world.init_resource::<ItemTemplates>();
        let (rat, roach) = (
            MobKey::parse("riftwatch:rat", ""),
            MobKey::parse("riftwatch:roach", ""),
        );
        let mut roach_def = MobDef::named("a giant cockroach");
        roach_def.tags = vec!["vermin".into()];
        world.insert_resource(MobTemplates(HashMap::from([
            (rat.clone(), MobDef::named("a rat")),
            (roach.clone(), roach_def),
        ])));
        let quest = QuestDef {
            stages: vec![
                Stage {
                    text: "Kill two rats.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Mob("rat".into()), 2)],
                },
                Stage {
                    text: "Kill a vermin.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Tag("vermin".into()), 1)],
                },
            ],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let room = world.spawn_empty().id();
        let ada = world
            .spawn((
                crate::world::InRoom(room),
                Player {
                    name: "Ada".into(),
                    character_id: id,
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
        let stranger = world.spawn_empty().id();
        let kill = |world: &mut World, mob: &MobKey, killer: Entity| {
            world.write_message(GameEvent::Died {
                who: room,
                name: "a rat".into(),
                room,
                mob: Some(mob.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_kills).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let stage = |world: &World| world.get::<QuestLog>(ada).unwrap().0[0].stage.clone();
        let owner = Owner::Quest(id, "riftwatch:rats".into());
        let count = |world: &World, key: &str| world.resource::<Facts>().number(&owner, key, 0);

        kill(&mut world, &rat, stranger);
        assert_eq!(
            count(&world, "1.kill.riftwatch:rat"),
            0,
            "a stranger's kill"
        );
        kill(&mut world, &roach, ada);
        assert_eq!(count(&world, "2.kill.tag.vermin"), 0, "not that stage yet");
        kill(&mut world, &rat, ada);
        assert_eq!(count(&world, "1.kill.riftwatch:rat"), 1);
        assert_eq!(stage(&world), "1");
        kill(&mut world, &rat, ada);
        assert_eq!(stage(&world), "2", "both rats: on to the next stage");
        kill(&mut world, &roach, ada);
        assert_eq!(count(&world, "2.kill.tag.vermin"), 1, "by its tag");
        assert_eq!(stage(&world), "2", "the last stage waits for a report");
        let said: Vec<String> = world
            .resource::<Outbox>()
            .0
            .iter()
            .filter_map(|o| match o {
                Outgoing::To(_, text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(said.iter().any(|t| t.contains("rat killed, 2 of 2. Done.")));
        assert!(said.iter().any(|t| t.contains("Kill a vermin.")));
        assert!(
            said.iter()
                .any(|t| t.contains("that's all it asks. Report to"))
        );
    }
}
