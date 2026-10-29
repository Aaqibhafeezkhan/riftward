//! Counting what holders do toward their quests (kills, things obtained, places reached), as the
//! quest's own facts, and moving them from stage to stage.

use bevy::prelude::*;

use super::actions::{Holder, Stagehand};
use super::def::{Objective, QuestDef, Quests, Stage};
use super::journal::{HELD, QuestLog};
use crate::db::{Database, QuestRecord};
use crate::events::GameEvent;
use crate::facts::{Facts, Owner};
use crate::items::ItemTemplates;
use crate::mobs::MobTemplates;
use crate::output::{Outbox, Outgoing, deliver};
use crate::requirements::ItemMatch;
use crate::time::unix_now;
use crate::world::{InRoom, ItemKey, MobDef, MobKey, Player, Room, RoomKey, without_article};

/// Something a holder did that an objective may count: a mob killed, or an item come by.
#[derive(Clone, Copy)]
pub(super) enum Deed<'a> {
    Killed {
        mob: &'a MobKey,
        def: Option<&'a MobDef>,
        name: &'a str,
    },
    Obtained {
        item: &'a ItemKey,
        tags: &'a [String],
        name: &'a str,
    },
    Reached {
        room: &'a RoomKey,
        name: &'a str,
    },
}

impl Deed<'_> {
    /// Whether `objective`, in a quest of `zone`, counts it.
    fn counts_for(&self, objective: &Objective, zone: &str) -> bool {
        match (self, objective) {
            (Deed::Killed { mob, def, .. }, Objective::Kill(which, _)) => {
                which.matches(mob, *def, zone)
            }
            (Deed::Obtained { item, tags, .. }, Objective::Obtain(which, _)) => match which {
                ItemMatch::Item(id) => ItemKey::parse(id, zone) == **item,
                ItemMatch::Tag(tag) => tags.contains(tag),
            },
            (Deed::Reached { room, .. }, Objective::Reach(id)) => {
                RoomKey::parse(id, zone) == **room
            }
            _ => false,
        }
    }

    /// How it's told: "sewer rat killed", "sealed letter found".
    fn told(&self) -> String {
        match self {
            Deed::Killed { name, .. } => format!("{} killed", without_article(name)),
            Deed::Obtained { name, .. } => format!("{} found", without_article(name)),
            Deed::Reached { name, .. } => format!("{name} reached"),
        }
    }
}

/// What holders do counts for the quests they hold, for them and every member of their group in
/// the room who holds the same quest: a kill (by whoever struck it), an item come by (picked
/// up, given, bought), or a room walked into. Each objective the deed answers on the stage that
/// holder is on goes up by one, to what it asks; a stage whose objectives are all counted and
/// done ends (its `on_end`) and leads to its `next` (whose `on_begin` follows), and the last says
/// it's time to report.
#[allow(clippy::too_many_arguments)]
pub(super) fn count_progress(
    mut events: MessageReader<GameEvent>,
    mut logs: Query<(&mut QuestLog, &Player, &InRoom)>,
    groups: crate::groups::Groups,
    quests: Res<Quests>,
    mut hand: Stagehand,
    (mobs, items): (Res<MobTemplates>, Res<ItemTemplates>),
    places: Query<(&Room, &RoomKey)>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let mut out = Vec::new();
    for event in events.read() {
        let (doer, deed) = match event {
            GameEvent::Died {
                mob: Some(mob),
                killer: Some(killer),
                name,
                ..
            } => (
                *killer,
                Deed::Killed {
                    mob,
                    def: mobs.0.get(mob),
                    name,
                },
            ),
            GameEvent::Entered { who, room, .. } => {
                let Ok((place, key)) = places.get(*room) else {
                    continue;
                };
                (
                    *who,
                    Deed::Reached {
                        room: key,
                        name: &place.name,
                    },
                )
            }
            GameEvent::Obtained { who, key, .. } => {
                let Some(def) = items.0.get(key) else {
                    continue;
                };
                (
                    *who,
                    Deed::Obtained {
                        item: key,
                        tags: &def.tags,
                        name: &def.name,
                    },
                )
            }
            _ => continue,
        };
        // A member who doesn't hold a quest doesn't move it.
        let Ok((log, _, at)) = logs.get(doer) else {
            continue;
        };
        let room = at.0;
        let theirs: Vec<String> = log
            .0
            .iter()
            .filter(|r| r.state == HELD)
            .map(|r| r.quest.clone())
            .collect();
        let there: Vec<Entity> = crate::groups::members(doer, &groups)
            .into_iter()
            .filter(|e| logs.get(*e).is_ok_and(|(.., at)| at.0 == room))
            .collect();
        for quest_id in theirs {
            let Some(quest) = quests.0.get(&quest_id) else {
                continue;
            };
            for entity in &there {
                let Ok((mut log, player, at)) = logs.get_mut(*entity) else {
                    continue;
                };
                let Some(mut record) = log.get(&quest_id).filter(|r| r.state == HELD).cloned()
                else {
                    continue;
                };
                let holder = Holder {
                    entity: *entity,
                    character_id: player.character_id,
                    room: at.0,
                };
                let (mut lines, finished) = tally(
                    &record,
                    quest,
                    holder.character_id,
                    deed,
                    &mut hand.story,
                    &hand.db,
                    now,
                );
                // All counted and done: it ends, and on to the next stage, or back to report.
                // The last stage ends when it's reported.
                if let Some(i) = finished {
                    match quest.next_after(i) {
                        Some(next) => {
                            lines.extend(hand.act(&quest.stages[i].on_end, quest, holder));
                            let stage = &quest.stages[next];
                            lines.push(format!("\x1b[1;33m{}:\x1b[0m {}", quest.name, stage.text));
                            record.stage = quest.stage_key(next);
                            log.put(record, holder.character_id, &hand.db);
                            lines.extend(hand.act(&stage.on_begin, quest, holder));
                        }
                        None => lines.push(format!(
                            "\x1b[1;33m{}:\x1b[0m that's all it asks. Report to {}.",
                            quest.name,
                            quest.reported_to(&mobs)
                        )),
                    }
                }
                out.extend(lines.into_iter().map(|line| Outgoing::line(*entity, line)));
            }
        }
    }
    deliver(out, &mut outbox);
}

