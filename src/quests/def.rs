//! What a quest is, as written in its file: the format, checking it, and loading every quest
//! into `Quests`, hot-reloaded.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::journal::Lookup;
use crate::guild::guild;
use crate::mobs::MobTemplates;
use crate::requirements::{ItemMatch, Requirement};
use crate::world::{Every, ItemKey, MobDef, MobKey, RoomKey, ron_options, without_article};

/// Asset folder holding one folder of quests per zone.
pub const QUESTS_PATH: &str = "quests";
/// The extension quest files use.
const QUEST_EXTENSION: &str = "quest.ron";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted_when: Option<Requirement>,
    /// A guild contract: its rank, and the merit it pays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract: Option<Contract>,
    pub availability: Availability,
    /// Who may take it, beyond a contract's rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<Requirement>,
    /// How soon a character can take it again once they've finished it.
    pub repeat: Repeat,
    /// Being written: off the board until it's posted (`quest edit <id> post`), though a GM can
    /// still `quest grant` it to try it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub draft: bool,
    /// In order; a stage's objectives in any order, all of them needed.
    pub stages: Vec<Stage>,
    /// The mob it's reported to; the guild's clerk if none, for a contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<String>,
    /// Items handed to whoever takes it from the board (the letter to carry), taken back if
    /// it's given up or fails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gives: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rewards: Vec<Reward>,
    /// The ways it can end, by name, each with its journal line and its own rewards, in place of
    /// `rewards` (and of a contract's merit, which an ending pays only as `Merit`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub endings: BTreeMap<String, EndingDef>,
    /// What fails it, for the holders it happens to, while they hold it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fails_if: Vec<Failure>,
}

/// Something that fails a quest.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Failure {
    /// A mob of this template dies: the one the quest spawned, for those it came for; any
    /// other (an escort, Narina), for everyone holding the quest.
    Dies(String),
    /// A holder who has had this item no longer has it on them. Handing it over as the quest
    /// asks isn't losing it.
    Lost(String),
}

/// One way a quest can end.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct EndingDef {
    /// What the journal says of it, once done: "You sold the guild's crystals to Talla Mirk."
    pub journal: String,
    #[serde(default)]
    pub rewards: Vec<Reward>,
}

/// Where a stage or a choice leads: another stage (`next: "alley"`), or an ending, reported for
/// (`next: End("sold")`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Next {
    Stage(String),
    Way(Way),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Way {
    End(String),
}

/// One way through a stage that offers a choice: whichever's objectives are done first is the
/// way taken, and the others close.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Choice {
    /// How the journal puts it: "Sell them to Talla Mirk."
    pub text: String,
    pub objectives: Vec<Objective>,
    /// Where it leads; without one, to reporting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Next>,
    /// Who this way is reported to, if not whoever the quest is: selling to the fence rather
    /// than returning to the guild.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_to: Option<String>,
}

/// Where a stage leads, once done.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Lead<'a> {
    /// To the stage at this place among them.
    Stage(usize),
    /// To reporting, and the ending it's reported for, if one.
    Report(Option<&'a str>),
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Stage {
    /// Its name, for `next` to lead to and to keep someone's place by: `"alley"`. Without one,
    /// it goes by its number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// What the journal says while it's the stage they're on.
    #[serde(alias = "journal")]
    pub text: String,
    /// What it asks, whichever way through it; a stage offering choices may ask nothing more.
    #[serde(default)]
    pub objectives: Vec<Objective>,
    /// The stage it leads to once done, by id. Without one, the stage after it; after the last,
    /// it's time to report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Next>,
    /// Ways through it, each with its own objectives and where it leads: the first done is the
    /// way taken. Its own `objectives`, if any, are needed whichever way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<Choice>,
    /// What mobs say while it's the stage a holder is on, when asked (`ask <mob> about <topic>`):
    /// by mob, each topic and its answer, written as it's to be read.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub topics: BTreeMap<String, Vec<(String, String)>>,
    /// What happens as a holder comes to it (on taking the quest, for the first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_begin: Vec<Action>,
    /// What happens as a holder finishes it (on reporting, for the last).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_end: Vec<Action>,
}

/// Something a stage does as it begins or ends, for the holder it's begun or ended for.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Action {
    /// Hand them one of this item: `Give("guild_letter")`.
    Give(String),
    /// Take one of this item from them, if they have it.
    Take(String),
    /// Set a fact: `char.`, `world.` or `quest.` (this quest's own).
    Set(String, crate::facts::Value),
    /// Add to a number fact.
    Add(String, i64),
    /// Clear a fact.
    Clear(String),
    /// A mob says something to them: `Say("contract_clerk", "Mind the culvert.")`.
    Say(String, String),
    /// A mob comes into the world for the quest, in a room: `Spawn("courier",
    /// "sewers:stalls_culvert")`. Anyone can see and fight it; it counts only for the holders
    /// it came for (`Kill(Quest("courier"))`), and it's cleared away once none of them holds the
    /// quest. One already there for the quest is shared, not doubled.
    Spawn(String, String),
    /// The quest fails for them, as a claim running out does; nothing after it in the list
    /// happens.
    Fail,
}

