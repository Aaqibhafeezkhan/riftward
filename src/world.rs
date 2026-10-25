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

use crate::attributes::Attribute;
use crate::net::Connection;
use crate::targeting::Keywords;
use crate::world_events::{When, WorldEventDef};

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
    /// Mob templates, by id. Spawns elsewhere refer to them as `zone:mob`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mobs: BTreeMap<String, MobDef>,
    /// What this zone keeps populated, by id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spawns: BTreeMap<String, SpawnDef>,
    /// How the world reacts to what happens, by id (see `world_events.rs`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, WorldEventDef>,
    pub rooms: BTreeMap<String, RoomDef>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RoomDef {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exits: BTreeMap<String, ExitDef>,
    /// Wandering mobs never enter, like a shrine or a shop.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_wander: bool,
    /// Under a roof or underground: the sky can't be seen, so dawn and dusk pass unnoticed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub indoors: bool,
    /// What the ground is like, and so how tiring it is to walk into.
    #[serde(default, skip_serializing_if = "Terrain::is_paved")]
    pub terrain: Terrain,
}

/// What a room's ground is like. Walking into a room costs its terrain's stamina, doubled for
/// someone carrying too much; with too little, they can't go on.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Terrain {
    /// Streets and floors.
    #[default]
    Paved,
    Road,
    /// Sewer walkways, forest tracks, rough grass.
    Uneven,
    /// Rubble, scree, steep slopes.
    Rough,
    Mud,
    /// Water up to the waist.
    Wading,
}

impl Terrain {
    pub const ALL: [Terrain; 6] = [
        Terrain::Paved,
        Terrain::Road,
        Terrain::Uneven,
        Terrain::Rough,
        Terrain::Mud,
        Terrain::Wading,
    ];

    fn is_paved(&self) -> bool {
        *self == Terrain::Paved
    }

    /// Stamina it takes to walk into a room of this terrain.
    pub fn cost(self) -> f32 {
        match self {
            Terrain::Paved | Terrain::Road => 0.0,
            Terrain::Uneven => 0.5,
            Terrain::Rough => 1.0,
            Terrain::Mud => 1.5,
            Terrain::Wading => 2.0,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Terrain::Paved => "paved",
            Terrain::Road => "road",
            Terrain::Uneven => "uneven",
            Terrain::Rough => "rough",
            Terrain::Mud => "mud",
            Terrain::Wading => "wading",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Terrain::ALL
            .into_iter()
            .find(|t| t.name() == s.to_ascii_lowercase())
    }

    /// The end of "You're too exhausted ...", for walking into it.
    pub fn too_tired(self) -> &'static str {
        match self {
            Terrain::Paved | Terrain::Road => "to go on",
            Terrain::Uneven => "to pick your way onward",
            Terrain::Rough => "to climb over the rubble",
            Terrain::Mud => "to trudge into the mud",
            Terrain::Wading => "to wade in",
        }
    }
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
    #[serde(default)]
    pub bulk: Bulk,
    /// Kinds like `key`, `scroll` or `potion`; specialised containers accept them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// The slot it's worn in, if it can be worn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wear: Option<Slot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerDef>,
    /// Can't be picked up, like a chest bolted to the floor.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed: bool,
    /// Only one can ever exist: once a player has found it, its spawn never makes another.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unique: bool,
    /// What it does as a weapon, if it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<WeaponDef>,
    /// What it does as armour, if it is some: worn in its slot, or held, like a shield.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armour: Option<ArmourDef>,
}

/// Armour: worn pieces take damage off every hit and hamper dodging; a held shield blocks.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArmourDef {
    /// Taken off every hit, even to nothing.
    #[serde(default)]
    pub reduction: i32,
    /// Taken off the wearer's defence: heavy armour is hard to dodge in.
    #[serde(default)]
    pub penalty: i32,
    /// Added to the holder's defence, for a shield.
    #[serde(default)]
    pub block: i32,
}

