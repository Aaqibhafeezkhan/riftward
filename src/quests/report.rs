//! Reporting a finished quest (`report`): handing over what it asks to be brought, and paying
//! everyone who did it.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::actions::{Holder, Stagehand};
use super::claims::Claims;
use super::def::{Lead, Objective, QuestDef, Quests, Reward, chose_key};
use super::journal::{DONE, HELD, Lookup, OVER, QuestLog};
use crate::attributes::Attributes;
use crate::command::Invocation;
use crate::db::QuestRecord;
use crate::facts::Owner;
use crate::guild::Membership;
use crate::items::ItemLocation;
use crate::money;
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::requirements::{ItemMatch, Requirement, Subject, Who};
use crate::skills::Skills;
use crate::time::unix_now;
use crate::world::{ItemKey, MobKey, RoomKey};

/// What a quest ready to report comes to: the items it takes, `Bring`'s due, and the ending it's
/// reported for, if one (the way a choice led, or where its last stage leads).
#[derive(Debug, PartialEq)]
pub(super) struct Ready {
    pub(super) taken: Vec<Entity>,
    pub(super) ending: Option<String>,
}

/// What's still wanted before `record` can be reported: Err with each thing missing, in the
/// journal's words; or what it comes to. Without `carried` (a holder reported for, not
/// reporting), what's to be brought isn't asked. A stage offering choices is ready when its own
/// objectives and one way's are done: the way already taken, or the first done.
fn readiness(
    (quest, to): (&QuestDef, Option<&MobKey>),
    record: &QuestRecord,
    character_id: i64,
    carried: Option<&[(Entity, ItemKey, Vec<String>)]>,
    meets: &dyn Fn(&Requirement) -> bool,
    lookup: &Lookup,
) -> Result<Ready, Vec<String>> {
    let Some((i, stage)) = quest.stage_at(&record.stage) else {
        return Err(vec!["its place in it is lost".into()]);
    };
    let place = quest.stage_key(i);
    let owner = Owner::Quest(character_id, quest.id.clone());
    let check = |objectives: &[Objective], at: &str, taken: &mut Vec<Entity>| {
        unmet(quest, objectives, at, &owner, carried, meets, lookup, taken)
    };
    let mut taken = Vec::new();
    let mut missing = check(&stage.objectives, &place, &mut taken);
    if stage.choices.is_empty() {
        return match quest.lead(i, None) {
            Lead::Stage(_) => Err(vec![stage.text.clone()]),
            Lead::Report(_) if !missing.is_empty() => Err(missing),
            Lead::Report(ending) => Ok(Ready {
                taken,
                ending: ending.map(String::from),
            }),
        };
    }
    let chosen = lookup.story.number(&owner, &chose_key(&place), lookup.now);
    let mut ways = Vec::new();
    for (c, choice) in stage.choices.iter().enumerate().map(|(c, ch)| (c + 1, ch)) {
        if chosen != 0 && chosen != c as i64 {
            continue;
        }
        // A way reported to someone else isn't taken here.
        if to.is_some_and(|to| quest.report_key(Some(choice)) != *to) {
            continue;
        }
        // A way that leads on to a stage is taken as it's done, not on reporting.
        let Lead::Report(ending) = quest.lead(i, Some(c)) else {
            continue;
        };
        let mut theirs = taken.clone();
        let short = check(&choice.objectives, &format!("{place}.{c}"), &mut theirs);
        if short.is_empty() && missing.is_empty() {
            return Ok(Ready {
                taken: theirs,
                ending: ending.map(String::from),
            });
        }
        if !short.is_empty() {
            ways.push(format!("{} ({})", choice.text, short.join("; ")));
        }
    }
    if !ways.is_empty() {
        missing.push(format!("Either: {}", ways.join(" Or: ")));
    }
    if missing.is_empty() {
        missing.push(stage.text.clone());
    }
    Err(missing)
}

