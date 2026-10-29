//! Counting what holders do toward their quests (kills, things obtained, places reached), as the
//! quest's own facts, and moving them from stage to stage.

use bevy::prelude::*;

use super::actions::{Holder, Stagehand};
use super::def::{Lead, MobMatch, Objective, QuestDef, Quests, chose_key};
use super::escort::Escort;
use super::journal::{HELD, QuestLog};
use super::spawned::QuestMobs;
use crate::db::{Database, QuestRecord};
use crate::events::GameEvent;
use crate::facts::{Facts, Owner, Value};
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
        /// The one killed, for a quest's own mob.
        who: Entity,
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
    Asked {
        mob: &'a MobKey,
        topic: &'a str,
        name: &'a str,
    },
    /// They gave an item to a mob.
    Gave {
        /// The item given, for the quest to take.
        given: Entity,
        item: &'a ItemKey,
        to: &'a MobKey,
        name: &'a str,
        whom: &'a str,
    },
    /// A mob they're escorting walked into a room.
    Escorted {
        mob: &'a MobKey,
        room: &'a RoomKey,
        name: &'a str,
    },
}

impl Deed<'_> {
    /// Whether `objective`, in `quest`, counts it for the holder with id `holder`: a quest's own
    /// mob only for those it came for.
    fn counts_for(
        &self,
        objective: &Objective,
        quest: &QuestDef,
        holder: i64,
        quest_mobs: &QuestMobs,
    ) -> bool {
        let zone = quest.zone();
        match (self, objective) {
            (Deed::Killed { who, mob, .. }, Objective::Kill(MobMatch::Quest(id), _)) => {
                MobKey::parse(id, zone) == **mob
                    && quest_mobs.came_for(*who, &quest.id, mob, holder)
            }
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
            (Deed::Gave { item, to, .. }, Objective::Give { item: id, to: whom }) => {
                ItemKey::parse(id, zone) == **item && MobKey::parse(whom, zone) == **to
            }
            (Deed::Escorted { mob, room, .. }, Objective::Escort { mob: id, to }) => {
                MobKey::parse(id, zone) == **mob && RoomKey::parse(to, zone) == **room
            }
            (Deed::Asked { mob, topic, .. }, Objective::Ask(id, about)) => {
                MobKey::parse(id, zone) == **mob && about.eq_ignore_ascii_case(topic)
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
            Deed::Escorted { name, .. } => format!("{} brought safely", crate::combat::the(name)),
            Deed::Gave { name, whom, .. } => format!(
                "{} given to {}",
                without_article(name),
                crate::combat::the(whom)
            ),
            Deed::Asked { name, topic, .. } => {
                format!("{} asked about {topic}", crate::combat::the(name))
            }
        }
    }
}

