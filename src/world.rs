//! The game world: rooms are entities, and players point at the room they're in.
//!
//! Rooms are defined in zone files, `assets/zones/<zone>.ron`, each loaded as a Bevy asset. Room
//! ids only need to be unique within their zone; elsewhere a room is referred to by its full id,
//! `zone:room`. When a zone file is added, changed, or removed on disk (or edited in game by a
//! builder), the room entities are updated in place: existing rooms keep their entity (so players
//! stay put), new rooms are spawned, and removed rooms are despawned after moving anyone inside
//! them to the starting room.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;

use bevy::asset::io::Reader;
use bevy::asset::io::file::FileAssetReader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::net::Connection;
use crate::targeting::Keywords;

/// Asset folder holding one `.ron` file per zone.
pub const ZONES_PATH: &str = "zones";

/// Where new characters (and anyone whose room disappears) end up.
pub const START_ROOM: &str = "riftwatch:market";

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ZoneDef>()
            .register_asset_loader(ZoneDefLoader)
            .init_resource::<RoomIndex>()
            .init_resource::<DoorIndex>()
            .add_systems(Startup, load_zones)
            .add_systems(Update, sync_world);
    }
}

/// The contents of a zone `.ron` file.
#[derive(Asset, TypePath, Serialize, Deserialize, Debug, Clone)]
pub struct ZoneDef {
    /// Taken from the file name, e.g. `town` for `zones/town.ron`.
    #[serde(skip)]
    pub id: String,
    /// Asset path of the file this zone was loaded from, used when saving builder edits.
    #[serde(skip)]
    pub path: PathBuf,
    pub name: String,
    /// Doors on this zone's exits, by id. An exit names its door; both sides of a doorway name
    /// the same one, so they share its state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub doors: BTreeMap<String, DoorDef>,
    /// Item templates, by id. Items elsewhere refer to them as `zone:item`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, ItemDef>,
    pub rooms: BTreeMap<String, RoomDef>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomDef {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exits: BTreeMap<String, ExitDef>,
}

/// Where an exit leads: a room id in this zone, or `zone:room` for another zone. Plain exits are
/// written as just the target (`"north": "gate"`); exits with a door name it too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum ExitDef {
    To(String),
    Door { to: String, door: String },
}

impl ExitDef {
    pub fn to(&self) -> &str {
        match self {
            ExitDef::To(to) | ExitDef::Door { to, .. } => to,
        }
    }

    /// The door's id: local to the zone, or `zone:door`.
    pub fn door(&self) -> Option<&str> {
        match self {
            ExitDef::To(_) => None,
            ExitDef::Door { door, .. } => Some(door),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct DoorDef {
    pub name: String,
    /// Words players can call it by. When empty, the words of its name are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The state it starts in, and returns to on a reset.
    #[serde(default)]
    pub state: DoorState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<LockDef>,
}

impl DoorDef {
    pub fn keywords(&self) -> Keywords {
        if !self.keywords.is_empty() {
            return Keywords(self.keywords.iter().map(|k| k.to_lowercase()).collect());
        }
        let mut keywords = Keywords::from_name(&self.name);
        keywords
            .0
            .retain(|w| !matches!(w.as_str(), "a" | "an" | "the"));
        keywords
    }
}

/// An item template.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ItemDef {
    pub name: String,
    /// Words players can call it by. When empty, the words of its name are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
}

fn default_weight() -> u32 {
    1
}

impl ItemDef {
    pub fn keywords(&self) -> Keywords {
        if !self.keywords.is_empty() {
            return Keywords(self.keywords.iter().map(|k| k.to_lowercase()).collect());
        }
        Keywords::from_name(without_article(&self.name))
    }
}

/// An item template's full id: its zone and its id there. Written `zone:item`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemKey {
    pub zone: String,
    pub item: String,
}

impl ItemKey {
    /// Parse `zone:item`, or a bare `item` relative to `current_zone`.
    pub fn parse(s: &str, current_zone: &str) -> Self {
        let RoomKey { zone, room } = RoomKey::parse(s, current_zone);
        Self { zone, item: room }
    }
}

impl fmt::Display for ItemKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.zone, self.item)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DoorState {
    Open,
    #[default]
    Closed,
    Locked,
}

