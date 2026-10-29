//! The board where quests are posted (`board`), and taking one from it (`accept`), alone or
//! offered to the rest of a group.

use bevy::prelude::*;

use super::actions::{Holder, Stagehand};
use super::claims::{Claim, Claims};
use super::def::{Availability, QuestDef, Quests, Repeat, Reward};
use super::journal::{DONE, HELD, QuestLog, time_left};
use crate::attributes::Attributes;
use crate::command::Invocation;
use crate::db::{Database, QuestRecord};
use crate::facts::{Facts, Owner};
use crate::guild::{Membership, guild};
use crate::items::ItemTemplates;
use crate::money;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::requirements::{Subject, Surroundings, Who};
use crate::skills::Skills;
use crate::time::unix_now;
use crate::world::{ItemKey, RoomKey};

/// Quests offered to the rest of a group in the room by the member who took one: each of them
/// can take it (`accept`) for a while, sharing its claim and time.
#[derive(Resource, Default)]
pub struct Offers(Vec<Offer>);

struct Offer {
    to: Entity,
    quest: String,
    at: i64,
}

/// How long an offer stands, in seconds.
const OFFER_SECS: i64 = 120;

impl QuestDef {
    /// Where a contract's rank stands among the guild's: 0 for Bone, and for anything that
    /// isn't a contract.
    fn rank(&self) -> usize {
        self.contract
            .as_ref()
            .and_then(|c| guild().rank(&c.rank))
            .unwrap_or(0)
    }

    /// How the board lists it: its rank, if a contract, its name and summary.
    fn notice_line(&self, n: usize) -> String {
        let rank = match &self.contract {
            Some(_) => format!("[{}] ", guild().ranks[self.rank()].name),
            None => String::new(),
        };
        format!("  {n}. {rank}{}: {}", self.name, self.summary)
    }

    /// The notice in full: what it is, what it asks, what it pays, and how long it can be held.
    fn notice(&self, templates: &ItemTemplates) -> String {
        let mut lines = vec![match &self.contract {
            Some(_) => format!(
                "{} (a {} contract)",
                self.name,
                guild().ranks[self.rank()].name
            ),
            None => self.name.clone(),
        }];
        lines.push(self.description.clone());
        let copper: u64 = self
            .rewards
            .iter()
            .map(|reward| match reward {
                Reward::Coin(copper) => *copper,
                Reward::Item(_) | Reward::Set(..) | Reward::Merit(_) => 0,
            })
            .sum();
        let mut pays: Vec<String> = (copper > 0)
            .then(|| money::coins(copper))
            .into_iter()
            .collect();
        pays.extend(self.rewards.iter().filter_map(|reward| {
            match reward {
                Reward::Item(id) => Some(
                    templates
                        .0
                        .get(&ItemKey::parse(id, self.zone()))
                        .map_or_else(|| id.clone(), |def| def.name.clone()),
                ),
                // What a quest sets is for the world to notice, not the notice to say.
                Reward::Coin(_) | Reward::Set(..) | Reward::Merit(_) => None,
            }
        }));
        pays.extend(self.pays(None).1.map(|merit| format!("{merit} merit")));
        // A quest with endings pays as it ends.
        if !self.endings.is_empty() {
            pays.clear();
        }
        if let [rest @ .., last] = pays.as_slice() {
            // "2 gold and 5 silver, and 10 merit"; "2 gold and 10 merit".
            let text = match rest {
                [] => last.clone(),
                _ if rest.iter().any(|part| part.contains(" and ")) => {
                    format!("{}, and {last}", rest.join(", "))
                }
                _ => format!("{} and {last}", rest.join(", ")),
            };
            lines.push(format!("It pays {text}."));
        }
        if !self.endings.is_empty() {
            lines.push("What it pays depends on how it ends.".into());
        }
        lines.push(match &self.availability {
            Availability::Open => "Anyone may take it, and any number at once.".to_string(),
            Availability::Claimed { lasts } => format!(
                "One at a time: whoever takes it has {} to finish it.",
                lasts.period()
            ),
        });
        lines.push(match &self.repeat {
            Repeat::Once => "It can be done only once.".to_string(),
            Repeat::Every(every) => {
                format!("It can be taken again {} after it's done.", every.period())
            }
            Repeat::Always => "It can be taken again as soon as it's done.".to_string(),
        });
        lines.join("\r\n")
    }
}