/// A weapon: the damage it does, and the skill it's used with. It does its damage in anyone's
/// hands; training in its skill makes it hit more often and harder.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WeaponDef {
    /// The attack skill, `tree:skill`, like `swordplay:basics`.
    pub skill: String,
    /// Damage before the Strength bonus, low to high.
    pub damage: (u32, u32),
}

/// Size and awkwardness, as in Pathfinder 2e: 10 Light make 1 Bulk.
// `Bulk::Bulk` reads oddly in Rust, but zone files read naturally: `bulk: Bulk(2)`.
#[allow(clippy::enum_variant_names)]
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Bulk {
    Negligible,
    #[default]
    Light,
    Bulk(u32),
}

impl Bulk {
    /// In tenths of 1 Bulk, so Light is 1 and Bulk(1) is 10.
    pub fn tenths(self) -> u32 {
        match self {
            Bulk::Negligible => 0,
            Bulk::Light => 1,
            Bulk::Bulk(n) => n * 10,
        }
    }

    /// Parse a builder's `negligible`, `light`, or a whole number of Bulk.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "negligible" | "-" => Some(Bulk::Negligible),
            "light" | "l" => Some(Bulk::Light),
            n => n.parse().ok().map(Bulk::Bulk),
        }
    }
}

/// Show a quantity of Bulk in tenths the way players read it: "2", "L", "2.3".
pub fn format_bulk(tenths: u32) -> String {
    match (tenths / 10, tenths % 10) {
        (0, 0) => "-".into(),
        (0, 1) => "L".into(),
        (whole, 0) => whole.to_string(),
        (whole, tenth) => format!("{whole}.{tenth}"),
    }
}

/// Where an item is worn.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Slot {
    Head,
    Neck,
    Body,
    Hands,
    Back,
    Belt,
    Legs,
    Feet,
}

impl Slot {
    pub const ALL: [Slot; 8] = [
        Slot::Head,
        Slot::Neck,
        Slot::Body,
        Slot::Hands,
        Slot::Back,
        Slot::Belt,
        Slot::Legs,
        Slot::Feet,
    ];

    /// How many items can be worn in this slot at once: a belt takes several attachments.
    pub fn capacity(self) -> usize {
        match self {
            Slot::Belt => 3,
            _ => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Slot::Head => "head",
            Slot::Neck => "neck",
            Slot::Body => "body",
            Slot::Hands => "hands",
            Slot::Back => "back",
            Slot::Belt => "belt",
            Slot::Legs => "legs",
            Slot::Feet => "feet",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Slot::ALL
            .into_iter()
            .find(|slot| slot.name() == s.to_ascii_lowercase())
    }
}

/// What a container holds and how much.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ContainerDef {
    pub capacity: Capacity,
    /// Tags it accepts; empty means anything. Specialised containers are filled first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts: Vec<String>,
    /// The largest single item it takes, so a sword can't go in a pocket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_item: Option<Bulk>,
    /// Whether it starts open, closed or locked. Most containers are always open.
    #[serde(default = "always_open", skip_serializing_if = "is_open")]
    pub state: DoorState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<LockDef>,
}

fn always_open() -> DoorState {
    DoorState::Open
}

