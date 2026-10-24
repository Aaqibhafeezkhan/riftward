//! Items: entities made from templates in zone files, lying in rooms or carried by characters.
//!
//! An item becomes "real", saved in the `items` table, the moment a player touches it; every move
//! after that updates its row. Real items lying in rooms are restored at startup, and the items a
//! character carries are loaded when they enter the game and unloaded when they leave.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, ItemPlace, Permission};
use crate::net::{self, Connection};
use crate::output::{
    Actor, Outgoing, PlayerQuery, Reply, RoomQuery, deliver, reply_line, targets_in_room,
};
use crate::targeting::{Keywords, Target};
use crate::world::{ItemDef, ItemKey, Player, RoomIndex, RoomKey, START_ROOM, ZoneDef};

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
                    .summary("list what you're carrying"),
                inventory,
            )
            .add_command(
                CommandSpec::new("examine", Information)
                    .shortcuts(&["exa"])
                    .usage("examine <item>")
                    .summary("look closely at something here or in your pack"),
                examine,
            )
            .add_command(
                CommandSpec::new("get", Interaction)
                    .shortcuts(&["take"])
                    .usage("get <item> | get all | get all.<item>")
                    .summary("pick something up"),
                get,
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
                CommandSpec::new("create", Category::Building)
                    .usage("create <item>")
                    .summary("make an item from a template (`item` or `zone:item`)")
                    .permission(Permission::Build),
                create,
            )
            .add_command(
                CommandSpec::new("destroy", Category::Building)
                    .usage("destroy <item>")
                    .summary("destroy an item here or in your pack, for good")
                    .permission(Permission::Build),
                destroy,
            );
    }
}

/// An item in the world.
#[derive(Component, Debug)]
pub struct Item {
    pub key: ItemKey,
    pub name: String,
    /// Its row in the `items` table, once it's real.
    pub id: Option<i64>,
}

/// Where an item is.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemLocation {
    Room(Entity),
    Carried(Entity),
}

/// Read-only access to every item, for looking and listing.
pub type ItemQuery<'w, 's> = Query<'w, 's, (Entity, &'static Item, &'static ItemLocation)>;

/// Write access to every item, for commands that move them.
type ItemMutQuery<'w, 's> = Query<'w, 's, (Entity, &'static mut Item, &'static mut ItemLocation)>;

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

/// The items at `location` with their keywords, oldest first, for targeting. `placed` is every
/// item's entity and location.
pub fn items_at<'a>(
    location: ItemLocation,
    placed: impl IntoIterator<Item = (Entity, ItemLocation)>,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    let mut found: Vec<_> = placed
        .into_iter()
        .filter(|(_, loc)| *loc == location)
        .filter_map(|(entity, _)| Some((entity, keywords.get(entity).ok()?)))
        .collect();
    found.sort_by_key(|(entity, _)| *entity);
    found
}

/// Names of the items at `location`, oldest first, with repeats counted: "a torch (x2)".
pub fn item_list(location: ItemLocation, items: &ItemQuery) -> Vec<String> {
    let mut here: Vec<_> = items
        .iter()
        .filter(|(_, _, loc)| **loc == location)
        .map(|(entity, item, _)| (entity, item.name.as_str()))
        .collect();
    here.sort_by_key(|(entity, _)| *entity);

    let mut counted: Vec<(&str, usize)> = Vec::new();
    for (_, name) in here {
        match counted.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counted.push((name, 1)),
        }
    }
    counted
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
    let item = Item {
        key,
        name: def.name.clone(),
        id,
    };
    Some(commands.spawn((item, def.keywords(), location)).id())
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

/// Once the world has loaded, put real items back in the rooms they were left in. Items whose
/// room no longer exists are moved to the start room.
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
        let key = ItemKey::parse(&template, "");
        spawn_item(
            &mut commands,
            &templates,
            key,
            Some(id),
            ItemLocation::Room(room),
        );
    }
    info!("restored {count} items lying in rooms");
}

/// Load a character's carried items when they enter the game.
fn load_carried_items(
    mut commands: Commands,
    entered: Query<(Entity, &Player), Added<Player>>,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    for (entity, player) in &entered {
        match db.items_carried(player.character_id) {
            Ok(rows) => {
                for (id, template) in rows {
                    let key = ItemKey::parse(&template, "");
                    let location = ItemLocation::Carried(entity);
                    spawn_item(&mut commands, &templates, key, Some(id), location);
                }
            }
            Err(err) => error!("failed to load items for {}: {err}", player.name),
        }
    }
}