impl Action {
    /// The fact it changes, if it changes one.
    fn fact(&self) -> Option<&str> {
        match self {
            Action::Set(key, _) | Action::Add(key, _) | Action::Clear(key) => Some(key),
            _ => None,
        }
    }
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Objective {
    /// So many of these mobs killed by the holder's side.
    Kill(MobMatch, u32),
    /// So many of these items had when reporting, and handed over.
    Bring(ItemMatch, u32),
    /// So many of these items come by (picked up, given, bought) while on the stage: nothing is
    /// handed over. A clue; the letter you're to carry.
    Obtain(ItemMatch, u32),
    /// The room reached, while on the stage: `Reach("forest:ravine")`.
    Reach(String),
    /// A requirement that holds when reporting: a flag, a skill's level, a world flag.
    Meet(Requirement),
    /// Someone asked about something, of a topic the stage gives them: `Ask("contract_clerk",
    /// "crystals")`.
    Ask(String, String),
    /// A mob of this template, following them on the quest's say-so, walks into the room:
    /// `Escort(mob: "herbalist", to: "forest:ravine")`.
    Escort { mob: String, to: String },
    /// One of this item given to a mob of this template: `Give(item: "stolen_crystals", to:
    /// "contract_clerk")`. The quest takes what's given, as `Bring` does.
    Give { item: String, to: String },
}

/// Which mobs: a template, any with a tag, or the one a quest spawned (`Quest("courier")`, by
/// the template it was spawned from).
#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum MobMatch {
    Mob(String),
    Tag(String),
    Quest(String),
}