/// `board [<n>]`: the notices posted on the board here that the reader may see (contracts at
/// their rank and one above; Bone work for anyone else), numbered by rank and then name; or one
/// in full.
#[allow(clippy::too_many_arguments)]
pub(super) fn board(
    In(inv): In<Invocation>,
    players: Characters,
    rooms: RoomQuery,
    memberships: Query<&Membership>,
    (quests, claims): (Res<Quests>, Res<Claims>),
    story: Res<Facts>,
    templates: Res<ItemTemplates>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let Ok((_, here, _)) = rooms.get(actor.room) else {
        return;
    };
    let member = memberships.get(actor.entity).ok().filter(|m| !m.expelled);
    let notices = posted(here, member.map(|m| m.rank), &quests, &claims, &story);
    // A board is wherever any quest is posted, whether or not this reader may see it.
    let text = if !quests.0.values().any(|q| q.board().as_ref() == Some(here)) {
        "There's no board here.".to_string()
    } else if inv.args.trim().is_empty() {
        let mut lines = vec!["Notices on the board:".to_string()];
        if notices.is_empty() {
            lines.push("  None for you just now.".into());
        }
        lines.extend(
            notices
                .iter()
                .enumerate()
                .map(|(i, q)| q.notice_line(i + 1)),
        );
        lines.push(match member {
            Some(_) => "Type 'board <n>' to read a notice.".into(),
            None => "Type 'board <n>' to read a notice. Only guild members can take contracts \
                     ('join guild')."
                .into(),
        });
        lines.join("\r\n")
    } else {
        match inv
            .args
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| notices.get(n.checked_sub(1)?))
        {
            Some(quest) => quest.notice(&templates),
            None => format!("There's no notice {} on the board.", inv.args.trim()),
        }
    };
    deliver(vec![Outgoing::line(actor.entity, text)], &mut outbox);
}

/// What's posted on the board in room `here` for a reader of guild rank `rank` (none for a
/// non-member), in the order it's listed: not what's claimed.
fn posted<'a>(
    here: &RoomKey,
    rank: Option<usize>,
    quests: &'a Quests,
    claims: &Claims,
    story: &Facts,
) -> Vec<&'a QuestDef> {
    let highest = rank.map_or(0, |r| r + 1);
    let mut notices: Vec<&QuestDef> = quests
        .0
        .values()
        .filter(|q| q.board().as_ref() == Some(here))
        .filter(|q| !claims.0.contains_key(&q.id))
        .filter(|q| q.contract.is_none() || q.rank() <= highest)
        .filter(|q| {
            q.posted_when
                .as_ref()
                .is_none_or(|when| when.met(&Subject::world(story, q.zone(), None)))
        })
        .collect();
    notices.sort_by(|a, b| (a.rank(), &a.name).cmp(&(b.rank(), &b.name)));
    notices
}

/// Why `quest` can't be taken by someone with this log, at `now`, if it can't: already held,
/// done for good, or done too lately to take again.
fn refusal(quest: &QuestDef, log: &QuestLog, now: i64) -> Option<String> {
    let record = log.get(&quest.id)?;
    match (record.state.as_str(), &quest.repeat, record.finished_at) {
        (HELD, ..) => Some(format!("You've already taken {}.", quest.name)),
        (DONE, Repeat::Once, _) => {
            Some(format!("You've done {}; it's not done twice.", quest.name))
        }
        (DONE, Repeat::Every(every), Some(finished)) if finished + every.secs() > now => {
            Some(format!(
                "You did {} too lately: it can be taken again {} after it's done.",
                quest.name,
                every.period()
            ))
        }
        _ => None,
    }
}

