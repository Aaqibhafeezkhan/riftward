//! Items: entities made from templates in zone files. An item lies in a room, is held in a
//! character's hands, is worn in a slot, or is inside another item (a container).
//!
//! An item becomes "real", saved in the `items` table, the moment a player touches it; every move
//! after that updates its row. Real items in rooms are restored at startup, with their contents,
//! and the items a character holds and wears load and unload with them.
//!
//! Space and burden use Bulk, as in Pathfinder 2e. The rules live in [`Snapshot`], a plain view of
//! every item, so they can be tested without a running world.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, ItemPlace, Permission};
use crate::net::{self, Connection};
use crate::output::{
    Actor, Outgoing, PlayerQuery, Reply, RoomQuery, deliver, reply_line, targets_in_room,
};
use crate::targeting::{Keywords, Target};
use crate::world::{
    Capacity, DoorState, ItemDef, ItemKey, Player, RoomIndex, RoomKey, START_ROOM, Slot, ZoneDef,
    format_bulk, without_article,
};

pub struct ItemsPlugin;

impl Plugin for ItemsPlugin {
    fn build(&self, app: &mut App) {
        use Category::*;
        app.init_resource::<ItemTemplates>()
            .add_systems(
                Update,
                (sync_templates, restore_room_items, load_carried_items).chain(),
            )
            .add_systems(
                PostUpdate,
                unload_carried_items.before(net::cleanup_disconnected),
            )
            .add_command(
                CommandSpec::new("inventory", Information)
                    .shortcuts(&["i", "inv"])
                    .summary("list what you hold, wear and carry, and your Bulk"),
                inventory,
            )
            .add_command(
                CommandSpec::new("examine", Information)
                    .shortcuts(&["exa"])
                    .usage("examine <item>")
                    .summary("look closely at something; containers show their contents"),
                examine,
            )
            .add_command(
                CommandSpec::new("get", Interaction)
                    .shortcuts(&["take"])
                    .usage("get <item> [<container>]")
                    .summary("pick something up, or take it out of a container"),
                get,
            )
            .add_command(
                CommandSpec::new("put", Interaction)
                    .usage("put <item> <container>")
                    .summary("put something into a container"),
                put,
            )
            .add_command(
                CommandSpec::new("drop", Interaction)
                    .usage("drop <item> | drop all | drop all.<item>")
                    .summary("put something down"),
                drop,
            )
            .add_command(
                CommandSpec::new("give", Interaction)
                    .usage("give <item> <player>")
                    .summary("hand something to someone here"),
                give,
            )
            .add_command(
                CommandSpec::new("sheath", Interaction)
                    .shortcuts(&["sheathe"])
                    .usage("sheath [<weapon>]")
                    .summary(
                        "put a weapon away in a worn sheath or loop; alone, whatever you hold",
                    ),
                sheath,
            )
            .add_command(
                CommandSpec::new("draw", Interaction)
                    .usage("draw <weapon>")
                    .summary("draw a weapon from its sheath or loop into your hand"),
                draw,
            )
            .add_command(
                CommandSpec::new("hold", Interaction)
                    .usage("hold <item>")
                    .summary("take something in your hand"),
                hold,
            )
            .add_command(
                CommandSpec::new("wear", Interaction)
                    .usage("wear <item>")
                    .summary("wear something, like a backpack, pouch or coat"),
                wear,
            )
            .add_command(
                CommandSpec::new("remove", Interaction)
                    .usage("remove <item>")
                    .summary("take off something you're wearing"),
                remove,
            )
            .add_command(
                CommandSpec::new("create", Building)
                    .usage("create <item>")
                    .summary("make an item from a template (`item` or `zone:item`)")
                    .permission(Permission::Build),
                create,
            )
            .add_command(
                CommandSpec::new("destroy", Building)
                    .usage("destroy <item>")
                    .summary("destroy an item (and its contents) here or on you, for good")
                    .permission(Permission::Build),
                destroy,
            );
    }
}

/// How many items fit in a character's hands.
pub const HANDS: usize = 2;

/// An item in the world.
#[derive(Component, Debug)]
pub struct Item {
    pub key: ItemKey,
    pub name: String,
    /// Its row in the `items` table, once it's real.
    pub id: Option<i64>,
    /// Open, closed or locked, for containers that close. Other items are always open.
    pub state: DoorState,
}

/// Where an item is.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemLocation {
    Room(Entity),
    Held(Entity),
    Worn(Entity, Slot),
    Inside(Entity),
}

impl ItemLocation {
    /// The character holding or wearing it, if that's where it is directly.
    pub fn holder(self) -> Option<Entity> {
        match self {
            ItemLocation::Held(who) | ItemLocation::Worn(who, _) => Some(who),
            _ => None,
        }
    }
}

/// Read-only access to every item, for looking and listing.
pub type ItemQuery<'w, 's> = Query<'w, 's, (Entity, &'static Item, &'static ItemLocation)>;

/// Write access to every item, for commands that move them.
pub type ItemMutQuery<'w, 's> =
    Query<'w, 's, (Entity, &'static mut Item, &'static mut ItemLocation)>;

/// Every item template from every loaded zone.
#[derive(Resource, Default)]
pub struct ItemTemplates(pub HashMap<ItemKey, ItemDef>);

/// "A torch", to start a sentence.
pub fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A tag as a plural noun: "key" -> "keys", "axe" -> "axes".
fn plural(tag: &str) -> String {
    if tag.ends_with('s') || tag.ends_with('x') || tag.ends_with("sh") || tag.ends_with("ch") {
        format!("{tag}es")
    } else {
        format!("{tag}s")
    }
}