#[allow(dead_code)]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Reward {
    /// So much copper.
    Coin(u64),
    /// One of this item: `Item("healing_draught")`. Say it twice for two.
    Item(String),
    /// Guild merit, for an ending: `Merit(30)`.
    Merit(i64),
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
        let ids: Vec<&str> = self.stages.iter().filter_map(|s| s.id.as_deref()).collect();
        for (n, id) in ids.iter().enumerate() {
            if ids[..n].contains(id) {
                return Err(format!("has two stages called {id}"));
            }
        }
        for stage in &self.stages {
            // What it asks to be asked, it gives an answer to.
            for objective in stage.all_objectives() {
                if let Objective::Ask(mob, topic) = objective
                    && !stage.topics.get(mob).is_some_and(|topics| {
                        topics.iter().any(|(t, _)| t.eq_ignore_ascii_case(topic))
                    })
                {
                    return Err(format!(
                        "asks {mob} about {topic}, and gives them no answer"
                    ));
                }
            }
            let leads = std::iter::once(&stage.next).chain(stage.choices.iter().map(|c| &c.next));
            for next in leads {
                match next {
                    Some(Next::Stage(id)) if !ids.contains(&id.as_str()) => {
                        return Err(format!("leads to a stage it doesn't have: {id}"));
                    }
                    Some(Next::Way(Way::End(end))) if !self.endings.contains_key(end) => {
                        return Err(format!("ends a way it doesn't have: {end}"));
                    }
                    _ => {}
                }
            }
            // What's brought or met is checked on reporting, so it can't lead on to a stage.
            let at_report = |objectives: &[Objective]| {
                objectives
                    .iter()
                    .any(|o| matches!(o, Objective::Bring(..) | Objective::Meet(_)))
            };
            let leads_on = |next: &Option<Next>| matches!(next, Some(Next::Stage(_)));
            if at_report(&stage.objectives)
                && (leads_on(&stage.next) || stage.choices.iter().any(|c| leads_on(&c.next)))
            {
                return Err("asks for something on reporting, and leads on to a stage".into());
            }
            if stage
                .choices
                .iter()
                .any(|c| at_report(&c.objectives) && leads_on(&c.next))
            {
                return Err(
                    "offers a way that asks for something on reporting, and leads on".into(),
                );
            }
            if stage
                .choices
                .iter()
                .any(|c| blank(&c.text) || c.objectives.is_empty())
            {
                return Err("offers a way with no text or nothing to do".into());
            }
            for action in stage.on_begin.iter().chain(&stage.on_end) {
                if let Some(key) = action.fact()
                    && !["char.", "world.", "quest."]
                        .iter()
                        .any(|s| key.starts_with(s))
                {
                    return Err(format!(
                        "changes {key}: actions change char., world. or quest. facts"
                    ));
                }
            }
        }
        for (n, stage) in self.stages.iter().enumerate() {
            if blank(&stage.text) {
                return Err(format!("stage {} has no text", n + 1));
            }
            if stage.objectives.is_empty() && stage.choices.is_empty() {
                return Err(format!("stage {} has no objectives", n + 1));
            }
            if stage.all_objectives().any(|o| o.count() == 0) {
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
        for (name, ending) in &self.endings {
            if blank(&ending.journal) {
                return Err(format!("ends {name} with nothing for the journal"));
            }
        }
        for reward in self
            .rewards
            .iter()
            .chain(self.endings.values().flat_map(|e| &e.rewards))
        {
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
        for failure in &self.fails_if {
            match failure {
                Failure::Dies(mob) => names.mobs.push(full(mob)),
                Failure::Lost(item) => names.items.push(full(item)),
            }
        }
        names.mobs.extend(
            self.stages
                .iter()
                .flat_map(|s| &s.choices)
                .filter_map(|c| c.report_to.as_deref())
                .map(full),
        );
        names.items.extend(self.gives.iter().map(|id| full(id)));
        names
            .items
            .extend(self.rewards.iter().filter_map(|reward| match reward {
                Reward::Item(id) => Some(full(id)),
                Reward::Coin(_) | Reward::Set(..) | Reward::Merit(_) => None,
            }));
        names
            .items
            .extend(
                self.endings
                    .values()
                    .flat_map(|e| &e.rewards)
                    .filter_map(|r| match r {
                        Reward::Item(id) => Some(full(id)),
                        _ => None,
                    }),
            );
        for objective in self.stages.iter().flat_map(|s| s.all_objectives()) {
            match objective {
                Objective::Kill(MobMatch::Mob(mob) | MobMatch::Quest(mob), _) => {
                    names.mobs.push(full(mob))
                }
                Objective::Bring(ItemMatch::Item(item), _)
                | Objective::Obtain(ItemMatch::Item(item), _) => names.items.push(full(item)),
                Objective::Reach(room) => names.rooms.push(full(room)),
                Objective::Ask(mob, _) => names.mobs.push(full(mob)),
                Objective::Escort { mob, to } => {
                    names.mobs.push(full(mob));
                    names.rooms.push(full(to));
                }
                Objective::Give { item, to } => {
                    names.items.push(full(item));
                    names.mobs.push(full(to));
                }
                Objective::Kill(MobMatch::Tag(_), _)
                | Objective::Bring(ItemMatch::Tag(_), _)
                | Objective::Obtain(ItemMatch::Tag(_), _)
                | Objective::Meet(_) => {}
            }
        }
        names.mobs.extend(
            self.stages
                .iter()
                .flat_map(|s| s.topics.keys().map(|mob| full(mob))),
        );
        for action in self
            .stages
            .iter()
            .flat_map(|s| s.on_begin.iter().chain(&s.on_end))
        {
            match action {
                Action::Give(item) | Action::Take(item) => names.items.push(full(item)),
                Action::Say(mob, _) => names.mobs.push(full(mob)),
                Action::Spawn(mob, room) => {
                    names.mobs.push(full(mob));
                    names.rooms.push(full(room));
                }
                Action::Set(..) | Action::Add(..) | Action::Clear(_) | Action::Fail => {}
            }
        }
        names
    }
}

impl QuestDef {
    /// The room whose board it's posted on.
    pub(super) fn board(&self) -> Option<RoomKey> {
        match &self.posted {
            Posted::Board(room) => Some(RoomKey::parse(room, self.zone())),
        }
    }
}

impl QuestDef {
    /// Who it's reported to, by name: its `report_to`, or the guild's clerk.
    pub(super) fn reported_to(&self, mobs: &MobTemplates) -> String {
        self.named(&self.report_key(None), mobs)
    }

    /// A mob's name, by template: "Marin Harrow", "the hunter".
    pub(super) fn named(&self, key: &MobKey, mobs: &MobTemplates) -> String {
        mobs.0
            .get(key)
            .map_or_else(|| key.to_string(), |def| crate::combat::the(&def.name))
    }

    /// Whom it's reported to, the way `choice` if one was taken that says (`report_to` on the
    /// way), or else the quest's `report_to`, or else the guild's clerk.
    pub(super) fn report_key(&self, choice: Option<&Choice>) -> MobKey {
        match choice
            .and_then(|c| c.report_to.as_ref())
            .or(self.report_to.as_ref())
        {
            Some(mob) => MobKey::parse(mob, self.zone()),
            None => MobKey::parse(&guild().clerk, ""),
        }
    }

    /// Everyone `place` (a holder's stage) could be reported to: the quest's reporter, or for a
    /// stage offering choices, each open way's.
    pub(super) fn report_keys(&self, place: &str, chosen: i64) -> Vec<MobKey> {
        let Some((_, stage)) = self.stage_at(place) else {
            return vec![self.report_key(None)];
        };
        if stage.choices.is_empty() {
            return vec![self.report_key(None)];
        }
        stage
            .choices
            .iter()
            .enumerate()
            .filter(|(c, _)| chosen == 0 || chosen == *c as i64 + 1)
            .map(|(_, choice)| self.report_key(Some(choice)))
            .collect()
    }
}

impl MobMatch {
    /// Whether a mob of template `key` (`def`) is one of these, for a quest in `zone`. A quest's
    /// own mob is matched by who it came for, not by its template alone (`Deed::counts_for`).
    pub(super) fn matches(&self, key: &MobKey, def: Option<&MobDef>, zone: &str) -> bool {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone) == *key,
            MobMatch::Tag(tag) => def.is_some_and(|def| def.tags.contains(tag)),
            MobMatch::Quest(_) => false,
        }
    }

    /// As a fact's key: `sewers:sewer_rat`, or `tag.vermin`.
    pub(super) fn label(&self, zone: &str) -> String {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone).to_string(),
            MobMatch::Tag(tag) => format!("tag.{tag}"),
            MobMatch::Quest(id) => format!("quest.{}", MobKey::parse(id, zone)),
        }
    }
}

