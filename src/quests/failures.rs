//! Quests failing as their `fails_if` says: a mob that mustn't die dying (an escort, the quest's
//! own mob), or something a holder had, and was to keep, gone from them. Failing is as a claim
//! running out: written off in Marin's record, merit forfeited, what it gave taken back.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::actions::PendingFailures;
use super::claims::{Claims, take_back, write_off};
use super::def::{Failure, QuestDef, Quests};
use super::journal::{FAILED, HELD, QuestLog};
use super::spawned::QuestMobs;
use crate::db::Database;
use crate::events::GameEvent;
use crate::facts::{Facts, Owner, Value};
use crate::guild::Membership;
use crate::items::{ItemQuery, ItemTemplates};
use crate::output::{Outbox, Outgoing};
use crate::time::unix_now;
use crate::world::{ItemKey, MobKey, Player};

/// What failing a quest reads and changes.
#[derive(SystemParam)]
pub(super) struct Failing<'w, 's> {
    commands: Commands<'w, 's>,
    holders: Query<
        'w,
        's,
        (
            Entity,
            &'static Player,
            &'static mut QuestLog,
            Option<&'static mut Membership>,
        ),
    >,
    claims: ResMut<'w, Claims>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    db: Res<'w, Database>,
    outbox: ResMut<'w, Outbox>,
}

impl Failing<'_, '_> {
    /// Fail `quest` for the character with id `id`, telling them `why` (if anything) if they're
    /// in the game.
    fn fail(&mut self, id: i64, quest: &QuestDef, why: &str) {
        let now = unix_now();
        let online = self
            .holders
            .iter_mut()
            .find(|(_, player, ..)| player.character_id == id);
        match online {
            Some((entity, _, mut log, membership)) => {
                let Some(record) = log.get(&quest.id).filter(|r| r.state == HELD).cloned() else {
                    return;
                };
                let why = if why.is_empty() {
                    String::new()
                } else {
                    format!("{why} ")
                };
                let mut lines = vec![format!(
                    "\x1b[1;31m{}: {why}It's failed.\x1b[0m",
                    quest.name
                )];
                lines.extend(write_off(
                    Some(quest),
                    record,
                    FAILED,
                    "failed",
                    id,
                    Some(&mut log),
                    membership.map(|m| m.into_inner()),
                    &mut self.claims,
                    &self.db,
                    now,
                ));
                lines.extend(take_back(
                    entity,
                    quest,
                    &self.items,
                    &self.templates,
                    &mut self.commands,
                    &self.db,
                ));
                self.outbox.0.push(Outgoing::line(entity, lines.join(" ")));
            }
            None => {
                let record = self
                    .db
                    .character_quests(id)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|r| r.quest == quest.id && r.state == HELD);
                if let Some(record) = record {
                    write_off(
                        Some(quest),
                        record,
                        FAILED,
                        "failed",
                        id,
                        None,
                        None,
                        &mut self.claims,
                        &self.db,
                        now,
                    );
                }
            }
        }
    }
}

/// A death a quest's `fails_if` names fails it: the quest's own mob, for those it came for; any
/// other of that template, for everyone holding the quest, in the game or not.
pub(super) fn fail_on_deaths(
    mut events: MessageReader<GameEvent>,
    quests: Res<Quests>,
    quest_mobs: Res<QuestMobs>,
    mut failing: Failing,
) {
    for event in events.read() {
        let GameEvent::Died {
            who,
            mob: Some(mob),
            name,
            ..
        } = event
        else {
            continue;
        };
        for quest in quests.0.values() {
            let dies = quest
                .fails_if
                .iter()
                .any(|f| matches!(f, Failure::Dies(id) if MobKey::parse(id, quest.zone()) == *mob));
            if !dies {
                continue;
            }
            let holders = match quest_mobs.0.get(who) {
                Some(q) if q.quest == quest.id => q.holders.clone(),
                Some(_) => continue,
                None => failing.db.quest_holders(&quest.id).unwrap_or_default(),
            };
            let why = format!(
                "{} is dead.",
                crate::items::capitalized(&crate::combat::the(name))
            );
            for id in holders {
                failing.fail(id, quest, &why);
            }
        }
    }
}

