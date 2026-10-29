//! `why <player> <quest>`: for GMs, everything about where a player stands in a quest and why:
//! its stage, each objective with the fact it reads and what it needs, each requirement term by
//! term and whether it holds, and whether it can be reported. For when a quest "isn't working".

use bevy::prelude::*;

use super::claims::Claims;
use super::def::{Lead, Objective, QuestDef, Quests, chose_key};
use super::journal::{HELD, Lookup, QuestLog, room_name, time_left};
use crate::attributes::Attributes;
use crate::command::Invocation;
use crate::db::{Database, QuestRecord};
use crate::facts::Owner;
use crate::items::ItemTemplates;
use crate::mobs::MobTemplates;
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::requirements::{ItemMatch, Requirement, Subject, Surroundings, Who};
use crate::skills::Skills;
use crate::time::unix_now;
use crate::world::{ItemKey, Room, RoomIndex, RoomKey};

/// `why <player> <quest>`: a player (online or not) and a quest they've taken, by id or name.
#[allow(clippy::too_many_arguments)]
pub(super) fn why(
    In(inv): In<Invocation>,
    players: Characters,
    logs: Query<&QuestLog>,
    sheets: Query<(&Attributes, &Skills)>,
    around: Surroundings,
    (quests, claims): (Res<Quests>, Res<Claims>),
    (mobs, items): (Res<MobTemplates>, Res<ItemTemplates>),
    (index, rooms): (Res<RoomIndex>, Query<&Room>),
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let text = (|| {
        let (name, wanted) = inv
            .args
            .trim()
            .split_once(' ')
            .map(|(name, quest)| (name, quest.trim().to_lowercase()))
            .ok_or_else(|| inv.usage_error())?;
        let (id, name) = db
            .character_named(name)
            .ok()
            .flatten()
            .ok_or_else(|| format!("No one is called {name}."))?;
        // In the game, their log and themselves; away, what's saved.
        let online = players
            .iter()
            .find(|(_, p, ..)| p.is_some_and(|p| p.character_id == id))
            .and_then(|(e, ..)| Actor::get(&players, e));
        let records = match online.as_ref().and_then(|a| logs.get(a.entity).ok()) {
            Some(log) => log.0.clone(),
            None => db.character_quests(id).unwrap_or_default(),
        };
        let quest = quests
            .0
            .values()
            .find(|q| q.id == wanted || q.name.to_lowercase().contains(&wanted))
            .ok_or_else(|| format!("There's no quest like '{wanted}'."))?;
        let record = records
            .into_iter()
            .find(|r| r.quest == quest.id)
            .ok_or_else(|| format!("{name} has never taken {}.", quest.name))?;
        let places = |key: &RoomKey| room_name(key, &index, &rooms);
        let lookup = Lookup {
            quests: &quests,
            story: &around.story,
            mobs: &mobs,
            items: &items,
            places: &places,
            now: unix_now(),
        };
        // How they'd answer a requirement, if they're here to be asked.
        let known = online.as_ref().map(|a| around.of(a.entity));
        let sheet = online.as_ref().and_then(|a| sheets.get(a.entity).ok());
        let subject = match (&online, &known, sheet) {
            (Some(actor), Some(known), Some((attributes, skills))) => Some(Subject::of(
                Who {
                    attributes,
                    race: &actor.race,
                    skills: Some(skills),
                    guild_rank: known.guild_rank,
                    character: Some(id),
                    conditions: around.conditions.get(actor.entity).ok(),
                    room: known.room.as_ref(),
                    carrying: Some(&known.carrying),
                },
                Some(&around.story),
            )),
            _ => None,
        };
        let carried = known.as_ref().map(|k| k.carrying.as_slice());
        let holders = |ids: &[i64]| {
            let names: Vec<String> = ids
                .iter()
                .map(|id| {
                    db.character_name(*id)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| format!("character {id}"))
                })
                .collect();
            names.join(", ")
        };
        Ok(explain(
            &name,
            quest,
            &record,
            id,
            subject.as_ref(),
            carried,
            &claims,
            &holders,
            &lookup,
        ))
    })()
    .unwrap_or_else(|err: String| err);
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// The whole account of `name`'s `record` of `quest`. `subject` and `carried` are them, if
/// they're in the game; away, what needs them to be asked is said to be unknown.
#[allow(clippy::too_many_arguments)]
fn explain(
    name: &str,
    quest: &QuestDef,
    record: &QuestRecord,
    id: i64,
    subject: Option<&Subject>,
    carried: Option<&[(ItemKey, Vec<String>)]>,
    claims: &Claims,
    holders: &dyn Fn(&[i64]) -> String,
    lookup: &Lookup,
) -> String {
    let zone = quest.zone();
    let mut lines = Vec::new();
    let away = if subject.is_some() {
        ""
    } else {
        " (not in the game)"
    };
    lines.push(format!(
        "{name}{away}, {} ({}): {}.",
        quest.name, quest.id, record.state
    ));
    if let Some(due) = record.due_at {
        lines.push(format!("Due in {}.", time_left(due - lookup.now)));
    }
    if let Some(claim) = claims.0.get(&quest.id) {
        lines.push(format!("Claimed by {}.", holders(&claim.holders)));
    }
    let world = Subject::world(lookup.story, zone, None);
    if let Some(when) = &quest.posted_when {
        lines.push("Posted when:".into());
        terms(when, &world, 1, &mut lines);
    }
    if let Some(requires) = &quest.requires {
        lines.push("Requires:".into());
        match subject {
            Some(subject) => terms(requires, subject, 1, &mut lines),
            None => lines.push(format!(
                "  {} (can't tell while they're away)",
                said(requires)
            )),
        }
    }
    let owner = Owner::Quest(id, quest.id.clone());
    if record.state == HELD {
        match quest.stage_at(&record.stage) {
            None => lines.push(format!(
                "On stage '{}', which the quest doesn't have: broken.",
                record.stage
            )),
            Some((i, stage)) => {
                let place = quest.stage_key(i);
                let last = quest.next_after(i).is_none();
                lines.push(format!(
                    "On stage {place} ({} of {}): {}",
                    i + 1,
                    quest.stages.len(),
                    stage.text
                ));
                // Each objective, and whether all of them are done.
                let show = |objectives: &[Objective], at: &str, depth: usize| {
                    let (mut shown, mut all) = (Vec::new(), true);
                    let pad = "  ".repeat(depth);
                    for objective in objectives {
                        let (line, done) =
                            objective_line(objective, at, zone, &owner, subject, carried, lookup);
                        all &= done.unwrap_or(false);
                        shown.push(format!("{pad}{line}"));
                        if let (Objective::Meet(requirement), Some(subject)) = (objective, subject)
                        {
                            terms(requirement, subject, depth + 1, &mut shown);
                        }
                    }
                    (shown, all)
                };
                let (shown, own) = show(&stage.objectives, &place, 1);
                lines.extend(shown);
                let mut ready = last && own;
                let chosen = lookup.story.number(&owner, &chose_key(&place), lookup.now);
                for (c, choice) in stage.choices.iter().enumerate().map(|(c, ch)| (c + 1, ch)) {
                    let state = match chosen {
                        0 => "open",
                        n if n == c as i64 => "taken",
                        _ => "closed",
                    };
                    let lead = quest.lead(i, Some(c));
                    let leads = match lead {
                        Lead::Stage(next) => format!("leads to {}", quest.stage_key(next)),
                        Lead::Report(Some(end)) => format!("ends {end}"),
                        Lead::Report(None) => "leads to reporting".into(),
                    };
                    let to = quest.named(&quest.report_key(Some(choice)), lookup.mobs);
                    lines.push(format!(
                        "  Way {c} ({state}, {leads}, reported to {to}): {}",
                        choice.text
                    ));
                    let (shown, done) = show(&choice.objectives, &format!("{place}.{c}"), 2);
                    lines.extend(shown);
                    if state != "closed" && own && done && matches!(lead, Lead::Report(_)) {
                        ready = true;
                    }
                }
                lines.push(if !last && stage.choices.is_empty() {
                    "Not the last stage: it moves on when every objective here is done.".into()
                } else if ready {
                    format!("Ready to report to {}.", quest.reported_to(lookup.mobs))
                } else {
                    format!(
                        "Not ready to report to {} yet.",
                        quest.reported_to(lookup.mobs)
                    )
                });
            }
        }
    }
    let facts = lookup.story.of(&owner, lookup.now);
    if !facts.is_empty() {
        let shown: Vec<String> = facts.iter().map(|(k, v)| v.shown(k)).collect();
        lines.push(format!("Quest facts: {}.", shown.join(", ")));
    }
    lines.join("\r\n")
}

