//! What a quest is, as written in its file: the format, checking it, and loading every quest
//! into `Quests`, hot-reloaded.

use std::collections::HashMap;
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
    /// Items handed to whoever takes it from the board (the letter to carry), taken back if
    /// it's given up or fails.
    #[serde(default)]
    pub gives: Vec<String>,
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
    /// So many of these items come by (picked up, given, bought) while on the stage: nothing is
    /// handed over. A clue; the letter you're to carry.
    Obtain(ItemMatch, u32),
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
        names.items.extend(self.gives.iter().map(|id| full(id)));
        names
            .items
            .extend(self.rewards.iter().filter_map(|reward| match reward {
                Reward::Item(id) => Some(full(id)),
                Reward::Coin(_) | Reward::Set(..) => None,
            }));
        for objective in self.stages.iter().flat_map(|s| &s.objectives) {
            match objective {
                Objective::Kill(MobMatch::Mob(mob), _) => names.mobs.push(full(mob)),
                Objective::Bring(ItemMatch::Item(item), _)
                | Objective::Obtain(ItemMatch::Item(item), _) => names.items.push(full(item)),
                Objective::Kill(MobMatch::Tag(_), _)
                | Objective::Bring(ItemMatch::Tag(_), _)
                | Objective::Obtain(ItemMatch::Tag(_), _) => {}
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
    pub(super) fn matches(&self, key: &MobKey, def: Option<&MobDef>, zone: &str) -> bool {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone) == *key,
            MobMatch::Tag(tag) => def.is_some_and(|def| def.tags.contains(tag)),
        }
    }

    /// As a fact's key: `sewers:sewer_rat`, or `tag.vermin`.
    pub(super) fn label(&self, zone: &str) -> String {
        match self {
            MobMatch::Mob(id) => MobKey::parse(id, zone).to_string(),
            MobMatch::Tag(tag) => format!("tag.{tag}"),
        }
    }
}

impl Objective {
    /// The quest fact that counts it on stage `stage`, if it's counted as it happens:
    /// `1.kill.sewers:sewer_rat`, `1.obtain.riftwatch:guild_letter`, `1.obtain.tag.herb`.
    /// `Bring` is checked when reporting.
    pub(super) fn key(&self, stage: usize, zone: &str) -> Option<String> {
        match self {
            Objective::Kill(which, _) => Some(format!("{stage}.kill.{}", which.label(zone))),
            Objective::Obtain(ItemMatch::Item(id), _) => {
                Some(format!("{stage}.obtain.{}", ItemKey::parse(id, zone)))
            }
            Objective::Obtain(ItemMatch::Tag(tag), _) => Some(format!("{stage}.obtain.tag.{tag}")),
            Objective::Bring(..) => None,
        }
    }

    /// What it's after, as the journal names it: "sewer rat", "any vermin".
    pub(super) fn what(&self, zone: &str, lookup: &Lookup) -> String {
        match self {
            Objective::Kill(MobMatch::Mob(id), _) => lookup
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
        }
    }

    pub(super) fn count(&self) -> u32 {
        match self {
            Objective::Kill(_, n) | Objective::Bring(_, n) | Objective::Obtain(_, n) => *n,
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