impl QuestDef {
    /// The stage someone's place says they're on (`record.stage`): by its id, or by number for
    /// a stage without one. With where it stands among the stages, from 0.
    pub(super) fn stage_at(&self, place: &str) -> Option<(usize, &Stage)> {
        self.stages
            .iter()
            .position(|s| s.id.as_deref() == Some(place))
            .or_else(|| place.parse::<usize>().ok()?.checked_sub(1))
            .and_then(|i| Some((i, self.stages.get(i)?)))
    }

    /// The place to keep for the stage at `i`: its id, or its number.
    pub(super) fn stage_key(&self, i: usize) -> String {
        match self.stages.get(i).and_then(|s| s.id.clone()) {
            Some(id) => id,
            None => (i + 1).to_string(),
        }
    }

    /// Where the stage at `i` leads once done, the way `choice` (a number from 1) if it offers
    /// choices: its `next` (a stage, or an ending to report for); without one, the stage after
    /// it, or for the last (or a way chosen), reporting.
    pub(super) fn lead(&self, i: usize, choice: Option<usize>) -> Lead<'_> {
        let Some(stage) = self.stages.get(i) else {
            return Lead::Report(None);
        };
        let chosen = choice
            .and_then(|c| stage.choices.get(c.checked_sub(1)?))
            .map(|c| &c.next);
        match chosen.unwrap_or(&stage.next) {
            Some(Next::Stage(id)) => self
                .stages
                .iter()
                .position(|s| s.id.as_deref() == Some(id))
                .map_or(Lead::Report(None), Lead::Stage),
            Some(Next::Way(Way::End(end))) => Lead::Report(Some(end)),
            None if chosen.is_none() && i + 1 < self.stages.len() => Lead::Stage(i + 1),
            None => Lead::Report(None),
        }
    }

    /// The stage the one at `i` leads to, if it leads to one rather than to reporting (choices
    /// aside: a stage offering them leads where the way taken does).
    pub(super) fn next_after(&self, i: usize) -> Option<usize> {
        match self.lead(i, None) {
            Lead::Stage(next) => Some(next),
            Lead::Report(_) => None,
        }
    }
}

