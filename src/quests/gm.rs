//! The GMs' `quest` tools, for running quests by hand: give one, push someone past a stage, pay
//! one out, or take a claimed notice back.

use bevy::prelude::*;

use super::actions::Holder;
use super::claims::{Claim, take_back};
use super::def::{Availability, Lead, QuestDef, Quests, chose_key};
use super::journal::{ABANDONED, HELD};
use super::report::{Desk, pay};
use crate::command::Invocation;
use crate::db::QuestRecord;
use crate::facts::{Owner, Value};
use crate::output::{Actor, Outgoing, deliver};
use crate::time::unix_now;

/// `quest grant|advance|reward <player> <quest> [<way>|<ending>]`, `quest release <quest>`.
pub(super) fn quest_cmd(In(inv): In<Invocation>, mut d: Desk) {
    let Some(gm) = Actor::get(&d.players, inv.entity) else {
        return;
    };
    let args = inv.args.trim();
    let (sub, rest) = args.split_once(' ').unwrap_or((args, ""));
    let mut out = Vec::new();
    let reply = match sub {
        "release" => match find(&d.quests, rest.trim()) {
            Some((quest, _)) => release(&mut d, &quest, &mut out),
            None => format!("There's no quest like '{}'.", rest.trim()),
        },
        "grant" | "advance" | "reward" => {
            let (name, what) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
            let player = d
                .players
                .iter()
                .find(|(_, p, ..)| p.is_some_and(|p| p.name.eq_ignore_ascii_case(name)))
                .and_then(|(e, ..)| Actor::get(&d.players, e));
            match (player, find(&d.quests, what.trim())) {
                (None, _) => format!("No one called {name} is in the game."),
                (_, None) => format!("There's no quest like '{}'.", what.trim()),
                (Some(player), Some((quest, option))) => match sub {
                    "grant" => grant(&mut d, &gm, &player, &quest, &mut out),
                    "advance" => advance(&mut d, &player, &quest, option.as_deref(), &mut out),
                    _ => reward(&mut d, &player, &quest, option.as_deref(), &mut out),
                },
            }
        }
        _ => inv.usage_error(),
    };
    out.insert(0, Outgoing::line(gm.entity, reply));
    deliver(out, &mut d.outbox);
}

/// The quest `text` names (its id, or its name or the start of it), and, if a last word was
/// left over, that word: a way's number, or an ending.
fn find(quests: &Quests, text: &str) -> Option<(QuestDef, Option<String>)> {
    let named = |text: &str| {
        let text = text.trim().to_lowercase();
        (!text.is_empty())
            .then(|| {
                quests.0.values().find(|q| {
                    q.id == text
                        || q.name.to_lowercase() == text
                        || q.name.to_lowercase().starts_with(&text)
                })
            })
            .flatten()
            .cloned()
    };
    if let Some(quest) = named(text) {
        return Some((quest, None));
    }
    let (head, last) = text.trim().rsplit_once(' ')?;
    named(head).map(|quest| (quest, Some(last.to_string())))
}

/// Their record of `quest`, if they hold it.
fn held(d: &Desk, who: &Actor, quest: &QuestDef) -> Option<QuestRecord> {
    d.logs
        .get(who.entity)
        .ok()?
        .get(&quest.id)
        .filter(|r| r.state == HELD)
        .cloned()
}

fn holder(who: &Actor) -> Option<Holder> {
    Some(Holder {
        entity: who.entity,
        character_id: who.character_id?,
        room: who.room,
    })
}