/// One objective: what it is, what it reads, and whether it's done (`None` if that can't be told
/// while they're away).
fn objective_line(
    objective: &Objective,
    place: &str,
    zone: &str,
    owner: &Owner,
    subject: Option<&Subject>,
    carried: Option<&[(ItemKey, Vec<String>)]>,
    lookup: &Lookup,
) -> (String, Option<bool>) {
    let what = objective.what(zone, lookup);
    let need = objective.count();
    let state = |done: bool| if done { "done" } else { "not yet" };
    match objective {
        Objective::Kill(..) | Objective::Obtain(..) | Objective::Reach(_) | Objective::Ask(..) => {
            let key = objective.key(place, zone).unwrap_or_default();
            let have = lookup.story.number(owner, &key, lookup.now);
            let done = have >= i64::from(need);
            let verb = match objective {
                Objective::Kill(..) => "Kill",
                Objective::Obtain(..) => "Find",
                Objective::Ask(..) => "Ask",
                _ => "Reach",
            };
            (
                format!(
                    "{verb} {what}: quest.{key} = {have}, needs {need}: {}",
                    state(done)
                ),
                Some(done),
            )
        }
        Objective::Bring(which, need) => match carried {
            None => (
                format!("Bring {what} x{need}: what they carry can't be told while they're away"),
                None,
            ),
            Some(carried) => {
                let have = carried
                    .iter()
                    .filter(|(key, tags)| match which {
                        ItemMatch::Item(id) => ItemKey::parse(id, zone) == *key,
                        ItemMatch::Tag(tag) => tags.contains(tag),
                    })
                    .count();
                let done = have >= *need as usize;
                (
                    format!(
                        "Bring {what}: carries {have}, needs {need}: {}",
                        state(done)
                    ),
                    Some(done),
                )
            }
        },
        Objective::Meet(requirement) => match subject {
            None => (format!("Meet {what}: can't tell while they're away"), None),
            Some(subject) => {
                let done = requirement.met(subject);
                (
                    format!(
                        "Meet {what}: {}",
                        if done { "holds" } else { "doesn't hold" }
                    ),
                    Some(done),
                )
            }
        },
    }
}