/// When a player leaves, their carried items leave with them. They're already saved.
fn unload_carried_items(
    mut commands: Commands,
    items: Query<(Entity, &ItemLocation), With<Item>>,
    connections: Query<&Connection>,
) {
    for (entity, location) in &items {
        if let ItemLocation::Carried(holder) = location
            && connections.get(*holder).is_ok_and(|conn| conn.closing)
        {
            commands.entity(entity).despawn();
        }
    }
}

/// Save an item at `place`: creates its row the first time a player touches it.
fn save(db: &Database, item: &mut Item, place: &ItemPlace) {
    let result = match item.id {
        Some(id) => db.move_item(id, place),
        None => db
            .create_item(&item.key.to_string(), place)
            .map(|id| item.id = Some(id)),
    };
    if let Err(err) = result {
        error!("failed to save item [{}]: {err}", item.key);
    }
}

fn inventory(In(inv): In<Invocation>, mut players: PlayerQuery, items: ItemQuery) {
    let list = item_list(ItemLocation::Carried(inv.entity), &items);
    let text = if list.is_empty() {
        "You aren't carrying anything.".to_string()
    } else {
        format!("You are carrying:\r\n  {}", list.join("\r\n  "))
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
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
    let text = match find_item(&inv.args, &actor, &items, &keywords) {
        Ok(entity) => describe_item(entity, &items, &templates),
        Err(err) => err,
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// One item the player names, in their pack or here in the room (pack first).
pub fn find_item(
    arg: &str,
    actor: &Actor,
    items: &ItemQuery,
    keywords: &Query<&Keywords>,
) -> Result<Entity, String> {
    let target = Target::parse(arg).ok_or_else(|| "Look at what?".to_string())?;
    let placed: Vec<_> = items.iter().map(|(e, _, loc)| (e, *loc)).collect();
    let mut candidates = items_at(
        ItemLocation::Carried(actor.entity),
        placed.clone(),
        keywords,
    );
    candidates.extend(items_at(ItemLocation::Room(actor.room), placed, keywords));
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

/// Resolve a `get`/`drop` target among the items at `location`: one item, or every match for
/// `all` forms. On failure, returns the parsed target (if any) so the caller can word the reply.
fn targets(
    arg: &str,
    viewer: Entity,
    location: ItemLocation,
    items: &ItemMutQuery,
    keywords: &Query<&Keywords>,
) -> Result<Vec<Entity>, Option<Target>> {
    let target = Target::parse(arg).ok_or(None)?;
    let placed = items.iter().map(|(e, _, loc)| (e, *loc));
    let found: Vec<_> = target
        .find_all(viewer, items_at(location, placed, keywords))
        .into_iter()
        .filter(|e| *e != viewer)
        .collect();
    if found.is_empty() {
        return Err(Some(target));
    }
    Ok(found)
}

fn get(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let here = ItemLocation::Room(actor.room);
    let mut out = Vec::new();
    match targets(&inv.args, actor.entity, here, &items, &keywords) {
        Ok(found) => {
            for entity in found {
                let Ok((_, mut item, mut location)) = items.get_mut(entity) else {
                    continue;
                };
                save(&db, &mut item, &ItemPlace::Carried(actor.character_id));
                *location = ItemLocation::Carried(actor.entity);
                out.push(Outgoing::line(
                    actor.entity,
                    format!("You pick up {}.", item.name),
                ));
                out.push(Outgoing::Room {
                    room: actor.room,
                    except: actor.entity,
                    text: format!("{} picks up {}.\r\n", actor.name, item.name),
                });
            }
        }
        Err(None) => out.push(Outgoing::line(actor.entity, "Get what?")),
        Err(Some(Target::All { word: None })) => out.push(Outgoing::line(
            actor.entity,
            "There's nothing here to pick up.",
        )),
        Err(Some(Target::All { .. })) => out.push(Outgoing::line(
            actor.entity,
            "There's nothing like that here.",
        )),
        Err(Some(target)) => out.push(Outgoing::line(
            actor.entity,
            format!("You don't see {} here.", target.describe()),
        )),
    }
    deliver(out, &mut players);
}

fn drop(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let Ok((_, room_key, _)) = rooms.get(actor.room) else {
        return;
    };
    let pack = ItemLocation::Carried(actor.entity);
    let mut out = Vec::new();
    match targets(&inv.args, actor.entity, pack, &items, &keywords) {
        Ok(found) => {
            for entity in found {
                let Ok((_, mut item, mut location)) = items.get_mut(entity) else {
                    continue;
                };
                save(&db, &mut item, &ItemPlace::Room(room_key.to_string()));
                *location = ItemLocation::Room(actor.room);
                out.push(Outgoing::line(
                    actor.entity,
                    format!("You drop {}.", item.name),
                ));
                out.push(Outgoing::Room {
                    room: actor.room,
                    except: actor.entity,
                    text: format!("{} drops {}.\r\n", actor.name, item.name),
                });
            }
        }
        Err(None) => out.push(Outgoing::line(actor.entity, "Drop what?")),
        Err(Some(Target::All { .. })) => out.push(Outgoing::line(
            actor.entity,
            "You aren't carrying anything like that.",
        )),
        Err(Some(target)) => out.push(Outgoing::line(
            actor.entity,
            format!("You aren't carrying {}.", target.describe()),
        )),
    }
    deliver(out, &mut players);
}

fn give(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut items: ItemMutQuery,
    keywords: Query<&Keywords>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut handover = || -> Result<Vec<Outgoing>, String> {
        let (what, who) = inv
            .args
            .split_once(' ')
            .map(|(w, p)| (w, p.trim()))
            .filter(|(_, p)| !p.is_empty())
            .ok_or_else(|| inv.usage_error())?;
        let what = Target::parse(what).ok_or_else(|| inv.usage_error())?;
        let placed = items.iter().map(|(e, _, loc)| (e, *loc));
        let pack = items_at(ItemLocation::Carried(actor.entity), placed, &keywords);
        let entity = what
            .find_one(actor.entity, pack)
            .filter(|e| *e != actor.entity)
            .ok_or_else(|| format!("You aren't carrying {}.", what.describe()))?;
        let who = Target::parse(who).ok_or_else(|| inv.usage_error())?;
        let receiver = who
            .find_one(
                actor.entity,
                targets_in_room(actor.room, &players, &keywords),
            )
            .and_then(|e| Actor::get(&players, e))
            .ok_or_else(|| format!("You don't see {} here.", who.describe()))?;
        if receiver.entity == actor.entity {
            return Err("You already have it.".into());
        }

        let bystanders: Vec<Entity> = targets_in_room(actor.room, &players, &keywords)
            .into_iter()
            .map(|(e, _)| e)
            .filter(|e| *e != actor.entity && *e != receiver.entity)
            .collect();
        let (_, mut item, mut location) = items.get_mut(entity).map_err(|_| String::new())?;
        save(&db, &mut item, &ItemPlace::Carried(receiver.character_id));
        *location = ItemLocation::Carried(receiver.entity);
        let mut out = vec![
            Outgoing::line(
                actor.entity,
                format!("You give {} to {}.", item.name, receiver.name),
            ),
            Outgoing::line(
                receiver.entity,
                format!("{} gives you {}.", actor.name, item.name),
            ),
        ];
        let seen = format!("{} gives {} to {}.", actor.name, item.name, receiver.name);
        out.extend(
            bystanders
                .into_iter()
                .map(|e| Outgoing::line(e, seen.clone())),
        );
        Ok(out)
    };
    let out = match handover() {
        Ok(out) => out,
        Err(err) => vec![Outgoing::line(actor.entity, err)],
    };
    deliver(out, &mut players);
}

fn create(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    templates: Res<ItemTemplates>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut make = || -> Reply {
        if inv.args.is_empty() {
            return Err(inv.usage_error());
        }
        let (_, here, _) = rooms.get(actor.room).map_err(|_| String::new())?;
        let key = ItemKey::parse(&inv.args, &here.zone);
        let name = templates
            .0
            .get(&key)
            .map(|def| def.name.clone())
            .ok_or_else(|| format!("There's no item template [{key}]."))?;
        let id = db
            .create_item(&key.to_string(), &ItemPlace::Carried(actor.character_id))
            .map_err(|err| format!("Couldn't save the item: {err}"))?;
        let location = ItemLocation::Carried(actor.entity);
        spawn_item(&mut commands, &templates, key.clone(), Some(id), location);
        Ok(format!("You create {name} [{key}]."))
    };
    let reply = make();
    deliver(vec![reply_line(actor.entity, reply)], &mut players);
}

fn destroy(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    items: ItemQuery,
    keywords: Query<&Keywords>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply: Reply = find_item(&inv.args, &actor, &items, &keywords).and_then(|entity| {
        let (_, item, _) = items.get(entity).map_err(|_| String::new())?;
        if let Some(id) = item.id
            && let Err(err) = db.delete_item(id)
        {
            return Err(format!("Couldn't delete the item: {err}"));
        }
        commands.entity(entity).despawn();
        Ok(format!("You destroy {}.", item.name))
    });
    deliver(vec![reply_line(actor.entity, reply)], &mut players);
}
