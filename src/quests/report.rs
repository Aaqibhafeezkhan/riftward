//! Reporting a finished quest (`report`): handing over what it asks to be brought, and paying
//! everyone who did it.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::claims::Claims;
use super::def::{Objective, QuestDef, Quests, Reward};
use super::journal::{DONE, HELD, Lookup, OVER, QuestLog, stage_number};
use crate::command::Invocation;
use crate::db::{Database, QuestRecord};
use crate::facts::{Facts, Owner};
use crate::guild::{Membership, guild};
use crate::items::{ItemLocation, ItemTemplates};
use crate::mobs::MobTemplates;
use crate::money;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::requirements::ItemMatch;
use crate::time::unix_now;
use crate::world::{ItemKey, MobKey};

/// What's still wanted before `record` can be reported: Err with each thing missing, in the
/// journal's words; or Ok with the items it takes from `carried`, `Bring`'s due. Without
/// `carried` (a holder reported for, not reporting), what's to be brought isn't asked.
fn readiness(
    quest: &QuestDef,
    record: &QuestRecord,
    character_id: i64,
    carried: Option<&[(Entity, ItemKey, Vec<String>)]>,
    lookup: &Lookup,
) -> Result<Vec<Entity>, Vec<String>> {
    let Some(n) = stage_number(record) else {
        return Err(vec!["its place in it is lost".into()]);
    };
    let Some(stage) = quest.stages.get(n - 1) else {
        return Err(vec!["its place in it is lost".into()]);
    };
    if n < quest.stages.len() {
        return Err(vec![stage.text.clone()]);
    }
    let owner = Owner::Quest(character_id, quest.id.clone());
    let zone = quest.zone();
    let (mut missing, mut taken) = (Vec::new(), Vec::new());
    for objective in &stage.objectives {
        match objective {
            Objective::Kill(_, need) | Objective::Obtain(_, need) => {
                let key = objective.key(n, zone);
                let have = key.map(|key| lookup.story.number(&owner, &key, lookup.now));
                if have.unwrap_or(0) < i64::from(*need) {
                    missing.push(objective.progress(zone, have, lookup));
                }
            }
            // What's brought is brought once, by whoever reports: it isn't asked of the rest.
            Objective::Bring(which, need) => {
                let Some(carried) = carried else {
                    continue;
                };
                let found: Vec<Entity> = carried
                    .iter()
                    .filter(|(entity, key, tags)| {
                        !taken.contains(entity)
                            && match which {
                                ItemMatch::Item(id) => ItemKey::parse(id, zone) == *key,
                                ItemMatch::Tag(tag) => tags.contains(tag),
                            }
                    })
                    .map(|(entity, ..)| *entity)
                    .take(*need as usize)
                    .collect();
                if found.len() < *need as usize {
                    let what = objective.what(zone, lookup);
                    missing.push(format!("Bring {what}: {} of {need}", found.len()));
                }
                taken.extend(found);
            }
        }
    }
    if missing.is_empty() {
        Ok(taken)
    } else {
        Err(missing)
    }
}

/// What `report` reads and changes.
#[derive(SystemParam)]
pub(super) struct Desk<'w, 's> {
    commands: Commands<'w, 's>,
    players: Characters<'w, 's>,
    mobs: crate::mobs::MobQuery<'w, 's>,
    logs: Query<'w, 's, &'static mut QuestLog>,
    memberships: Query<'w, 's, &'static mut Membership>,
    purses: Query<'w, 's, &'static mut crate::money::Purse>,
    items: crate::items::ItemQuery<'w, 's>,
    item_templates: Res<'w, ItemTemplates>,
    mob_templates: Res<'w, MobTemplates>,
    rooms: RoomQuery<'w, 's>,
    quests: Res<'w, Quests>,
    claims: ResMut<'w, Claims>,
    story: ResMut<'w, Facts>,
    db: Res<'w, Database>,
    outbox: ResMut<'w, Outbox>,
    groups: crate::groups::Groups<'w, 's>,
}