/// "a", "a and b", "a, b and c".
fn list(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// "your brass keyring", for an item someone carries.
fn yours(name: &str) -> String {
    format!("your {}", without_article(name))
}

/// The most Bulk, in tenths, a character can carry before they're encumbered, and before they
/// can't carry any more: 5 and 10 Bulk, plus Strength / 2.
pub fn bulk_limits(strength: i32) -> (u32, u32) {
    let bonus = (strength.max(0) as u32) * 5;
    (50 + bonus, 100 + bonus)
}

/// Whether `who` carries a copy of the `key` item anywhere on them, at any depth.
pub fn carries<'a>(
    items: impl IntoIterator<Item = (Entity, &'a Item, ItemLocation)>,
    who: Entity,
    key: &ItemKey,
) -> bool {
    let items: Vec<_> = items.into_iter().collect();
    let snapshot = Snapshot::new(items.iter().copied(), &ItemTemplates::default());
    items.iter().any(|(entity, item, _)| {
        item.key == *key && snapshot.root(*entity).and_then(ItemLocation::holder) == Some(who)
    })
}

/// One item, as the rules see it.
#[derive(Clone, Debug)]
pub struct ItemInfo {
    pub entity: Entity,
    pub name: String,
    pub location: ItemLocation,
    pub def: Option<ItemDef>,
    pub state: DoorState,
}

/// A plain view of every item, for answering questions about space and placement.
pub struct Snapshot {
    items: Vec<ItemInfo>,
}

impl Snapshot {
    pub fn new<'a>(
        items: impl IntoIterator<Item = (Entity, &'a Item, ItemLocation)>,
        templates: &ItemTemplates,
    ) -> Self {
        let mut items: Vec<_> = items
            .into_iter()
            .map(|(entity, item, location)| ItemInfo {
                entity,
                name: item.name.clone(),
                location,
                def: templates.0.get(&item.key).cloned(),
                state: item.state,
            })
            .collect();
        items.sort_by_key(|info| info.entity);
        Self { items }
    }

    pub fn get(&self, entity: Entity) -> Option<&ItemInfo> {
        self.items.iter().find(|info| info.entity == entity)
    }

    /// Record a move, so later questions in the same command see it.
    pub fn set_location(&mut self, entity: Entity, location: ItemLocation) {
        if let Some(info) = self.items.iter_mut().find(|info| info.entity == entity) {
            info.location = location;
        }
    }

    pub fn at(&self, location: ItemLocation) -> impl Iterator<Item = &ItemInfo> {
        self.items
            .iter()
            .filter(move |info| info.location == location)
    }

    /// Where an item ultimately is: following containers outward to a room or a character.
    pub fn root(&self, entity: Entity) -> Option<ItemLocation> {
        let mut location = self.get(entity)?.location;
        for _ in 0..32 {
            match location {
                ItemLocation::Inside(container) => location = self.get(container)?.location,
                other => return Some(other),
            }
        }
        None
    }

    /// Whether `entity` is `ancestor`, or inside it at any depth.
    pub fn is_within(&self, entity: Entity, ancestor: Entity) -> bool {
        let mut current = entity;
        for _ in 0..32 {
            if current == ancestor {
                return true;
            }
            match self.get(current).map(|info| info.location) {
                Some(ItemLocation::Inside(container)) => current = container,
                _ => return false,
            }
        }
        false
    }

    /// Everything a character has on them, at any depth.
    pub fn carried_by(&self, who: Entity) -> impl Iterator<Item = &ItemInfo> {
        self.items.iter().filter(move |info| {
            self.root(info.entity)
                .and_then(ItemLocation::holder)
                .is_some_and(|holder| holder == who)
        })
    }

    pub fn is_container(&self, entity: Entity) -> bool {
        self.get(entity)
            .and_then(|info| info.def.as_ref())
            .is_some_and(|def| def.container.is_some())
    }

    fn is_fixed(&self, entity: Entity) -> bool {
        self.get(entity)
            .and_then(|info| info.def.as_ref())
            .is_some_and(|def| def.fixed)
    }

    fn own_bulk(&self, entity: Entity) -> u32 {
        self.get(entity)
            .and_then(|info| info.def.as_ref())
            .map_or(0, |def| def.bulk.tenths())
    }

    /// An item's Bulk plus everything inside it, in tenths.
    pub fn total_bulk(&self, entity: Entity) -> u32 {
        self.own_bulk(entity)
            + self
                .at(ItemLocation::Inside(entity))
                .map(|child| self.total_bulk(child.entity))
                .sum::<u32>()
    }

    /// Everything a character holds, wears and carries in containers, in tenths of Bulk.
    pub fn burden(&self, who: Entity) -> u32 {
        self.carried_by(who)
            .map(|info| self.own_bulk(info.entity))
            .sum()
    }

    /// How full a container is: (used, capacity), with `used` in the capacity's own units.
    pub fn fill(&self, container: Entity) -> Option<(u32, Capacity)> {
        let capacity = self
            .get(container)?
            .def
            .as_ref()?
            .container
            .as_ref()?
            .capacity;
        let used = match capacity {
            Capacity::Count(_) => self.at(ItemLocation::Inside(container)).count() as u32,
            _ => self
                .at(ItemLocation::Inside(container))
                .map(|child| self.total_bulk(child.entity))
                .sum(),
        };
        Some((used, capacity))
    }

    /// Whether `item` can go into `container`; the error says why not.
    pub fn fits(&self, container: Entity, item: Entity) -> Result<(), String> {
        let (Some(holder), Some(info)) = (self.get(container), self.get(item)) else {
            return Err(String::new());
        };
        let Some(spec) = holder.def.as_ref().and_then(|def| def.container.as_ref()) else {
            return Err(format!("{} isn't a container.", capitalized(&holder.name)));
        };
        if self.is_within(container, item) {
            return Err(format!("You can't put {} inside itself.", info.name));
        }
        if holder.state != DoorState::Open {
            return Err(format!("{} is closed.", capitalized(&holder.name)));
        }
        let tags = info
            .def
            .as_ref()
            .map(|def| def.tags.as_slice())
            .unwrap_or(&[]);
        if !spec.accepts.is_empty() && !spec.accepts.iter().any(|tag| tags.contains(tag)) {
            let kinds: Vec<String> = spec.accepts.iter().map(|tag| plural(tag)).collect();
            return Err(format!("Only {} go in {}.", list(&kinds), holder.name));
        }
        let size = self.total_bulk(item);
        if spec.max_item.is_some_and(|max| size > max.tenths()) {
            return Err(format!(
                "{} is too big for {}.",
                capitalized(&info.name),
                holder.name
            ));
        }
        let (used, capacity) = self.fill(container).ok_or_else(String::new)?;
        let room_left = match capacity {
            Capacity::Count(n) => used < n,
            Capacity::Bulk(n) => used + size <= n * 10,
            Capacity::Light(n) => used + size <= n,
        };
        if !room_left {
            return Err(format!("{} is full.", capitalized(&holder.name)));
        }
        Ok(())
    }

    /// The containers a character holds or wears directly, except `except` and `item` itself.
    fn containers_on(&self, who: Entity, item: Entity, except: Option<Entity>) -> Vec<&ItemInfo> {
        self.items
            .iter()
            .filter(|info| info.location.holder() == Some(who))
            .filter(|info| Some(info.entity) != except && info.entity != item)
            .filter(|info| self.is_container(info.entity))
            .collect()
    }

    fn accepts_count(info: &ItemInfo) -> usize {
        info.def
            .as_ref()
            .and_then(|def| def.container.as_ref())
            .map_or(0, |spec| spec.accepts.len())
    }

    /// A container the character holds or wears that specifically accepts `item` and has room:
    /// a keyring for a key, a sheath for a sword.
    pub fn specialised_for(
        &self,
        who: Entity,
        item: Entity,
        except: Option<Entity>,
    ) -> Option<Entity> {
        self.containers_on(who, item, except)
            .into_iter()
            .filter(|info| Self::accepts_count(info) > 0)
            .find(|info| self.fits(info.entity, item).is_ok())
            .map(|info| info.entity)
    }

    /// Where an item should go when a character takes it without saying where: a container they
    /// hold or wear that specifically accepts it, then a general container (smallest first), then
    /// a free hand. Containers skip the general ones, so a pouch taken out of a pack goes to your
    /// hand, ready to use, rather than into your pockets. `except` is a container to skip, such
    /// as the one it's coming out of.
    pub fn place_for(
        &self,
        who: Entity,
        item: Entity,
        except: Option<Entity>,
    ) -> Option<ItemLocation> {
        if let Some(found) = self.specialised_for(who, item, except) {
            return Some(ItemLocation::Inside(found));
        }
        let size = |info: &ItemInfo| match info.def.as_ref().and_then(|d| d.container.as_ref()) {
            Some(spec) => match spec.capacity {
                Capacity::Bulk(n) => n * 10,
                Capacity::Light(n) | Capacity::Count(n) => n,
            },
            None => 0,
        };

        let containers = if self.is_container(item) {
            Vec::new()
        } else {
            self.containers_on(who, item, except)
        };
        let mut general: Vec<_> = containers
            .iter()
            .filter(|info| Self::accepts_count(info) == 0)
            .collect();
        general.sort_by_key(|info| size(info));
        if let Some(found) = general
            .iter()
            .find(|info| self.fits(info.entity, item).is_ok())
        {
            return Some(ItemLocation::Inside(found.entity));
        }
        (self.at(ItemLocation::Held(who)).count() < HANDS).then_some(ItemLocation::Held(who))
    }

    /// What a character can see to look at: what they carry, at any depth, then what lies in
    /// the room (not inside room containers).
    pub fn reachable(&self, who: Entity, room: Entity) -> Vec<Entity> {
        let mut found: Vec<_> = self.carried_by(who).map(|info| info.entity).collect();
        found.extend(self.at(ItemLocation::Room(room)).map(|info| info.entity));
        found
    }

    /// Whether a location is directly on a character or in their room, not packed in anything.
    fn top_level(location: ItemLocation, who: Entity, room: Entity) -> bool {
        match location {
            ItemLocation::Held(holder) | ItemLocation::Worn(holder, _) => holder == who,
            ItemLocation::Room(here) => here == room,
            ItemLocation::Inside(_) => false,
        }
    }

    /// What a character can lay hands on without unpacking anything: what they hold or wear,
    /// what lies in the room, and what's directly inside an open container among those. A
    /// loaf in a pouch packed in a backpack isn't at hand until the pouch comes out. Their own
    /// things come first.
    pub fn at_hand(&self, who: Entity, room: Entity) -> Vec<Entity> {
        let at_hand = |info: &ItemInfo| match info.location {
            ItemLocation::Inside(container) => self.get(container).is_some_and(|outer| {
                Self::top_level(outer.location, who, room) && outer.state == DoorState::Open
            }),
            location => Self::top_level(location, who, room),
        };
        let (mut mine, theirs): (Vec<_>, Vec<_>) = self
            .items
            .iter()
            .filter(|info| at_hand(info))
            .map(|info| info.entity)
            .partition(|e| self.root(*e).and_then(ItemLocation::holder) == Some(who));
        mine.extend(theirs);
        mine
    }

    /// The things at hand that a character carries, rather than ones lying in the room.
    pub fn at_hand_carried(&self, who: Entity, room: Entity) -> Vec<Entity> {
        self.at_hand(who, room)
            .into_iter()
            .filter(|e| self.root(*e).and_then(ItemLocation::holder) == Some(who))
            .collect()
    }
}

/// Keywords for a list of item entities, keeping their order, for targeting.
pub fn with_keywords<'a>(
    entities: impl IntoIterator<Item = Entity>,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    entities
        .into_iter()
        .filter_map(|entity| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}

/// Names with repeats counted, in first-seen order: "a torch (x2)".
pub fn counted<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for name in names {
        match counts.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counts.push((name, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(name, n)| {
            if n > 1 {
                format!("{name} (x{n})")
            } else {
                name.to_string()
            }
        })
        .collect()
}

fn spawn_item(
    commands: &mut Commands,
    templates: &ItemTemplates,
    key: ItemKey,
    id: Option<i64>,
    location: ItemLocation,
) -> Option<Entity> {
    let Some(def) = templates.0.get(&key) else {
        warn!("no item template [{key}]");
        return None;
    };
    let state = def
        .container
        .as_ref()
        .map_or(DoorState::Open, |container| container.state);
    let item = Item {
        key,
        name: def.name.clone(),
        id,
        state,
    };
    Some(commands.spawn((item, def.keywords(), location)).id())
}

/// Spawn a saved item and, recursively, everything saved inside it.
fn spawn_saved(
    commands: &mut Commands,
    templates: &ItemTemplates,
    db: &Database,
    id: i64,
    template: &str,
    location: ItemLocation,
) {
    let key = ItemKey::parse(template, "");
    let Some(entity) = spawn_item(commands, templates, key, Some(id), location) else {
        return;
    };
    match db.items_inside(id) {
        Ok(contents) => {
            for (child, template) in contents {
                let inside = ItemLocation::Inside(entity);
                spawn_saved(commands, templates, db, child, &template, inside);
            }
        }
        Err(err) => error!("failed to load the contents of item {id}: {err}"),
    }
}

/// Rebuild the template table when zones change, and refresh existing items' names.
fn sync_templates(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<ZoneDef>>,
    zones: Res<Assets<ZoneDef>>,
    mut templates: ResMut<ItemTemplates>,
    mut items: Query<(Entity, &mut Item)>,
) {
    if events.read().count() == 0 {
        return;
    }
    templates.0.clear();
    for (_, zone) in zones.iter() {
        for (id, def) in &zone.items {
            let key = ItemKey {
                zone: zone.id.clone(),
                item: id.clone(),
            };
            templates.0.insert(key, def.clone());
        }
    }
    for (entity, mut item) in &mut items {
        if let Some(def) = templates.0.get(&item.key) {
            if item.name != def.name {
                item.name = def.name.clone();
            }
            commands.entity(entity).insert(def.keywords());
        }
    }
}

/// Once the world has loaded, put real items back in the rooms they were left in, with their
/// contents. Items whose room no longer exists are moved to the start room.
fn restore_room_items(
    mut commands: Commands,
    mut done: Local<bool>,
    index: Res<RoomIndex>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    if *done || templates.0.is_empty() {
        return;
    }
    let Some(start) = index.start() else {
        return;
    };
    *done = true;

    let rows = match db.items_in_rooms() {
        Ok(rows) => rows,
        Err(err) => {
            error!("failed to load items: {err}");
            return;
        }
    };
    let count = rows.len();
    for (id, template, room) in rows {
        let room = match index.0.get(&RoomKey::parse(&room, "")) {
            Some(&room) => room,
            None => {
                warn!("item {id} was in missing room '{room}'; moving it to the start room");
                if let Err(err) = db.move_item(id, &ItemPlace::Room(START_ROOM.into())) {
                    error!("failed to move item {id}: {err}");
                }
                start
            }
        };
        let location = ItemLocation::Room(room);
        spawn_saved(&mut commands, &templates, &db, id, &template, location);
    }
    info!("restored {count} items lying in rooms");
}

/// Load what a character holds and wears, with its contents, when they enter the game.
fn load_carried_items(
    mut commands: Commands,
    entered: Query<(Entity, &Player), Added<Player>>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    for (entity, player) in &entered {
        match db.items_on_character(player.character_id) {
            Ok(rows) => {
                for (id, template, slot) in rows {
                    let location = match Slot::parse(&slot) {
                        Some(slot) => ItemLocation::Worn(entity, slot),
                        None => ItemLocation::Held(entity),
                    };
                    spawn_saved(&mut commands, &templates, &db, id, &template, location);
                }
            }
            Err(err) => error!("failed to load items for {}: {err}", player.name),
        }
    }
}

/// When a player leaves, everything on them leaves too. It's already saved.
fn unload_carried_items(mut commands: Commands, items: ItemQuery, connections: Query<&Connection>) {
    let snapshot = Snapshot::new(
        items.iter().map(|(e, i, l)| (e, i, *l)),
        &ItemTemplates::default(),
    );
    for info in &snapshot.items {
        if let Some(holder) = snapshot.root(info.entity).and_then(ItemLocation::holder)
            && connections.get(holder).is_ok_and(|conn| conn.closing)
        {
            commands.entity(info.entity).despawn();
        }
    }
}

/// What an item-moving command works with, gathered once.
struct Mover<'a, 'w, 's> {
    actor: Actor,
    snapshot: Snapshot,
    items: &'a mut ItemMutQuery<'w, 's>,
    /// Character ids of everyone online, for saving held and worn items.
    characters: HashMap<Entity, i64>,
    /// The actor's room id, for saving dropped items.
    room_key: String,
    db: &'a Database,
}

impl<'a, 'w, 's> Mover<'a, 'w, 's> {
    fn new(
        actor: Actor,
        items: &'a mut ItemMutQuery<'w, 's>,
        players: &PlayerQuery,
        rooms: &RoomQuery,
        templates: &ItemTemplates,
        db: &'a Database,
    ) -> Self {
        let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), templates);
        let characters = players
            .iter()
            .filter_map(|(entity, _, player, _)| Some((entity, player?.character_id)))
            .collect();
        let room_key = rooms
            .get(actor.room)
            .map(|(_, key, _)| key.to_string())
            .unwrap_or_default();
        Self {
            actor,
            snapshot,
            items,
            characters,
            room_key,
            db,
        }
    }

    fn me(&self) -> Entity {
        self.actor.entity
    }

    /// How an item location is saved. A container becomes real first if it isn't already.
    fn place(&mut self, location: ItemLocation) -> Option<ItemPlace> {
        Some(match location {
            ItemLocation::Room(room) if room == self.actor.room => {
                ItemPlace::Room(self.room_key.clone())
            }
            ItemLocation::Room(_) => return None,
            ItemLocation::Held(who) => ItemPlace::Held(*self.characters.get(&who)?),
            ItemLocation::Worn(who, slot) => {
                ItemPlace::Worn(*self.characters.get(&who)?, slot.name())
            }
            ItemLocation::Inside(container) => ItemPlace::Inside(self.make_real(container)?),
        })
    }

    /// An item's id, saving it where it is now if no player has touched it before.
    fn make_real(&mut self, entity: Entity) -> Option<i64> {
        let (_, item, location) = self.items.get(entity).ok()?;
        if let Some(id) = item.id {
            return Some(id);
        }
        let location = *location;
        let place = self.place(location)?;
        let (_, mut item, _) = self.items.get_mut(entity).ok()?;
        match self.db.create_item(&item.key.to_string(), &place) {
            Ok(id) => {
                item.id = Some(id);
                Some(id)
            }
            Err(err) => {
                error!("failed to save item [{}]: {err}", item.key);
                None
            }
        }
    }

    /// Move an item and save where it went.
    fn move_to(&mut self, entity: Entity, location: ItemLocation) {
        let Some(place) = self.place(location) else {
            return;
        };
        let Ok((_, mut item, mut current)) = self.items.get_mut(entity) else {
            return;
        };
        let result = match item.id {
            Some(id) => self.db.move_item(id, &place),
            None => self
                .db
                .create_item(&item.key.to_string(), &place)
                .map(|id| item.id = Some(id)),
        };
        if let Err(err) = result {
            error!("failed to save item [{}]: {err}", item.key);
        }
        *current = location;
        self.snapshot.set_location(entity, location);
    }

    fn name(&self, entity: Entity) -> String {
        self.snapshot
            .get(entity)
            .map(|info| info.name.clone())
            .unwrap_or_default()
    }

    /// "in your hand", "in your belt pouch", "in an old oak chest".
    fn where_phrase(&self, location: ItemLocation) -> String {
        match location {
            ItemLocation::Held(_) => "in your hand".into(),
            ItemLocation::Inside(container) => {
                let name = self.name(container);
                match self.snapshot.root(container).and_then(ItemLocation::holder) {
                    Some(who) if who == self.me() => format!("in {}", yours(&name)),
                    _ => format!("in {name}"),
                }
            }
            ItemLocation::Worn(_, slot) => format!("on your {}", slot.name()),
            ItemLocation::Room(_) => "on the ground".into(),
        }
    }

    fn carries(&self, entity: Entity) -> bool {
        self.snapshot.root(entity).and_then(ItemLocation::holder) == Some(self.me())
    }

    /// Whether taking `entity` would push the actor past their carrying limit.
    fn too_heavy(&self, entity: Entity, sheets: &Query<&Attributes>) -> bool {
        let strength = sheets
            .get(self.me())
            .map_or(1, |a| a.get(Attribute::Strength));
        let (_, max) = bulk_limits(strength);
        self.snapshot.burden(self.me()) + self.snapshot.total_bulk(entity) > max
    }

    /// A container the actor can reach into, by name: one they hold or wear, or one lying in
    /// the room. A container packed inside another has to come out first.
    fn find_container(
        &self,
        target: &Target,
        keywords: &Query<&Keywords>,
    ) -> Result<Entity, String> {
        let (me, room) = (self.me(), self.actor.room);
        let top: Vec<_> = self
            .snapshot
            .reachable(me, room)
            .into_iter()
            .filter(|e| {
                self.snapshot
                    .get(*e)
                    .is_some_and(|info| Snapshot::top_level(info.location, me, room))
            })
            .collect();
        let Some(found) = target
            .find_one(me, with_keywords(top, keywords))
            .filter(|e| *e != me)
        else {
            // Name a packed container, if that's what they meant.
            let packed = target
                .find_one(
                    me,
                    with_keywords(self.snapshot.reachable(me, room), keywords),
                )
                .filter(|e| *e != me);
            if let Some(packed) = packed
                && let Some(ItemLocation::Inside(outer)) =
                    self.snapshot.get(packed).map(|info| info.location)
            {
                return Err(format!(
                    "{} is packed inside {}; take it out first.",
                    capitalized(&self.name(packed)),
                    self.where_phrase(ItemLocation::Inside(outer))
                        .trim_start_matches("in ")
                ));
            }
            return Err(format!("You don't see {} here.", target.describe()));
        };
        if !self.snapshot.is_container(found) {
            return Err(format!(
                "{} isn't a container.",
                capitalized(&self.name(found))
            ));
        }
        Ok(found)
    }

    /// One item at hand, among those `only` allows.
    fn find_one(
        &self,
        arg: &str,
        keywords: &Query<&Keywords>,
        only: impl Fn(&ItemInfo) -> bool,
    ) -> Result<Entity, String> {
        let target = Target::parse(arg).ok_or_else(String::new)?;
        let reach: Vec<_> = self
            .snapshot
            .at_hand(self.me(), self.actor.room)
            .into_iter()
            .filter(|e| self.snapshot.get(*e).is_some_and(&only))
            .collect();
        target
            .find_one(self.me(), with_keywords(reach, keywords))
            .filter(|e| *e != self.me())
            .ok_or_else(|| format!("You don't see {} here.", target.describe()))
    }
}