/// Count `deed` toward one holder's `record`: what to tell them, and the stage (where it stands)
/// if this finished the one they were on and every objective on it is counted and done.
fn tally(
    record: &QuestRecord,
    quest: &QuestDef,
    character_id: i64,
    deed: Deed,
    story: &mut Facts,
    db: &Database,
    now: i64,
) -> (Vec<String>, Option<usize>) {
    let mut lines = Vec::new();
    let Some((i, stage)) = quest.stage_at(&record.stage) else {
        return (lines, None);
    };
    let place = quest.stage_key(i);
    let owner = Owner::Quest(character_id, quest.id.clone());
    let mut counted = false;
    for objective in &stage.objectives {
        let Some(key) = objective.key(&place, quest.zone()) else {
            continue;
        };
        if !deed.counts_for(objective, quest.zone()) {
            continue;
        }
        let need = i64::from(objective.count());
        if story.number(&owner, &key, now) >= need {
            continue;
        }
        let have = story.add(&owner, &key, 1, now, db);
        counted = true;
        let done = if have >= need { " Done." } else { "" };
        lines.push(format!(
            "\x1b[1;33m{}:\x1b[0m {}, {have} of {need}.{done}",
            quest.name,
            deed.told()
        ));
    }
    let finished = counted && stage_done(stage, &place, quest, &owner, story, now);
    (lines, finished.then_some(i))
}