fn is_open(state: &DoorState) -> bool {
    *state == DoorState::Open
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capacity {
    /// This much Bulk.
    Bulk(u32),
    /// This many light items' worth: a belt pouch holds `Light(4)`.
    Light(u32),
    /// This many items, whatever their Bulk: a keyring holds `Count(10)`.
    Count(u32),
}

impl ItemDef {
    /// A plain item with just a name, as builders create them.
    pub fn named(name: &str) -> Self {
        Self {
            name: name.into(),
            keywords: Vec::new(),
            description: String::new(),
            bulk: Bulk::default(),
            tags: Vec::new(),
            wear: None,
            container: None,
            fixed: false,
            unique: false,
            weapon: None,
            armour: None,
        }
    }

    pub fn keywords(&self) -> Keywords {
        if !self.keywords.is_empty() {
            return Keywords(self.keywords.iter().map(|k| k.to_lowercase()).collect());
        }
        Keywords::from_name(without_article(&self.name))
    }
}

/// A mob template: a monster, an animal or a townsperson.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MobDef {
    pub name: String,
    /// Words players can call it by. When empty, the words of its name are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// The line a room shows while it's there, like "A sewer rat scurries along the walkway.".
    /// When empty, "A sewer rat is here.".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub here: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Attribute ratings; any not listed are at the base (3, Luck 1, Magic 0).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<Attribute, i32>,
    /// Skills it knows, `tree:skill` to level. Mobs fight with skills by the same rules as players,
    /// but don't improve.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skills: BTreeMap<String, u32>,
    /// The attack it makes each round; without one, the basic attack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack: Option<String>,
    /// Items it's made with (`item` or `zone:item` templates): wearable ones worn, the rest held,
    /// two at most. It fights with a weapon in hand, and its gear falls into its corpse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub equipment: Vec<String>,
    /// Its health, instead of the usual 8 + Body / 2: a rat is small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<i32>,
    /// The chance, in percent, that it uses one of its technique skills on its turn instead of
    /// its attack; without one, 25.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tactics: Option<u32>,
    /// Attacks players who come near, instead of only fighting back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aggressive: bool,
    /// Skills it teaches (`tree:skill`), to students who meet their requirements.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teaches: Vec<String>,
}

impl MobDef {
    /// A plain mob with just a name, as builders create them.
    pub fn named(name: &str) -> Self {
        Self {
            name: name.into(),
            keywords: Vec::new(),
            here: String::new(),
            description: String::new(),
            attributes: BTreeMap::new(),
            skills: BTreeMap::new(),
            attack: None,
            equipment: Vec::new(),
            health: None,
            tactics: None,
            aggressive: false,
            teaches: Vec::new(),
        }
    }

    pub fn keywords(&self) -> Keywords {
        if !self.keywords.is_empty() {
            return Keywords(self.keywords.iter().map(|k| k.to_lowercase()).collect());
        }
        Keywords::from_name(without_article(&self.name))
    }

    /// The line a room shows for it.
    pub fn here_line(&self) -> String {
        if self.here.is_empty() {
            let mut chars = self.name.chars();
            let name: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            format!("{name} is here.")
        } else {
            self.here.clone()
        }
    }
}

/// Keeps a room stocked with mobs or items: what, where, how often, how many, and how far mobs
/// roam. A spawn names either a `mob` or an `item`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SpawnDef {
    /// The mob template, `mob` or `zone:mob`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mob: Option<String>,
    /// The item template, `item` or `zone:item`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// For a container item: what each reset puts back inside it, as item templates. A template
    /// listed twice keeps two.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contents: Vec<String>,
    /// Where they appear, `room` or `zone:room`.
    pub room: String,
    pub every: Every,
    pub refill: Refill,
    #[serde(default, skip_serializing_if = "Wander::is_never")]
    pub wander: Wander,
    /// How many of its mobs one room holds comfortably. When set, its mobs stay put until a room
    /// holds more than this, then the extras move on toward the least crowded room they may enter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crowd: Option<u32>,
    /// The most of its mobs that will fight one player at once; the rest wait their turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<u32>,
    /// Only fills while this is true, like `WorldFlag("rats_broke_out")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<When>,
}

/// What a spawn makes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spawned<'a> {
    Mob(&'a str),
    Item(&'a str),
}

impl SpawnDef {
    /// What it makes, if it names exactly one mob or item.
    pub fn what(&self) -> Option<Spawned<'_>> {
        match (&self.mob, &self.item) {
            (Some(mob), None) => Some(Spawned::Mob(mob)),
            (None, Some(item)) => Some(Spawned::Item(item)),
            _ => None,
        }
    }

    /// The template it makes, mob or item, for showing to builders.
    pub fn template(&self) -> &str {
        match self.what() {
            Some(Spawned::Mob(id) | Spawned::Item(id)) => id,
            None => "(nothing)",
        }
    }
}

/// When a spawn resets, in real time.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Every {
    Minutes(u64),
    Hours(u64),
    Days(u64),
    /// Once, this long after its last mob is gone: the dragon returns 30 days after it's killed.
    AfterDeath(Box<Every>),
}