fn inventory(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    sheets: Query<&Attributes>,
) {
    let who = inv.entity;
    let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), &templates);
    let mut text = String::new();
    let held: Vec<_> = snapshot.at(ItemLocation::Held(who)).collect();
    if held.is_empty() {
        text += "Your hands are empty.\r\n";
    } else {
        text += "You are holding:\r\n";
        for info in held {
            text += &format!("  {}{}\r\n", info.name, fill_note(&snapshot, info.entity));
            for line in contents_lines(&snapshot, info.entity, 4) {
                text += &format!("{line}\r\n");
            }
        }
    }
    let worn: Vec<_> = Slot::ALL
        .into_iter()
        .flat_map(|slot| snapshot.at(ItemLocation::Worn(who, slot)))
        .collect();
    if !worn.is_empty() {
        text += "You are wearing:\r\n";
        for info in worn {
            let ItemLocation::Worn(_, slot) = info.location else {
                continue;
            };
            let slot = format!("<{}>", slot.name());
            let note = fill_note(&snapshot, info.entity);
            text += &format!("  {slot:<7} {}{note}\r\n", info.name);
            for line in contents_lines(&snapshot, info.entity, 12) {
                text += &format!("{line}\r\n");
            }
        }
    }
    let strength = sheets.get(who).map_or(1, |a| a.get(Attribute::Strength));
    let (encumbered, max) = bulk_limits(strength);
    let burden = snapshot.burden(who);
    let state = if burden > encumbered {
        " You are encumbered."
    } else {
        ""
    };
    text += &format!(
        "Bulk: {} (encumbered above {}, limit {}).{state}",
        format_bulk(burden),
        format_bulk(encumbered),
        format_bulk(max)
    );
    deliver(vec![Outgoing::line(who, text)], &mut players);
}