/// Whether every objective on the stage kept as `place` is counted as it happens, and done.
fn stage_done(
    stage: &Stage,
    place: &str,
    quest: &QuestDef,
    owner: &Owner,
    story: &Facts,
    now: i64,
) -> bool {
    stage.objectives.iter().all(|objective| {
        objective
            .key(place, quest.zone())
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
        world.init_resource::<ItemTemplates>();
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
            world.run_system_once(count_progress).unwrap();
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
    fn what_a_holder_comes_by_counts_too() {
        use crate::requirements::ItemMatch;
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        world.init_resource::<MobTemplates>();
        let (letter, knife) = (
            ItemKey::parse("riftwatch:letter", ""),
            ItemKey::parse("riftwatch:knife", ""),
        );
        world.insert_resource(ItemTemplates(HashMap::from([
            (
                letter.clone(),
                crate::world::ItemDef::named("a sealed letter"),
            ),
            (knife.clone(), crate::world::ItemDef::named("a knife")),
        ])));
        let quest = QuestDef {
            stages: vec![
                Stage {
                    text: "Find the letter.".into(),
                    objectives: vec![Objective::Obtain(ItemMatch::Item("letter".into()), 1)],
                    ..Default::default()
                },
                Stage {
                    text: "Now take it down the ravine.".into(),
                    objectives: vec![Objective::Reach("ravine".into())],
                    ..Default::default()
                },
            ],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let place = |world: &mut World, id: &str, name: &str| {
            world
                .spawn((
                    Room {
                        name: name.into(),
                        description: String::new(),
                        no_wander: false,
                        indoors: false,
                        terrain: Default::default(),
                    },
                    RoomKey::parse(id, ""),
                ))
                .id()
        };
        let street = place(&mut world, "riftwatch:street", "A Street");
        let room = place(&mut world, "riftwatch:ravine", "Ravine Floor");
        let ada = world
            .spawn((
                InRoom(room),
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
        let obtain = |world: &mut World, key: &ItemKey| {
            let item = world.spawn_empty().id();
            world.write_message(GameEvent::Obtained {
                who: ada,
                item,
                key: key.clone(),
                how: crate::events::Got::PickedUp,
            });
            world.run_system_once(count_progress).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let stage = |world: &World| world.get::<QuestLog>(ada).unwrap().0[0].stage.clone();
        obtain(&mut world, &knife);
        assert_eq!(stage(&world), "1", "not what it asks for");
        obtain(&mut world, &letter);
        assert_eq!(stage(&world), "2", "the letter found: on to the ravine");
        let enter = |world: &mut World, room: Entity| {
            world.write_message(GameEvent::Entered {
                who: ada,
                name: "Ada".into(),
                room,
                way: crate::events::Way::Walked(None),
                witnesses: None,
            });
            world.run_system_once(count_progress).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let reached = |world: &World| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "2.reach.riftwatch:ravine",
                0,
            )
        };
        enter(&mut world, street);
        assert_eq!(reached(&world), 0, "somewhere else");
        enter(&mut world, room);
        assert_eq!(reached(&world), 1);
        let said = |text: &str| {
            world.resource::<Outbox>().0.iter().any(|o| {
                matches!(o,
                Outgoing::To(_, line) if line.contains(text))
            })
        };
        assert!(said("sealed letter found, 1 of 1. Done."));
        assert!(said("Ravine Floor reached, 1 of 1. Done."));
        assert!(said("that's all it asks"));
    }

    #[test]
    fn a_stage_ends_leads_where_it_says_and_the_next_begins() {
        use crate::quests::def::Action;
        use crate::world::{ItemDef, ItemKey};
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        let letter = ItemKey::parse("riftwatch:letter", "");
        world.insert_resource(ItemTemplates(HashMap::from([(
            letter.clone(),
            ItemDef::named("a sealed letter"),
        )])));
        let rat = MobKey::parse("riftwatch:rat", "");
        world.insert_resource(MobTemplates(HashMap::from([
            (rat.clone(), MobDef::named("a rat")),
            (
                MobKey::parse("riftwatch:clerk", ""),
                MobDef::named("Marin Harrow"),
            ),
        ])));
        let kill_rat = || vec![Objective::Kill(MobMatch::Mob("rat".into()), 1)];
        let quest = QuestDef {
            stages: vec![
                Stage {
                    id: Some("nest".into()),
                    text: "Find the nest.".into(),
                    objectives: kill_rat(),
                    next: Some("king".into()),
                    on_end: vec![
                        Action::Set("quest.found_nest".into(), crate::facts::Value::Bool(true)),
                        Action::Say("clerk".into(), "Now the king.".into()),
                    ],
                    ..Default::default()
                },
                Stage {
                    id: Some("skipped".into()),
                    text: "Never reached.".into(),
                    objectives: kill_rat(),
                    ..Default::default()
                },
                Stage {
                    id: Some("king".into()),
                    text: "Kill the king.".into(),
                    objectives: kill_rat(),
                    on_begin: vec![
                        Action::Give("letter".into()),
                        Action::Add("char.nests_found".into(), 1),
                    ],
                    ..Default::default()
                },
            ],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let room = world.spawn_empty().id();
        let ada = world
            .spawn((
                InRoom(room),
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
                    stage: "nest".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }]),
            ))
            .id();
        world.write_message(GameEvent::Died {
            who: room,
            name: "a rat".into(),
            room,
            mob: Some(rat),
            killer: Some(ada),
        });
        world.run_system_once(count_progress).unwrap();
        world.flush();

        assert_eq!(
            world.get::<QuestLog>(ada).unwrap().0[0].stage,
            "king",
            "past 'skipped'"
        );
        let story = world.resource::<Facts>();
        let quest_owner = Owner::Quest(id, "riftwatch:rats".into());
        assert!(
            story.get(&quest_owner, "found_nest", 0).is_some(),
            "the nest's end"
        );
        assert_eq!(
            story.number(&Owner::Character(id), "nests_found", 0),
            1,
            "the king's beginning"
        );
        assert!(
            world
                .query::<&crate::items::Item>()
                .iter(&world)
                .any(|item| item.key == letter),
            "the letter given"
        );
        let said: Vec<String> = world
            .resource::<Outbox>()
            .0
            .iter()
            .filter_map(|o| match o {
                Outgoing::To(_, text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            said.iter()
                .any(|t| t.contains("Marin Harrow says, \"Now the king.\""))
        );
        assert!(said.iter().any(|t| t.contains("Kill the king.")));
        assert!(
            said.iter()
                .any(|t| t.contains("You're given a sealed letter"))
        );
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
                    ..Default::default()
                },
                Stage {
                    text: "Kill a vermin.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Tag("vermin".into()), 1)],
                    ..Default::default()
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
            world.run_system_once(count_progress).unwrap();
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
