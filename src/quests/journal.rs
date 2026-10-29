//! Each character's quests (`QuestLog`), where they stand, and the `journal` that shows them.

use bevy::prelude::*;

use super::def::Quests;
use crate::command::Invocation;
use crate::db::{Database, QuestRecord};
use crate::facts::{Facts, Owner};
use crate::items::ItemTemplates;
use crate::mobs::MobTemplates;
use crate::output::{Outbox, Outgoing, deliver};
use crate::time::unix_now;
use crate::world::{Player, Room, RoomIndex, RoomKey};

/// The quests a character has taken, and how each stands: loaded when they enter the game and
/// saved as it changes.
#[derive(Component, Default, Debug)]
pub struct QuestLog(pub Vec<QuestRecord>);

impl QuestLog {
    pub(super) fn get(&self, quest: &str) -> Option<&QuestRecord> {
        self.0.iter().find(|record| record.quest == quest)
    }

    /// Set down how `record` stands now, in place of how it stood, and save it.
    pub(super) fn put(&mut self, record: QuestRecord, character_id: i64, db: &Database) {
        if let Err(err) = db.save_character_quest(character_id, &record) {
            error!(
                "failed to save quest {} for {character_id}: {err}",
                record.quest
            );
        }
        match self.0.iter_mut().find(|r| r.quest == record.quest) {
            Some(old) => *old = record,
            None => self.0.push(record),
        }
    }
}

/// Where a quest stands for someone.
pub const HELD: &str = "held";
pub const DONE: &str = "done";
pub const FAILED: &str = "failed";
pub const ABANDONED: &str = "abandoned";
/// A claimed contract the rest of its holders reported while this one was elsewhere: the work's
/// done, unpaid for them.
pub const OVER: &str = "over";

/// `journal`: each quest you're on, where you are in it and how far along each objective is;
/// then what you've done.
pub(super) fn journal(
    In(inv): In<Invocation>,
    logs: Query<(&QuestLog, &Player)>,
    quests: Res<Quests>,
    story: Res<Facts>,
    (mobs, items): (Res<MobTemplates>, Res<ItemTemplates>),
    (index, rooms): (Res<RoomIndex>, Query<&Room>),
    mut outbox: ResMut<Outbox>,
) {
    let Ok((log, player)) = logs.get(inv.entity) else {
        return;
    };
    let places = |key: &RoomKey| room_name(key, &index, &rooms);
    let lookup = Lookup {
        quests: &quests,
        story: &story,
        mobs: &mobs,
        items: &items,
        places: &places,
        now: unix_now(),
    };
    deliver(
        vec![Outgoing::line(
            inv.entity,
            journal_text(log, player.character_id, &lookup),
        )],
        &mut outbox,
    );
}

/// What a journal or a count needs to look things up by.
pub(super) struct Lookup<'a> {
    pub(super) quests: &'a Quests,
    pub(super) story: &'a Facts,
    pub(super) mobs: &'a MobTemplates,
    pub(super) items: &'a ItemTemplates,
    /// A room's name, by its key.
    pub(super) places: &'a dyn Fn(&RoomKey) -> Option<String>,
    pub(super) now: i64,
}

fn journal_text(log: &QuestLog, character_id: i64, lookup: &Lookup) -> String {
    let quests = lookup.quests;
    let name = |id: &str| {
        quests
            .0
            .get(id)
            .map_or_else(|| format!("{id} (no longer known)"), |q| q.name.clone())
    };
    let held: Vec<String> = log
        .0
        .iter()
        .filter(|r| r.state == HELD)
        .map(|r| {
            let Some((quest, n, stage)) = quests.0.get(&r.quest).and_then(|q| {
                let n = stage_number(r)?;
                Some((q, n, q.stages.get(n - 1)?))
            }) else {
                return format!("  {}", name(&r.quest));
            };
            let owner = Owner::Quest(character_id, quest.id.clone());
            let due = r
                .due_at
                .map(|due| format!(" (due in {})", time_left(due - lookup.now)))
                .unwrap_or_default();
            let mut text = format!("  {}{due}\r\n    {}", quest.name, stage.text);
            for objective in &stage.objectives {
                let have = objective
                    .key(n, quest.zone())
                    .map(|key| lookup.story.number(&owner, &key, lookup.now));
                text += &format!(
                    "\r\n      {}",
                    objective.progress(quest.zone(), have, lookup)
                );
            }
            text
        })
        .collect();
    let done: Vec<String> = log
        .0
        .iter()
        .filter(|r| r.state == DONE)
        .map(|r| name(&r.quest))
        .collect();
    let mut lines = vec![if held.is_empty() {
        "You aren't on any quests. The board in the Contract Hall has work.".to_string()
    } else {
        "You're on:".to_string()
    }];
    lines.extend(held);
    if !done.is_empty() {
        lines.push(format!("Done: {}.", done.join(", ")));
    }
    lines.join("\r\n")
}