/// " [2/4 Bulk]" for a container, " [closed]" for a closed one, nothing for anything else.
fn fill_note(snapshot: &Snapshot, container: Entity) -> String {
    let Some((used, capacity)) = snapshot.fill(container) else {
        return String::new();
    };
    if snapshot
        .get(container)
        .is_some_and(|info| info.state != DoorState::Open)
    {
        return " [closed]".into();
    }
    match capacity {
        Capacity::Count(n) => format!(" [{used}/{n} items]"),
        Capacity::Bulk(n) if used == 0 => format!(" [0/{n} Bulk]"),
        Capacity::Bulk(n) => format!(" [{}/{n} Bulk]", format_bulk(used)),
        Capacity::Light(n) => format!(" [{used}/{n} light]"),
    }
}

/// The contents of a container as indented lines, each nested container followed by its own
/// contents. Repeats of ordinary items are counted ("a torch (x2)"); closed containers keep
/// their contents hidden.
fn contents_lines(snapshot: &Snapshot, container: Entity, indent: usize) -> Vec<String> {
    if snapshot
        .get(container)
        .is_some_and(|info| info.state != DoorState::Open)
    {
        return Vec::new();
    }
    let pad = " ".repeat(indent);
    let inside: Vec<_> = snapshot.at(ItemLocation::Inside(container)).collect();
    let mut lines: Vec<String> = counted(
        inside
            .iter()
            .filter(|info| !snapshot.is_container(info.entity))
            .map(|info| info.name.as_str()),
    )
    .into_iter()
    .map(|name| format!("{pad}{name}"))
    .collect();
    for info in inside
        .iter()
        .filter(|info| snapshot.is_container(info.entity))
    {
        lines.push(format!(
            "{pad}{}{}",
            info.name,
            fill_note(snapshot, info.entity)
        ));
        lines.extend(contents_lines(snapshot, info.entity, indent + 2));
    }
    lines
}