/// `quest grant`: give `quest` to `who` directly, past the board and its checks; not one they
/// hold, nor one someone else has claimed.
fn grant(
    d: &mut Desk,
    gm: &Actor,
    who: &Actor,
    quest: &QuestDef,
    out: &mut Vec<Outgoing>,
) -> String {
    let Some(them) = holder(who) else {
        return String::new();
    };
    if held(d, who, quest).is_some() {
        return format!("{} already holds {}.", who.name, quest.name);
    }
    if d.claims.0.contains_key(&quest.id) {
        return format!("{} is claimed: 'quest release' it first.", quest.name);
    }
    let now = unix_now();
    let due_at = match &quest.availability {
        Availability::Claimed { lasts } => Some(now + lasts.secs()),
        Availability::Open => None,
    };
    if let Some(until) = due_at {
        d.claims.0.insert(
            quest.id.clone(),
            Claim {
                holders: vec![them.character_id],
                until,
            },
        );
    }
    let finished_at = d
        .logs
        .get(who.entity)
        .ok()
        .and_then(|log| log.get(&quest.id))
        .and_then(|r| r.finished_at);
    let record = QuestRecord {
        quest: quest.id.clone(),
        state: HELD.into(),
        stage: quest.stage_key(0),
        taken_at: now,
        due_at,
        finished_at,
    };
    if let Ok(mut log) = d.logs.get_mut(who.entity) {
        log.put(record, them.character_id, &d.hand.db);
    }
    d.hand.story.forget(
        &Owner::Quest(them.character_id, quest.id.clone()),
        &d.hand.db,
    );
    if quest.contract.is_some()
        && let Err(err) = d
            .hand
            .db
            .add_guild_record(them.character_id, &quest.id, "accepted", now)
    {
        error!(
            "failed to write {} into the guild's record: {err}",
            quest.id
        );
    }
    let mut lines = vec![format!(
        "{} gives you {}. Type 'journal' to see it.",
        gm.name, quest.name
    )];
    let gives: Vec<_> = quest
        .gives
        .iter()
        .map(|id| super::def::Action::Give(id.clone()))
        .collect();
    lines.extend(d.hand.act(&gives, quest, them));
    lines.extend(d.hand.act(&quest.stages[0].on_begin, quest, them));
    out.push(Outgoing::line(who.entity, lines.join("\r\n")));
    format!("{} now holds {}.", who.name, quest.name)
}

/// `quest advance`: push `who` past the stage they're on, the way `way` (a number) if it offers
/// choices; a quest whose stage is gone goes back to its first.
fn advance(
    d: &mut Desk,
    who: &Actor,
    quest: &QuestDef,
    way: Option<&str>,
    out: &mut Vec<Outgoing>,
) -> String {
    let (Some(them), Some(mut record)) = (holder(who), held(d, who, quest)) else {
        return format!("{} doesn't hold {}.", who.name, quest.name);
    };
    let mut lines = Vec::new();
    let next = match quest.stage_at(&record.stage) {
        // A broken place starts over.
        None => 0,
        Some((i, stage)) => {
            let way = match (
                stage.choices.len(),
                way.and_then(|w| w.parse::<usize>().ok()),
            ) {
                (0, _) => None,
                (n, Some(w)) if (1..=n).contains(&w) => Some(w),
                (n, _) => {
                    return format!(
                        "{} offers {n} ways here: say which (quest advance {} {} <1-{n}>).",
                        quest.name, who.name, quest.name
                    );
                }
            };
            let place = quest.stage_key(i);
            let owner = Owner::Quest(them.character_id, quest.id.clone());
            if let Some(w) = way {
                d.hand.story.set(
                    &owner,
                    &chose_key(&place),
                    Value::Int(w as i64),
                    None,
                    &d.hand.db,
                );
            }
            match quest.lead(i, way) {
                Lead::Report(_) => {
                    return format!(
                        "{} is on the last of {}: 'quest reward' pays it out.",
                        who.name, quest.name
                    );
                }
                Lead::Stage(next) => {
                    lines.extend(d.hand.act(&stage.on_end, quest, them));
                    next
                }
            }
        }
    };
    let stage = &quest.stages[next];
    record.stage = quest.stage_key(next);
    if let Ok(mut log) = d.logs.get_mut(who.entity) {
        log.put(record, them.character_id, &d.hand.db);
    }
    lines.push(format!("\x1b[1;33m{}:\x1b[0m {}", quest.name, stage.text));
    lines.extend(d.hand.act(&stage.on_begin, quest, them));
    out.push(Outgoing::line(who.entity, lines.join("\r\n")));
    format!(
        "{} is on stage {} of {} now.",
        who.name,
        quest.stage_key(next),
        quest.name
    )
}

