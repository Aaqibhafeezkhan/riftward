//! Quests: written as data, one RON file each, in a folder named for the zone they belong to
//! (`assets/quests/riftwatch/rats_in_the_drains.quest.ron` is `riftwatch:rats_in_the_drains`).
//! They're loaded, checked and hot-reloaded here, and read from the board where they're posted;
//! taking them, progress and rewards come later (see `docs/quests.md`).

use std::collections::HashMap;
use std::path::Path;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::facts::Facts;
use crate::guild::{Membership, guild};
use crate::items::ItemTemplates;
use crate::money;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::requirements::{ItemMatch, Requirement, Subject};
use crate::world::{Every, ItemKey, RoomKey, ron_options};

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
            .add_systems(Startup, load_quests)
            .add_systems(Update, sync_quests)
            .add_command(
                CommandSpec::new("board", Category::Interaction)
                    .shortcuts(&["contracts"])
                    .usage("board [<n>]")
                    .summary("read the notices posted on a quest board here, or one in full"),
                board,
            );
    }
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
                Reward::Coin(_) => None,
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
                Reward::Item(_) => 0,
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
                Reward::Coin(_) => None,
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
    quests: Res<Quests>,
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
    let notices = posted(here, member.map(|m| m.rank), &quests, &story);
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
/// non-member), in the order it's listed.
fn posted<'a>(
    here: &RoomKey,
    rank: Option<usize>,
    quests: &'a Quests,
    story: &Facts,
) -> Vec<&'a QuestDef> {
    let highest = rank.map_or(0, |r| r + 1);
    let mut notices: Vec<&QuestDef> = quests
        .0
        .values()
        .filter(|q| q.board().as_ref() == Some(here))
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

impl QuestDef {
    /// The room whose board it's posted on.
    fn board(&self) -> Option<RoomKey> {
        match &self.posted {
            Posted::Board(room) => Some(RoomKey::parse(room, self.zone())),
        }
    }
}

impl Objective {
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
            posted(&hall, rank, &quests, story)
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
