//! What a stage does as it begins or ends (`on_begin`, `on_end`), for the holder it begins or
//! ends for: hand them an item or take one, change a fact, or have someone say something.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::def::{Action, QuestDef};
use super::spawned::{QuestMob, QuestMobs};
use crate::db::Database;
use crate::facts::{Facts, Owner};
use crate::items::{ItemQuery, ItemTemplates, Recipient};
use crate::mobs::MobTemplates;
use crate::output::RoomQuery;
use crate::world::{ItemKey, MobKey, RoomKey};

/// What stage actions read and change.
#[derive(SystemParam)]
pub(super) struct Stagehand<'w, 's> {
    pub(super) commands: Commands<'w, 's>,
    pub(super) items: ItemQuery<'w, 's>,
    pub(super) item_templates: Res<'w, ItemTemplates>,
    pub(super) mob_templates: Res<'w, MobTemplates>,
    pub(super) rooms: RoomQuery<'w, 's>,
    pub(super) story: ResMut<'w, Facts>,
    pub(super) db: Res<'w, Database>,
    /// For a quest's mobs: where they go, and how they're made.
    pub(super) quest_mobs: ResMut<'w, QuestMobs>,
    index: Res<'w, crate::world::RoomIndex>,
    places: Query<'w, 's, &'static crate::world::InRoom>,
    skill_defs: Res<'w, crate::skills::SkillDefs>,
    races: Res<'w, crate::characters::Races>,
    dice: ResMut<'w, crate::dice::Dice>,
    /// Quests a `Fail` failed, for `failures::fail_pending` to write off.
    pub(super) failed: ResMut<'w, PendingFailures>,
}

/// Quests a stage's `Fail` has failed, by character id and quest, waiting to be written off.
#[derive(Resource, Default, Debug)]
pub struct PendingFailures(pub Vec<(i64, String)>);

/// One holder a stage begins or ends for: a player, by their saved id.
#[derive(Clone, Copy, Debug)]
pub(super) struct Holder {
    pub(super) entity: Entity,
    pub(super) character_id: i64,
    pub(super) room: Entity,
}

impl From<Holder> for Recipient {
    fn from(holder: Holder) -> Self {
        Self {
            entity: holder.entity,
            character_id: Some(holder.character_id),
            room: holder.room,
        }
    }
}

impl Stagehand<'_, '_> {
    /// Carry out `actions` of `quest` for `holder`. What to tell them.
    pub(super) fn act(
        &mut self,
        actions: &[Action],
        quest: &QuestDef,
        holder: Holder,
    ) -> Vec<String> {
        let zone = quest.zone();
        let mut lines = Vec::new();
        let mut held = crate::items::hands_full(holder.entity, &self.items);
        let mut taken = Vec::new();
        for action in actions {
            match action {
                Action::Give(id) => {
                    let given = crate::items::hand_new(
                        holder.into(),
                        ItemKey::parse(id, zone),
                        &mut held,
                        &mut self.commands,
                        &self.item_templates,
                        &self.rooms,
                        &self.db,
                    );
                    lines.push(format!("You're given {given}."));
                }
                Action::Take(id) => {
                    let key = ItemKey::parse(id, zone);
                    let found = crate::items::carried_items(
                        holder.entity,
                        &self.items,
                        &self.item_templates,
                    )
                    .into_iter()
                    .find(|(entity, k, _)| *k == key && !taken.contains(entity));
                    if let Some((entity, ..)) = found
                        && let Ok((_, item, _)) = self.items.get(entity)
                    {
                        taken.push(entity);
                        lines.push(format!("You hand over {}.", item.name));
                        // Handed over as asked, it isn't lost.
                        self.story.clear(
                            &Owner::Quest(holder.character_id, quest.id.clone()),
                            &super::failures::had_key(&key),
                            &self.db,
                        );
                        crate::healing::used_up(&mut self.commands, &self.db, entity, item);
                    }
                }
                Action::Set(key, value) => {
                    if let Some((owner, key)) = whose(key, quest, holder) {
                        self.story.set(&owner, key, value.clone(), None, &self.db);
                    }
                }
                Action::Add(key, n) => {
                    if let Some((owner, key)) = whose(key, quest, holder) {
                        let now = crate::time::unix_now();
                        self.story.add(&owner, key, *n, now, &self.db);
                    }
                }
                Action::Clear(key) => {
                    if let Some((owner, key)) = whose(key, quest, holder) {
                        self.story.clear(&owner, key, &self.db);
                    }
                }
                Action::Spawn(mob, room) => {
                    let key = MobKey::parse(mob, zone);
                    let Some(&room) = self.index.0.get(&RoomKey::parse(room, zone)) else {
                        warn!("{}: no room {room} to spawn {key} in", quest.id);
                        continue;
                    };
                    // One already there for the quest is theirs too.
                    if let Some(there) = self.quest_mobs.here(&quest.id, &key, room, &self.places) {
                        let q = self.quest_mobs.0.get_mut(&there);
                        if let Some(q) = q.filter(|q| !q.holders.contains(&holder.character_id)) {
                            q.holders.push(holder.character_id);
                        }
                        continue;
                    }
                    if let Some(entity) = crate::mobs::spawn_mob(
                        &mut self.commands,
                        &self.mob_templates,
                        &self.item_templates,
                        &self.skill_defs,
                        &key,
                        room,
                        &self.races,
                        &mut self.dice,
                    ) {
                        self.quest_mobs.0.insert(
                            entity,
                            QuestMob {
                                quest: quest.id.clone(),
                                mob: key,
                                holders: vec![holder.character_id],
                            },
                        );
                    }
                }
                Action::Fail => {
                    self.failed.0.push((holder.character_id, quest.id.clone()));
                    break;
                }
                Action::Say(mob, text) => {
                    let name = self
                        .mob_templates
                        .0
                        .get(&MobKey::parse(mob, zone))
                        .map_or_else(
                            || "Someone".to_string(),
                            |def| crate::items::capitalized(&crate::combat::the(&def.name)),
                        );
                    lines.push(format!("{name} says, \"{text}\""));
                }
            }
        }
        lines
    }
}

/// Whose fact `key` (`char.title`, `world.drains_cleared`, `quest.warned`) is, for `holder` on
/// `quest`, and the key within its scope.
fn whose<'a>(key: &'a str, quest: &QuestDef, holder: Holder) -> Option<(Owner, &'a str)> {
    let (scope, rest) = key.split_once('.')?;
    let owner = match scope {
        "char" => Owner::Character(holder.character_id),
        "world" => Owner::World,
        "quest" => Owner::Quest(holder.character_id, quest.id.clone()),
        _ => return None,
    };
    Some((owner, rest))
}