/// `quest reward`: pay `quest` out to `who` by hand and mark it done, as reporting would, for
/// `ending` if given (or the one the way they took leads to).
fn reward(
    d: &mut Desk,
    who: &Actor,
    quest: &QuestDef,
    ending: Option<&str>,
    out: &mut Vec<Outgoing>,
) -> String {
    let (Some(them), Some(record)) = (holder(who), held(d, who, quest)) else {
        return format!("{} doesn't hold {}.", who.name, quest.name);
    };
    // The ending the way they're on leads to, unless the GM says.
    let reached = quest.stage_at(&record.stage).and_then(|(i, _)| {
        let owner = Owner::Quest(them.character_id, quest.id.clone());
        let chosen = d
            .hand
            .story
            .number(&owner, &chose_key(&quest.stage_key(i)), unix_now());
        match quest.lead(i, (chosen > 0).then_some(chosen as usize)) {
            Lead::Report(Some(end)) => Some(end.to_string()),
            _ => None,
        }
    });
    let ending = ending.map(String::from).or(reached);
    if !quest.endings.is_empty()
        && ending
            .as_ref()
            .is_none_or(|e| !quest.endings.contains_key(e))
    {
        let names: Vec<&str> = quest.endings.keys().map(String::as_str).collect();
        return format!(
            "{} ends one of these ways: {}. Say which (quest reward {} {} <ending>).",
            quest.name,
            names.join(", "),
            who.name,
            quest.name
        );
    }
    if d.claims
        .0
        .get(&quest.id)
        .is_some_and(|c| c.holders.contains(&them.character_id))
    {
        d.claims.0.remove(&quest.id);
    }
    let copper: u64 = quest
        .pays(ending.as_deref())
        .0
        .iter()
        .map(|r| match r {
            super::def::Reward::Coin(c) => *c,
            _ => 0,
        })
        .sum();
    let mut hands = crate::items::hands_full(who.entity, &d.hand.items);
    let mut lines = vec![format!("{} is done, by the GM's hand.", quest.name)];
    lines.extend(pay(
        d,
        who,
        record,
        (quest, ending.as_deref()),
        copper,
        &mut hands,
        unix_now(),
    ));
    out.push(Outgoing::line(who.entity, lines.join("\r\n")));
    format!("{} is paid for {}.", who.name, quest.name)
}

/// `quest release`: take a claimed quest back from whoever holds it, in the game or not, at no
/// cost to them, and put its notice back on the board.
fn release(d: &mut Desk, quest: &QuestDef, out: &mut Vec<Outgoing>) -> String {
    let Some(claim) = d.claims.0.remove(&quest.id) else {
        return format!("No one holds a claim on {}.", quest.name);
    };
    let now = unix_now();
    for id in &claim.holders {
        let online = d
            .players
            .iter()
            .find(|(_, p, ..)| p.is_some_and(|p| p.character_id == *id))
            .map(|(e, ..)| e);
        let record = match online.and_then(|e| d.logs.get(e).ok()) {
            Some(log) => log.get(&quest.id).cloned(),
            None => d
                .hand
                .db
                .character_quests(*id)
                .unwrap_or_default()
                .into_iter()
                .find(|r| r.quest == quest.id),
        };
        let Some(mut record) = record.filter(|r| r.state == HELD) else {
            continue;
        };
        record.state = ABANDONED.into();
        record.due_at = None;
        match online.and_then(|e| d.logs.get_mut(e).ok()) {
            Some(mut log) => log.put(record, *id, &d.hand.db),
            None => {
                if let Err(err) = d.hand.db.save_character_quest(*id, &record) {
                    error!("failed to save quest {} for {id}: {err}", quest.id);
                }
            }
        }
        if quest.contract.is_some()
            && let Err(err) = d.hand.db.add_guild_record(*id, &quest.id, "released", now)
        {
            error!(
                "failed to write {} into the guild's record: {err}",
                quest.id
            );
        }
        if let Some(entity) = online {
            let mut lines = vec![format!(
                "{} has been taken back by the guild. It costs you nothing.",
                quest.name
            )];
            lines.extend(take_back(
                entity,
                quest,
                &d.hand.items,
                &d.hand.item_templates,
                &mut d.hand.commands,
                &d.hand.db,
            ));
            out.push(Outgoing::line(entity, lines.join(" ")));
        }
    }
    format!("{} is back on the board.", quest.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::testing::rats;
    use std::collections::HashMap;

    #[test]
    fn a_quest_is_found_by_id_or_name_with_a_word_after() {
        let crystals = QuestDef {
            id: "riftwatch:crystals_gone_missing".into(),
            name: "Crystals Gone Missing".into(),
            ..rats()
        };
        let quests = Quests(HashMap::from([(crystals.id.clone(), crystals)]));
        let found = |text: &str| find(&quests, text).map(|(q, w)| (q.name, w));
        let named = Some(("Crystals Gone Missing".to_string(), None));
        assert_eq!(found("riftwatch:crystals_gone_missing"), named);
        assert_eq!(found("crystals gone missing"), named);
        assert_eq!(found("crystals"), named, "the start of its name");
        assert_eq!(
            found("crystals sold"),
            Some(("Crystals Gone Missing".into(), Some("sold".into()))),
            "and an ending after it"
        );
        assert_eq!(found("rats"), None);
        assert_eq!(found(""), None);
    }
}