/// `report [<quest>]`: report the quests you hold to whoever here they're for (a quest's
/// `report_to`, or the guild's clerk). One that's done hands over what it asks to be brought and
/// pays: coin, items, a contract's merit, and the facts it sets. It's done too for everyone in
/// your group here who holds it and has done their part: each is paid its items, merit and
/// facts, and the coin is split. A claimed one is over, unpaid, for its holders who aren't here.
/// One that isn't done says what's left.
pub(super) fn report(In(inv): In<Invocation>, mut d: Desk) {
    let Some(actor) = Actor::get(&d.players, inv.entity) else {
        return;
    };
    let Some(character_id) = actor.character_id else {
        return;
    };
    let now = unix_now();
    let here: Vec<(MobKey, String)> = d
        .mobs
        .iter()
        .filter(|(_, _, at)| at.0 == actor.room)
        .map(|(_, mob, _)| (mob.key.clone(), crate::combat::the(&mob.name)))
        .collect();
    let wanted = inv.args.trim().to_lowercase();
    let Ok(log) = d.logs.get(actor.entity) else {
        return;
    };
    // The quests held that are reported to someone here, with whom.
    let due: Vec<(QuestRecord, String)> = log
        .0
        .iter()
        .filter(|r| r.state == HELD)
        .filter_map(|r| {
            let quest = d.quests.0.get(&r.quest)?;
            let to = match &quest.report_to {
                Some(mob) => MobKey::parse(mob, quest.zone()),
                None => MobKey::parse(&guild().clerk, ""),
            };
            let (_, name) = here.iter().find(|(key, _)| *key == to)?;
            (wanted.is_empty() || quest.name.to_lowercase().contains(&wanted))
                .then(|| (r.clone(), name.clone()))
        })
        .collect();
    let mut lines = Vec::new();
    let mut out = Vec::new();
    if due.is_empty() {
        lines.push(if wanted.is_empty() {
            "You have nothing to report to anyone here.".to_string()
        } else {
            format!(
                "You have no quest like '{}' to report here.",
                inv.args.trim()
            )
        });
    }
    let mut carried = crate::items::carried_items(actor.entity, &d.items, &d.item_templates);
    let mut held = crate::items::hands_full(actor.entity, &d.items);
    for (record, to) in due {
        let Some(quest) = d.quests.0.get(&record.quest).cloned() else {
            continue;
        };
        let lookup = Lookup {
            quests: &d.quests,
            story: &d.story,
            mobs: &d.mob_templates,
            items: &d.item_templates,
            now,
        };
        let taken = match readiness(&quest, &record, character_id, Some(&carried), &lookup) {
            Ok(taken) => taken,
            Err(missing) => {
                lines.push(format!(
                    "{}: not done yet. {}.",
                    quest.name,
                    missing.join("; ")
                ));
                continue;
            }
        };
        // The rest of the group here who hold it and have done their part.
        let mates: Vec<(Actor, QuestRecord)> = crate::groups::members(actor.entity, &d.groups)
            .into_iter()
            .filter(|e| *e != actor.entity)
            .filter_map(|e| Actor::get(&d.players, e))
            .filter(|mate| mate.room == actor.room)
            .filter_map(|mate| {
                let id = mate.character_id?;
                let record = d.logs.get(mate.entity).ok()?.get(&quest.id)?.clone();
                (record.state == HELD && readiness(&quest, &record, id, None, &lookup).is_ok())
                    .then_some((mate, record))
            })
            .collect();
        lines.push(format!("You report to {to}: {} is done.", quest.name));
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} reports to {to}.\r\n", actor.name),
        });

        // What it asked to be brought, handed over.
        let mut handed: Vec<(String, usize)> = Vec::new();
        for entity in &taken {
            let Ok((_, item, location)) = d.items.get(*entity) else {
                continue;
            };
            match handed.iter_mut().find(|(name, _)| *name == item.name) {
                Some((_, n)) => *n += 1,
                None => handed.push((item.name.clone(), 1)),
            }
            // Out of their hands, it leaves one free for what they're given.
            if *location == ItemLocation::Held(actor.entity) {
                held = held.saturating_sub(1);
            }
            crate::healing::used_up(&mut d.commands, &d.db, *entity, item);
        }
        carried.retain(|(entity, ..)| !taken.contains(entity));
        if !handed.is_empty() {
            let shown: Vec<String> = handed
                .iter()
                .map(|(name, n)| match n {
                    1 => name.clone(),
                    n => format!("{name} (x{n})"),
                })
                .collect();
            lines.push(format!("You hand over {}.", shown.join(", ")));
        }

        // Paid: the coin split among them, the odd coins to whoever reported.
        let copper: u64 = quest
            .rewards
            .iter()
            .map(|reward| match reward {
                Reward::Coin(copper) => *copper,
                _ => 0,
            })
            .sum();
        let share = copper / (mates.len() as u64 + 1);
        let odd = copper - share * (mates.len() as u64 + 1);
        lines.extend(pay(
            &mut d,
            &actor,
            record,
            &quest,
            share + odd,
            &mut held,
            now,
        ));
        for (mate, record) in mates {
            let mut theirs = vec![format!(
                "{} reports {} to {to}, for you too: it's done.",
                actor.name, quest.name
            )];
            let mut hands = crate::items::hands_full(mate.entity, &d.items);
            theirs.extend(pay(&mut d, &mate, record, &quest, share, &mut hands, now));
            out.push(Outgoing::line(mate.entity, theirs.join("\r\n")));
        }

        // A claimed contract is over now, unpaid, for those who weren't here for it.
        if let Some(claim) = d.claims.0.remove(&quest.id) {
            for id in claim.holders {
                let entity = d
                    .players
                    .iter()
                    .find(|(_, p, ..)| p.is_some_and(|p| p.character_id == id))
                    .map(|(e, ..)| e);
                let record = match entity.and_then(|e| d.logs.get(e).ok()) {
                    Some(log) => log.get(&quest.id).cloned(),
                    None => {
                        d.db.character_quests(id)
                            .ok()
                            .and_then(|rs| rs.into_iter().find(|r| r.quest == quest.id))
                    }
                };
                let Some(mut record) = record.filter(|r| r.state == HELD) else {
                    continue;
                };
                record.state = OVER.into();
                record.due_at = None;
                match entity.and_then(|e| d.logs.get_mut(e).ok()) {
                    Some(mut log) => log.put(record, id, &d.db),
                    None => {
                        if let Err(err) = d.db.save_character_quest(id, &record) {
                            error!("failed to save quest {} for {id}: {err}", quest.id);
                        }
                    }
                }
                if let Some(entity) = entity {
                    out.push(Outgoing::line(
                        entity,
                        format!(
                            "{} reported {} while you were away: it's over, and nothing's owed \
                             you for it.",
                            actor.name, quest.name
                        ),
                    ));
                }
            }
        }
    }
    out.insert(0, Outgoing::line(actor.entity, lines.join("\r\n")));
    deliver(out, &mut d.outbox);
}