impl Every {
    pub fn secs(&self) -> i64 {
        let secs = match self {
            Every::Minutes(n) => n * 60,
            Every::Hours(n) => n * 3600,
            Every::Days(n) => n * 86400,
            Every::AfterDeath(every) => return every.secs(),
        };
        secs as i64
    }

    /// Parse a builder's `10m`, `2h`, `30d`, or `death 30d`.
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(rest) = s.strip_prefix("death ") {
            return Every::parse(rest.trim())
                .filter(|every| !matches!(every, Every::AfterDeath(_)))
                .map(|every| Every::AfterDeath(Box::new(every)));
        }
        let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
        let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
        match unit {
            "m" => Some(Every::Minutes(n)),
            "h" => Some(Every::Hours(n)),
            "d" => Some(Every::Days(n)),
            _ => None,
        }
    }
}

impl Every {
    /// The length of time on its own: "4 hours", "an hour", "a day".
    pub fn period(&self) -> String {
        let (n, unit) = match self {
            Every::Minutes(n) => (*n, "minute"),
            Every::Hours(n) => (*n, "hour"),
            Every::Days(n) => (*n, "day"),
            Every::AfterDeath(every) => return every.period(),
        };
        match (n, unit) {
            (1, "hour") => "an hour".into(),
            (1, unit) => format!("a {unit}"),
            (n, unit) => format!("{n} {unit}s"),
        }
    }
}

impl fmt::Display for Every {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Every::AfterDeath(_) => write!(f, "{} after the last one dies", self.period()),
            _ => match self.period().split_once(' ') {
                Some(("a" | "an", unit)) => write!(f, "every {unit}"),
                _ => write!(f, "every {}", self.period()),
            },
        }
    }
}

/// How many mobs a spawn wants after each reset.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Refill {
    /// This many: a reset replaces whatever died.
    Keep(u32),
    /// A population that grows on each reset, doubling every `doubles_every` while it's small
    /// and slowing as it nears `max`. Once wiped out it stays gone, or for `reseed` if set, after
    /// which a single new mob appears and breeding starts again. It starts with `start` mobs the
    /// first time the world runs, and with its saved count after a restart.
    Breed {
        doubles_every: Every,
        max: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reseed: Option<Every>,
        #[serde(default = "one", skip_serializing_if = "is_one")]
        start: u32,
    },
}

fn one() -> u32 {
    1
}

fn is_one(n: &u32) -> bool {
    *n == 1
}

impl Refill {
    /// The most it ever wants.
    pub fn wanted(&self) -> u32 {
        match self {
            Refill::Keep(n) => *n,
            Refill::Breed { max, .. } => *max,
        }
    }
}

/// How far a spawn's mobs roam, and how often they move: a random number of seconds in `secs`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub enum Wander {
    #[default]
    Never,
    /// Among these rooms (`room` or `zone:room`), like a dog in the market.
    Rooms {
        rooms: Vec<String>,
        secs: (u32, u32),
    },
    /// Anywhere in the spawn's zone, like a city guard.
    Zone { secs: (u32, u32) },
    /// Anywhere at all, including other zones.
    Anywhere { secs: (u32, u32) },
}

impl Wander {
    pub fn is_never(&self) -> bool {
        *self == Wander::Never
    }

    pub fn secs(&self) -> Option<(u32, u32)> {
        match self {
            Wander::Never => None,
            Wander::Rooms { secs, .. } | Wander::Zone { secs } | Wander::Anywhere { secs } => {
                Some(*secs)
            }
        }
    }
}

/// A mob template's full id: its zone and its id there. Written `zone:mob`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MobKey {
    pub zone: String,
    pub mob: String,
}

impl MobKey {
    /// Parse `zone:mob`, or a bare `mob` relative to `current_zone`.
    pub fn parse(s: &str, current_zone: &str) -> Self {
        let RoomKey { zone, room } = RoomKey::parse(s, current_zone);
        Self { zone, mob: room }
    }
}

