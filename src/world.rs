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

/// Asset folder holding one `.ron` file per zone.
pub const ZONES_PATH: &str = "zones";

/// Where new characters (and anyone whose room disappears) end up.
pub const START_ROOM: &str = "town:square";

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ZoneDef>()
            .register_asset_loader(ZoneDefLoader)
            .init_resource::<RoomIndex>()
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
    pub rooms: BTreeMap<String, RoomDef>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomDef {
    pub name: String,
    pub description: String,
    /// Direction name -> destination: a room id in this zone, or `zone:room` for another zone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exits: BTreeMap<String, String>,
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

/// Direction name -> destination room.
#[derive(Component, Default, PartialEq)]
pub struct Exits(pub BTreeMap<String, Entity>);

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
    mut rooms: Query<(&mut Room, &mut Exits)>,
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
        match index.0.get(key).and_then(|&e| rooms.get_mut(e).ok().map(|r| (e, r))) {
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
                    "\r\nThe world dissolves around you, and you find yourself somewhere familiar.\r\n> ",
                );
            }
        }
        commands.entity(entity).despawn();
        index.0.remove(&key);
    }

    // Resolve exits now that every room id maps to an entity. Newly spawned rooms don't have
    // their components yet, so their exits are inserted through commands instead.
    for (key, def) in &defs {
        let mut exits = BTreeMap::new();
        for (dir, target) in &def.exits {
            match index.0.get(&RoomKey::parse(target, &key.zone)) {
                Some(&to) => {
                    exits.insert(dir.clone(), to);
                }
                None => warn!("room '{key}' exit '{dir}' leads to unknown room '{target}'"),
            }
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
            conn.send_raw("\r\nYour surroundings shift. (type 'look')\r\n> ");
        }
    }

    // A single edit can trigger several syncs (the in-memory edit, then the file watcher), so only
    // log the ones that changed something.
    if first_load {
        info!("loaded {} rooms from {} zones", defs.len(), zones.len());
    } else if !touched.is_empty() || index.0.len() != room_count {
        info!("world updated: {} rooms in {} zones", defs.len(), zones.len());
    }
}