impl QuestDef {
    /// What it pays, ending `ending` if it ended one way: that ending's rewards, and its merit (a
    /// contract's merit is paid only as an ending's `Merit`); otherwise its own rewards, and a
    /// contract's merit. The merit is `None` for a quest that isn't a contract.
    pub(super) fn pays(&self, ending: Option<&str>) -> (&[Reward], Option<i64>) {
        let merit_in = |rewards: &[Reward]| -> i64 {
            rewards
                .iter()
                .map(|r| match r {
                    Reward::Merit(n) => *n,
                    _ => 0,
                })
                .sum()
        };
        match ending.and_then(|e| self.endings.get(e)) {
            Some(ending) => (
                &ending.rewards,
                self.contract.as_ref().map(|_| merit_in(&ending.rewards)),
            ),
            None => (
                &self.rewards,
                self.contract
                    .as_ref()
                    .map(|c| c.merit + merit_in(&self.rewards)),
            ),
        }
    }
}

impl Stage {
    /// Every objective on it, its own and each choice's.
    pub(super) fn all_objectives(&self) -> impl Iterator<Item = &Objective> {
        self.objectives
            .iter()
            .chain(self.choices.iter().flat_map(|c| &c.objectives))
    }

    /// Its parts, each with the place its facts are kept under: its own objectives (needed
    /// whichever way) under the stage's place, then each choice's, by number from 1, under
    /// `<place>.<n>`.
    pub(super) fn parts<'a>(
        &'a self,
        place: &str,
    ) -> Vec<(Option<usize>, String, &'a [Objective])> {
        let mut parts = Vec::new();
        if !self.objectives.is_empty() {
            parts.push((None, place.to_string(), self.objectives.as_slice()));
        }
        for (c, choice) in self.choices.iter().enumerate() {
            parts.push((
                Some(c + 1),
                format!("{place}.{}", c + 1),
                choice.objectives.as_slice(),
            ));
        }
        parts
    }
}

/// The quest fact that keeps which way was taken through the stage kept as `place`.
pub(super) fn chose_key(place: &str) -> String {
    format!("{place}.chose")
}

impl Objective {
    /// The quest fact that counts it on stage `stage`, if it's counted as it happens:
    /// `1.kill.sewers:sewer_rat`, `1.obtain.riftwatch:guild_letter`, `1.obtain.tag.herb`.
    /// `Bring` is checked when reporting.
    pub(super) fn key(&self, stage: &str, zone: &str) -> Option<String> {
        match self {
            Objective::Kill(which, _) => Some(format!("{stage}.kill.{}", which.label(zone))),
            Objective::Obtain(ItemMatch::Item(id), _) => {
                Some(format!("{stage}.obtain.{}", ItemKey::parse(id, zone)))
            }
            Objective::Obtain(ItemMatch::Tag(tag), _) => Some(format!("{stage}.obtain.tag.{tag}")),
            Objective::Reach(room) => Some(format!("{stage}.reach.{}", RoomKey::parse(room, zone))),
            Objective::Ask(mob, topic) => Some(format!(
                "{stage}.ask.{}.{}",
                MobKey::parse(mob, zone),
                topic.to_lowercase()
            )),
            Objective::Escort { mob, to } => Some(format!(
                "{stage}.escort.{}.{}",
                MobKey::parse(mob, zone),
                RoomKey::parse(to, zone)
            )),
            Objective::Give { item, to } => Some(format!(
                "{stage}.give.{}.{}",
                ItemKey::parse(item, zone),
                MobKey::parse(to, zone)
            )),
            Objective::Bring(..) | Objective::Meet(_) => None,
        }
    }