fn examine(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    items: ItemQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = match find_item(&inv.args, &actor, &items, &keywords, &templates) {
        Ok(entity) => {
            let mut text = describe_item(entity, &items, &templates);
            let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), &templates);
            if let Some(info) = snapshot.get(entity)
                && snapshot.is_container(entity)
            {
                if info.state != DoorState::Open {
                    text += "\r\nIt is closed.";
                } else {
                    let lines = contents_lines(&snapshot, entity, 2);
                    if lines.is_empty() {
                        text += "\r\nIt is empty.";
                    } else {
                        text += &format!("\r\nIt holds:\r\n{}", lines.join("\r\n"));
                    }
                }
            }
            text
        }
        Err(err) => err,
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// One item the player names, on them (at any depth) or lying here, theirs first.
pub fn find_item(
    arg: &str,
    actor: &Actor,
    items: &ItemQuery,
    keywords: &Query<&Keywords>,
    templates: &ItemTemplates,
) -> Result<Entity, String> {
    let target = Target::parse(arg).ok_or_else(|| "Look at what?".to_string())?;
    let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), templates);
    let candidates = with_keywords(snapshot.reachable(actor.entity, actor.room), keywords);
    target
        .find_one(actor.entity, candidates)
        .filter(|e| *e != actor.entity)
        .ok_or_else(|| format!("You don't see {} here.", target.describe()))
}

/// What `look <item>` and `examine <item>` show.
pub fn describe_item(entity: Entity, items: &ItemQuery, templates: &ItemTemplates) -> String {
    let Ok((_, item, _)) = items.get(entity) else {
        return String::new();
    };
    match templates.0.get(&item.key) {
        Some(def) if !def.description.is_empty() => def.description.clone(),
        _ => format!("You see nothing special about {}.", item.name),
    }
}

/// Split `get`/`put` arguments: the item, and an optional container.
fn item_and_container(args: &str) -> (Option<Target>, Option<Target>) {
    let mut words = args.split_whitespace();
    let item = words.next().and_then(Target::parse);
    let container = words.next().and_then(Target::parse);
    (item, container)
}

#[allow(clippy::too_many_arguments)]
fn get(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    sheets: Query<&Attributes>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let out = get_items(&inv, &mut mover, &keywords, &sheets);
    deliver(out, &mut players);
}