/// Rooms, by their keys and names.
type Places<'w, 's> = Query<'w, 's, (&'static Room, &'static RoomKey)>;
/// Mobs escorting someone.
type Escorts<'w, 's> = Query<'w, 's, (&'static Escort, &'static crate::mobs::Mob)>;

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
    (places, escorts, takers): (Places, Escorts, Query<&crate::mobs::Mob>),
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let mut out = Vec::new();
    for event in events.read() {
        let (doer, deed) = match event {
            GameEvent::Died {
                who,
                mob: Some(mob),
                killer: Some(killer),
                name,
                ..
            } => (
                *killer,
                Deed::Killed {
                    who: *who,
                    mob,
                    def: mobs.0.get(mob),
                    name,
                },
            ),
            // An escort walking in is its holder's deed.
            GameEvent::Entered { who, room, .. } if escorts.get(*who).is_ok() => {
                let (Ok((escort, mob)), Ok((_, key))) = (escorts.get(*who), places.get(*room))
                else {
                    continue;
                };
                (
                    escort.holder,
                    Deed::Escorted {
                        mob: &mob.key,
                        room: key,
                        name: &mob.name,
                    },
                )
            }
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
            // Given to a mob: someone a quest may want it given to.
            GameEvent::Gave { who, item, key, to } => {
                let (Ok(mob), Some(def)) = (takers.get(*to), items.0.get(key)) else {
                    continue;
                };
                (
                    *who,
                    Deed::Gave {
                        given: *item,
                        item: key,
                        to: &mob.key,
                        name: &def.name,
                        whom: &mob.name,
                    },
                )
            }
            GameEvent::Asked { who, mob, topic } => (
                *who,
                Deed::Asked {
                    mob,
                    topic,
                    name: mobs.0.get(mob).map_or("someone", |def| def.name.as_str()),
                },
            ),
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
        // What the quest takes, given as it asked.
        let mut taken = None;
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
                let (mut lines, counted, finished) = tally(
                    &record,
                    quest,
                    holder.character_id,
                    deed,
                    &mut hand.story,
                    (&hand.db, &hand.quest_mobs),
                    now,
                );
                // Given as the quest asked, the quest takes it; and it isn't lost.
                if counted && let Deed::Gave { given, item, .. } = deed {
                    taken = Some(given);
                    hand.story.clear(
                        &Owner::Quest(holder.character_id, quest.id.clone()),
                        &super::failures::had_key(item),
                        &hand.db,
                    );
                }
                // All counted and done: it ends, and on to the next stage, or back to report.
                // The last stage ends when it's reported.
                if let Some((i, way)) = finished {
                    match quest.lead(i, way) {
                        Lead::Stage(next) => {
                            lines.extend(hand.act(&quest.stages[i].on_end, quest, holder));
                            // A stage that failed it leads nowhere.
                            let failed = (holder.character_id, quest.id.clone());
                            if hand.failed.0.contains(&failed) {
                                out.extend(
                                    lines.drain(..).map(|line| Outgoing::line(*entity, line)),
                                );
                                continue;
                            }
                            let stage = &quest.stages[next];
                            lines.push(format!("\x1b[1;33m{}:\x1b[0m {}", quest.name, stage.text));
                            record.stage = quest.stage_key(next);
                            log.put(record, holder.character_id, &hand.db);
                            lines.extend(hand.act(&stage.on_begin, quest, holder));
                        }
                        Lead::Report(_) => {
                            // To whoever the way taken is reported to.
                            let choice = way.and_then(|c| quest.stages[i].choices.get(c - 1));
                            let to = quest.named(&quest.report_key(choice), &mobs);
                            lines.push(format!(
                                "\x1b[1;33m{}:\x1b[0m that's all it asks. Report to {to}.",
                                quest.name
                            ))
                        }
                    }
                }
                out.extend(lines.into_iter().map(|line| Outgoing::line(*entity, line)));
            }
        }
        if let Some(given) = taken
            && let Ok(mut entity) = hand.commands.get_entity(given)
        {
            entity.despawn();
        }
    }
    deliver(out, &mut outbox);
}

/// A stage finished: where it stands, and the way taken through it, if it offers choices.
type Finished = Option<(usize, Option<usize>)>;