/// A requirement as GMs read it.
fn said(requirement: &Requirement) -> String {
    requirement.describe(&|id| id.to_string())
}

/// A requirement term by term, each with whether it holds, indented `depth` deep.
fn terms(requirement: &Requirement, subject: &Subject, depth: usize, lines: &mut Vec<String>) {
    let pad = "  ".repeat(depth);
    let held = |r: &Requirement| if r.met(subject) { "yes" } else { "no" };
    match requirement {
        Requirement::All(parts) | Requirement::Any(parts) => {
            let how = match requirement {
                Requirement::All(_) => "all of",
                _ => "any of",
            };
            lines.push(format!("{pad}{how}: {}", held(requirement)));
            for part in parts {
                terms(part, subject, depth + 1, lines);
            }
        }
        Requirement::Not(part) => {
            lines.push(format!("{pad}not: {}", held(requirement)));
            terms(part, subject, depth + 1, lines);
        }
        leaf => lines.push(format!("{pad}{}: {}", said(leaf), held(leaf))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Facts, Value};
    use crate::quests::def::{MobMatch, Stage};
    use crate::quests::journal::DONE;
    use crate::quests::testing::rats;

    #[test]
    fn why_tells_every_objective_and_test() {
        let db = Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let flag = || Requirement::WorldFlag("rats_broke_out".into());
        let quest = QuestDef {
            posted_when: Some(flag()),
            stages: vec![
                Stage {
                    text: "Find the nest.".into(),
                    objectives: vec![Objective::Reach("nest".into())],
                    ..Default::default()
                },
                Stage {
                    text: "Clear it.".into(),
                    objectives: vec![
                        Objective::Kill(MobMatch::Mob("rat".into()), 2),
                        Objective::Bring(ItemMatch::Item("tail".into()), 1),
                        Objective::Meet(flag()),
                    ],
                    ..Default::default()
                },
            ],
            ..rats()
        };
        let record = QuestRecord {
            quest: quest.id.clone(),
            state: HELD.into(),
            stage: "2".into(),
            taken_at: 0,
            due_at: None,
            finished_at: None,
        };
        let mut story = Facts::default();
        let owner = Owner::Quest(id, quest.id.clone());
        story.set(&owner, "2.kill.riftwatch:rat", Value::Int(2), None, &db);
        let (mobs, items, quests) = (
            MobTemplates::default(),
            ItemTemplates::default(),
            Quests::default(),
        );
        fn lookup<'a>(
            quests: &'a Quests,
            story: &'a Facts,
            mobs: &'a MobTemplates,
            items: &'a ItemTemplates,
        ) -> Lookup<'a> {
            Lookup {
                quests,
                story,
                mobs,
                items,
                places: &|_| None,
                now: 0,
            }
        }
        let names = |_: &[i64]| String::new();

        // Away: what needs them in the game can't be told.
        let away = explain(
            "Ada",
            &quest,
            &record,
            id,
            None,
            None,
            &Claims::default(),
            &names,
            &lookup(&quests, &story, &mobs, &items),
        );
        for line in [
            "Ada (not in the game), Rats (riftwatch:rats): held.",
            "Posted when:\r\n  rats broke out: no",
            "On stage 2 (2 of 2): Clear it.",
            "Kill rat: quest.2.kill.riftwatch:rat = 2, needs 2: done",
            "Bring tail x1: what they carry can't be told while they're away",
            "Meet rats broke out: can't tell while they're away",
            "Not ready to report to",
            "Quest facts: 2.kill.riftwatch:rat = 2.",
        ] {
            assert!(away.contains(line), "missing {line:?} in:\n{away}");
        }

        // Here, with the tail, and the rats out: ready.
        story.set(
            &Owner::World,
            "rats_broke_out",
            Value::Bool(true),
            None,
            &db,
        );
        let world = Subject::world(&story, "riftwatch", None);
        let carried = [(ItemKey::parse("riftwatch:tail", ""), Vec::new())];
        let here = explain(
            "Ada",
            &quest,
            &record,
            id,
            Some(&world),
            Some(&carried),
            &Claims::default(),
            &names,
            &lookup(&quests, &story, &mobs, &items),
        );
        for line in [
            "Bring tail: carries 1, needs 1: done",
            "Meet rats broke out: holds\r\n    rats broke out: yes",
            "Ready to report to",
        ] {
            assert!(here.contains(line), "missing {line:?} in:\n{here}");
        }

        // Done, it says so and nothing more of its stages.
        let done = QuestRecord {
            state: DONE.into(),
            ..record
        };
        let text = explain(
            "Ada",
            &quest,
            &done,
            id,
            None,
            None,
            &Claims::default(),
            &names,
            &lookup(&quests, &story, &mobs, &items),
        );
        assert!(text.contains(": done.") && !text.contains("On stage"));
    }
}