/// `accept <n>`: take notice `n` from the board here, in front of the guild's clerk, who writes
/// a contract into the guild's record. A claimed quest comes off the board, theirs until its
/// time is up. Taken in a group, it's offered to the others here; `accept` alone takes what
/// another member offered, sharing their claim and its time.
#[allow(clippy::too_many_arguments)]
pub(super) fn accept(
    In(inv): In<Invocation>,
    (players, groups): (Characters, crate::groups::Groups),
    rooms: RoomQuery,
    mobs: crate::mobs::MobQuery,
    (memberships, mut logs): (Query<&Membership>, Query<&mut QuestLog>),
    sheets: Query<(&Attributes, &Skills)>,
    (quests, mut claims, mut offers): (Res<Quests>, ResMut<Claims>, ResMut<Offers>),
    // What they are and carry for `requires`, and the facts to clear a quest taken afresh.
    mut both: ParamSet<(Surroundings, Stagehand)>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let (Some(character_id), Ok((_, here, _))) = (actor.character_id, rooms.get(actor.room)) else {
        return;
    };
    let member = memberships.get(actor.entity).ok().filter(|m| !m.expelled);
    let around = both.p0();
    let notices = posted(
        here,
        member.map(|m| m.rank),
        &quests,
        &claims,
        &around.story,
    );
    let now = unix_now();
    offers.0.retain(|offer| now - offer.at <= OFFER_SECS);
    let offered = matches!(inv.args.trim(), "" | "offer")
        .then(|| offers.0.iter().rev().find(|o| o.to == actor.entity))
        .flatten()
        .and_then(|offer| quests.0.get(&offer.quest));
    let reply = (|| {
        let quest = match offered {
            Some(quest) => quest,
            None => {
                if !quests.0.values().any(|q| q.board().as_ref() == Some(here)) {
                    return Err("There's no board here.".to_string());
                }
                inv.args
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| notices.get(n.checked_sub(1)?))
                    .ok_or_else(|| match inv.args.trim() {
                        "" | "offer" => inv.usage_error(),
                        n => format!("There's no notice {n} on the board."),
                    })?
            }
        };
        // Shared, a claim must still be out to be shared.
        if offered.is_some()
            && matches!(quest.availability, Availability::Claimed { .. })
            && !claims.0.contains_key(&quest.id)
        {
            return Err(format!("No one holds {} now to share it.", quest.name));
        }
        let clerk = crate::guild::clerk_in(actor.room, &mobs)
            .map(|(_, name)| crate::combat::the(&name))
            .ok_or("There's no one here to take it to.")?;
        if quest.contract.is_some() && member.is_none() {
            return Err("Only guild members can take contracts ('join guild').".into());
        }
        let log = logs.get(actor.entity).ok();
        if let Some(refused) = log.and_then(|log| refusal(quest, log, now)) {
            return Err(refused);
        }
        if let Some(requires) = &quest.requires {
            let (attributes, skills) = sheets.get(actor.entity).map_err(|_| String::new())?;
            let known = around.of(actor.entity);
            let facts = Subject::of(
                Who {
                    attributes,
                    race: &actor.race,
                    skills: Some(skills),
                    guild_rank: known.guild_rank,
                    character: Some(character_id),
                    conditions: around.conditions.get(actor.entity).ok(),
                    room: known.room.as_ref(),
                    carrying: Some(&known.carrying),
                },
                Some(&around.story),
            );
            if !requires.met(&facts) {
                return Err(format!(
                    "{} can't be yours yet: it needs {}.",
                    quest.name,
                    requires.describe(&|id| id.to_string())
                ));
            }
        }
        Ok((quest, clerk))
    })();
    let mut told = Vec::new();
    let mut handed = String::new();
    let text = match reply {
        Err(text) => text,
        Ok((quest, clerk)) => {
            // A claimed quest is theirs alone (or their group's), until its time is up.
            let due_at = match (&quest.availability, claims.0.get(&quest.id)) {
                (Availability::Claimed { .. }, Some(claim)) => Some(claim.until),
                (Availability::Claimed { lasts }, None) => Some(now + lasts.secs()),
                (Availability::Open, _) => None,
            };
            let record = QuestRecord {
                quest: quest.id.clone(),
                state: HELD.into(),
                stage: quest.stage_key(0),
                taken_at: now,
                due_at,
                finished_at: log_finished(logs.get(actor.entity).ok(), &quest.id),
            };
            if let Some(until) = due_at {
                claims
                    .0
                    .entry(quest.id.clone())
                    .or_insert(Claim {
                        holders: Vec::new(),
                        until,
                    })
                    .holders
                    .push(character_id);
            }
            offers
                .0
                .retain(|o| !(o.to == actor.entity && o.quest == quest.id));
            // What it gives to carry goes to whoever took it from the board, not to every
            // member who shares it.
            let mut hand = both.p1();
            if offered.is_none() && !quest.gives.is_empty() {
                let mut held = crate::items::hands_full(actor.entity, &hand.items);
                let given: Vec<String> = quest
                    .gives
                    .iter()
                    .map(|id| {
                        crate::items::hand_new(
                            (&actor).into(),
                            ItemKey::parse(id, quest.zone()),
                            &mut held,
                            &mut hand.commands,
                            &hand.item_templates,
                            &hand.rooms,
                            &hand.db,
                        )
                    })
                    .collect();
                handed = format!(
                    "\r\n{} hands you {}.",
                    crate::items::capitalized(&clerk),
                    given.join(", ")
                );
            }
            // Taken from the board in a group, it's offered to the rest of the group here who
            // don't hold it already.
            if offered.is_none() {
                for mate in crate::groups::members(actor.entity, &groups) {
                    let holds = logs
                        .get(mate)
                        .is_ok_and(|log| log.get(&quest.id).is_some_and(|r| r.state == HELD));
                    let here = Actor::get(&players, mate)
                        .is_some_and(|a| a.room == actor.room && a.character_id.is_some());
                    if mate == actor.entity || holds || !here {
                        continue;
                    }
                    offers.0.push(Offer {
                        to: mate,
                        quest: quest.id.clone(),
                        at: now,
                    });
                    told.push(Outgoing::line(
                        mate,
                        format!(
                            "{} offers to share {} with you: type 'accept' to take it on \
                             together.",
                            actor.name, quest.name
                        ),
                    ));
                }
            }
            match logs.get_mut(actor.entity) {
                Ok(mut log) => log.put(record, character_id, &db),
                Err(_) => error!("{} has no quest log", actor.name),
            }
            // Taken again, it starts from nothing; then its first stage begins.
            hand.story
                .forget(&Owner::Quest(character_id, quest.id.clone()), &db);
            let holder = Holder {
                entity: actor.entity,
                character_id,
                room: actor.room,
            };
            for line in hand.act(&quest.stages[0].on_begin, quest, holder) {
                handed += &format!("\r\n{line}");
            }
            if quest.contract.is_some() {
                if let Err(err) = db.add_guild_record(character_id, &quest.id, "accepted", now) {
                    error!(
                        "failed to write {} into the guild's record: {err}",
                        quest.id
                    );
                }
                let time = match due_at {
                    Some(due) => format!(" You have {} to finish it.", time_left(due - now)),
                    None => String::new(),
                };
                format!(
                    "{} writes {} into the guild's record against your name.{time} Type \
                     'journal' to see what you've taken on.",
                    crate::items::capitalized(&clerk),
                    quest.name
                )
            } else {
                format!(
                    "You take on {}. Type 'journal' to see what you've taken on.",
                    quest.name
                )
            }
        }
    };
    told.insert(0, Outgoing::line(actor.entity, text + &handed));
    deliver(told, &mut outbox);
}