/// Pay `who` for `quest`, done: `copper` of its coin, its items, a contract's merit (and a line
/// in Marin's record), and the facts it sets; and mark their `record` done. What to tell them.
fn pay(
    d: &mut Desk,
    who: &Actor,
    mut record: QuestRecord,
    quest: &QuestDef,
    copper: u64,
    held: &mut usize,
    now: i64,
) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(character_id) = who.character_id else {
        return lines;
    };
    if copper > 0
        && let Ok(mut purse) = d.purses.get_mut(who.entity)
    {
        purse.0 += copper;
        purse.save(Some(character_id), &d.db);
        lines.push(format!("You're paid {}.", money::coins(copper)));
    }
    for reward in &quest.rewards {
        match reward {
            Reward::Item(id) => {
                let given = crate::items::hand_new(
                    who,
                    ItemKey::parse(id, quest.zone()),
                    held,
                    &mut d.commands,
                    &d.item_templates,
                    &d.rooms,
                    &d.db,
                );
                lines.push(format!("You're given {given}."));
            }
            Reward::Set(key, value) => {
                let (owner, key) = match key.split_once('.') {
                    Some(("char", rest)) => (Owner::Character(character_id), rest),
                    Some(("world", rest)) => (Owner::World, rest),
                    _ => continue,
                };
                d.story.set(&owner, key, value.clone(), None, &d.db);
            }
            Reward::Coin(_) => {}
        }
    }
    if let Some(contract) = &quest.contract {
        if let Ok(mut m) = d.memberships.get_mut(who.entity)
            && !m.expelled
        {
            m.contracts += 1;
            let promoted = m.earn(contract.merit);
            m.save(Some(character_id), &d.db);
            lines.push(format!("You earn {} merit.", contract.merit));
            lines.extend(promoted);
        }
        if let Err(err) =
            d.db.add_guild_record(character_id, &quest.id, "completed", now)
        {
            error!(
                "failed to write {} into the guild's record: {err}",
                quest.id
            );
        }
    }
    record.state = DONE.into();
    record.finished_at = Some(now);
    record.due_at = None;
    if let Ok(mut log) = d.logs.get_mut(who.entity) {
        log.put(record, character_id, &d.db);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::def::{MobMatch, Stage};
    use crate::quests::testing::rats;

    #[test]
    fn a_quest_is_ready_to_report_when_all_it_asks_is_done() {
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let quest = QuestDef {
            stages: vec![
                Stage {
                    text: "Find the nest.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Mob("rat".into()), 1)],
                },
                Stage {
                    text: "Clear it.".into(),
                    objectives: vec![
                        Objective::Kill(MobMatch::Mob("rat".into()), 2),
                        Objective::Bring(ItemMatch::Item("sewers:rat_tail".into()), 2),
                        Objective::Bring(ItemMatch::Tag("herb".into()), 1),
                    ],
                },
            ],
            ..rats()
        };
        let mut story = Facts::default();
        let (mobs, items) = (MobTemplates::default(), ItemTemplates::default());
        let record = |stage: &str| QuestRecord {
            quest: quest.id.clone(),
            state: HELD.into(),
            stage: stage.into(),
            taken_at: 0,
            due_at: None,
            finished_at: None,
        };
        let mut world = World::new();
        let (tail, tail2, yarrow, knife) = (
            world.spawn_empty().id(),
            world.spawn_empty().id(),
            world.spawn_empty().id(),
            world.spawn_empty().id(),
        );
        let tail_key = ItemKey::parse("sewers:rat_tail", "");
        let carried = vec![
            (knife, ItemKey::parse("riftwatch:knife", ""), Vec::new()),
            (tail, tail_key.clone(), vec!["trophy".to_string()]),
            (
                yarrow,
                ItemKey::parse("forest:yarrow", ""),
                vec!["herb".into()],
            ),
        ];
        let ready = |record: &QuestRecord, story: &Facts, carried: &[_]| {
            let lookup = Lookup {
                quests: &Quests::default(),
                story,
                mobs: &mobs,
                items: &items,
                now: 0,
            };
            readiness(&quest, record, id, Some(carried), &lookup)
        };
        assert_eq!(
            ready(&record("1"), &story, &carried),
            Err(vec!["Find the nest.".to_string()]),
            "not on the last stage"
        );
        assert_eq!(
            ready(&record("2"), &story, &carried),
            Err(vec![
                "Kill rat: 0 of 2".to_string(),
                "Bring sewers:rat_tail: 1 of 2".to_string()
            ])
        );
        let owner = Owner::Quest(id, quest.id.clone());
        story.set(
            &owner,
            "2.kill.riftwatch:rat",
            crate::facts::Value::Int(2),
            None,
            &db,
        );
        let mut carried = carried;
        carried.push((tail2, tail_key, Vec::new()));
        let mut taken = ready(&record("2"), &story, &carried).unwrap();
        taken.sort();
        let mut wanted = vec![tail, tail2, yarrow];
        wanted.sort();
        assert_eq!(taken, wanted, "the tails and the herb, not the knife");
    }
}