/// Which of `objectives`, kept under `at`, aren't done: each in the journal's words. What it's
/// to bring is found in `carried` and added to `taken`.
#[allow(clippy::too_many_arguments)]
fn unmet(
    quest: &QuestDef,
    objectives: &[Objective],
    at: &str,
    owner: &Owner,
    carried: Option<&[(Entity, ItemKey, Vec<String>)]>,
    meets: &dyn Fn(&Requirement) -> bool,
    lookup: &Lookup,
    taken: &mut Vec<Entity>,
) -> Vec<String> {
    let zone = quest.zone();
    let mut missing = Vec::new();
    for objective in objectives {
        match objective {
            Objective::Kill(..)
            | Objective::Obtain(..)
            | Objective::Reach(_)
            | Objective::Ask(..)
            | Objective::Escort { .. } => {
                let need = objective.count();
                let key = objective.key(at, zone);
                let have = key.map(|key| lookup.story.number(owner, &key, lookup.now));
                if have.unwrap_or(0) < i64::from(need) {
                    missing.push(objective.progress(zone, have, lookup));
                }
            }
            Objective::Meet(requirement) => {
                if !meets(requirement) {
                    missing.push(format!("Needs {}", objective.what(zone, lookup)));
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
    missing
}

/// What `report` reads and changes.
#[derive(SystemParam)]
pub(super) struct Desk<'w, 's> {
    /// Items, rooms, templates, facts and the database, shared with stage actions.
    pub(super) hand: Stagehand<'w, 's>,
    pub(super) players: Characters<'w, 's>,
    mobs: crate::mobs::MobQuery<'w, 's>,
    pub(super) logs: Query<'w, 's, &'static mut QuestLog>,
    memberships: Query<'w, 's, &'static mut Membership>,
    purses: Query<'w, 's, &'static mut crate::money::Purse>,
    pub(super) quests: ResMut<'w, Quests>,
    pub(super) claims: ResMut<'w, Claims>,
    pub(super) outbox: ResMut<'w, Outbox>,
    groups: crate::groups::Groups<'w, 's>,
    index: Res<'w, crate::world::RoomIndex>,
    sheets: Query<'w, 's, (&'static Attributes, &'static Skills)>,
    conditions: Query<'w, 's, &'static crate::combat::Conditions>,
    shown: Query<'w, 's, &'static crate::guild::ShownRank>,
}

/// Whether `who` meets `requirement` now, as reporting tests a `Meet`.
fn meets(d: &Desk, who: &Actor, requirement: &Requirement) -> bool {
    let Ok((attributes, skills)) = d.sheets.get(who.entity) else {
        return false;
    };
    let room = d
        .hand
        .rooms
        .get(who.room)
        .ok()
        .map(|(_, key, _)| key.clone());
    let carrying = crate::items::carried(who.entity, &d.hand.items, &d.hand.item_templates);
    let subject = Subject::of(
        Who {
            attributes,
            race: &who.race,
            skills: Some(skills),
            guild_rank: crate::guild::shown_rank(who.entity, &d.shown),
            character: who.character_id,
            conditions: d.conditions.get(who.entity).ok(),
            room: room.as_ref(),
            carrying: Some(&carrying),
        },
        Some(&d.hand.story),
    );
    requirement.met(&subject)
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
    let due: Vec<(QuestRecord, String, MobKey)> = log
        .0
        .iter()
        .filter(|r| r.state == HELD)
        .filter_map(|r| {
            let quest = d.quests.0.get(&r.quest)?;
            // Whoever here it could be reported to, by the ways still open.
            let (i, _) = quest.stage_at(&r.stage)?;
            let owner = Owner::Quest(character_id, quest.id.clone());
            let chosen = d
                .hand
                .story
                .number(&owner, &chose_key(&quest.stage_key(i)), now);
            let tos = quest.report_keys(&r.stage, chosen);
            let (key, name) = here.iter().find(|(key, _)| tos.contains(key))?;
            (wanted.is_empty() || quest.name.to_lowercase().contains(&wanted))
                .then(|| (r.clone(), name.clone(), key.clone()))
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
    let mut carried =
        crate::items::carried_items(actor.entity, &d.hand.items, &d.hand.item_templates);
    let mut held = crate::items::hands_full(actor.entity, &d.hand.items);
    for (record, to, to_key) in due {
        let Some(quest) = d.quests.0.get(&record.quest).cloned() else {
            continue;
        };
        let places = |key: &RoomKey| {
            let room = d.index.0.get(key)?;
            d.hand
                .rooms
                .get(*room)
                .ok()
                .map(|(room, ..)| room.name.clone())
        };
        let lookup = Lookup {
            quests: &d.quests,
            story: &d.hand.story,
            mobs: &d.hand.mob_templates,
            items: &d.hand.item_templates,
            places: &places,
            now,
        };
        let mine = |requirement: &Requirement| meets(&d, &actor, requirement);
        let Ready { taken, ending } = match readiness(
            (&quest, Some(&to_key)),
            &record,
            character_id,
            Some(&carried),
            &mine,
            &lookup,
        ) {
            Ok(ready) => ready,
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
                let theirs = |requirement: &Requirement| meets(&d, &mate, requirement);
                (record.state == HELD
                    && readiness((&quest, Some(&to_key)), &record, id, None, &theirs, &lookup)
                        .is_ok())
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
            let Ok((_, item, location)) = d.hand.items.get(*entity) else {
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
            crate::healing::used_up(&mut d.hand.commands, &d.hand.db, *entity, item);
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
            .pays(ending.as_deref())
            .0
            .iter()
            .map(|reward| match reward {
                Reward::Coin(copper) => *copper,
                _ => 0,
            })
            .sum();
        let share = copper / (mates.len() as u64 + 1);
        let odd = copper - share * (mates.len() as u64 + 1);
        let ending = ending.as_deref();
        lines.extend(pay(
            &mut d,
            &actor,
            record,
            (&quest, ending),
            share + odd,
            &mut held,
            now,
        ));
        for (mate, record) in mates {
            let mut theirs = vec![format!(
                "{} reports {} to {to}, for you too: it's done.",
                actor.name, quest.name
            )];
            let mut hands = crate::items::hands_full(mate.entity, &d.hand.items);
            theirs.extend(pay(
                &mut d,
                &mate,
                record,
                (&quest, ending),
                share,
                &mut hands,
                now,
            ));
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
                    None => d
                        .hand
                        .db
                        .character_quests(id)
                        .ok()
                        .and_then(|rs| rs.into_iter().find(|r| r.quest == quest.id)),
                };
                let Some(mut record) = record.filter(|r| r.state == HELD) else {
                    continue;
                };
                record.state = OVER.into();
                record.due_at = None;
                match entity.and_then(|e| d.logs.get_mut(e).ok()) {
                    Some(mut log) => log.put(record, id, &d.hand.db),
                    None => {
                        if let Err(err) = d.hand.db.save_character_quest(id, &record) {
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

/// Pay `who` for `quest`, done (ending `ending`, if one): `copper` of its coin, its items, its
/// merit for a contract (and a line in Marin's record), and the facts it sets; and mark their
/// `record` done, with the ending kept as the quest's `ending` fact. What to tell them.
pub(super) fn pay(
    d: &mut Desk,
    who: &Actor,
    mut record: QuestRecord,
    (quest, ending): (&QuestDef, Option<&str>),
    copper: u64,
    held: &mut usize,
    now: i64,
) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(character_id) = who.character_id else {
        return lines;
    };
    // How it ended, first.
    if let Some(def) = ending.and_then(|e| quest.endings.get(e)) {
        lines.push(def.journal.clone());
    }
    if copper > 0
        && let Ok(mut purse) = d.purses.get_mut(who.entity)
    {
        purse.0 += copper;
        purse.save(Some(character_id), &d.hand.db);
        lines.push(format!("You're paid {}.", money::coins(copper)));
    }
    let (rewards, merit) = quest.pays(ending);
    for reward in rewards {
        match reward {
            Reward::Item(id) => {
                let given = crate::items::hand_new(
                    who.into(),
                    ItemKey::parse(id, quest.zone()),
                    held,
                    &mut d.hand.commands,
                    &d.hand.item_templates,
                    &d.hand.rooms,
                    &d.hand.db,
                );
                lines.push(format!("You're given {given}."));
            }
            Reward::Set(key, value) => {
                let (owner, key) = match key.split_once('.') {
                    Some(("char", rest)) => (Owner::Character(character_id), rest),
                    Some(("world", rest)) => (Owner::World, rest),
                    _ => continue,
                };
                d.hand
                    .story
                    .set(&owner, key, value.clone(), None, &d.hand.db);
            }
            Reward::Coin(_) | Reward::Merit(_) => {}
        }
    }
    if let Some(merit) = merit {
        if let Ok(mut m) = d.memberships.get_mut(who.entity)
            && !m.expelled
        {
            m.contracts += 1;
            let promoted = m.earn(merit);
            m.save(Some(character_id), &d.hand.db);
            if merit > 0 {
                lines.push(format!("You earn {merit} merit."));
            }
            lines.extend(promoted);
        }
        if let Err(err) = d
            .hand
            .db
            .add_guild_record(character_id, &quest.id, "completed", now)
        {
            error!(
                "failed to write {} into the guild's record: {err}",
                quest.id
            );
        }
    }
    if let Some(ending) = ending {
        d.hand.story.set(
            &Owner::Quest(character_id, quest.id.clone()),
            "ending",
            crate::facts::Value::Word(ending.into()),
            None,
            &d.hand.db,
        );
    }
    // Its last stage ends with it.
    if let Some((i, _)) = quest.stage_at(&record.stage) {
        let holder = Holder {
            entity: who.entity,
            character_id,
            room: who.room,
        };
        lines.extend(d.hand.act(&quest.stages[i].on_end, quest, holder));
    }
    record.state = DONE.into();
    record.finished_at = Some(now);
    record.due_at = None;
    if let Ok(mut log) = d.logs.get_mut(who.entity) {
        log.put(record, character_id, &d.hand.db);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::Facts;
    use crate::items::ItemTemplates;
    use crate::mobs::MobTemplates;
    use crate::quests::def::{MobMatch, Stage};
    use crate::quests::testing::rats;

    #[test]
    fn a_way_asking_for_something_is_taken_on_reporting() {
        use crate::quests::def::{Choice, EndingDef, Next, Way};
        let ends = |end: &str| Some(Next::Way(Way::End(end.into())));
        let quest = QuestDef {
            stages: vec![Stage {
                text: "Return them, or sell them.".into(),
                choices: vec![
                    Choice {
                        text: "Return them.".into(),
                        objectives: vec![Objective::Bring(ItemMatch::Item("crystals".into()), 1)],
                        next: ends("returned"),
                        report_to: None,
                    },
                    Choice {
                        text: "Sell them.".into(),
                        objectives: vec![Objective::Meet(
                            crate::requirements::Requirement::WorldFlag("fence_open".into()),
                        )],
                        next: ends("sold"),
                        report_to: None,
                    },
                ],
                ..Default::default()
            }],
            endings: [
                (
                    "returned".to_string(),
                    EndingDef {
                        journal: "Returned.".into(),
                        rewards: Vec::new(),
                    },
                ),
                (
                    "sold".to_string(),
                    EndingDef {
                        journal: "Sold.".into(),
                        rewards: Vec::new(),
                    },
                ),
            ]
            .into(),
            ..rats()
        };
        assert!(quest.check().is_ok(), "{:?}", quest.check());
        let record = QuestRecord {
            quest: quest.id.clone(),
            state: HELD.into(),
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
            quests: &Quests::default(),
            story: &story,
            mobs: &mobs,
            items: &items,
            places: &|_| None,
            now: 0,
        };
        let mut world = World::new();
        let crystals = world.spawn_empty().id();
        let carrying = [(
            crystals,
            ItemKey::parse("riftwatch:crystals", ""),
            Vec::new(),
        )];
        let ending = |carried: &[_], met: bool| {
            readiness((&quest, None), &record, 1, Some(carried), &|_| met, &lookup)
                .map(|r| r.ending)
        };
        assert_eq!(ending(&carrying, false), Ok(Some("returned".into())));
        assert_eq!(ending(&[], true), Ok(Some("sold".into())));
        assert_eq!(
            ending(&carrying, true),
            Ok(Some("returned".into())),
            "the first way done"
        );
        assert!(ending(&[], false).unwrap_err()[0].starts_with("Either: Return them."));
    }

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
                    ..Default::default()
                },
                Stage {
                    text: "Clear it.".into(),
                    objectives: vec![
                        Objective::Kill(MobMatch::Mob("rat".into()), 2),
                        Objective::Bring(ItemMatch::Item("sewers:rat_tail".into()), 2),
                        Objective::Bring(ItemMatch::Tag("herb".into()), 1),
                        Objective::Meet(crate::requirements::Requirement::WorldFlag(
                            "rats_broke_out".into(),
                        )),
                    ],
                    ..Default::default()
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
        let ready = |record: &QuestRecord, story: &Facts, carried: &[_], met: bool| {
            let lookup = Lookup {
                quests: &Quests::default(),
                story,
                mobs: &mobs,
                items: &items,
                places: &|_| None,
                now: 0,
            };
            readiness((&quest, None), record, id, Some(carried), &|_| met, &lookup)
        };
        assert_eq!(
            ready(&record("1"), &story, &carried, true),
            Err(vec!["Find the nest.".to_string()]),
            "not on the last stage"
        );
        assert_eq!(
            ready(&record("2"), &story, &carried, false),
            Err(vec![
                "Kill rat: 0 of 2".to_string(),
                "Bring sewers:rat_tail: 1 of 2".to_string(),
                "Needs rats broke out".to_string()
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
        let mut taken = ready(&record("2"), &story, &carried, true).unwrap().taken;
        taken.sort();
        let mut wanted = vec![tail, tail2, yarrow];
        wanted.sort();
        assert_eq!(taken, wanted, "the tails and the herb, not the knife");
    }
}