/// Something a holder in the game has had for a quest that fails if it's lost, and hasn't now,
/// fails it. Having it is kept as the quest's `had.<item>` fact, from the first time they carry
/// it, so a quest isn't failed for what it hasn't given them yet.
pub(super) fn fail_on_losses(quests: Res<Quests>, mut story: ResMut<Facts>, mut failing: Failing) {
    let now = unix_now();
    let mut lost = Vec::new();
    for (entity, player, log, _) in &failing.holders {
        let carried: Vec<ItemKey> =
            crate::items::carried(entity, &failing.items, &failing.templates)
                .into_iter()
                .map(|(key, _)| key)
                .collect();
        for record in log.0.iter().filter(|r| r.state == HELD) {
            let Some(quest) = quests.0.get(&record.quest) else {
                continue;
            };
            let owner = Owner::Quest(player.character_id, quest.id.clone());
            for failure in &quest.fails_if {
                let Failure::Lost(id) = failure else {
                    continue;
                };
                let key = ItemKey::parse(id, quest.zone());
                let had = had_key(&key);
                match (
                    carried.contains(&key),
                    story.get(&owner, &had, now).is_some(),
                ) {
                    (true, false) => story.set(&owner, &had, Value::Bool(true), None, &failing.db),
                    (false, true) => {
                        let name = failing
                            .templates
                            .0
                            .get(&key)
                            .map_or_else(|| key.to_string(), |d| d.name.clone());
                        lost.push((player.character_id, quest.id.clone(), name));
                    }
                    _ => {}
                }
            }
        }
    }
    for (id, quest_id, name) in lost {
        if let Some(quest) = quests.0.get(&quest_id) {
            failing.fail(id, quest, &format!("You no longer have {name}."));
        }
    }
}

/// Quests a stage's `Fail` failed, written off.
pub(super) fn fail_pending(
    mut pending: ResMut<PendingFailures>,
    quests: Res<Quests>,
    mut failing: Failing,
) {
    for (id, quest_id) in std::mem::take(&mut pending.0) {
        if let Some(quest) = quests.0.get(&quest_id) {
            failing.fail(id, quest, "");
        }
    }
}

