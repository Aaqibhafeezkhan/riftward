//! Claimed quests: held by one character or group until a deadline, failing when it passes, and
//! given up with `abandon`.

use std::collections::HashMap;

use bevy::prelude::*;

use super::def::{QuestDef, Quests};
use super::journal::{ABANDONED, FAILED, HELD, QuestLog};
use crate::command::Invocation;
use crate::db::{Database, QuestRecord};
use crate::guild::Membership;
use crate::output::{Outbox, Outgoing, deliver};
use crate::time::unix_now;
use crate::world::Player;

/// The claimed quests that are out: each held by one character, or a group who took it together,
/// until a time (unix seconds), off the board till then. They're the quests held with a deadline, so they come back from
/// `character_quests` at startup and a restart neither frees a notice nor forgets one.
#[derive(Resource, Default, Debug)]
pub struct Claims(pub HashMap<String, Claim>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Characters, by id.
    pub holders: Vec<i64>,
    pub until: i64,
}

pub(super) fn load_claims(db: Res<Database>, mut claims: ResMut<Claims>) {
    match db.claims() {
        Ok(rows) => {
            claims.0.clear();
            for (character_id, quest, until) in rows {
                claims
                    .0
                    .entry(quest)
                    .or_insert(Claim {
                        holders: Vec::new(),
                        until,
                    })
                    .holders
                    .push(character_id);
            }
        }
        Err(err) => error!("failed to load quest claims: {err}"),
    }
}

/// What failing or giving up a contract costs: half its merit, rounded up.
fn forfeit(quest: &QuestDef) -> i64 {
    quest.contract.as_ref().map_or(0, |c| (c.merit + 1) / 2)
}

/// Write off `record` as `state` (failed or abandoned): saved, in Marin's record if a contract
/// (`what`: "failed", "gave up"), merit forfeited from `membership`, the claim freed. What to
/// tell them about it, beyond the news itself.
#[allow(clippy::too_many_arguments)]
fn write_off(
    quest: Option<&QuestDef>,
    mut record: QuestRecord,
    state: &str,
    what: &str,
    character_id: i64,
    log: Option<&mut QuestLog>,
    membership: Option<&mut Membership>,
    claims: &mut Claims,
    db: &Database,
    now: i64,
) -> Vec<String> {
    // Theirs no longer; the notice goes back up once no one holds it.
    if let Some(claim) = claims.0.get_mut(&record.quest) {
        claim.holders.retain(|id| *id != character_id);
        if claim.holders.is_empty() {
            claims.0.remove(&record.quest);
        }
    }
    let quest_id = record.quest.clone();
    record.state = state.into();
    record.due_at = None;
    match log {
        Some(log) => log.put(record, character_id, db),
        None => {
            if let Err(err) = db.save_character_quest(character_id, &record) {
                error!("failed to save quest {quest_id} for {character_id}: {err}");
            }
        }
    }
    let Some(quest) = quest.filter(|q| q.contract.is_some()) else {
        return Vec::new();
    };
    if let Err(err) = db.add_guild_record(character_id, &quest.id, what, now) {
        error!(
            "failed to write {} into the guild's record: {err}",
            quest.id
        );
    }
    let cost = forfeit(quest);
    // Someone not in the game has their standing changed where it's kept.
    let mut saved;
    let membership = match membership {
        Some(m) => Some(m),
        None => {
            saved = db
                .membership(character_id, crate::guild::GUILD)
                .ok()
                .flatten()
                .and_then(|r| Membership::from_record(&r));
            saved.as_mut()
        }
    };
    match membership {
        Some(m) if cost > 0 && !m.expelled => {
            m.earn(-cost);
            m.save(Some(character_id), db);
            vec![format!("It costs you {cost} merit.")]
        }
        _ => Vec::new(),
    }
}

/// Claims whose time is up fail: each holder is told if they're here, it's written off for each
/// whether or not, and the notice goes back on the board.
pub(super) fn expire_claims(
    mut claims: ResMut<Claims>,
    mut holders: Query<(Entity, &Player, &mut QuestLog, Option<&mut Membership>)>,
    quests: Res<Quests>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let due: Vec<(String, Claim)> = claims
        .0
        .iter()
        .filter(|(_, claim)| claim.until <= now)
        .map(|(quest, claim)| (quest.clone(), claim.clone()))
        .collect();
    for (quest_id, claim) in due {
        let quest = quests.0.get(&quest_id);
        let name = quest.map_or(quest_id.as_str(), |q| q.name.as_str());
        for character_id in &claim.holders {
            let online = holders
                .iter_mut()
                .find(|(_, player, ..)| player.character_id == *character_id);
            let offline = match online {
                Some((entity, _, mut log, membership)) => {
                    let Some(record) = log.get(&quest_id).cloned() else {
                        continue;
                    };
                    let mut lines = vec![format!(
                        "\x1b[1;31mYour time for {name} has run out: it's failed, and the \
                         notice is back on the board.\x1b[0m"
                    )];
                    lines.extend(write_off(
                        quest,
                        record,
                        FAILED,
                        "failed",
                        *character_id,
                        Some(&mut log),
                        membership.map(|m| m.into_inner()),
                        &mut claims,
                        &db,
                        now,
                    ));
                    outbox.0.push(Outgoing::line(entity, lines.join(" ")));
                    continue;
                }
                None => db
                    .character_quests(*character_id)
                    .ok()
                    .and_then(|records| records.into_iter().find(|r| r.quest == quest_id)),
            };
            if let Some(record) = offline.filter(|r| r.state == HELD) {
                write_off(
                    quest,
                    record,
                    FAILED,
                    "failed",
                    *character_id,
                    None,
                    None,
                    &mut claims,
                    &db,
                    now,
                );
            }
        }
        claims.0.remove(&quest_id);
        info!("{name}, claimed by {:?}, ran out", claim.holders);
    }
}

