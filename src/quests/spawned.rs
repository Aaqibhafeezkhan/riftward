//! Quest mobs: those a stage's `Spawn` brought into the world for the holders it began for. Anyone
//! can see and fight one, but it counts only for them (`Kill(Quest("courier"))`), and it's
//! cleared away once none of them holds the quest.

use std::collections::HashMap;

use bevy::prelude::*;

use super::journal::{HELD, QuestLog};
use crate::db::Database;
use crate::world::{MobKey, Player};

/// Every quest mob, by its entity: kept after it dies, so its death still counts, until no one
/// it came for holds the quest.
#[derive(Resource, Default, Debug)]
pub struct QuestMobs(pub HashMap<Entity, QuestMob>);

#[derive(Debug, Clone, PartialEq)]
pub struct QuestMob {
    /// `zone:quest`.
    pub quest: String,
    /// The template it was spawned from, as `Kill(Quest(...))` names it.
    pub mob: MobKey,
    /// Who it came for, by character id.
    pub holders: Vec<i64>,
}

impl QuestMobs {
    /// The quest mob of `quest` from template `mob` already about in `room`, if there is one.
    pub(super) fn here(
        &self,
        quest: &str,
        mob: &MobKey,
        room: Entity,
        rooms: &Query<&crate::world::InRoom>,
    ) -> Option<Entity> {
        self.0
            .iter()
            .find(|(entity, q)| {
                q.quest == quest && q.mob == *mob && rooms.get(**entity).is_ok_and(|r| r.0 == room)
            })
            .map(|(entity, _)| *entity)
    }

    /// Whether `entity` was a quest mob of `quest`, of template `mob`, that came for `holder`.
    pub(super) fn came_for(&self, entity: Entity, quest: &str, mob: &MobKey, holder: i64) -> bool {
        self.0
            .get(&entity)
            .is_some_and(|q| q.quest == quest && q.mob == *mob && q.holders.contains(&holder))
    }
}

/// Quest mobs whose holders all hold the quest no more (done, failed, given up, or gone) are
/// cleared away, and forgotten.
pub(super) fn clear_quest_mobs(
    mut commands: Commands,
    mut quest_mobs: ResMut<QuestMobs>,
    logs: Query<(&QuestLog, &Player)>,
    alive: Query<(), With<crate::mobs::Mob>>,
    db: Res<Database>,
) {
    if quest_mobs.0.is_empty() {
        return;
    }
    let holds = |id: i64, quest: &str| match logs.iter().find(|(_, p)| p.character_id == id) {
        Some((log, _)) => log.get(quest).is_some_and(|r| r.state == HELD),
        None => db
            .character_quests(id)
            .unwrap_or_default()
            .iter()
            .any(|r| r.quest == quest && r.state == HELD),
    };
    let mut gone = Vec::new();
    for (entity, q) in quest_mobs.0.iter_mut() {
        q.holders.retain(|id| holds(*id, &q.quest));
        if q.holders.is_empty() {
            gone.push(*entity);
        }
    }
    for entity in gone {
        quest_mobs.0.remove(&entity);
        if alive.get(entity).is_ok() {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QuestRecord;
    use crate::quests::journal::DONE;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn a_quest_mob_goes_once_no_one_it_came_for_holds_the_quest() {
        let db = Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let ada = db.create_character(account.id, "Ada").unwrap().id;
        let bob = db.create_character(account.id, "Bob").unwrap().id;
        // Bob is away, still on the quest.
        let held = |state: &str| QuestRecord {
            quest: "riftwatch:crystals".into(),
            state: state.into(),
            stage: "1".into(),
            taken_at: 0,
            due_at: None,
            finished_at: None,
        };
        db.save_character_quest(bob, &held(HELD)).unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        let player = |name: &str, id| Player {
            name: name.into(),
            character_id: id,
            account: "ada".into(),
            role: crate::db::Role::Player,
            race: "human".into(),
            background: String::new(),
            description: String::new(),
        };
        let her = world
            .spawn((player("Ada", ada), QuestLog(vec![held(HELD)])))
            .id();
        let courier = world
            .spawn(crate::mobs::Mob {
                key: MobKey::parse("sewers:courier", ""),
                name: "a courier".into(),
                here: String::new(),
                description: String::new(),
                race: None,
            })
            .id();
        let mut quest_mobs = QuestMobs::default();
        quest_mobs.0.insert(
            courier,
            QuestMob {
                quest: "riftwatch:crystals".into(),
                mob: MobKey::parse("sewers:courier", ""),
                holders: vec![ada, bob],
            },
        );
        world.insert_resource(quest_mobs);
        let run = |world: &mut World| {
            world.run_system_once(clear_quest_mobs).unwrap();
            world.flush();
        };
        run(&mut world);
        assert!(world.get_entity(courier).is_ok(), "both still on it");
        world.get_mut::<QuestLog>(her).unwrap().0[0].state = DONE.into();
        run(&mut world);
        assert!(world.get_entity(courier).is_ok(), "Bob, away, still is");
        assert_eq!(world.resource::<QuestMobs>().0[&courier].holders, vec![bob]);
        let db = world.resource::<Database>();
        db.save_character_quest(bob, &held("abandoned")).unwrap();
        run(&mut world);
        assert!(world.get_entity(courier).is_err(), "cleared away");
        assert!(world.resource::<QuestMobs>().0.is_empty());
    }
}