/// Count `deed` toward one holder's `record`: what to tell them, whether it counted, and, if this
/// finished the stage
/// they were on (every objective counted as it happens, and done), where it stands and the way
/// taken through it, if it offers choices. The first way done is the way taken; the others close.
fn tally(
    record: &QuestRecord,
    quest: &QuestDef,
    character_id: i64,
    deed: Deed,
    story: &mut Facts,
    (db, quest_mobs): (&Database, &QuestMobs),
    now: i64,
) -> (Vec<String>, bool, Finished) {
    let mut lines = Vec::new();
    let Some((i, stage)) = quest.stage_at(&record.stage) else {
        return (lines, false, None);
    };
    let place = quest.stage_key(i);
    let owner = Owner::Quest(character_id, quest.id.clone());
    let chosen = story.number(&owner, &chose_key(&place), now);
    let open = |choice: Option<usize>| choice.is_none_or(|c| chosen == 0 || chosen == c as i64);
    let parts = stage.parts(&place);
    let mut counted = false;
    for (_, at, objectives) in parts.iter().filter(|(choice, ..)| open(*choice)) {
        for objective in objectives.iter() {
            let Some(key) = objective.key(at, quest.zone()) else {
                continue;
            };
            if !deed.counts_for(objective, quest, character_id, quest_mobs) {
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
    }
    // Done when all of it is counted and done: a way that asks for something on reporting is
    // taken on reporting.
    let done = |objectives: &[Objective], at: &str| {
        objectives.iter().all(|objective| {
            objective
                .key(at, quest.zone())
                .is_some_and(|key| story.number(&owner, &key, now) >= i64::from(objective.count()))
        })
    };
    if !counted || !done(&stage.objectives, &place) {
        return (lines, counted, None);
    }
    if stage.choices.is_empty() {
        return (lines, true, Some((i, None)));
    }
    let way = parts.iter().find_map(|(choice, at, objectives)| {
        let c = (*choice)?;
        (open(Some(c)) && done(objectives, at)).then_some(c)
    });
    let Some(c) = way else {
        return (lines, true, None);
    };
    if chosen == 0 {
        story.set(&owner, &chose_key(&place), Value::Int(c as i64), None, db);
        lines.push(format!(
            "\x1b[1;33m{}:\x1b[0m {}",
            quest.name,
            stage.choices[c - 1].text
        ));
    }
    (lines, true, Some((i, Some(c))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What stage actions need, besides what each test sets up.
    fn stagehand(world: &mut World) {
        world.init_resource::<QuestMobs>();
        world.init_resource::<crate::quests::actions::PendingFailures>();
        world.init_resource::<crate::world::RoomIndex>();
        world.init_resource::<crate::skills::SkillDefs>();
        world.init_resource::<crate::characters::Races>();
        world.insert_resource(crate::dice::Dice::seeded(1));
    }
    use crate::items::ItemTemplates;
    use crate::quests::def::{MobMatch, Next, Stage};
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
        stagehand(&mut world);
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
        stagehand(&mut world);
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
        stagehand(&mut world);
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
                    next: Some(Next::Stage("king".into())),
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
    fn the_first_way_done_is_the_way_taken() {
        use crate::quests::def::{Choice, Way};
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        stagehand(&mut world);
        world.init_resource::<ItemTemplates>();
        let (rat, roach) = (
            MobKey::parse("riftwatch:rat", ""),
            MobKey::parse("riftwatch:roach", ""),
        );
        world.insert_resource(MobTemplates(HashMap::from([
            (rat.clone(), MobDef::named("a rat")),
            (roach.clone(), MobDef::named("a roach")),
        ])));
        let kill = |id: &str| vec![Objective::Kill(MobMatch::Mob(id.into()), 1)];
        let quest = QuestDef {
            stages: vec![
                Stage {
                    id: Some("which".into()),
                    text: "Rats or roaches?".into(),
                    choices: vec![
                        Choice {
                            text: "Rats, then.".into(),
                            objectives: kill("rat"),
                            next: Some(Next::Way(Way::End("rats".into()))),
                            report_to: None,
                        },
                        Choice {
                            text: "Roaches, then.".into(),
                            objectives: kill("roach"),
                            next: Some(Next::Stage("more".into())),
                            report_to: None,
                        },
                    ],
                    ..Default::default()
                },
                Stage {
                    id: Some("more".into()),
                    text: "More roaches.".into(),
                    objectives: kill("roach"),
                    ..Default::default()
                },
            ],
            endings: [(
                "rats".to_string(),
                crate::quests::def::EndingDef {
                    journal: "Rats it was.".into(),
                    rewards: Vec::new(),
                },
            )]
            .into(),
            ..rats()
        };
        assert!(quest.check().is_ok(), "{:?}", quest.check());
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let room = world.spawn_empty().id();
        let holder = |world: &mut World, name: &str| {
            let id = world
                .resource::<Database>()
                .create_character(account.id, name)
                .unwrap()
                .id;
            let e = world
                .spawn((
                    InRoom(room),
                    Player {
                        name: name.into(),
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
                        stage: "which".into(),
                        taken_at: 0,
                        due_at: None,
                        finished_at: None,
                    }]),
                ))
                .id();
            (e, id)
        };
        let (ada, ada_id) = holder(&mut world, "Ada");
        let (bob, bob_id) = holder(&mut world, "Bob");
        let kill = |world: &mut World, mob: &MobKey, killer: Entity| {
            world.write_message(GameEvent::Died {
                who: room,
                name: "a beast".into(),
                room,
                mob: Some(mob.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_progress).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let stage =
            |world: &World, who: Entity| world.get::<QuestLog>(who).unwrap().0[0].stage.clone();
        let chose = |world: &World, id: i64| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "which.chose",
                0,
            )
        };
        // Ada kills a roach: that way, on to more roaches.
        kill(&mut world, &roach, ada);
        assert_eq!(
            (stage(&world, ada), chose(&world, ada_id)),
            ("more".into(), 2)
        );
        // Bob kills a rat: that way, which ends for reporting; he stays to report.
        kill(&mut world, &rat, bob);
        assert_eq!(
            (stage(&world, bob), chose(&world, bob_id)),
            ("which".into(), 1)
        );
        // The roach way is closed to him now.
        kill(&mut world, &roach, bob);
        assert_eq!(stage(&world, bob), "which", "a closed way counts no more");
        assert_eq!(
            world.resource::<Facts>().number(
                &Owner::Quest(bob_id, "riftwatch:rats".into()),
                "which.2.kill.riftwatch:roach",
                0
            ),
            0
        );
    }

    #[test]
    fn a_quest_mob_counts_only_for_those_it_came_for() {
        use crate::quests::spawned::QuestMob;
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        stagehand(&mut world);
        world.init_resource::<ItemTemplates>();
        let courier = MobKey::parse("sewers:courier", "");
        world.insert_resource(MobTemplates(HashMap::from([(
            courier.clone(),
            MobDef::named("a courier"),
        )])));
        let quest = QuestDef {
            stages: vec![Stage {
                text: "Find the courier.".into(),
                objectives: vec![Objective::Kill(MobMatch::Quest("sewers:courier".into()), 1)],
                ..Default::default()
            }],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let room = world.spawn_empty().id();
        let holder = |world: &mut World, name: &str| {
            let id = world
                .resource::<Database>()
                .create_character(account.id, name)
                .unwrap()
                .id;
            let e = world
                .spawn((
                    InRoom(room),
                    Player {
                        name: name.into(),
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
            (e, id)
        };
        let (ada, ada_id) = holder(&mut world, "Ada");
        let (bob, bob_id) = holder(&mut world, "Bob");
        // Ada's courier, and an ordinary one.
        let (hers, ordinary) = (world.spawn_empty().id(), world.spawn_empty().id());
        world.resource_mut::<QuestMobs>().0.insert(
            hers,
            QuestMob {
                quest: "riftwatch:rats".into(),
                mob: courier.clone(),
                holders: vec![ada_id],
            },
        );
        let kill = |world: &mut World, who: Entity, killer: Entity| {
            world.write_message(GameEvent::Died {
                who,
                name: "a courier".into(),
                room,
                mob: Some(courier.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_progress).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let count = |world: &World, id: i64| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "1.kill.quest.sewers:courier",
                0,
            )
        };
        kill(&mut world, ordinary, ada);
        assert_eq!(count(&world, ada_id), 0, "any courier won't do");
        kill(&mut world, hers, bob);
        assert_eq!(count(&world, bob_id), 0, "it didn't come for Bob");
        kill(&mut world, hers, ada);
        assert_eq!(count(&world, ada_id), 1, "hers");
    }

    #[test]
    fn an_escort_walking_in_counts_for_its_holder() {
        use crate::quests::escort::Escort;
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        stagehand(&mut world);
        world.init_resource::<ItemTemplates>();
        world.init_resource::<MobTemplates>();
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
        let place = |world: &mut World, id: &str| {
            world
                .spawn((
                    Room {
                        name: id.into(),
                        description: String::new(),
                        no_wander: false,
                        indoors: false,
                        terrain: Default::default(),
                    },
                    RoomKey::parse(id, ""),
                ))
                .id()
        };
        let (road, ravine) = (
            place(&mut world, "forest:road"),
            place(&mut world, "forest:ravine"),
        );
        let ada = world
            .spawn((
                InRoom(ravine),
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
        let narina = world
            .spawn((
                crate::mobs::Mob {
                    key: MobKey::parse("riftwatch:herbalist", ""),
                    name: "Narina Greenthorn".into(),
                    here: String::new(),
                    description: String::new(),
                    race: None,
                },
                Escort {
                    holder: ada,
                    quest: "riftwatch:rats".into(),
                    home: road,
                },
            ))
            .id();
        let walk = |world: &mut World, room: Entity| {
            world.write_message(GameEvent::Entered {
                who: narina,
                name: "Narina Greenthorn".into(),
                room,
                way: crate::events::Way::Walked(None),
                witnesses: None,
            });
            world.run_system_once(count_progress).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let count = |world: &World| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "1.escort.riftwatch:herbalist.forest:ravine",
                0,
            )
        };
        walk(&mut world, road);
        assert_eq!(count(&world), 0, "not there yet");
        walk(&mut world, ravine);
        assert_eq!(count(&world), 1, "brought safely");
    }

    #[test]
    fn what_is_given_as_asked_counts_and_the_quest_takes_it() {
        use crate::items::{Item, ItemLocation, Quality};
        use crate::world::{DoorState, ItemDef};
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        stagehand(&mut world);
        let crystals = ItemKey::parse("riftwatch:crystals", "");
        world.insert_resource(ItemTemplates(HashMap::from([(
            crystals.clone(),
            ItemDef::named("a pouch of crystals"),
        )])));
        world.init_resource::<MobTemplates>();
        let quest = QuestDef {
            stages: vec![Stage {
                text: "Return the crystals.".into(),
                objectives: vec![Objective::Give {
                    item: "crystals".into(),
                    to: "clerk".into(),
                }],
                ..Default::default()
            }],
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
                    stage: "1".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }]),
            ))
            .id();
        let mob = |world: &mut World, id: &str, name: &str| {
            world
                .spawn((
                    crate::mobs::Mob {
                        key: MobKey::parse(id, ""),
                        name: name.into(),
                        here: String::new(),
                        description: String::new(),
                        race: None,
                    },
                    InRoom(room),
                ))
                .id()
        };
        let (marin, talla) = (
            mob(&mut world, "riftwatch:clerk", "Marin Harrow"),
            mob(&mut world, "riftwatch:fence", "Talla Mirk"),
        );
        let owner = Owner::Quest(id, "riftwatch:rats".into());
        let had = super::super::failures::had_key(&crystals);
        world.resource_scope(|world, mut story: Mut<Facts>| {
            let db = world.resource::<Database>();
            story.set(&owner, &had, Value::Bool(true), None, db);
        });
        let give = |world: &mut World, to: Entity| {
            let item = world
                .spawn((
                    Item {
                        key: crystals.clone(),
                        name: "a pouch of crystals".into(),
                        id: None,
                        state: DoorState::Open,
                        quality: Quality::Standard,
                    },
                    ItemLocation::Held(to),
                ))
                .id();
            world.write_message(GameEvent::Gave {
                who: ada,
                item,
                key: crystals.clone(),
                to,
            });
            world.run_system_once(count_progress).unwrap();
            world.flush();
            world.resource_mut::<Messages<GameEvent>>().clear();
            item
        };
        let given = |world: &World| {
            world
                .resource::<Facts>()
                .number(&owner, "1.give.riftwatch:crystals.riftwatch:clerk", 0)
        };
        let to_talla = give(&mut world, talla);
        assert_eq!(given(&world), 0, "not who it asks for");
        assert!(world.get_entity(to_talla).is_ok(), "Talla keeps hers");
        let to_marin = give(&mut world, marin);
        assert_eq!(given(&world), 1);
        assert!(world.get_entity(to_marin).is_err(), "the quest takes it");
        assert!(
            world.resource::<Facts>().get(&owner, &had, 0).is_none(),
            "given as asked, it isn't lost"
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
        stagehand(&mut world);
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