    /// What it's after, as the journal names it: "sewer rat", "any vermin".
    pub(super) fn what(&self, zone: &str, lookup: &Lookup) -> String {
        match self {
            Objective::Kill(MobMatch::Mob(id) | MobMatch::Quest(id), _) => lookup
                .mobs
                .0
                .get(&MobKey::parse(id, zone))
                .map_or(id.as_str(), |def| without_article(&def.name))
                .to_string(),
            Objective::Bring(ItemMatch::Item(id), _)
            | Objective::Obtain(ItemMatch::Item(id), _) => lookup
                .items
                .0
                .get(&ItemKey::parse(id, zone))
                .map_or(id.as_str(), |def| without_article(&def.name))
                .to_string(),
            Objective::Kill(MobMatch::Tag(tag), _)
            | Objective::Bring(ItemMatch::Tag(tag), _)
            | Objective::Obtain(ItemMatch::Tag(tag), _) => format!("any {tag}"),
            Objective::Reach(room) => {
                let key = RoomKey::parse(room, zone);
                (lookup.places)(&key).unwrap_or_else(|| key.to_string())
            }
            Objective::Meet(requirement) => requirement.describe(&|id| id.to_string()),
            Objective::Give { item, to } => {
                let thing = lookup
                    .items
                    .0
                    .get(&ItemKey::parse(item, zone))
                    .map_or(item.as_str(), |def| without_article(&def.name))
                    .to_string();
                let whom = lookup
                    .mobs
                    .0
                    .get(&MobKey::parse(to, zone))
                    .map_or_else(|| to.clone(), |def| crate::combat::the(&def.name));
                format!("{thing} to {whom}")
            }
            Objective::Escort { mob, to } => {
                let name = lookup
                    .mobs
                    .0
                    .get(&MobKey::parse(mob, zone))
                    .map_or_else(|| mob.clone(), |def| crate::combat::the(&def.name));
                let to = RoomKey::parse(to, zone);
                let place = (lookup.places)(&to).unwrap_or_else(|| to.to_string());
                format!("{name} to {place}")
            }
            Objective::Ask(mob, topic) => {
                let name = lookup
                    .mobs
                    .0
                    .get(&MobKey::parse(mob, zone))
                    .map_or_else(|| mob.clone(), |def| crate::combat::the(&def.name));
                format!("{name} about {topic}")
            }
        }
    }

    /// How the journal shows it: what, and how far along (`have`, if it's counted).
    pub(super) fn progress(&self, zone: &str, have: Option<i64>, lookup: &Lookup) -> String {
        let what = self.what(zone, lookup);
        match self {
            Objective::Kill(_, need) | Objective::Obtain(_, need) => {
                let have = have.unwrap_or(0).min(i64::from(*need));
                let done = if have >= i64::from(*need) {
                    " (done)"
                } else {
                    ""
                };
                let verb = match self {
                    Objective::Kill(..) => "Kill",
                    _ => "Find",
                };
                format!("{verb} {what}: {have} of {need}{done}")
            }
            Objective::Bring(_, need) => format!("Bring {what}: {need}, counted when you report"),
            Objective::Reach(_) => match have.unwrap_or(0) {
                0 => format!("Reach {what}"),
                _ => format!("Reach {what} (done)"),
            },
            Objective::Meet(_) => format!("Needs {what}, checked when you report"),
            Objective::Ask(..) => match have.unwrap_or(0) {
                0 => format!("Ask {what}"),
                _ => format!("Ask {what} (done)"),
            },
            Objective::Escort { .. } => match have.unwrap_or(0) {
                0 => format!("Escort {what}"),
                _ => format!("Escort {what} (done)"),
            },
            Objective::Give { .. } => match have.unwrap_or(0) {
                0 => format!("Give {what}"),
                _ => format!("Give {what} (done)"),
            },
        }
    }