/// `abandon <quest>`: give up a quest you're on, anywhere. A contract is written off in Marin's
/// record as given up, and costs some merit; a claimed notice goes back on the board.
pub(super) fn abandon(
    In(inv): In<Invocation>,
    mut holders: Query<(&Player, &mut QuestLog, Option<&mut Membership>)>,
    quests: Res<Quests>,
    mut claims: ResMut<Claims>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let Ok((player, mut log, membership)) = holders.get_mut(inv.entity) else {
        return;
    };
    let wanted = inv.args.trim().to_lowercase();
    let text = if wanted.is_empty() {
        inv.usage_error()
    } else {
        let name = |id: &str| quests.0.get(id).map_or(id.to_string(), |q| q.name.clone());
        let found = log
            .0
            .iter()
            .find(|r| r.state == HELD && name(&r.quest).to_lowercase().contains(&wanted))
            .cloned();
        match found {
            None => format!("You aren't on a quest like '{}'.", inv.args.trim()),
            Some(record) => {
                let quest = quests.0.get(&record.quest);
                let mut lines = vec![format!("You give up {}.", name(&record.quest))];
                lines.extend(write_off(
                    quest,
                    record,
                    ABANDONED,
                    "gave up",
                    player.character_id,
                    Some(&mut log),
                    membership.map(|m| m.into_inner()),
                    &mut claims,
                    &db,
                    unix_now(),
                ));
                lines.join(" ")
            }
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::def::Contract;
    use crate::quests::testing::rats;

    #[test]
    fn claims_run_out_whether_or_not_their_holder_is_here() {
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let ada = db.create_character(account.id, "Ada").unwrap().id;
        let bob = db.create_character(account.id, "Bob").unwrap().id;
        let held = |quest: &str, due| QuestRecord {
            quest: quest.into(),
            state: HELD.into(),
            stage: "1".into(),
            taken_at: 0,
            due_at: Some(due),
            finished_at: None,
        };
        let now = unix_now();
        // Ada is here; Bob isn't, and his standing is where it's kept.
        let member = |merit| Membership {
            rank: 0,
            merit,
            contracts: 0,
            expelled: false,
        };
        member(20).save(Some(bob), &db);
        db.save_character_quest(bob, &held("riftwatch:roaches", now - 1))
            .unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Outbox>();
        let contract = |id: &str| QuestDef {
            id: format!("riftwatch:{id}"),
            contract: Some(Contract {
                rank: "bone".into(),
                merit: 9,
            }),
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([
            ("riftwatch:rats".into(), contract("rats")),
            ("riftwatch:roaches".into(), contract("roaches")),
            ("riftwatch:king".into(), contract("king")),
        ])));
        world.insert_resource(Claims(HashMap::from([
            (
                "riftwatch:rats".into(),
                Claim {
                    holders: vec![ada],
                    until: now - 1,
                },
            ),
            (
                "riftwatch:roaches".into(),
                Claim {
                    holders: vec![bob],
                    until: now - 1,
                },
            ),
            (
                "riftwatch:king".into(),
                Claim {
                    holders: vec![ada],
                    until: now + 3600,
                },
            ),
        ])));
        let player = Player {
            name: "Ada".into(),
            character_id: ada,
            account: "ada".into(),
            role: crate::db::Role::Player,
            race: "human".into(),
            background: String::new(),
            description: String::new(),
        };
        let her = world
            .spawn((
                player,
                QuestLog(vec![
                    held("riftwatch:rats", now - 1),
                    held("riftwatch:king", now + 3600),
                ]),
                member(20),
            ))
            .id();
        world.run_system_once(expire_claims).unwrap();

        let claims = &world.resource::<Claims>().0;
        assert_eq!(claims.len(), 1, "only the claim still in time");
        assert!(claims.contains_key("riftwatch:king"));
        let log = world.get::<QuestLog>(her).unwrap();
        assert_eq!(log.get("riftwatch:rats").unwrap().state, FAILED);
        assert_eq!(log.get("riftwatch:king").unwrap().state, HELD);
        assert_eq!(
            world.get::<Membership>(her).unwrap().merit,
            15,
            "half of 9, rounded up"
        );
        let db = world.resource::<Database>();
        let bobs = db.character_quests(bob).unwrap();
        assert_eq!(bobs[0].state, FAILED, "failed while he was away");
        let standing = db.membership(bob, crate::guild::GUILD).unwrap().unwrap();
        assert_eq!(standing.merit, 15);
        assert_eq!(
            db.guild_record(bob).unwrap(),
            vec![("riftwatch:roaches".into(), "failed".into(), now)]
        );
        assert!(world.resource::<Outbox>().0.iter().any(|o| matches!(o,
            Outgoing::To(who, text) if *who == her && text.contains("Your time for Rats has run out"))));
    }
}
