//! Writing quests in the game: `quest create`, `quest edit` and `quest show`, saved to the
//! quest's file as builders' commands save zones. A new quest is a draft, off the board, until
//! it's posted. Choices, endings, stage actions and `fails_if` are written in the file, which
//! reloads by itself.

use std::collections::BTreeMap;

use super::def::{
    Availability, Contract, MobMatch, Objective, Posted, QuestDef, Repeat, Reward, Stage,
};
use super::report::Desk;
use crate::guild::guild;
use crate::output::Actor;
use crate::requirements::ItemMatch;
use crate::world::Every;

/// What `quest edit` can change, for its usage.
pub(super) const FIELDS: &str = "name|summary|description <text>, board, rank <rank> <merit>|none, \
     open, claimed <10m|2h|1d>, repeat once|always|<time>, report <mob>|none, reward coin <n>|item <id>|clear, \
     stage add <text>, stage <n> text <text>|set <objective>|add <objective>|remove|topic <mob> <topic> <answer>, post, unpost";

/// `quest create <zone:id> <name>`: a new quest, a draft on the board where the GM stands, with
/// one stage to write.
pub(super) fn create(d: &mut Desk, gm: &Actor, args: &str) -> String {
    let Some((id, name)) = args.trim().split_once(' ') else {
        return "Usage: quest create <zone:id> <name>".into();
    };
    let Some((zone, file)) = id.split_once(':') else {
        return "Give it as zone:id: quest create riftwatch:lost_cat The Lost Cat".into();
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    if !ok(zone) || !ok(file) {
        return "Ids are lowercase letters, digits and underscores.".into();
    }
    if d.quests.0.contains_key(id) {
        return format!("There's already a quest {id}.");
    }
    let Ok((_, here, _)) = d.hand.rooms.get(gm.room) else {
        return String::new();
    };
    let room = if here.zone == zone {
        here.room.clone()
    } else {
        here.to_string()
    };
    let quest = QuestDef {
        id: id.into(),
        name: name.trim().into(),
        summary: "Not written yet.".into(),
        description: "Not written yet.".into(),
        posted: Posted::Board(room.clone()),
        posted_when: None,
        contract: None,
        availability: Availability::Open,
        requires: None,
        repeat: Repeat::Always,
        draft: true,
        stages: vec![Stage {
            text: "Not written yet.".into(),
            objectives: vec![Objective::Reach(room)],
            ..Default::default()
        }],
        report_to: None,
        gives: Vec::new(),
        rewards: Vec::new(),
        endings: BTreeMap::new(),
        fails_if: Vec::new(),
    };
    match keep(d, quest) {
        Ok(()) => format!(
            "Created {id}, a draft on the board here. Write it with 'quest edit {id} ...', see \
             it with 'quest show {id}', and 'quest edit {id} post' when it's ready."
        ),
        Err(err) => err,
    }
}

/// `quest show <id>`: the quest as its file is written.
pub(super) fn show(d: &Desk, args: &str) -> String {
    match named(d, args.trim()) {
        None => format!("There's no quest {}.", args.trim()),
        Some(quest) => quest
            .file_contents()
            .map(|text| text.replace('\n', "\r\n"))
            .unwrap_or_else(|err| err.to_string()),
    }
}

/// `quest edit <id> <field> ...`: change one thing, check the quest still holds together, and
/// save it.
pub(super) fn edit(d: &mut Desk, gm: &Actor, args: &str) -> String {
    let usage = format!("Usage: quest edit <id> {FIELDS}");
    let mut words = args.trim().splitn(2, ' ');
    let (Some(id), rest) = (words.next(), words.next().unwrap_or("").trim()) else {
        return usage;
    };
    let Some(mut quest) = named(d, id).cloned() else {
        return format!("There's no quest {id}.");
    };
    let (field, value) = rest.split_once(' ').unwrap_or((rest, ""));
    let value = value.trim();
    let changed = match field {
        "name" | "summary" | "description" if !value.is_empty() => {
            let text = value.to_string();
            match field {
                "name" => quest.name = text,
                "summary" => quest.summary = text,
                _ => quest.description = text,
            }
            Ok(format!("its {field}"))
        }
        "board" => match d.hand.rooms.get(gm.room) {
            Ok((_, here, _)) => {
                quest.posted = Posted::Board(match here.zone == quest.zone() {
                    true => here.room.clone(),
                    false => here.to_string(),
                });
                Ok("its board, to the one here".into())
            }
            Err(_) => Err(String::new()),
        },
        "rank" => match value.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["none"] => {
                quest.contract = None;
                Ok("it isn't a contract now".into())
            }
            [rank, merit] if guild().rank(rank).is_some() => match merit.parse() {
                Ok(merit) => {
                    quest.contract = Some(Contract {
                        rank: rank.to_string(),
                        merit,
                    });
                    Ok(format!("a {rank} contract for {merit} merit"))
                }
                Err(_) => Err("Merit is a number.".into()),
            },
            _ => Err(format!(
                "Give a rank and its merit, or none. Ranks: {}.",
                guild()
                    .ranks
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
        "open" => {
            quest.availability = Availability::Open;
            Ok("open to anyone".into())
        }
        "claimed" => match Every::parse(value) {
            Some(lasts) => {
                quest.availability = Availability::Claimed { lasts };
                Ok(format!("claimed, for {value}"))
            }
            None => Err("How long: 10m, 2h, 1d.".into()),
        },
        "repeat" => {
            quest.repeat = match value {
                "once" => Repeat::Once,
                "always" => Repeat::Always,
                time => match Every::parse(time) {
                    Some(every) => Repeat::Every(every),
                    None => return "repeat once, always, or a time: 10m, 2h, 1d.".into(),
                },
            };
            Ok(format!("repeated {value}"))
        }
        "report" => {
            quest.report_to = (value != "none" && !value.is_empty()).then(|| value.to_string());
            Ok(format!(
                "reported to {}",
                if value.is_empty() { "none" } else { value }
            ))
        }
        "reward" => match value.split_once(' ').unwrap_or((value, "")) {
            ("clear", _) => {
                quest.rewards.clear();
                Ok("no rewards".into())
            }
            ("coin", n) => match n.trim().parse() {
                Ok(n) => {
                    quest.rewards.push(Reward::Coin(n));
                    Ok(format!("a reward of {n} copper"))
                }
                Err(_) => Err("reward coin <copper>".into()),
            },
            ("item", id) if !id.trim().is_empty() => {
                quest.rewards.push(Reward::Item(id.trim().into()));
                Ok(format!("a reward of {}", id.trim()))
            }
            _ => Err("reward coin <n>, reward item <id>, or reward clear".into()),
        },
        "stage" => stage(&mut quest, value),
        "post" | "unpost" => {
            quest.draft = field == "unpost";
            Ok(if quest.draft {
                "off the board"
            } else {
                "posted on its board"
            }
            .into())
        }
        _ => Err(usage),
    };
    let changed = match changed {
        Ok(changed) => changed,
        Err(err) => return err,
    };
    if let Err(err) = quest.check() {
        return format!("Not saved: the quest {err}.");
    }
    match keep(d, quest) {
        Ok(()) => format!("Saved {id}: {changed}."),
        Err(err) => err,
    }
}

/// `quest edit <id> stage ...`.
fn stage(quest: &mut QuestDef, value: &str) -> Result<String, String> {
    let usage = || {
        "stage add <text>, or stage <n> text <text>|set <objective>|add <objective>|remove|topic <mob> <topic> <answer>".to_string()
    };
    let (what, rest) = value.split_once(' ').unwrap_or((value, ""));
    if what == "add" {
        if rest.trim().is_empty() {
            return Err(usage());
        }
        // A new stage asks to come back to the board, until it's written.
        let Posted::Board(room) = &quest.posted;
        quest.stages.push(Stage {
            text: rest.trim().into(),
            objectives: vec![Objective::Reach(room.clone())],
            ..Default::default()
        });
        return Ok(format!("stage {}", quest.stages.len()));
    }
    let n: usize = what.parse().map_err(|_| usage())?;
    let Some(stage) = n.checked_sub(1).and_then(|i| quest.stages.get_mut(i)) else {
        return Err(format!("It has {} stages.", quest.stages.len()));
    };
    let (op, rest) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
    let rest = rest.trim();
    match op {
        "text" if !rest.is_empty() => {
            stage.text = rest.into();
            Ok(format!("stage {n}'s text"))
        }
        "set" => {
            stage.objectives = vec![objective(rest)?];
            Ok(format!("stage {n}'s objective"))
        }
        "add" => {
            stage.objectives.push(objective(rest)?);
            Ok(format!("an objective on stage {n}"))
        }
        "topic" => {
            let mut words = rest.splitn(3, ' ');
            let (Some(mob), Some(topic), Some(answer)) = (words.next(), words.next(), words.next())
            else {
                return Err("stage <n> topic <mob> <topic> <answer>".into());
            };
            stage
                .topics
                .entry(mob.into())
                .or_default()
                .push((topic.into(), answer.trim().into()));
            Ok(format!("what {mob} says of {topic} on stage {n}"))
        }
        "remove" if quest.stages.len() > 1 => {
            quest.stages.remove(n - 1);
            Ok(format!("stage {n} removed"))
        }
        "remove" => Err("A quest needs a stage.".into()),
        _ => Err(usage()),
    }
}

/// An objective as typed: `kill <mob> <n>`, `kill tag <tag> <n>`, `bring <item> <n>`, `bring tag
/// <tag> <n>`, `find <item> <n>`, `reach <room>`, `ask <mob> <topic>`, `escort <mob> <room>`.
fn objective(text: &str) -> Result<Objective, String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let n = |w: &str| {
        w.parse::<u32>()
            .map_err(|_| format!("'{w}' isn't a number."))
    };
    Ok(match words.as_slice() {
        ["kill", "tag", tag, count] => Objective::Kill(MobMatch::Tag(tag.to_string()), n(count)?),
        ["kill", mob, count] => Objective::Kill(MobMatch::Mob(mob.to_string()), n(count)?),
        ["bring", "tag", tag, count] => {
            Objective::Bring(ItemMatch::Tag(tag.to_string()), n(count)?)
        }
        ["bring", item, count] => Objective::Bring(ItemMatch::Item(item.to_string()), n(count)?),
        ["find", item, count] => Objective::Obtain(ItemMatch::Item(item.to_string()), n(count)?),
        ["reach", room] => Objective::Reach(room.to_string()),
        ["ask", mob, topic] => Objective::Ask(mob.to_string(), topic.to_string()),
        ["escort", mob, room] => Objective::Escort {
            mob: mob.to_string(),
            to: room.to_string(),
        },
        _ => {
            return Err(
                "An objective: kill <mob> <n>, kill tag <tag> <n>, bring <item> <n>, \
                        bring tag <tag> <n>, find <item> <n>, reach <room>, ask <mob> <topic>, \
                        escort <mob> <room>."
                    .into(),
            );
        }
    })
}

/// A quest by its id, `zone:id`, or by the id alone if only one zone has it.
fn named<'a>(d: &'a Desk, id: &str) -> Option<&'a QuestDef> {
    d.quests.0.get(id).or_else(|| {
        let mut found = d
            .quests
            .0
            .values()
            .filter(|q| q.id.ends_with(&format!(":{id}")));
        let first = found.next();
        found.next().is_none().then_some(first).flatten()
    })
}

