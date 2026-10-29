//! Quests: written as data, one RON file each, in a folder named for the zone they belong to
//! (`assets/quests/riftwatch/rats_in_the_drains.quest.ron` is `riftwatch:rats_in_the_drains`).
//! They're loaded, checked and hot-reloaded here, read from the board where they're posted, taken
//! with the guild's clerk (`accept`), and kept in each character's `QuestLog` and `journal`.
//! Objectives are counted as the quest's own facts (`quest.` facts, by stage: `1.kill.sewers:
//! sewer_rat`) from game events, and a stage whose objectives are all counted and done moves on
//! to the next. Reporting and rewards come later (see `docs/quests.md`).

use std::collections::HashMap;
use std::path::Path;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::attributes::Attributes;
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, QuestRecord};
use crate::events::GameEvent;
use crate::facts::{Facts, Owner};
use crate::guild::{Membership, guild};
use crate::items::{ItemLocation, ItemTemplates};
use crate::mobs::MobTemplates;
use crate::money;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::requirements::{ItemMatch, Requirement, Subject, Surroundings, Who};
use crate::skills::Skills;
use crate::time::unix_now;
use crate::world::{Every, ItemKey, MobDef, MobKey, Player, RoomKey, ron_options, without_article};

/// Asset folder holding one folder of quests per zone.
pub const QUESTS_PATH: &str = "quests";
/// The extension quest files use.
const QUEST_EXTENSION: &str = "quest.ron";

pub struct QuestsPlugin;

impl Plugin for QuestsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<QuestDef>()
            .register_asset_loader(QuestLoader)
            .init_resource::<Quests>()
            .init_resource::<Claims>()
            .init_resource::<Offers>()
            .add_systems(Startup, (load_quests, load_claims))
            .add_systems(Update, (sync_quests, count_kills))
            .add_systems(Update, expire_claims.run_if(crate::time::heartbeat()))
            .add_command(
                CommandSpec::new("board", Category::Interaction)
                    .shortcuts(&["contracts"])
                    .usage("board [<n>]")
                    .summary("read the notices posted on a quest board here, or one in full"),
                board,
            )
            .add_command(
                CommandSpec::new("accept", Category::Interaction)
                    .usage("accept <n>")
                    .summary("take a notice from the board, with the guild's clerk here"),
                accept,
            )
            .add_command(
                CommandSpec::new("report", Category::Interaction)
                    .usage("report [<quest>]")
                    .summary("report a finished quest to whoever it's for, here, and be paid"),
                report,
            )
            .add_command(
                CommandSpec::new("abandon", Category::Interaction)
                    .usage("abandon <quest>")
                    .summary("give up a quest you're on; a contract costs some merit"),
                abandon,
            )
            .add_command(
                CommandSpec::new("journal", Category::Character)
                    .shortcuts(&["quests"])
                    .summary("the quests you're on, where you are in each, and those you've done"),
                journal,
            );
    }
}

/// The quests a character has taken, and how each stands: loaded when they enter the game and
/// saved as it changes.
#[derive(Component, Default, Debug)]
pub struct QuestLog(pub Vec<QuestRecord>);

impl QuestLog {
    fn get(&self, quest: &str) -> Option<&QuestRecord> {
        self.0.iter().find(|record| record.quest == quest)
    }