/// When `quest` was last finished, kept over its being taken again, for `repeat`.
fn log_finished(log: Option<&QuestLog>, quest: &str) -> Option<i64> {
    log?.get(quest)?.finished_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quests::def::{Contract, Posted};
    use crate::quests::testing::rats;
    use crate::requirements::Requirement;

    #[test]
    fn the_board_shows_work_at_your_rank_and_one_above() {
        let notice = |id: &str, name: &str, rank: Option<&str>| QuestDef {
            id: format!("riftwatch:{id}"),
            name: name.into(),
            contract: rank.map(|rank| Contract {
                rank: rank.into(),
                merit: 5,
            }),
            ..rats()
        };
        let mut quests = Quests::default();
        for quest in [
            notice("rats", "Rats", Some("bone")),
            notice("roaches", "Roaches", Some("bone")),
            notice("king", "The Rat King", Some("copper")),
            notice("ravine", "The Ravine", Some("iron")),
            notice("herbs", "Herbs", None),
            QuestDef {
                posted: Posted::Board("sewers:culvert".into()),
                ..notice("elsewhere", "Elsewhere", None)
            },
            QuestDef {
                posted_when: Some(Requirement::WorldFlag("rats_broke_out".into())),
                ..notice("breakout", "Breakout", Some("bone"))
            },
        ] {
            quests.0.insert(quest.id.clone(), quest);
        }
        let hall = RoomKey::parse("riftwatch:contract_hall", "");
        let db = crate::db::Database::open(":memory:").unwrap();
        let mut story = Facts::default();
        let names = |rank: Option<usize>, story: &Facts| -> Vec<String> {
            posted(&hall, rank, &quests, &Claims::default(), story)
                .iter()
                .map(|q| q.name.clone())
                .collect()
        };
        assert_eq!(
            names(None, &story),
            ["Herbs", "Rats", "Roaches"],
            "a non-member sees Bone work, and what isn't a contract"
        );
        assert_eq!(
            names(Some(0), &story),
            ["Herbs", "Rats", "Roaches", "The Rat King"],
            "Bone, and Copper above"
        );
        assert_eq!(names(Some(1), &story).len(), 5, "Copper sees Iron work");
        story.set(
            &crate::facts::Owner::World,
            "rats_broke_out",
            crate::facts::Value::Bool(true),
            None,
            &db,
        );
        assert!(names(Some(0), &story).contains(&"Breakout".to_string()));
        assert!(!names(Some(4), &story).contains(&"Elsewhere".to_string()));
        // A claimed notice is off the board while it's held.
        let mut claims = Claims::default();
        claims.0.insert(
            "riftwatch:rats".into(),
            Claim {
                holders: vec![1],
                until: 100,
            },
        );
        let shown: Vec<&str> = posted(&hall, Some(0), &quests, &claims, &story)
            .iter()
            .map(|q| q.name.as_str())
            .collect();
        assert!(!shown.contains(&"Rats") && shown.contains(&"Roaches"));
    }

    #[test]
    fn a_notice_says_what_it_pays_and_how_long_it_lasts() {
        let quest = QuestDef {
            contract: Some(Contract {
                rank: "bone".into(),
                merit: 10,
            }),
            ..rats()
        };
        assert_eq!(
            quest.notice(&ItemTemplates::default()),
            "Rats (a Bone contract)\r\nRats, at length.\r\nIt pays 2 gold and 5 silver, and 10 merit.\r\n\
             One at a time: whoever takes it has a day to finish it.\r\n\
             It can be taken again a day after it's done."
        );
        assert_eq!(quest.notice_line(1), "  1. [Bone] Rats: Rats.");
        // Items too, by name.
        let mut templates = ItemTemplates::default();
        templates.0.insert(
            ItemKey::parse("riftwatch:healing_draught", ""),
            crate::world::ItemDef::named("a healing draught"),
        );
        let quest = QuestDef {
            rewards: vec![Reward::Coin(20), Reward::Item("healing_draught".into())],
            ..quest
        };
        assert!(
            quest
                .notice(&templates)
                .contains("It pays 2 silver, a healing draught and 10 merit.")
        );
    }

    #[test]
    fn a_quest_is_taken_again_only_as_it_allows() {
        let record = |state: &str, finished_at| QuestRecord {
            quest: "riftwatch:rats".into(),
            state: state.into(),
            stage: "1".into(),
            taken_at: 0,
            due_at: None,
            finished_at,
        };
        let day = 86_400;
        let rats = rats();
        let log = |r| QuestLog(vec![r]);
        assert!(
            refusal(&rats, &QuestLog::default(), 0).is_none(),
            "never taken"
        );
        assert!(
            refusal(&rats, &log(record(HELD, None)), 0).is_some(),
            "held"
        );
        let done = log(record(DONE, Some(100)));
        assert!(refusal(&rats, &done, 100 + day - 1).is_some(), "too soon");
        assert!(refusal(&rats, &done, 100 + day).is_none(), "a day on");
        let once = QuestDef {
            repeat: Repeat::Once,
            ..rats.clone()
        };
        assert!(refusal(&once, &done, 100 * day).is_some());
        let always = QuestDef {
            repeat: Repeat::Always,
            ..rats.clone()
        };
        assert!(refusal(&always, &done, 101).is_none());
        // Failing or giving one up doesn't stop it being taken again.
        assert!(refusal(&rats, &log(record("abandoned", None)), 0).is_none());
    }
}