/// Save `quest` to its file and keep it, so the next edit sees it before the file reloads.
fn keep(d: &mut Desk, quest: QuestDef) -> Result<(), String> {
    quest
        .save()
        .map_err(|err| format!("Couldn't save {}: {err}", quest.id))?;
    d.quests.0.insert(quest.id.clone(), quest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::testing::rats;

    #[test]
    fn objectives_are_typed_as_they_read() {
        assert_eq!(
            objective("kill sewers:sewer_rat 20"),
            Ok(Objective::Kill(
                MobMatch::Mob("sewers:sewer_rat".into()),
                20
            ))
        );
        assert_eq!(
            objective("bring tag herb 5"),
            Ok(Objective::Bring(ItemMatch::Tag("herb".into()), 5))
        );
        assert_eq!(
            objective("reach forest:ravine"),
            Ok(Objective::Reach("forest:ravine".into()))
        );
        assert_eq!(
            objective("escort herbalist stalls"),
            Ok(Objective::Escort {
                mob: "herbalist".into(),
                to: "stalls".into()
            })
        );
        assert!(objective("kill rat many").is_err());
        assert!(objective("dance").is_err());
    }

    #[test]
    fn stages_are_written_one_edit_at_a_time() {
        let mut quest = rats();
        assert!(stage(&mut quest, "add Find the cat.").is_ok());
        assert_eq!(quest.stages.len(), 2);
        assert!(stage(&mut quest, "2 set ask contract_clerk cat").is_ok());
        assert!(
            quest.check().is_err(),
            "asked about what no one answers yet"
        );
        assert!(stage(&mut quest, "2 topic contract_clerk cat \"Try the cellar.\"").is_ok());
        assert!(quest.check().is_ok(), "{:?}", quest.check());
        assert_eq!(
            quest.stages[1].topics["contract_clerk"][0].1,
            "\"Try the cellar.\""
        );
        assert!(stage(&mut quest, "2 text Find the cat in the cellar.").is_ok());
        assert_eq!(quest.stages[1].text, "Find the cat in the cellar.");
        assert!(stage(&mut quest, "1 remove").is_ok());
        assert!(
            stage(&mut quest, "1 remove").is_err(),
            "a quest needs a stage"
        );
        assert!(stage(&mut quest, "7 text nothing").is_err());
    }
}