/// How long is left, roughly: "40 minutes", "5 hours", "2 days".
pub(super) fn time_left(secs: i64) -> String {
    let (n, unit) = match secs.max(0) {
        s if s < 3600 => ((s / 60).max(1), "minute"),
        s if s < 48 * 3600 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// A room's name, by its key, if it's in the world.
pub(super) fn room_name(key: &RoomKey, index: &RoomIndex, rooms: &Query<&Room>) -> Option<String> {
    let entity = index.0.get(key)?;
    rooms.get(*entity).ok().map(|room| room.name.clone())
}

/// The stage someone is on, counted from 1.
pub(super) fn stage_number(record: &QuestRecord) -> Option<usize> {
    record.stage.parse().ok().filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::testing::rats;

    #[test]
    fn time_left_is_said_roughly() {
        assert_eq!(time_left(30), "1 minute");
        assert_eq!(time_left(40 * 60), "40 minutes");
        assert_eq!(time_left(23 * 3600 + 59 * 60), "23 hours");
        assert_eq!(time_left(3 * 86_400), "3 days");
        assert_eq!(time_left(-5), "1 minute");
    }

    #[test]
    fn the_journal_tells_where_you_are_in_each_quest() {
        let mut quests = Quests::default();
        quests.0.insert("riftwatch:rats".into(), rats());
        let record = |quest: &str, state: &str| QuestRecord {
            quest: quest.into(),
            state: state.into(),
            stage: "1".into(),
            taken_at: 0,
            due_at: None,
            finished_at: None,
        };
        let (story, mobs, items) = (
            Facts::default(),
            MobTemplates::default(),
            ItemTemplates::default(),
        );
        let lookup = Lookup {
            quests: &quests,
            story: &story,
            mobs: &mobs,
            items: &items,
            places: &|_| None,
            now: 0,
        };
        assert_eq!(
            journal_text(&QuestLog::default(), 1, &lookup),
            "You aren't on any quests. The board in the Contract Hall has work."
        );
        let log = QuestLog(vec![
            record("riftwatch:rats", HELD),
            record("riftwatch:gone", DONE),
        ]);
        assert_eq!(
            journal_text(&log, 1, &lookup),
            "You're on:\r\n  Rats\r\n    Kill rats.\r\n      Kill rat: 0 of 3\r\n\
             Done: riftwatch:gone (no longer known)."
        );
    }

    #[test]
    fn quests_taken_are_saved() {
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let ada = db.create_character(account.id, "Ada").unwrap().id;
        let mut log = QuestLog::default();
        let mut record = QuestRecord {
            quest: "riftwatch:rats".into(),
            state: HELD.into(),
            stage: "1".into(),
            taken_at: 5,
            due_at: None,
            finished_at: None,
        };
        log.put(record.clone(), ada, &db);
        record.state = DONE.into();
        record.finished_at = Some(9);
        log.put(record.clone(), ada, &db);
        assert_eq!(log.0, vec![record.clone()], "one row a quest");
        assert_eq!(db.character_quests(ada).unwrap(), vec![record]);
        db.add_guild_record(ada, "riftwatch:rats", "accepted", 5)
            .unwrap();
        assert_eq!(
            db.guild_record(ada).unwrap(),
            vec![("riftwatch:rats".into(), "accepted".into(), 5)]
        );
    }
}