/// A lock on a door (and, later, on containers).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct LockDef {
    /// The key's item template. Keys arrive with items; until then only builders can use the lock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Lockpicking difficulty; `None` means it can't be picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick: Option<u32>,
    /// Sturdiness against `bash`; `None` means it can't be forced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bash: Option<u32>,
}

const ZONE_FILE_HEADER: &str = "\
// Zone file. Room ids are local to this zone; link to another zone with \"zone:room\".
// This file is rewritten by in-game builder commands, so comments other than this header are lost.
";

impl ZoneDef {
    /// A new zone with a single starting room, `entrance`, to be written with `create`.
    pub fn new(id: &str, name: &str) -> Self {
        let entrance = RoomDef {
            name: name.into(),
            description: "An unfinished room. (Use 'desc' to describe it.)".into(),
            exits: BTreeMap::new(),
        };
        Self {
            id: id.into(),
            path: PathBuf::from(ZONES_PATH).join(format!("{id}.ron")),
            name: name.into(),
            doors: BTreeMap::new(),
            items: BTreeMap::new(),
            rooms: BTreeMap::from([(ZONE_ENTRANCE.into(), entrance)]),
        }
    }

    /// Write this zone back to its file. The file watcher will pick up the change, which is
    /// harmless since the in-memory asset already matches.
    pub fn save(&self) -> Result<(), SaveError> {
        std::fs::write(self.file_path(), self.file_contents()?)?;
        Ok(())
    }

    /// Write a new zone file, failing if one already exists. The file watcher then loads it like
    /// any other zone file.
    pub fn create(&self) -> Result<(), SaveError> {
        use std::io::Write;
        let contents = self.file_contents()?;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.file_path())?
            .write_all(contents.as_bytes())?;
        Ok(())
    }

    fn file_path(&self) -> PathBuf {
        FileAssetReader::get_base_path()
            .join("assets")
            .join(&self.path)
    }

    fn file_contents(&self) -> Result<String, SaveError> {
        let pretty = ron::ser::PrettyConfig::new().struct_names(false);
        let body = ron::ser::to_string_pretty(self, pretty)?;
        Ok(format!("{ZONE_FILE_HEADER}{body}\n"))
    }
}

/// The room every zone created in game starts with.
pub const ZONE_ENTRANCE: &str = "entrance";

#[derive(Debug, Error)]
pub enum SaveError {
    #[error("could not write zone file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not serialize zone: {0}")]
    Ron(#[from] ron::Error),
}

#[derive(TypePath)]
struct ZoneDefLoader;

#[derive(Debug, Error)]
enum ZoneDefLoaderError {
    #[error("could not read zone file: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse zone file: {0}")]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for ZoneDefLoader {
    type Asset = ZoneDef;
    type Settings = ();
    type Error = ZoneDefLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<ZoneDef, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut zone: ZoneDef = ron::de::from_bytes(&bytes)?;
        let path = load_context.path().path();
        zone.id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        zone.path = path.to_path_buf();
        Ok(zone)
    }

    fn extensions(&self) -> &[&str] {
        &["ron"]
    }
}

/// A room's full id: which zone it belongs to and its id within that zone.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoomKey {
    pub zone: String,
    pub room: String,
}

impl RoomKey {
    /// Parse `zone:room`, or a bare `room` relative to `current_zone`.
    pub fn parse(s: &str, current_zone: &str) -> Self {
        match s.split_once(':') {
            Some((zone, room)) => Self {
                zone: zone.into(),
                room: room.into(),
            },
            None => Self {
                zone: current_zone.into(),
                room: s.into(),
            },
        }
    }
}

impl fmt::Display for RoomKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.zone, self.room)
    }
}

#[derive(Component)]
pub struct Room {
    pub name: String,
    pub description: String,
}

/// Direction name -> where that exit leads.
#[derive(Component, Default, PartialEq)]
pub struct Exits(pub BTreeMap<String, Exit>);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exit {
    pub to: Entity,
    pub door: Option<Entity>,
}

/// A door's full id: its zone and its id there. Written `zone:door`.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DoorKey {
    pub zone: String,
    pub door: String,
}