    pub(super) fn count(&self) -> u32 {
        match self {
            Objective::Kill(_, n) | Objective::Bring(_, n) | Objective::Obtain(_, n) => *n,
            Objective::Reach(_)
            | Objective::Meet(_)
            | Objective::Ask(..)
            | Objective::Escort { .. }
            | Objective::Give { .. } => 1,
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

/// What a quest file written in the game starts with: comments are lost when it's rewritten.
const QUEST_FILE_HEADER: &str = "\
// Quest file. Ids are this zone's own unless written zone:id.
// Rewritten by the GMs' quest create and quest edit, so comments other than this header are lost.
";

impl QuestDef {
    /// The quest as its file is written.
    pub(super) fn file_contents(&self) -> Result<String, ron::Error> {
        // Apostrophes left as they are, and a line with quotes in it as a raw string, `r#"..."#`,
        // so the file reads as it was written.
        let pretty = ron::ser::PrettyConfig::new()
            .struct_names(false)
            .escape_strings(false);
        let body = ron_options().to_string_pretty(self, pretty)?;
        Ok(format!("{QUEST_FILE_HEADER}{body}\n"))
    }

    /// Where its file is, under the asset folder: `quests/<zone>/<quest>.quest.ron`.
    pub(super) fn file_path(&self) -> std::path::PathBuf {
        let (zone, quest) = self.id.split_once(':').unwrap_or(("", &self.id));
        bevy::asset::io::file::FileAssetReader::get_base_path()
            .join("assets")
            .join(QUESTS_PATH)
            .join(zone)
            .join(format!("{quest}.{QUEST_EXTENSION}"))
    }

    /// Write it to its file, making the zone's folder if it's the first. The file watcher reloads
    /// it, which is harmless as `Quests` already has it.
    pub(super) fn save(&self) -> Result<(), String> {
        let path = self.file_path();
        let contents = self.file_contents().map_err(|err| err.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
        }
        std::fs::write(&path, contents).map_err(|err| err.to_string())
    }
}

/// Every quest, by id.
#[derive(Resource, Default)]
pub struct Quests(pub HashMap<String, QuestDef>);

#[derive(Resource)]
struct QuestsHandle(#[allow(dead_code)] Handle<LoadedFolder>);

pub(super) fn load_quests(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(QuestsHandle(assets.load_folder(QUESTS_PATH)));
}

/// Rebuild the quest table whenever a quest file loads, changes or goes away.
pub(super) fn sync_quests(
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
pub(super) struct QuestLoader;

#[derive(Debug, Error)]
pub(super) enum QuestLoaderError {
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
    use crate::quests::testing::rats;

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
    fn every_quest_file_reads_back_as_it_is_written() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(QUESTS_PATH);
        for folder in std::fs::read_dir(root).unwrap() {
            for entry in std::fs::read_dir(folder.unwrap().path()).unwrap() {
                let path = entry.unwrap().path();
                let quest: QuestDef = ron_options()
                    .from_str(&std::fs::read_to_string(&path).unwrap())
                    .unwrap();
                let written = quest.file_contents().unwrap();
                let back: QuestDef = ron_options()
                    .from_str(&written)
                    .unwrap_or_else(|err| panic!("{}: {err}\n{written}", path.display()));
                assert_eq!(back, quest, "{}", path.display());
            }
        }
    }

    #[test]
    fn quests_are_named_for_their_zone_and_file() {
        let path = Path::new("quests/riftwatch/rats_in_the_drains.quest.ron");
        assert_eq!(quest_id(path), "riftwatch:rats_in_the_drains");
    }

    #[test]
    fn stages_go_by_id_or_number_and_lead_where_they_say() {
        let stage = |id: Option<&str>, next: Option<&str>| Stage {
            id: id.map(String::from),
            next: next.map(|n| Next::Stage(n.into())),
            text: "Do it.".into(),
            objectives: vec![Objective::Reach("hall".into())],
            ..Default::default()
        };
        let quest = QuestDef {
            stages: vec![
                stage(Some("start"), Some("end")),
                stage(None, None),
                stage(Some("end"), None),
            ],
            ..rats()
        };
        assert!(quest.check().is_ok());
        assert_eq!(quest.stage_at("start").map(|(i, _)| i), Some(0));
        assert_eq!(
            quest.stage_at("2").map(|(i, _)| i),
            Some(1),
            "no id: its number"
        );
        assert_eq!(
            quest.stage_at("1").map(|(i, _)| i),
            Some(0),
            "a place kept by number"
        );
        assert!(quest.stage_at("gone").is_none());
        assert_eq!(
            (0..3).map(|i| quest.stage_key(i)).collect::<Vec<_>>(),
            ["start", "2", "end"]
        );
        assert_eq!(
            quest.next_after(0),
            Some(2),
            "its next, past the one after it"
        );
        assert_eq!(quest.next_after(1), Some(2), "the one after it");
        assert_eq!(quest.next_after(2), None, "the last: time to report");

        let broken = |stages| QuestDef { stages, ..rats() }.check().is_err();
        assert!(
            broken(vec![stage(Some("a"), Some("nowhere"))]),
            "leads nowhere"
        );
        assert!(
            broken(vec![stage(Some("a"), None), stage(Some("a"), None)]),
            "two called a"
        );
        let mut sets = stage(None, None);
        sets.on_end = vec![Action::Set("npc.mood".into(), crate::facts::Value::Int(1))];
        assert!(
            broken(vec![sets]),
            "actions change char., world. or quest. facts"
        );
        let mut asks = stage(None, None);
        asks.objectives = vec![Objective::Ask("clerk".into(), "crystals".into())];
        assert!(broken(vec![asks]), "asks about what no one answers");
    }

    #[test]
    fn choices_and_endings_read_as_the_design_writes_them() {
        let quest: QuestDef = ron_options()
            .from_str(
                r#"(
                name: "Crystals",
                summary: "Crystals.",
                description: "Crystals, at length.",
                posted: Board("contract_hall"),
                contract: Some((rank: "copper", merit: 30)),
                availability: Open,
                repeat: Once,
                stages: [
                    (id: "start", journal: "Ask around.", objectives: [Reach("alley")], next: "choice"),
                    (
                        id: "choice",
                        journal: "Return them, or sell them.",
                        choices: [
                            (text: "Return them.", objectives: [Bring(Item("crystals"), 1)], next: End("returned")),
                            (text: "Sell them.", objectives: [Reach("fence")], next: End("sold")),
                        ],
                    ),
                ],
                endings: {
                    "returned": (journal: "You returned them.", rewards: [Coin(300), Merit(30)]),
                    "sold": (journal: "You sold them.", rewards: [Coin(900), Set("char.sold_out", true)]),
                },
            )"#,
            )
            .unwrap();
        assert!(quest.check().is_ok(), "{:?}", quest.check());
        assert_eq!(quest.lead(0, None), Lead::Stage(1));
        assert_eq!(quest.lead(1, Some(1)), Lead::Report(Some("returned")));
        assert_eq!(quest.lead(1, Some(2)), Lead::Report(Some("sold")));
        // An ending pays its own, and a contract's merit only as it says.
        assert_eq!(quest.pays(Some("returned")).1, Some(30));
        assert_eq!(
            quest.pays(Some("sold")).1,
            Some(0),
            "selling out pays no merit"
        );
        assert_eq!(quest.pays(None).1, Some(30));

        let mut bad = quest.clone();
        bad.stages[1].choices[1].next = Some(Next::Way(Way::End("lost".into())));
        assert!(bad.check().is_err(), "an ending it doesn't have");
        let mut stuck = quest.clone();
        stuck.stages[0].objectives = vec![Objective::Bring(ItemMatch::Item("x".into()), 1)];
        assert!(
            stuck.check().is_err(),
            "what's brought on reporting can't lead on"
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
            gives: Vec::new(),
            endings: Default::default(),
            fails_if: Vec::new(),
            draft: false,
            rewards: Vec::new(),
        };
        let stage = |objectives| Stage {
            text: "Do it.".into(),
            objectives,
            ..Default::default()
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