fn get_items(
    inv: &Invocation,
    mover: &mut Mover,
    keywords: &Query<&Keywords>,
    sheets: &Query<&Attributes>,
) -> Vec<Outgoing> {
    let me = mover.me();
    let line = |text: String| vec![Outgoing::line(me, text)];
    let (Some(what), container) = item_and_container(&inv.args) else {
        return line("Get what?".into());
    };
    let (source, from) = match container {
        Some(target) => match mover.find_container(&target, keywords) {
            Ok(container) => {
                if mover
                    .snapshot
                    .get(container)
                    .is_some_and(|i| i.state != DoorState::Open)
                {
                    return line(format!(
                        "{} is closed.",
                        capitalized(&mover.name(container))
                    ));
                }
                (ItemLocation::Inside(container), Some(container))
            }
            Err(err) => return line(err),
        },
        None => (ItemLocation::Room(mover.actor.room), None),
    };
    let at_source: Vec<_> = mover.snapshot.at(source).map(|i| i.entity).collect();
    let found: Vec<_> = what
        .find_all(me, with_keywords(at_source, keywords))
        .into_iter()
        .filter(|e| *e != me)
        .collect();
    if found.is_empty() {
        return line(match (&what, from) {
            (Target::All { word: None }, None) => "There's nothing here to pick up.".into(),
            (Target::All { .. }, None) => "There's nothing like that here.".into(),
            (_, Some(container)) => format!(
                "{} doesn't hold anything like that.",
                capitalized(&mover.name(container))
            ),
            (target, None) => format!("You don't see {} here.", target.describe()),
        });
    }

    let already_carried = from.is_some_and(|c| mover.carries(c));
    let mut out = Vec::new();
    for entity in found {
        let name = mover.name(entity);
        if mover.snapshot.is_fixed(entity) {
            out.push(Outgoing::line(
                me,
                format!("{} won't budge.", capitalized(&name)),
            ));
            continue;
        }
        if !already_carried && mover.too_heavy(entity, sheets) {
            out.push(Outgoing::line(
                me,
                format!("You can't carry {name} as well; it's too much."),
            ));
            break;
        }
        let Some(target) = mover.snapshot.place_for(me, entity, from) else {
            out.push(Outgoing::line(
                me,
                format!("You have nowhere to put {name}."),
            ));
            break;
        };
        mover.move_to(entity, target);
        let phrase = mover.where_phrase(target);
        let text = match (from, target) {
            (None, ItemLocation::Held(_)) => format!("You pick up {name}."),
            (None, _) => format!("You pick up {name} and put it {phrase}."),
            (Some(c), ItemLocation::Held(_)) => format!(
                "You take {name} out of {}.",
                mover
                    .where_phrase(ItemLocation::Inside(c))
                    .trim_start_matches("in ")
            ),
            (Some(c), _) => format!(
                "You take {name} out of {} and put it {phrase}.",
                mover
                    .where_phrase(ItemLocation::Inside(c))
                    .trim_start_matches("in ")
            ),
        };
        out.push(Outgoing::line(me, text));
        if !already_carried {
            out.push(Outgoing::Room {
                room: mover.actor.room,
                except: me,
                text: format!("{} picks up {name}.\r\n", mover.actor.name),
            });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn put(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let out =
        put_items(&inv, &mut mover, &keywords).unwrap_or_else(|err| vec![Outgoing::line(me, err)]);
    deliver(out, &mut players);
}

fn put_items(
    inv: &Invocation,
    mover: &mut Mover,
    keywords: &Query<&Keywords>,
) -> Result<Vec<Outgoing>, String> {
    let me = mover.me();
    let (Some(what), Some(into)) = item_and_container(&inv.args) else {
        return Err(inv.usage_error());
    };
    let container = mover.find_container(&into, keywords)?;
    // Anything at hand you carry that isn't worn, isn't already in it, and isn't around it.
    let candidates: Vec<_> = mover
        .snapshot
        .at_hand_carried(me, mover.actor.room)
        .into_iter()
        .filter_map(|e| mover.snapshot.get(e))
        .filter(|info| !matches!(info.location, ItemLocation::Worn(..)))
        .filter(|info| info.location != ItemLocation::Inside(container))
        .filter(|info| !mover.snapshot.is_within(container, info.entity))
        .map(|info| info.entity)
        .collect();
    let found: Vec<_> = what
        .find_all(me, with_keywords(candidates, keywords))
        .into_iter()
        .filter(|e| *e != me)
        .collect();
    if found.is_empty() {
        let worn: Vec<_> = mover
            .snapshot
            .carried_by(me)
            .filter(|info| matches!(info.location, ItemLocation::Worn(..)))
            .map(|info| info.entity)
            .collect();
        if let Some(entity) = what
            .find_one(me, with_keywords(worn, keywords))
            .filter(|e| *e != me)
        {
            return Err(format!(
                "You're wearing {}; take it off first.",
                mover.name(entity)
            ));
        }
        return Err(format!("You aren't carrying {}.", what.describe()));
    }
    let mut out = Vec::new();
    for entity in found {
        if let Err(err) = mover.snapshot.fits(container, entity) {
            out.push(Outgoing::line(me, err));
            break;
        }
        let name = mover.name(entity);
        let target = ItemLocation::Inside(container);
        mover.move_to(entity, target);
        out.push(Outgoing::line(
            me,
            format!("You put {name} {}.", mover.where_phrase(target)),
        ));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn drop(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let out = drop_items(&inv, &mut mover, &keywords);
    deliver(out, &mut players);
}

fn drop_items(inv: &Invocation, mover: &mut Mover, keywords: &Query<&Keywords>) -> Vec<Outgoing> {
    let me = mover.me();
    let room = mover.actor.room;
    let Some(target) = Target::parse(&inv.args) else {
        return vec![Outgoing::line(me, "Drop what?")];
    };
    // `drop all` empties your hands and containers but leaves on what you wear.
    let dropping_all = matches!(target, Target::All { .. });
    let candidates: Vec<_> = mover
        .snapshot
        .at_hand_carried(me, room)
        .into_iter()
        .filter_map(|e| mover.snapshot.get(e))
        .filter(|info| !dropping_all || !matches!(info.location, ItemLocation::Worn(..)))
        .map(|info| info.entity)
        .collect();
    let found: Vec<_> = target
        .find_all(me, with_keywords(candidates, keywords))
        .into_iter()
        .filter(|e| *e != me)
        .collect();
    if found.is_empty() {
        let text = if dropping_all {
            "You aren't carrying anything like that.".to_string()
        } else {
            format!("You aren't carrying {}.", target.describe())
        };
        return vec![Outgoing::line(me, text)];
    }
    let mut out = Vec::new();
    for entity in found {
        // Something inside an item you've already dropped stays inside it.
        if mover.snapshot.root(entity) == Some(ItemLocation::Room(room)) {
            continue;
        }
        let name = mover.name(entity);
        mover.move_to(entity, ItemLocation::Room(room));
        out.push(Outgoing::line(me, format!("You drop {name}.")));
        out.push(Outgoing::Room {
            room,
            except: me,
            text: format!("{} drops {name}.\r\n", mover.actor.name),
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn give(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    sheets: Query<&Attributes>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let out = give_item(&inv, &mut mover, &players, &keywords, &sheets)
        .unwrap_or_else(|err| vec![Outgoing::line(me, err)]);
    deliver(out, &mut players);
}

fn give_item(
    inv: &Invocation,
    mover: &mut Mover,
    players: &PlayerQuery,
    keywords: &Query<&Keywords>,
    sheets: &Query<&Attributes>,
) -> Result<Vec<Outgoing>, String> {
    let me = mover.me();
    let (what, who) = inv
        .args
        .split_once(' ')
        .map(|(w, p)| (w, p.trim()))
        .filter(|(_, p)| !p.is_empty())
        .ok_or_else(|| inv.usage_error())?;
    let what = Target::parse(what).ok_or_else(|| inv.usage_error())?;
    let mine = mover.snapshot.at_hand_carried(me, mover.actor.room);
    let entity = what
        .find_one(me, with_keywords(mine, keywords))
        .filter(|e| *e != me)
        .ok_or_else(|| format!("You aren't carrying {}.", what.describe()))?;
    let who = Target::parse(who).ok_or_else(|| inv.usage_error())?;
    let here = targets_in_room(mover.actor.room, players, keywords);
    let bystanders: Vec<Entity> = here.iter().map(|(e, _)| *e).collect();
    let receiver = who
        .find_one(me, here)
        .and_then(|e| Actor::get(players, e))
        .ok_or_else(|| format!("You don't see {} here.", who.describe()))?;
    if receiver.entity == me {
        return Err("You already have it.".into());
    }
    let name = mover.name(entity);
    let strength = sheets
        .get(receiver.entity)
        .map_or(1, |a| a.get(Attribute::Strength));
    let (_, max) = bulk_limits(strength);
    if mover.snapshot.burden(receiver.entity) + mover.snapshot.total_bulk(entity) > max {
        return Err(format!("{} can't carry {name} as well.", receiver.name));
    }
    let target = mover
        .snapshot
        .place_for(receiver.entity, entity, None)
        .ok_or_else(|| format!("{} has nowhere to put {name}.", receiver.name))?;
    mover.move_to(entity, target);

    let mut out = vec![
        Outgoing::line(me, format!("You give {name} to {}.", receiver.name)),
        Outgoing::line(
            receiver.entity,
            format!("{} gives you {name}.", mover.actor.name),
        ),
    ];
    let seen = format!("{} gives {name} to {}.", mover.actor.name, receiver.name);
    out.extend(
        bystanders
            .into_iter()
            .filter(|e| *e != me && *e != receiver.entity)
            .map(|e| Outgoing::line(e, seen.clone())),
    );
    Ok(out)
}

fn sheath(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let reply: Reply = (|| {
        // Something you carry and aren't wearing; with no argument, only what's in your hands.
        let candidates: Vec<_> = mover
            .snapshot
            .at_hand_carried(me, mover.actor.room)
            .into_iter()
            .filter_map(|e| mover.snapshot.get(e))
            .filter(|info| !matches!(info.location, ItemLocation::Worn(..)))
            .filter(|info| !inv.args.is_empty() || info.location == ItemLocation::Held(me))
            .map(|info| info.entity)
            .collect();
        let entity = if inv.args.is_empty() {
            candidates
                .into_iter()
                .find(|e| mover.snapshot.specialised_for(me, *e, None).is_some())
                .ok_or("You aren't holding anything you can sheath.")?
        } else {
            let target = Target::parse(&inv.args).ok_or_else(|| inv.usage_error())?;
            target
                .find_one(me, with_keywords(candidates, &keywords))
                .filter(|e| *e != me)
                .ok_or_else(|| format!("You aren't carrying {}.", target.describe()))?
        };
        let name = mover.name(entity);
        let current = mover.snapshot.get(entity).map(|info| info.location);
        if let Some(ItemLocation::Inside(container)) = current
            && mover
                .snapshot
                .get(container)
                .is_some_and(|info| Snapshot::accepts_count(info) > 0)
        {
            return Err(format!(
                "{} is already {}.",
                capitalized(&name),
                mover.where_phrase(ItemLocation::Inside(container))
            ));
        }
        let holder = mover
            .snapshot
            .specialised_for(me, entity, None)
            .filter(|h| current != Some(ItemLocation::Inside(*h)))
            .ok_or_else(|| format!("You're wearing nothing to sheath {name} in."))?;
        let target = ItemLocation::Inside(holder);
        mover.move_to(entity, target);
        Ok(format!("You sheath {name} {}.", mover.where_phrase(target)))
    })();
    deliver(vec![reply_line(me, reply)], &mut players);
}

fn draw(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let reply: Reply = (|| {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        // Anything inside a container you carry.
        let entity = mover
            .find_one(&inv.args, &keywords, |info| {
                matches!(info.location, ItemLocation::Inside(_))
            })
            .ok()
            .filter(|e| mover.carries(*e))
            .ok_or_else(|| format!("You have no {} to draw.", inv.args))?;
        let name = mover.name(entity);
        if mover.snapshot.at(ItemLocation::Held(me)).count() >= HANDS {
            return Err(format!("Your hands are full; you can't draw {name}."));
        }
        mover.move_to(entity, ItemLocation::Held(me));
        Ok(format!("You draw {name}."))
    })();
    deliver(vec![reply_line(me, reply)], &mut players);
}

#[allow(clippy::too_many_arguments)]
fn hold(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    sheets: Query<&Attributes>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let reply: Reply = (|| {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        let entity = mover.find_one(&inv.args, &keywords, |_| true)?;
        let name = mover.name(entity);
        if mover.snapshot.get(entity).map(|i| i.location) == Some(ItemLocation::Held(me)) {
            return Err(format!("You're already holding {name}."));
        }
        if mover.snapshot.is_fixed(entity) {
            return Err(format!("{} won't budge.", capitalized(&name)));
        }
        if mover.snapshot.at(ItemLocation::Held(me)).count() >= HANDS {
            return Err("Your hands are full.".into());
        }
        if !mover.carries(entity) && mover.too_heavy(entity, &sheets) {
            return Err(format!("You can't carry {name} as well; it's too much."));
        }
        mover.move_to(entity, ItemLocation::Held(me));
        Ok(format!("You hold {name}."))
    })();
    deliver(vec![reply_line(me, reply)], &mut players);
}

#[allow(clippy::too_many_arguments)]
fn wear(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    sheets: Query<&Attributes>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let reply: Reply = (|| {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        let entity = mover.find_one(&inv.args, &keywords, |info| {
            !matches!(info.location, ItemLocation::Worn(..))
        })?;
        let name = mover.name(entity);
        let slot = mover
            .snapshot
            .get(entity)
            .and_then(|info| info.def.as_ref())
            .and_then(|def| def.wear)
            .ok_or_else(|| format!("You can't wear {name}."))?;
        let worn: Vec<_> = mover.snapshot.at(ItemLocation::Worn(me, slot)).collect();
        if worn.len() >= slot.capacity() {
            return Err(match worn.first() {
                Some(other) if slot.capacity() == 1 => format!(
                    "You're already wearing {} on your {}.",
                    other.name,
                    slot.name()
                ),
                _ => format!("Your {} is full.", slot.name()),
            });
        }
        if !mover.carries(entity) && mover.too_heavy(entity, &sheets) {
            return Err(format!("You can't carry {name} as well; it's too much."));
        }
        mover.move_to(entity, ItemLocation::Worn(me, slot));
        Ok(match slot {
            Slot::Belt => format!("You fasten {name} to your belt."),
            Slot::Back => format!("You sling {name} onto your back."),
            _ => format!("You put on {name}."),
        })
    })();
    deliver(vec![reply_line(me, reply)], &mut players);
}

#[allow(clippy::too_many_arguments)]
fn remove(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut mover = Mover::new(actor, &mut items, &players, &rooms, &templates, &db);
    let me = mover.me();
    let reply: Reply = (|| {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        let entity = mover
            .find_one(
                &inv.args,
                &keywords,
                |info| matches!(info.location, ItemLocation::Worn(who, _) if who == me),
            )
            .map_err(|_| format!("You aren't wearing anything called '{}'.", inv.args))?;
        let name = mover.name(entity);
        if mover.snapshot.at(ItemLocation::Held(me)).count() >= HANDS {
            return Err(format!("Your hands are full; you can't take off {name}."));
        }
        mover.move_to(entity, ItemLocation::Held(me));
        Ok(format!("You take off {name} and hold it."))
    })();
    deliver(vec![reply_line(me, reply)], &mut players);
}

#[allow(clippy::too_many_arguments)]
fn create(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply: Reply = (|| {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        let (_, here, _) = rooms.get(actor.room).map_err(|_| String::new())?;
        let key = ItemKey::parse(&inv.args, &here.zone);
        let def = templates
            .0
            .get(&key)
            .ok_or_else(|| format!("There's no item template [{key}]."))?;
        // Fixed things go in the room; anything else goes in the builder's hand, or at their
        // feet if their hands are full.
        let held = items
            .iter()
            .filter(|(_, _, loc)| **loc == ItemLocation::Held(actor.entity))
            .count();
        let (location, place, where_to) = if def.fixed || held >= HANDS {
            let place = ItemPlace::Room(here.to_string());
            (ItemLocation::Room(actor.room), place, "here")
        } else {
            let place = ItemPlace::Held(actor.character_id);
            (ItemLocation::Held(actor.entity), place, "in your hand")
        };
        let id = db
            .create_item(&key.to_string(), &place)
            .map_err(|err| format!("Couldn't save the item: {err}"))?;
        let name = def.name.clone();
        spawn_item(&mut commands, &templates, key.clone(), Some(id), location);
        Ok(format!("You create {name} [{key}] {where_to}."))
    })();
    deliver(vec![reply_line(actor.entity, reply)], &mut players);
}

fn destroy(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    items: ItemQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply: Reply =
        find_item(&inv.args, &actor, &items, &keywords, &templates).and_then(|entity| {
            let (_, item, _) = items.get(entity).map_err(|_| String::new())?;
            // Deleting the row deletes the rows of everything inside it, too.
            if let Some(id) = item.id
                && let Err(err) = db.delete_item(id)
            {
                return Err(format!("Couldn't delete the item: {err}"));
            }
            let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), &templates);
            for info in &snapshot.items {
                if snapshot.is_within(info.entity, entity) {
                    commands.entity(info.entity).despawn();
                }
            }
            Ok(format!("You destroy {}.", item.name))
        });
    deliver(vec![reply_line(actor.entity, reply)], &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Bulk, ContainerDef};

    fn def(name: &str, bulk: Bulk) -> ItemDef {
        ItemDef {
            bulk,
            ..ItemDef::named(name)
        }
    }

    fn container(name: &str, bulk: Bulk, capacity: Capacity, accepts: &[&str]) -> ItemDef {
        ItemDef {
            container: Some(ContainerDef {
                capacity,
                accepts: accepts.iter().map(|s| s.to_string()).collect(),
                max_item: None,
                state: DoorState::Open,
                lock: None,
            }),
            ..def(name, bulk)
        }
    }

    struct Fixture {
        world: World,
        snapshot: Snapshot,
        player: Entity,
        room: Entity,
    }

    impl Fixture {
        fn new() -> Self {
            let mut world = World::new();
            let player = world.spawn_empty().id();
            let room = world.spawn_empty().id();
            Self {
                world,
                snapshot: Snapshot { items: Vec::new() },
                player,
                room,
            }
        }

        fn add(&mut self, def: ItemDef, tags: &[&str], location: ItemLocation) -> Entity {
            let entity = self.world.spawn_empty().id();
            self.snapshot.items.push(ItemInfo {
                entity,
                name: def.name.clone(),
                location,
                def: Some(ItemDef {
                    tags: tags.iter().map(|s| s.to_string()).collect(),
                    ..def
                }),
                state: DoorState::Open,
            });
            entity
        }

        fn on_floor(&mut self, def: ItemDef, tags: &[&str]) -> Entity {
            let room = self.room;
            self.add(def, tags, ItemLocation::Room(room))
        }
    }

    #[test]
    fn specialised_containers_are_filled_first() {
        let mut f = Fixture::new();
        let p = f.player;
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        let keyring = f.add(
            container("a keyring", Bulk::Negligible, Capacity::Count(10), &["key"]),
            &[],
            ItemLocation::Worn(p, Slot::Belt),
        );
        let key = f.on_floor(def("a key", Bulk::Negligible), &["key"]);
        let torch = f.on_floor(def("a torch", Bulk::Light), &[]);

        let place = |item| f.snapshot.place_for(p, item, None);
        assert_eq!(place(key), Some(ItemLocation::Inside(keyring)));
        assert_eq!(place(torch), Some(ItemLocation::Inside(backpack)));
    }

    #[test]
    fn weapons_go_to_sheaths_and_loops() {
        let mut f = Fixture::new();
        let p = f.player;
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        let sheath = f.add(
            container("a sheath", Bulk::Light, Capacity::Count(1), &["blade"]),
            &[],
            ItemLocation::Worn(p, Slot::Belt),
        );
        let loop_ = f.add(
            container(
                "a loop",
                Bulk::Negligible,
                Capacity::Count(1),
                &["axe", "mace"],
            ),
            &[],
            ItemLocation::Worn(p, Slot::Belt),
        );
        let sword = f.on_floor(def("a sword", Bulk::Bulk(1)), &["blade"]);
        let axe = f.on_floor(def("an axe", Bulk::Light), &["axe"]);

        assert_eq!(f.snapshot.specialised_for(p, sword, None), Some(sheath));
        assert_eq!(f.snapshot.specialised_for(p, axe, None), Some(loop_));
        f.snapshot.set_location(sword, ItemLocation::Inside(sheath));
        let second = f.on_floor(def("a second sword", Bulk::Bulk(1)), &["blade"]);
        let place = f.snapshot.place_for(p, second, None);
        assert_eq!(
            place,
            Some(ItemLocation::Inside(backpack)),
            "the sheath holds one"
        );
    }

    #[test]
    fn smaller_general_containers_come_first_then_hands() {
        let mut f = Fixture::new();
        let p = f.player;
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        let pouch = f.add(
            container("a pouch", Bulk::Negligible, Capacity::Light(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Belt),
        );
        let torch = f.on_floor(def("a torch", Bulk::Light), &[]);
        let sword = f.on_floor(def("a sword", Bulk::Bulk(1)), &[]);
        let anvil = f.on_floor(def("an anvil", Bulk::Bulk(8)), &[]);

        let place = |item| f.snapshot.place_for(p, item, None);
        assert_eq!(place(torch), Some(ItemLocation::Inside(pouch)));
        assert_eq!(place(sword), Some(ItemLocation::Inside(backpack)));
        assert_eq!(place(anvil), Some(ItemLocation::Held(p)));
    }

    #[test]
    fn hands_hold_two_things() {
        let mut f = Fixture::new();
        let p = f.player;
        f.add(def("a torch", Bulk::Light), &[], ItemLocation::Held(p));
        let stick = f.on_floor(def("a stick", Bulk::Light), &[]);
        assert_eq!(
            f.snapshot.place_for(p, stick, None),
            Some(ItemLocation::Held(p))
        );
        f.snapshot.set_location(stick, ItemLocation::Held(p));
        let rock = f.on_floor(def("a rock", Bulk::Light), &[]);
        assert_eq!(f.snapshot.place_for(p, rock, None), None);
    }

    #[test]
    fn capacity_accepts_size_and_cycles_are_enforced() {
        let mut f = Fixture::new();
        let p = f.player;
        let pouch = f.add(
            container("a pouch", Bulk::Negligible, Capacity::Light(2), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Belt),
        );
        let keyring = f.add(
            container("a keyring", Bulk::Negligible, Capacity::Count(1), &["key"]),
            &[],
            ItemLocation::Held(p),
        );
        let mut pockets = container("a coat", Bulk::Light, Capacity::Count(2), &[]);
        if let Some(spec) = pockets.container.as_mut() {
            spec.max_item = Some(Bulk::Light);
        }
        let coat = f.add(pockets, &[], ItemLocation::Worn(p, Slot::Body));
        f.add(def("a", Bulk::Light), &[], ItemLocation::Inside(pouch));
        f.add(def("b", Bulk::Light), &[], ItemLocation::Inside(pouch));
        let c = f.add(def("c", Bulk::Light), &[], ItemLocation::Held(p));
        let sword = f.on_floor(def("a sword", Bulk::Bulk(1)), &[]);

        assert!(f.snapshot.fits(pouch, c).is_err(), "the pouch is full");
        assert!(
            f.snapshot.fits(keyring, c).is_err(),
            "keyrings only take keys"
        );
        assert!(
            f.snapshot.fits(coat, sword).is_err(),
            "a sword won't fit a pocket"
        );
        assert!(f.snapshot.fits(coat, c).is_ok());
        assert!(f.snapshot.fits(pouch, pouch).is_err(), "not inside itself");
        assert_eq!(f.snapshot.total_bulk(pouch), 2);
    }

    #[test]
    fn nested_containers_count_their_contents_and_never_loop() {
        let mut f = Fixture::new();
        let p = f.player;
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        let pouch = f.add(
            container("a pouch", Bulk::Negligible, Capacity::Light(4), &[]),
            &[],
            ItemLocation::Inside(backpack),
        );
        f.add(def("a loaf", Bulk::Light), &[], ItemLocation::Inside(pouch));
        let sword = f.add(
            def("a sword", Bulk::Bulk(1)),
            &[],
            ItemLocation::Inside(backpack),
        );

        assert_eq!(
            f.snapshot.total_bulk(backpack),
            10 + 1 + 10,
            "pack, loaf and sword"
        );
        assert_eq!(f.snapshot.burden(p), 21);
        assert_eq!(
            f.snapshot.root(pouch),
            Some(ItemLocation::Worn(p, Slot::Back))
        );
        assert!(
            f.snapshot.fits(pouch, backpack).is_err(),
            "a pack can't go in its own pouch"
        );
        assert!(
            f.snapshot.fits(pouch, sword).is_err(),
            "a sword won't fit in the pouch"
        );
        assert_eq!(
            f.snapshot.place_for(p, sword, Some(backpack)),
            Some(ItemLocation::Held(p)),
            "containers inside containers aren't automatic targets"
        );
    }

    #[test]
    fn only_one_container_deep_is_at_hand() {
        let mut f = Fixture::new();
        let (p, room) = (f.player, f.room);
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        let pouch = f.add(
            container("a pouch", Bulk::Negligible, Capacity::Light(4), &[]),
            &[],
            ItemLocation::Inside(backpack),
        );
        let bread = f.add(def("a loaf", Bulk::Light), &[], ItemLocation::Inside(pouch));
        let rock = f.on_floor(def("a rock", Bulk::Light), &[]);
        assert_eq!(
            f.snapshot.place_for(p, pouch, Some(backpack)),
            Some(ItemLocation::Held(p)),
            "a container taken out goes to your hand, not into another container"
        );

        let at_hand = f.snapshot.at_hand(p, room);
        assert!(at_hand.contains(&backpack) && at_hand.contains(&pouch));
        assert!(!at_hand.contains(&bread), "the loaf is packed two deep");
        assert_eq!(at_hand.last(), Some(&rock), "your own things come first");
        assert!(!f.snapshot.at_hand_carried(p, room).contains(&rock));

        f.snapshot.set_location(pouch, ItemLocation::Held(p));
        assert!(
            f.snapshot.at_hand(p, room).contains(&bread),
            "out of the pack, it's at hand"
        );
    }

    #[test]
    fn burden_counts_everything_carried_once() {
        let mut f = Fixture::new();
        let p = f.player;
        let backpack = f.add(
            container("a backpack", Bulk::Bulk(1), Capacity::Bulk(4), &[]),
            &[],
            ItemLocation::Worn(p, Slot::Back),
        );
        f.add(
            def("a sword", Bulk::Bulk(1)),
            &[],
            ItemLocation::Inside(backpack),
        );
        f.add(def("a torch", Bulk::Light), &[], ItemLocation::Held(p));
        f.on_floor(def("a rock", Bulk::Bulk(2)), &[]);
        assert_eq!(f.snapshot.burden(p), 21);
        assert_eq!(bulk_limits(1), (55, 105));
        assert_eq!(bulk_limits(0), (50, 100));
    }
}