/// The quest fact that keeps that a holder has had an item it mustn't lose.
pub(super) fn had_key(item: &ItemKey) -> String {
    format!("had.{item}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QuestRecord;
    use crate::items::{Item, ItemLocation, Quality};
    use crate::quests::spawned::QuestMob;
    use crate::quests::testing::rats;
    use crate::world::{DoorState, ItemDef};
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashMap;

    fn held() -> QuestRecord {
        QuestRecord {
            quest: "riftwatch:rats".into(),
            state: HELD.into(),
            stage: "1".into(),
            taken_at: 0,
            due_at: None,
            finished_at: None,
        }
    }

    /// A world with a quest that fails as `fails_if` says, and Ada in the game holding it; and
    /// Bob, away, holding it too.
    fn world(fails_if: Vec<Failure>) -> (World, Entity, i64, i64) {
        let db = Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let ada = db.create_character(account.id, "Ada").unwrap().id;
        let bob = db.create_character(account.id, "Bob").unwrap().id;
        db.save_character_quest(ada, &held()).unwrap();
        db.save_character_quest(bob, &held()).unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Claims>();
        world.init_resource::<Outbox>();
        world.init_resource::<Facts>();
        world.init_resource::<QuestMobs>();
        world.init_resource::<Messages<GameEvent>>();
        world.init_resource::<PendingFailures>();
        world.insert_resource(ItemTemplates(HashMap::from([(
            ItemKey::parse("riftwatch:letter", ""),
            ItemDef::named("a sealed letter"),
        )])));
        let quest = QuestDef { fails_if, ..rats() };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let her = world
            .spawn((
                Player {
                    name: "Ada".into(),
                    character_id: ada,
                    account: "ada".into(),
                    role: crate::db::Role::Player,
                    race: "human".into(),
                    background: String::new(),
                    description: String::new(),
                },
                QuestLog(vec![held()]),
            ))
            .id();
        (world, her, ada, bob)
    }

    fn state(world: &World, her: Entity, bob: i64) -> (String, String) {
        let db = world.resource::<Database>();
        (
            world.get::<QuestLog>(her).unwrap().0[0].state.clone(),
            db.character_quests(bob).unwrap()[0].state.clone(),
        )
    }

    fn died(world: &mut World, who: Entity, mob: &str) {
        world.write_message(GameEvent::Died {
            who,
            name: "Narina Greenthorn".into(),
            room: who,
            mob: Some(MobKey::parse(mob, "")),
            killer: None,
        });
        world.run_system_once(fail_on_deaths).unwrap();
        world.resource_mut::<Messages<GameEvent>>().clear();
    }

    #[test]
    fn an_escort_dying_fails_everyone_on_the_quest() {
        let (mut world, her, _, bob) = world(vec![Failure::Dies("herbalist".into())]);
        let someone = world.spawn_empty().id();
        died(&mut world, someone, "riftwatch:hunter");
        assert_eq!(
            state(&world, her, bob),
            (HELD.into(), HELD.into()),
            "someone else"
        );
        died(&mut world, someone, "riftwatch:herbalist");
        assert_eq!(state(&world, her, bob), (FAILED.into(), FAILED.into()));
        assert!(world.resource::<Outbox>().0.iter().any(|o| matches!(o,
            Outgoing::To(_, text) if text.contains("Narina Greenthorn is dead. It's failed."))));
    }

    #[test]
    fn a_quests_own_mob_dying_fails_only_those_it_came_for() {
        let (mut world, her, ada, bob) = world(vec![Failure::Dies("courier".into())]);
        let courier = world.spawn_empty().id();
        world.resource_mut::<QuestMobs>().0.insert(
            courier,
            QuestMob {
                quest: "riftwatch:rats".into(),
                mob: MobKey::parse("riftwatch:courier", ""),
                holders: vec![ada],
            },
        );
        died(&mut world, courier, "riftwatch:courier");
        assert_eq!(state(&world, her, bob), (FAILED.into(), HELD.into()));
    }

    #[test]
    fn a_stage_that_fails_it_does() {
        let (mut world, her, ada, bob) = world(Vec::new());
        world
            .resource_mut::<PendingFailures>()
            .0
            .push((ada, "riftwatch:rats".into()));
        world.run_system_once(fail_pending).unwrap();
        assert_eq!(state(&world, her, bob), (FAILED.into(), HELD.into()));
        assert!(world.resource::<PendingFailures>().0.is_empty());
        assert!(world.resource::<Outbox>().0.iter().any(|o| matches!(o,
            Outgoing::To(_, text) if text.contains("Rats: It's failed."))));
    }

    #[test]
    fn losing_what_you_had_fails_it_but_not_having_it_yet_doesnt() {
        let (mut world, her, _, bob) = world(vec![Failure::Lost("letter".into())]);
        let run = |world: &mut World| {
            world.run_system_once(fail_on_losses).unwrap();
            world.flush();
        };
        run(&mut world);
        assert_eq!(state(&world, her, bob).0, HELD, "not given it yet");
        let letter = world
            .spawn((
                Item {
                    key: ItemKey::parse("riftwatch:letter", ""),
                    name: "a sealed letter".into(),
                    id: None,
                    state: DoorState::Open,
                    quality: Quality::Standard,
                },
                ItemLocation::Held(her),
            ))
            .id();
        run(&mut world);
        assert_eq!(state(&world, her, bob).0, HELD, "has it");
        world.despawn(letter);
        run(&mut world);
        assert_eq!(state(&world, her, bob).0, FAILED, "lost it");
        assert_eq!(
            state(&world, her, bob).1,
            HELD,
            "Bob, away, can't have lost it"
        );
    }
}