impl fmt::Display for MobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.zone, self.mob)
    }
}

/// A spawn's full id: its zone and its id there. Written `zone:spawn`.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpawnKey {
    pub zone: String,
    pub spawn: String,
}

impl SpawnKey {
    /// Parse `zone:spawn`, or a bare `spawn` relative to `current_zone`.
    pub fn parse(s: &str, current_zone: &str) -> Self {
        let RoomKey { zone, room } = RoomKey::parse(s, current_zone);
        Self { zone, spawn: room }
    }
}

impl fmt::Display for SpawnKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.zone, self.spawn)
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

/// How zone files are read and written: optional fields can be written plainly, as
/// `wear: Back` rather than `wear: Some(Back)`. The explicit `Some(...)` form still works.
pub fn ron_options() -> ron::Options {
    ron::Options::default().with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
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
            no_wander: false,
            indoors: false,
            terrain: Terrain::Paved,
        };
        Self {
            id: id.into(),
            path: PathBuf::from(ZONES_PATH).join(format!("{id}.ron")),
            name: name.into(),
            doors: BTreeMap::new(),
            items: BTreeMap::new(),
            mobs: BTreeMap::new(),
            spawns: BTreeMap::new(),
            events: BTreeMap::new(),
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
        let body = ron_options().to_string_pretty(self, pretty)?;
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
        let mut zone: ZoneDef = ron_options().from_bytes(&bytes)?;
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
    /// Wandering mobs never enter.
    pub no_wander: bool,
    /// The sky can't be seen from here.
    pub indoors: bool,
    pub terrain: Terrain,
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
    /// Race and background ids, from `assets/races.ron` and `assets/backgrounds.ron`.
    pub race: String,
    pub background: String,
    /// What others see when they look at them.
    pub description: String,
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
                room.no_wander = def.no_wander;
                room.indoors = def.indoors;
                room.terrain = def.terrain;
            }
            None => {
                let entity = commands
                    .spawn((
                        Room {
                            name: def.name.clone(),
                            description: def.description.clone(),
                            no_wander: def.no_wander,
                            indoors: def.indoors,
                            terrain: def.terrain,
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
    fn optional_fields_are_written_plainly() {
        let def = ItemDef {
            wear: Some(Slot::Back),
            ..ItemDef::named("a backpack")
        };
        let pretty = ron::ser::PrettyConfig::new().struct_names(false);
        let written = ron_options().to_string_pretty(&def, pretty).unwrap();
        assert!(written.contains("wear: Back"), "{written}");
        let read: ItemDef = ron_options().from_str(&written).unwrap();
        assert_eq!(read, def);
        let explicit: ItemDef = ron_options()
            .from_str("(name: \"a pack\", wear: Some(Back))")
            .unwrap();
        assert_eq!(explicit.wear, Some(Slot::Back));
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
                let mut zone: ZoneDef = ron_options()
                    .from_str(&text)
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
        let mob_exists = |key: &MobKey| {
            zones
                .iter()
                .any(|z| z.id == key.zone && z.mobs.contains_key(&key.mob))
        };
        let door_exists = |key: &DoorKey| {
            zones
                .iter()
                .any(|z| z.id == key.zone && z.doors.contains_key(&key.door))
        };

        let mut problems = Vec::new();
        for zone in &zones {
            for (item_id, item) in &zone.items {
                let key = item.container.as_ref().and_then(|c| c.lock.as_ref());
                if let Some(key) = key.and_then(|lock| lock.key.as_ref())
                    && !item_exists(&ItemKey::parse(key, &zone.id))
                {
                    problems.push(format!("item {}:{item_id}: no key item '{key}'", zone.id));
                }
            }
            for (door_id, door) in &zone.doors {
                if let Some(key) = door.lock.as_ref().and_then(|lock| lock.key.as_ref())
                    && !item_exists(&ItemKey::parse(key, &zone.id))
                {
                    problems.push(format!("door {}:{door_id}: no key item '{key}'", zone.id));
                }
            }
            for (spawn_id, spawn) in &zone.spawns {
                let here = format!("spawn {}:{spawn_id}", zone.id);
                match spawn.what() {
                    Some(Spawned::Mob(mob)) => {
                        if !mob_exists(&MobKey::parse(mob, &zone.id)) {
                            problems.push(format!("{here}: no mob '{mob}'"));
                        }
                        if !spawn.contents.is_empty() {
                            problems.push(format!("{here}: only item spawns have contents"));
                        }
                    }
                    Some(Spawned::Item(item)) => {
                        if !item_exists(&ItemKey::parse(item, &zone.id)) {
                            problems.push(format!("{here}: no item '{item}'"));
                        }
                        if matches!(spawn.refill, Refill::Breed { .. })
                            || !spawn.wander.is_never()
                            || spawn.crowd.is_some()
                        {
                            problems.push(format!("{here}: items don't breed, wander or crowd"));
                        }
                        for inside in &spawn.contents {
                            if !item_exists(&ItemKey::parse(inside, &zone.id)) {
                                problems.push(format!("{here}: no content item '{inside}'"));
                            }
                        }
                    }
                    None => problems.push(format!("{here}: name one mob or one item")),
                }
                let mut rooms = vec![&spawn.room];
                if let Wander::Rooms { rooms: wander, .. } = &spawn.wander {
                    rooms.extend(wander);
                }
                for room in rooms {
                    if !room_exists(&RoomKey::parse(room, &zone.id)) {
                        problems.push(format!("{here}: no room '{room}'"));
                    }
                }
                if let Refill::Breed {
                    doubles_every,
                    reseed,
                    ..
                } = &spawn.refill
                    && [Some(doubles_every), reseed.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|every| matches!(every, Every::AfterDeath(_)))
                {
                    problems.push(format!("{here}: breeding periods can't be AfterDeath"));
                }
                if let Some((min, max)) = spawn.wander.secs()
                    && (min == 0 || min > max)
                {
                    problems.push(format!(
                        "{here}: wander secs must be 1 or more, low to high"
                    ));
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
    fn spawn_periods_parse_and_read_naturally() {
        assert_eq!(Every::parse("10m"), Some(Every::Minutes(10)));
        assert_eq!(Every::parse("2h"), Some(Every::Hours(2)));
        let dragon = Every::AfterDeath(Box::new(Every::Days(30)));
        assert_eq!(Every::parse("death 30d"), Some(dragon.clone()));
        assert_eq!(dragon.secs(), 30 * 86400);
        assert_eq!(dragon.to_string(), "30 days after the last one dies");
        assert_eq!(Every::Hours(1).to_string(), "every hour");
        assert_eq!(Every::Days(1).period(), "a day");
        assert_eq!(Every::Hours(4).period(), "4 hours");
        assert_eq!(Every::Minutes(20).to_string(), "every 20 minutes");
        let quick = Every::AfterDeath(Box::new(Every::Minutes(1)));
        assert_eq!(quick.to_string(), "a minute after the last one dies");
        assert_eq!(Every::parse("0m"), None);
        assert_eq!(Every::parse("10"), None);
        assert_eq!(Every::parse("death death 1d"), None);

        let spawn: SpawnDef = ron_options()
            .from_str(
                "(mob: \"rat\", room: \"nest\", every: AfterDeath(Days(7)), refill: Keep(1), \
                 wander: Zone(secs: (30, 90)))",
            )
            .unwrap();
        assert_eq!(spawn.every, Every::AfterDeath(Box::new(Every::Days(7))));
        assert_eq!(spawn.wander.secs(), Some((30, 90)));

        let rats: SpawnDef = ron_options()
            .from_str(
                "(mob: \"rat\", room: \"nest\", every: Minutes(20), crowd: 6, \
                 refill: Breed(doubles_every: Hours(4), max: 200, reseed: Days(7)))",
            )
            .unwrap();
        assert_eq!(
            rats.refill,
            Refill::Breed {
                doubles_every: Every::Hours(4),
                max: 200,
                reseed: Some(Every::Days(7)),
                start: 1,
            }
        );
        assert_eq!(rats.crowd, Some(6));
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