    /// Set down how `record` stands now, in place of how it stood, and save it.
    fn put(&mut self, record: QuestRecord, character_id: i64, db: &Database) {
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

fn load_claims(db: Res<Database>, mut claims: ResMut<Claims>) {
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
fn expire_claims(
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
fn abandon(
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

/// One quest, as written. Ids in it are its zone's own unless written `zone:id`.
// Read by the board, journal and rules, once they're built.
#[allow(dead_code)]
#[derive(Asset, TypePath, Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct QuestDef {
    /// Its id, `zone:quest`, from where the file is.
    #[serde(skip)]
    pub id: String,
    pub name: String,
    /// One line, as the board lists it.
    pub summary: String,
    /// Read in full from the notice, and kept in the journal.
    pub description: String,
    /// Where it's offered.
    pub posted: Posted,
    /// Only shown, and only takeable, while this holds.
    #[serde(default)]
    pub posted_when: Option<Requirement>,
    /// A guild contract: its rank, and the merit it pays.
    #[serde(default)]
    pub contract: Option<Contract>,
    pub availability: Availability,
    /// Who may take it, beyond a contract's rank.
    #[serde(default)]
    pub requires: Option<Requirement>,
    /// How soon a character can take it again once they've finished it.
    pub repeat: Repeat,
    /// In order; a stage's objectives in any order, all of them needed.
    pub stages: Vec<Stage>,
    /// The mob it's reported to; the guild's clerk if none, for a contract.
    #[serde(default)]
    pub report_to: Option<String>,
    #[serde(default)]
    pub rewards: Vec<Reward>,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Posted {
    /// On the board in this room.
    Board(String),
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Contract {
    /// A guild rank's id: `bone`, `copper`.
    pub rank: String,
    pub merit: i64,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Availability {
    /// Anyone may hold it, any number at once.
    Open,
    /// One holder (or group) at a time, off the board while held; not done in time, it fails.
    Claimed {
        #[serde(rename = "for")]
        lasts: Every,
    },
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Repeat {
    Once,
    Every(Every),
    Always,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Stage {
    /// What the journal says while it's the stage they're on.
    pub text: String,
    pub objectives: Vec<Objective>,
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Objective {
    /// So many of these mobs killed by the holder's side.
    Kill(MobMatch, u32),
    /// So many of these items had when reporting, and handed over.
    Bring(ItemMatch, u32),
}

/// Which mobs: a template, or any with a tag.
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum MobMatch {
    Mob(String),
    Tag(String),
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Reward {
    /// So much copper.
    Coin(u64),
    /// One of this item: `Item("healing_draught")`. Say it twice for two.
    Item(String),
    /// A fact about the character or the world, for others to test: `Set("char.title.
    /// rat_catcher", true)`, `Set("world.drains_cleared", true)`.
    Set(String, crate::facts::Value),
}

impl QuestDef {
    /// What every quest needs: its text, at least one stage, each with objectives, and times a
    /// quest can use. Whether what it names exists is checked against the zones.
    pub fn check(&self) -> Result<(), String> {
        let blank = |text: &str| text.trim().is_empty();
        if blank(&self.name) || blank(&self.summary) || blank(&self.description) {
            return Err("needs a name, a summary and a description".into());
        }
        if self.stages.is_empty() {
            return Err("has no stages".into());
        }
        for (n, stage) in self.stages.iter().enumerate() {
            if blank(&stage.text) {
                return Err(format!("stage {} has no text", n + 1));
            }
            if stage.objectives.is_empty() {
                return Err(format!("stage {} has no objectives", n + 1));
            }
            if stage.objectives.iter().any(|o| o.count() == 0) {
                return Err(format!("stage {} asks for none of something", n + 1));
            }
        }
        let times = [
            match &self.availability {
                Availability::Claimed { lasts } => Some(lasts),
                Availability::Open => None,
            },
            match &self.repeat {
                Repeat::Every(every) => Some(every),
                _ => None,
            },
        ];
        if times
            .into_iter()
            .flatten()
            .any(|every| matches!(every, Every::AfterDeath(_)))
        {
            return Err("can't wait for a death; give it minutes, hours or days".into());
        }
        for reward in &self.rewards {
            if let Reward::Set(key, _) = reward
                && !(key.starts_with("char.") || key.starts_with("world."))
            {
                return Err(format!("sets {key}: a reward sets char. or world. facts"));
            }
        }
        if let Some(contract) = &self.contract
            && !crate::guild::guild()
                .ranks
                .iter()
                .any(|rank| rank.id == contract.rank)
        {
            return Err(format!("is for a rank that isn't one: {}", contract.rank));
        }
        Ok(())
    }

    /// Its zone, from its id.
    pub fn zone(&self) -> &str {
        self.id.split_once(':').map_or("", |(zone, _)| zone)
    }

    /// Every room, mob and item it names, as `zone:id`, for checking against the zones (by the
    /// tests, for now: at load, the zones may not be in yet).
    #[cfg(test)]
    pub fn names(&self) -> Names {
        let zone = self.zone();
        let full = |id: &str| match id.contains(':') {
            true => id.to_string(),
            false => format!("{zone}:{id}"),
        };
        let mut names = Names::default();
        let Posted::Board(room) = &self.posted;
        names.rooms.push(full(room));
        names.mobs.extend(self.report_to.as_deref().map(full));
        names
            .items
            .extend(self.rewards.iter().filter_map(|reward| match reward {
                Reward::Item(id) => Some(full(id)),
                Reward::Coin(_) | Reward::Set(..) => None,
            }));
        for objective in self.stages.iter().flat_map(|s| &s.objectives) {
            match objective {
                Objective::Kill(MobMatch::Mob(mob), _) => names.mobs.push(full(mob)),
                Objective::Bring(ItemMatch::Item(item), _) => names.items.push(full(item)),
                Objective::Kill(MobMatch::Tag(_), _) | Objective::Bring(ItemMatch::Tag(_), _) => {}
            }
        }
        names
    }
}

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
                Reward::Item(_) | Reward::Set(..) => 0,
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
                Reward::Coin(_) | Reward::Set(..) => None,
            }
        }));
        pays.extend(self.contract.as_ref().map(|c| format!("{} merit", c.merit)));
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
fn board(
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
fn accept(
    In(inv): In<Invocation>,
    (players, groups): (Characters, crate::groups::Groups),
    rooms: RoomQuery,
    mobs: crate::mobs::MobQuery,
    (memberships, mut logs): (Query<&Membership>, Query<&mut QuestLog>),
    sheets: Query<(&Attributes, &Skills)>,
    (quests, mut claims, mut offers): (Res<Quests>, ResMut<Claims>, ResMut<Offers>),
    // What they are and carry for `requires`, and the facts to clear a quest taken afresh.
    mut both: ParamSet<(Surroundings, ResMut<Facts>)>,
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
                stage: "1".into(),
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
            // Taken again, it starts from nothing.
            both.p1()
                .forget(&Owner::Quest(character_id, quest.id.clone()), &db);
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
    told.insert(0, Outgoing::line(actor.entity, text));
    deliver(told, &mut outbox);
}

/// When `quest` was last finished, kept over its being taken again, for `repeat`.
fn log_finished(log: Option<&QuestLog>, quest: &str) -> Option<i64> {
    log?.get(quest)?.finished_at
}

/// `journal`: each quest you're on, where you are in it and how far along each objective is;
/// then what you've done.
fn journal(
    In(inv): In<Invocation>,
    logs: Query<(&QuestLog, &Player)>,
    quests: Res<Quests>,
    story: Res<Facts>,
    (mobs, items): (Res<MobTemplates>, Res<ItemTemplates>),
    mut outbox: ResMut<Outbox>,
) {
    let Ok((log, player)) = logs.get(inv.entity) else {
        return;
    };
    let lookup = Lookup {
        quests: &quests,
        story: &story,
        mobs: &mobs,
        items: &items,
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
struct Lookup<'a> {
    quests: &'a Quests,
    story: &'a Facts,
    mobs: &'a MobTemplates,
    items: &'a ItemTemplates,
    now: i64,
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
fn time_left(secs: i64) -> String {
    let (n, unit) = match secs.max(0) {
        s if s < 3600 => ((s / 60).max(1), "minute"),
        s if s < 48 * 3600 => (s / 3600, "hour"),
        s => (s / 86_400, "day"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// The stage someone is on, counted from 1.
fn stage_number(record: &QuestRecord) -> Option<usize> {
    record.stage.parse().ok().filter(|n| *n > 0)
}

/// Kills count for the quests their killer holds, for them and every member of their group in
/// the room who holds the same quest: each objective the dead mob answers on the stage that
/// holder is on goes up by one, to what it asks; a stage whose objectives are all counted and
/// done moves on to the next, and the last says it's time to report.
#[allow(clippy::too_many_arguments)]
fn count_kills(
    mut events: MessageReader<GameEvent>,
    mut logs: Query<(&mut QuestLog, &Player, &crate::world::InRoom)>,
    groups: crate::groups::Groups,
    quests: Res<Quests>,
    mut story: ResMut<Facts>,
    mobs: Res<MobTemplates>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let now = unix_now();
    let mut out = Vec::new();
    for event in events.read() {
        let GameEvent::Died {
            mob: Some(mob),
            killer: Some(killer),
            name,
            room,
            ..
        } = event
        else {
            continue;
        };
        // A member who doesn't hold a quest doesn't move it.
        let Ok((log, ..)) = logs.get(*killer) else {
            continue;
        };
        let theirs: Vec<String> = log
            .0
            .iter()
            .filter(|r| r.state == HELD)
            .map(|r| r.quest.clone())
            .collect();
        let there: Vec<Entity> = crate::groups::members(*killer, &groups)
            .into_iter()
            .filter(|e| logs.get(*e).is_ok_and(|(.., at)| at.0 == *room))
            .collect();
        let def = mobs.0.get(mob);
        for quest_id in theirs {
            let Some(quest) = quests.0.get(&quest_id) else {
                continue;
            };
            for holder in &there {
                let Ok((mut log, player, _)) = logs.get_mut(*holder) else {
                    continue;
                };
                let Some(record) = log.get(&quest_id).filter(|r| r.state == HELD).cloned() else {
                    continue;
                };
                let character_id = player.character_id;
                let (lines, moved) = tally(
                    &record,
                    quest,
                    character_id,
                    (mob, def, name),
                    &mut story,
                    &mobs,
                    &db,
                    now,
                );
                if let Some(record) = moved {
                    log.put(record, character_id, &db);
                }
                out.extend(lines.into_iter().map(|line| Outgoing::line(*holder, line)));
            }
        }
    }
    deliver(out, &mut outbox);
}

/// Count a kill of `mob` (its template, and its name) toward one holder's `record`: what to tell
/// them, and the record moved on to the next stage if this finished the one they were on.
#[allow(clippy::too_many_arguments)]
fn tally(
    record: &QuestRecord,
    quest: &QuestDef,
    character_id: i64,
    (mob, def, name): (&MobKey, Option<&MobDef>, &str),
    story: &mut Facts,
    mobs: &MobTemplates,
    db: &Database,
    now: i64,
) -> (Vec<String>, Option<QuestRecord>) {
    let mut lines = Vec::new();
    let Some(n) = stage_number(record) else {
        return (lines, None);
    };
    let Some(stage) = quest.stages.get(n - 1) else {
        return (lines, None);
    };
    let owner = Owner::Quest(character_id, quest.id.clone());
    let mut counted = false;
    for objective in &stage.objectives {
        let (Objective::Kill(which, need), Some(key)) = (objective, objective.key(n, quest.zone()))
        else {
            continue;
        };
        if !which.matches(mob, def, quest.zone()) {
            continue;
        }
        if story.number(&owner, &key, now) >= i64::from(*need) {
            continue;
        }
        let have = story.add(&owner, &key, 1, now, db);
        counted = true;
        let done = if have >= i64::from(*need) {
            " Done."
        } else {
            ""
        };
        lines.push(format!(
            "\x1b[1;33m{}:\x1b[0m {} killed, {have} of {need}.{done}",
            quest.name,
            without_article(name)
        ));
    }
    if !counted || !stage_done(stage, n, quest, &owner, story, now) {
        return (lines, None);
    }
    // All counted and done: on to the next stage, or back to report.
    match quest.stages.get(n) {
        Some(next) => {
            lines.push(format!("\x1b[1;33m{}:\x1b[0m {}", quest.name, next.text));
            let mut record = record.clone();
            record.stage = (n + 1).to_string();
            (lines, Some(record))
        }
        None => {
            lines.push(format!(
                "\x1b[1;33m{}:\x1b[0m that's all it asks. Report to {}.",
                quest.name,
                quest.reported_to(mobs)
            ));
            (lines, None)
        }
    }
}

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
            Objective::Kill(_, need) => {
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
struct Desk<'w, 's> {
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
fn report(In(inv): In<Invocation>, mut d: Desk) {
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
        let to = crate::items::capitalized(&to);
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

/// Whether every objective on stage `n` is counted as it happens, and done.
fn stage_done(
    stage: &Stage,
    n: usize,
    quest: &QuestDef,
    owner: &Owner,
    story: &Facts,
    now: i64,
) -> bool {
    stage.objectives.iter().all(|objective| {
        objective
            .key(n, quest.zone())
            .is_some_and(|key| story.number(owner, &key, now) >= i64::from(objective.count()))
    })
}

impl QuestDef {
    /// The room whose board it's posted on.
    fn board(&self) -> Option<RoomKey> {
        match &self.posted {
            Posted::Board(room) => Some(RoomKey::parse(room, self.zone())),
        }
    }
}

impl QuestDef {
    /// Who it's reported to, by name: its `report_to`, or the guild's clerk.
    fn reported_to(&self, mobs: &MobTemplates) -> String {
        let key = match &self.report_to {
            Some(mob) => MobKey::parse(mob, self.zone()),
            None => MobKey::parse(&guild().clerk, ""),
        };
        mobs.0
            .get(&key)
            .map_or_else(|| key.to_string(), |def| crate::combat::the(&def.name))
    }
}

impl MobMatch {
    /// Whether a mob of template `key` (`def`) is one of these, for a quest in `zone`.
    fn matches(&self, key: &MobKey, def: Option<&MobDef>, zone: &str) -> bool {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone) == *key,
            MobMatch::Tag(tag) => def.is_some_and(|def| def.tags.contains(tag)),
        }
    }

    /// As a fact's key: `sewers:sewer_rat`, or `tag.vermin`.
    fn label(&self, zone: &str) -> String {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone).to_string(),
            MobMatch::Tag(tag) => format!("tag.{tag}"),
        }
    }
}

impl Objective {
    /// The quest fact that counts it on stage `stage`, if it's counted as it happens:
    /// `1.kill.sewers:sewer_rat`. `Bring` is checked when reporting.
    fn key(&self, stage: usize, zone: &str) -> Option<String> {
        match self {
            Objective::Kill(which, _) => Some(format!("{stage}.kill.{}", which.label(zone))),
            Objective::Bring(..) => None,
        }
    }

    /// What it's after, as the journal names it: "sewer rat", "anything vermin".
    fn what(&self, zone: &str, lookup: &Lookup) -> String {
        match self {
            Objective::Kill(MobMatch::Mob(id), _) => lookup
                .mobs
                .0
                .get(&MobKey::parse(id, zone))
                .map_or(id.as_str(), |def| without_article(&def.name))
                .to_string(),
            Objective::Bring(ItemMatch::Item(id), _) => lookup
                .items
                .0
                .get(&ItemKey::parse(id, zone))
                .map_or(id.as_str(), |def| without_article(&def.name))
                .to_string(),
            Objective::Kill(MobMatch::Tag(tag), _) | Objective::Bring(ItemMatch::Tag(tag), _) => {
                format!("anything {tag}")
            }
        }
    }

    /// How the journal shows it: what, and how far along (`have`, if it's counted).
    fn progress(&self, zone: &str, have: Option<i64>, lookup: &Lookup) -> String {
        let what = self.what(zone, lookup);
        match self {
            Objective::Kill(_, need) => {
                let have = have.unwrap_or(0).min(i64::from(*need));
                let done = if have >= i64::from(*need) {
                    " (done)"
                } else {
                    ""
                };
                format!("Kill {what}: {have} of {need}{done}")
            }
            Objective::Bring(_, need) => format!("Bring {what}: {need}, counted when you report"),
        }
    }

    fn count(&self) -> u32 {
        match self {
            Objective::Kill(_, n) | Objective::Bring(_, n) => *n,
        }
    }
}

/// What a quest names, as `zone:id`.
#[cfg(test)]
#[derive(Default, Debug, PartialEq)]
pub struct Names {
    pub rooms: Vec<String>,
    pub mobs: Vec<String>,
    pub items: Vec<String>,
}

/// Every quest, by id.
#[derive(Resource, Default)]
pub struct Quests(pub HashMap<String, QuestDef>);

#[derive(Resource)]
struct QuestsHandle(#[allow(dead_code)] Handle<LoadedFolder>);

fn load_quests(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(QuestsHandle(assets.load_folder(QUESTS_PATH)));
}

/// Rebuild the quest table whenever a quest file loads, changes or goes away.
fn sync_quests(
    mut events: MessageReader<AssetEvent<QuestDef>>,
    defs: Res<Assets<QuestDef>>,
    mut quests: ResMut<Quests>,
) {
    if events.read().count() == 0 {
        return;
    }
    quests.0 = defs
        .iter()
        .map(|(_, quest)| (quest.id.clone(), quest.clone()))
        .collect();
    info!("loaded {} quests", quests.0.len());
}

#[derive(TypePath)]
struct QuestLoader;

#[derive(Debug, Error)]
enum QuestLoaderError {
    #[error("could not read quest: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse quest: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("quest {0}")]
    Incomplete(String),
}

impl AssetLoader for QuestLoader {
    type Asset = QuestDef;
    type Settings = ();
    type Error = QuestLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<QuestDef, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut quest: QuestDef = ron_options().from_bytes(&bytes)?;
        quest.id = quest_id(load_context.path().path());
        quest
            .check()
            .map_err(|err| QuestLoaderError::Incomplete(format!("{} {err}", quest.id)))?;
        Ok(quest)
    }

    fn extensions(&self) -> &[&str] {
        &[QUEST_EXTENSION]
    }
}

/// A quest's id from its file: `quests/riftwatch/rats_in_the_drains.quest.ron` is
/// `riftwatch:rats_in_the_drains`.
fn quest_id(path: &Path) -> String {
    let zone = path
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let quest = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(&format!(".{QUEST_EXTENSION}")))
        .unwrap_or_default();
    format!("{zone}:{quest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every quest file parses, passes its check, and names only rooms, mobs and items that
    /// exist.
    #[test]
    fn quest_files_are_consistent() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut zones = HashMap::new();
        for entry in std::fs::read_dir(root.join("zones")).unwrap() {
            let path = entry.unwrap().path();
            let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".zone.ron"))
            else {
                continue;
            };
            let zone: crate::world::ZoneDef = ron_options()
                .from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            zones.insert(id.to_string(), zone);
        }
        let exists = |full: &str, has: &dyn Fn(&crate::world::ZoneDef, &str) -> bool| {
            full.split_once(':')
                .is_some_and(|(zone, id)| zones.get(zone).is_some_and(|z| has(z, id)))
        };
        let mut count = 0;
        for folder in std::fs::read_dir(root.join(QUESTS_PATH)).unwrap() {
            for entry in std::fs::read_dir(folder.unwrap().path()).unwrap() {
                let path = entry.unwrap().path();
                let mut quest: QuestDef = ron_options()
                    .from_str(&std::fs::read_to_string(&path).unwrap())
                    .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
                quest.id = quest_id(&path);
                quest
                    .check()
                    .unwrap_or_else(|err| panic!("{}: {err}", quest.id));
                let names = quest.names();
                for room in &names.rooms {
                    assert!(
                        exists(room, &|z, id| z.rooms.contains_key(id)),
                        "{}: no room {room}",
                        quest.id
                    );
                }
                for mob in &names.mobs {
                    assert!(
                        exists(mob, &|z, id| z.mobs.contains_key(id)),
                        "{}: no mob {mob}",
                        quest.id
                    );
                }
                for item in &names.items {
                    assert!(
                        exists(item, &|z, id| z.items.contains_key(id)),
                        "{}: no item {item}",
                        quest.id
                    );
                }
                count += 1;
            }
        }
        assert!(count >= 1, "Rats in the Drains, at least");
    }

    #[test]
    fn quests_are_named_for_their_zone_and_file() {
        let path = Path::new("quests/riftwatch/rats_in_the_drains.quest.ron");
        assert_eq!(quest_id(path), "riftwatch:rats_in_the_drains");
    }

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
    fn time_left_is_said_roughly() {
        assert_eq!(time_left(30), "1 minute");
        assert_eq!(time_left(40 * 60), "40 minutes");
        assert_eq!(time_left(23 * 3600 + 59 * 60), "23 hours");
        assert_eq!(time_left(3 * 86_400), "3 days");
        assert_eq!(time_left(-5), "1 minute");
    }

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

    /// A quest to change for tests.
    fn rats() -> QuestDef {
        QuestDef {
            id: "riftwatch:rats".into(),
            name: "Rats".into(),
            summary: "Rats.".into(),
            description: "Rats, at length.".into(),
            posted: Posted::Board("contract_hall".into()),
            posted_when: None,
            contract: None,
            availability: Availability::Claimed {
                lasts: Every::Days(1),
            },
            requires: None,
            repeat: Repeat::Every(Every::Days(1)),
            stages: vec![Stage {
                text: "Kill rats.".into(),
                objectives: vec![Objective::Kill(MobMatch::Mob("rat".into()), 3)],
            }],
            report_to: None,
            rewards: vec![Reward::Coin(250)],
        }
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
    fn a_kill_counts_for_every_holder_in_the_group_who_is_there() {
        use crate::groups::InGroup;
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let mut world = World::new();
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        let rat = MobKey::parse("riftwatch:rat", "");
        world.insert_resource(MobTemplates(HashMap::from([(
            rat.clone(),
            MobDef::named("a rat"),
        )])));
        world.insert_resource(Quests(HashMap::from([("riftwatch:rats".into(), rats())])));
        let (cellar, street) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut ids = Vec::new();
        let mut spawn = |world: &mut World, name: &str, room: Entity, holds: bool| {
            let id = world
                .resource::<Database>()
                .create_character(account.id, name)
                .unwrap()
                .id;
            ids.push(id);
            let log = match holds {
                true => vec![QuestRecord {
                    quest: "riftwatch:rats".into(),
                    state: HELD.into(),
                    stage: "1".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }],
                false => Vec::new(),
            };
            world
                .spawn((
                    crate::world::InRoom(room),
                    Player {
                        name: name.into(),
                        character_id: id,
                        account: "ada".into(),
                        role: crate::db::Role::Player,
                        race: "human".into(),
                        background: String::new(),
                        description: String::new(),
                    },
                    QuestLog(log),
                ))
                .id()
        };
        world.insert_resource(db);
        let ada = spawn(&mut world, "Ada", cellar, true);
        let bob = spawn(&mut world, "Bob", cellar, true);
        spawn(&mut world, "Cara", cellar, true);
        let dan = spawn(&mut world, "Dan", street, true);
        let eve = spawn(&mut world, "Eve", cellar, false);
        // Ada leads Bob, Dan and Eve; Cara is on her own.
        for (n, who) in [ada, bob, dan, eve].into_iter().enumerate() {
            world.entity_mut(who).insert(InGroup {
                leader: ada,
                joined: n as u64,
            });
        }
        let kill = |world: &mut World, killer: Entity| {
            world.write_message(GameEvent::Died {
                who: cellar,
                name: "a rat".into(),
                room: cellar,
                mob: Some(rat.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_kills).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let count = |world: &World, id: i64| {
            world.resource::<Facts>().number(
                &Owner::Quest(id, "riftwatch:rats".into()),
                "1.kill.riftwatch:rat",
                0,
            )
        };
        kill(&mut world, eve);
        assert_eq!(
            count(&world, ids[0]),
            0,
            "Eve doesn't hold it, so her kill moves nothing"
        );
        kill(&mut world, bob);
        assert_eq!(count(&world, ids[0]), 1, "Bob's kill is Ada's too");
        assert_eq!(count(&world, ids[1]), 1);
        assert_eq!(count(&world, ids[2]), 0, "Cara isn't in their group");
        assert_eq!(count(&world, ids[3]), 0, "Dan isn't there");
    }

    #[test]
    fn kills_count_stage_by_stage() {
        use bevy::ecs::system::RunSystemOnce;
        let db = crate::db::Database::open(":memory:").unwrap();
        let account = db.create_account("ada", "password123").unwrap();
        let id = db.create_character(account.id, "Ada").unwrap().id;
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.init_resource::<Outbox>();
        world.init_resource::<Messages<GameEvent>>();
        world.init_resource::<ItemTemplates>();
        let (rat, roach) = (
            MobKey::parse("riftwatch:rat", ""),
            MobKey::parse("riftwatch:roach", ""),
        );
        let mut roach_def = MobDef::named("a giant cockroach");
        roach_def.tags = vec!["vermin".into()];
        world.insert_resource(MobTemplates(HashMap::from([
            (rat.clone(), MobDef::named("a rat")),
            (roach.clone(), roach_def),
        ])));
        let quest = QuestDef {
            stages: vec![
                Stage {
                    text: "Kill two rats.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Mob("rat".into()), 2)],
                },
                Stage {
                    text: "Kill a vermin.".into(),
                    objectives: vec![Objective::Kill(MobMatch::Tag("vermin".into()), 1)],
                },
            ],
            ..rats()
        };
        world.insert_resource(Quests(HashMap::from([(quest.id.clone(), quest)])));
        let room = world.spawn_empty().id();
        let ada = world
            .spawn((
                crate::world::InRoom(room),
                Player {
                    name: "Ada".into(),
                    character_id: id,
                    account: "ada".into(),
                    role: crate::db::Role::Player,
                    race: "human".into(),
                    background: String::new(),
                    description: String::new(),
                },
                QuestLog(vec![QuestRecord {
                    quest: "riftwatch:rats".into(),
                    state: HELD.into(),
                    stage: "1".into(),
                    taken_at: 0,
                    due_at: None,
                    finished_at: None,
                }]),
            ))
            .id();
        let stranger = world.spawn_empty().id();
        let kill = |world: &mut World, mob: &MobKey, killer: Entity| {
            world.write_message(GameEvent::Died {
                who: room,
                name: "a rat".into(),
                room,
                mob: Some(mob.clone()),
                killer: Some(killer),
            });
            world.run_system_once(count_kills).unwrap();
            world.resource_mut::<Messages<GameEvent>>().clear();
        };
        let stage = |world: &World| world.get::<QuestLog>(ada).unwrap().0[0].stage.clone();
        let owner = Owner::Quest(id, "riftwatch:rats".into());
        let count = |world: &World, key: &str| world.resource::<Facts>().number(&owner, key, 0);

        kill(&mut world, &rat, stranger);
        assert_eq!(
            count(&world, "1.kill.riftwatch:rat"),
            0,
            "a stranger's kill"
        );
        kill(&mut world, &roach, ada);
        assert_eq!(count(&world, "2.kill.tag.vermin"), 0, "not that stage yet");
        kill(&mut world, &rat, ada);
        assert_eq!(count(&world, "1.kill.riftwatch:rat"), 1);
        assert_eq!(stage(&world), "1");
        kill(&mut world, &rat, ada);
        assert_eq!(stage(&world), "2", "both rats: on to the next stage");
        kill(&mut world, &roach, ada);
        assert_eq!(count(&world, "2.kill.tag.vermin"), 1, "by its tag");
        assert_eq!(stage(&world), "2", "the last stage waits for a report");
        let said: Vec<String> = world
            .resource::<Outbox>()
            .0
            .iter()
            .filter_map(|o| match o {
                Outgoing::To(_, text) => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(said.iter().any(|t| t.contains("rat killed, 2 of 2. Done.")));
        assert!(said.iter().any(|t| t.contains("Kill a vermin.")));
        assert!(
            said.iter()
                .any(|t| t.contains("that's all it asks. Report to"))
        );
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

    #[test]
    fn a_quest_with_nothing_to_do_is_refused() {
        let quest = |stages: Vec<Stage>, contract: Option<Contract>| QuestDef {
            id: "test:quest".into(),
            name: "A quest".into(),
            summary: "Something to do.".into(),
            description: "Something to do, at length.".into(),
            posted: Posted::Board("hall".into()),
            posted_when: None,
            contract,
            availability: Availability::Open,
            requires: None,
            repeat: Repeat::Always,
            stages,
            report_to: None,
            rewards: Vec::new(),
        };
        let stage = |objectives| Stage {
            text: "Do it.".into(),
            objectives,
        };
        let kill = |n| Objective::Kill(MobMatch::Mob("rat".into()), n);
        assert!(quest(vec![stage(vec![kill(3)])], None).check().is_ok());
        assert!(quest(Vec::new(), None).check().is_err(), "no stages");
        assert!(quest(vec![stage(Vec::new())], None).check().is_err());
        assert!(quest(vec![stage(vec![kill(0)])], None).check().is_err());
        let tin = Contract {
            rank: "tin".into(),
            merit: 5,
        };
        assert!(
            quest(vec![stage(vec![kill(3)])], Some(tin))
                .check()
                .is_err()
        );
        // Local ids are the quest's zone's.
        let names = quest(vec![stage(vec![kill(3)])], None).names();
        assert_eq!(names.rooms, vec!["test:hall"]);
        assert_eq!(names.mobs, vec!["test:rat"]);
    }
}