impl DoorKey {
    /// Parse `zone:door`, or a bare `door` relative to `current_zone`.
    pub fn parse(s: &str, current_zone: &str) -> Self {
        let RoomKey { zone, room } = RoomKey::parse(s, current_zone);
        Self { zone, door: room }
    }
}

impl fmt::Display for DoorKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.zone, self.door)
    }
}

/// A door's live state. Lives on its own entity, shared by the exits on both sides.
#[derive(Component, Debug)]
pub struct Door {
    pub name: String,
    pub state: DoorState,
    /// The state it starts in, from the zone file.
    pub default: DoorState,
    pub lock: Option<LockDef>,
}

impl Door {
    pub fn is_open(&self) -> bool {
        self.state == DoorState::Open
    }

    /// "the heavy oak door", for the middle of a sentence.
    pub fn the(&self) -> String {
        format!("the {}", without_article(&self.name))
    }

    /// "The heavy oak door", to start a sentence.
    pub fn the_capitalized(&self) -> String {
        let the = self.the();
        let mut chars = the.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

/// A name without its leading "a", "an" or "the": "a heavy oak door" -> "heavy oak door".
pub fn without_article(name: &str) -> &str {
    let name = name.trim();
    ["a ", "an ", "the "]
        .iter()
        .find_map(|article| {
            name.to_lowercase()
                .starts_with(article)
                .then(|| &name[article.len()..])
        })
        .unwrap_or(name)
}

/// Full door id -> door entity, for every door currently spawned.
#[derive(Resource, Default)]
pub struct DoorIndex(pub HashMap<DoorKey, Entity>);

/// Full room id -> room entity, for every room currently spawned.
#[derive(Resource, Default)]
pub struct RoomIndex(pub HashMap<RoomKey, Entity>);

impl RoomIndex {
    pub fn start(&self) -> Option<Entity> {
        self.0.get(&RoomKey::parse(START_ROOM, "")).copied()
    }
}

/// Keeps the zone assets loaded (and therefore watched for changes).
#[derive(Resource)]
struct ZonesHandle(Handle<LoadedFolder>);

/// A logged-in character. Lives on the same entity as its `Connection`.
#[derive(Component)]
pub struct Player {
    pub name: String,
    pub character_id: i64,
    pub account: String,
    pub role: crate::db::Role,
}

/// Which room an entity is currently in.
#[derive(Component)]
pub struct InRoom(pub Entity);

fn load_zones(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(ZonesHandle(assets.load_folder(ZONES_PATH)));
}

/// Spawn, update and despawn door entities to match the zones. A door that already exists keeps
/// its current state (open, closed, locked) when its zone file changes, unless its default state
/// changed, as when a builder adds a lock; new doors start in their default state.
fn sync_doors(
    commands: &mut Commands,
    zones: &Assets<ZoneDef>,
    door_index: &mut DoorIndex,
    doors: &mut Query<&mut Door>,
) {
    let mut defs: HashMap<DoorKey, &DoorDef> = HashMap::new();
    for (_, zone) in zones.iter() {
        for (id, def) in &zone.doors {
            let key = DoorKey {
                zone: zone.id.clone(),
                door: id.clone(),
            };
            defs.insert(key, def);
        }
    }

    for (key, def) in &defs {
        // A door can't stay locked once its lock is removed.
        let fit = |state: DoorState| match (state, &def.lock) {
            (DoorState::Locked, None) => DoorState::Closed,
            (state, _) => state,
        };
        match door_index
            .0
            .get(key)
            .and_then(|&e| doors.get_mut(e).ok().map(|d| (e, d)))
        {
            Some((entity, mut door)) => {
                door.name = def.name.clone();
                door.lock = def.lock.clone();
                let default = fit(def.state);
                if door.default != default {
                    door.default = default;
                    door.state = default;
                }
                door.state = fit(door.state);
                commands.entity(entity).insert(def.keywords());
            }
            None => {
                let entity = commands
                    .spawn((
                        Door {
                            name: def.name.clone(),
                            state: fit(def.state),
                            default: fit(def.state),
                            lock: def.lock.clone(),
                        },
                        key.clone(),
                        def.keywords(),
                    ))
                    .id();
                door_index.0.insert(key.clone(), entity);
            }
        }
    }

    door_index.0.retain(|key, &mut entity| {
        let keep = defs.contains_key(key);
        if !keep {
            commands.entity(entity).despawn();
        }
        keep
    });
}

/// Apply every loaded zone to the room entities whenever any zone loads, changes, or goes away.
#[allow(clippy::too_many_arguments)]
fn sync_world(
    mut commands: Commands,
    mut zone_events: MessageReader<AssetEvent<ZoneDef>>,
    mut folder_events: MessageReader<AssetEvent<LoadedFolder>>,
    mut dirty: Local<bool>,
    handle: Res<ZonesHandle>,
    assets: Res<AssetServer>,
    zones: Res<Assets<ZoneDef>>,
    mut index: ResMut<RoomIndex>,
    mut door_index: ResMut<DoorIndex>,
    mut rooms: Query<(&mut Room, &mut Exits)>,
    mut doors: Query<&mut Door>,
    mut players: Query<(&mut Connection, &mut InRoom), With<Player>>,
) {
    if zone_events.read().count() > 0 || folder_events.read().count() > 0 {
        *dirty = true;
    }
    // Wait until every zone file has loaded, so cross-zone exits resolve on startup.
    if !*dirty || !assets.is_loaded_with_dependencies(&handle.0) {
        return;
    }
    *dirty = false;

    let mut defs: HashMap<RoomKey, &RoomDef> = HashMap::new();
    for (_, zone) in zones.iter() {
        for (room, def) in &zone.rooms {
            defs.insert(
                RoomKey {
                    zone: zone.id.clone(),
                    room: room.clone(),
                },
                def,
            );
        }
    }

    let start_key = RoomKey::parse(START_ROOM, "");
    if !defs.contains_key(&start_key) {
        error!("start room '{START_ROOM}' does not exist in any zone; keeping the current world");
        return;
    }

    let first_load = index.0.is_empty();
    let room_count = index.0.len();
    let mut touched = Vec::new();

    // Spawn new rooms and update existing ones. Exits are filled in once every room has an entity.
    for (key, def) in &defs {
        match index
            .0
            .get(key)
            .and_then(|&e| rooms.get_mut(e).ok().map(|r| (e, r)))
        {
            Some((entity, (mut room, _))) => {
                if room.name != def.name || room.description != def.description {
                    room.name = def.name.clone();
                    room.description = def.description.clone();
                    touched.push(entity);
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        Room {
                            name: def.name.clone(),
                            description: def.description.clone(),
                        },
                        key.clone(),
                        Exits::default(),
                    ))
                    .id();
                index.0.insert(key.clone(), entity);
            }
        }
    }

    let start = index.0[&start_key];

    // Despawn rooms that were removed, evacuating anyone inside.
    let removed: Vec<(RoomKey, Entity)> = index
        .0
        .iter()
        .filter(|(key, _)| !defs.contains_key(*key))
        .map(|(key, &e)| (key.clone(), e))
        .collect();
    for (key, entity) in removed {
        for (mut conn, mut in_room) in &mut players {
            if in_room.0 == entity {
                in_room.0 = start;
                conn.send_raw(
                    "The world dissolves around you, and you find yourself somewhere familiar.\r\n",
                );
            }
        }
        commands.entity(entity).despawn();
        index.0.remove(&key);
    }

    sync_doors(&mut commands, &zones, &mut door_index, &mut doors);

    // Resolve exits now that every room and door id maps to an entity. Newly spawned rooms don't
    // have their components yet, so their exits are inserted through commands instead.
    for (key, def) in &defs {
        let mut exits = BTreeMap::new();
        for (dir, exit) in &def.exits {
            let Some(&to) = index.0.get(&RoomKey::parse(exit.to(), &key.zone)) else {
                warn!(
                    "room '{key}' exit '{dir}' leads to unknown room '{}'",
                    exit.to()
                );
                continue;
            };
            let door = exit.door().and_then(|door| {
                let found = door_index.0.get(&DoorKey::parse(door, &key.zone)).copied();
                if found.is_none() {
                    warn!("room '{key}' exit '{dir}' names unknown door '{door}'");
                }
                found
            });
            exits.insert(dir.clone(), Exit { to, door });
        }
        let exits = Exits(exits);
        let entity = index.0[key];
        match rooms.get_mut(entity) {
            Ok((_, mut current)) => {
                if *current != exits {
                    *current = exits;
                    touched.push(entity);
                }
            }
            Err(_) => {
                commands.entity(entity).insert(exits);
            }
        }
    }

    // Let players know when the room they're standing in changed.
    for (mut conn, in_room) in &mut players {
        if touched.contains(&in_room.0) {
            conn.send("Your surroundings shift. (type 'look')");
        }
    }

    // A single edit can trigger several syncs (the in-memory edit, then the file watcher), so only
    // log the ones that changed something.
    if first_load {
        info!("loaded {} rooms from {} zones", defs.len(), zones.len());
    } else if !touched.is_empty() || index.0.len() != room_count {
        info!(
            "world updated: {} rooms in {} zones",
            defs.len(),
            zones.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_exits_stay_plain_in_zone_files() {
        let plain = ExitDef::To("gate".into());
        assert_eq!(ron::to_string(&plain).unwrap(), "\"gate\"");
        let door = ExitDef::Door {
            to: "cellar".into(),
            door: "cellar_door".into(),
        };
        let written = ron::to_string(&door).unwrap();
        assert_eq!(ron::from_str::<ExitDef>(&written).unwrap(), door);
        assert_eq!(ron::from_str::<ExitDef>("\"gate\"").unwrap(), plain);
    }

    #[test]
    fn door_names_read_naturally() {
        let door = |name: &str| Door {
            name: name.into(),
            state: DoorState::Closed,
            default: DoorState::Closed,
            lock: None,
        };
        assert_eq!(door("a heavy oak door").the(), "the heavy oak door");
        assert_eq!(door("An iron gate").the_capitalized(), "The iron gate");
        assert_eq!(door("trapdoor").the(), "the trapdoor");
        assert_eq!(without_article("a trapdoor"), "trapdoor");
    }

    /// Every zone file in `assets/zones` parses, every exit leads to a real room, every door an
    /// exit names exists, every lock's key is a real item, and the start room exists.
    #[test]
    fn zone_files_are_consistent() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/zones");
        let mut zones = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "ron") {
                let text = std::fs::read_to_string(&path).unwrap();
                let mut zone: ZoneDef = ron::from_str(&text)
                    .unwrap_or_else(|err| panic!("{} doesn't parse: {err}", path.display()));
                zone.id = path.file_stem().unwrap().to_string_lossy().into_owned();
                zones.push(zone);
            }
        }
        let room_exists = |key: &RoomKey| {
            zones
                .iter()
                .any(|z| z.id == key.zone && z.rooms.contains_key(&key.room))
        };
        let item_exists = |key: &ItemKey| {
            zones
                .iter()
                .any(|z| z.id == key.zone && z.items.contains_key(&key.item))
        };
        let door_exists = |key: &DoorKey| {
            zones
                .iter()
                .any(|z| z.id == key.zone && z.doors.contains_key(&key.door))
        };

        let mut problems = Vec::new();
        for zone in &zones {
            for (door_id, door) in &zone.doors {
                if let Some(key) = door.lock.as_ref().and_then(|lock| lock.key.as_ref())
                    && !item_exists(&ItemKey::parse(key, &zone.id))
                {
                    problems.push(format!("door {}:{door_id}: no key item '{key}'", zone.id));
                }
            }
            for (room_id, room) in &zone.rooms {
                for (dir, exit) in &room.exits {
                    let here = format!("{}:{room_id} {dir}", zone.id);
                    if !room_exists(&RoomKey::parse(exit.to(), &zone.id)) {
                        problems.push(format!("{here}: no room '{}'", exit.to()));
                    }
                    if let Some(door) = exit.door()
                        && !door_exists(&DoorKey::parse(door, &zone.id))
                    {
                        problems.push(format!("{here}: no door '{door}'"));
                    }
                }
            }
        }
        if !room_exists(&RoomKey::parse(START_ROOM, "")) {
            problems.push(format!("start room '{START_ROOM}' doesn't exist"));
        }
        assert!(
            problems.is_empty(),
            "zone problems:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn door_keywords_skip_articles() {
        let def = DoorDef {
            name: "a heavy oak door".into(),
            keywords: Vec::new(),
            state: DoorState::Closed,
            lock: None,
        };
        assert_eq!(def.keywords().0, ["heavy", "oak", "door"]);
    }
}
