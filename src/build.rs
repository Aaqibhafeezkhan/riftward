//! Builder and admin commands. Builder commands edit the zone definitions in memory and save them
//! back to their files; `world::sync_world` then applies the change to the room entities, exactly
//! as if the file had been edited by hand.
//!
//! Each command is registered with the command system, which hides it from players without the
//! permission its `CommandSpec` names.

use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::commands::movement::{full_direction, move_player};
use crate::db::{Database, Permission, Role};
use crate::net::Connection;
use crate::output::{
    Actor, Outgoing, PlayerQuery, Reply, RoomQuery, deliver, reply_line, targets_online,
};
use crate::targeting::{Keywords, Target};
use crate::world::{RoomDef, RoomIndex, RoomKey, ZONE_ENTRANCE, ZoneDef};

pub struct BuildPlugin;

impl Plugin for BuildPlugin {
    fn build(&self, app: &mut App) {
        let build = |name, usage, summary| {
            CommandSpec::new(name, Category::Building)
                .usage(usage)
                .summary(summary)
                .permission(Permission::Build)
        };
        let admin = |name, usage, summary| {
            CommandSpec::new(name, Category::Admin)
                .usage(usage)
                .summary(summary)
                .permission(Permission::ManageRoles)
        };
        app.init_resource::<PendingZones>()
            .add_systems(Update, announce_new_zones)
            .add_command(build("where", "where", "show the current room's id"), where_cmd)
            .add_command(build("rooms", "rooms", "list the rooms in this zone"), rooms_cmd)
            .add_command(
                build("goto", "goto <room>", "teleport to a room (`room` or `zone:room`)"),
                goto_cmd,
            )
            .add_command(
                build("dig", "dig <dir> <name>", "create a room through a new exit, with a way back"),
                dig_cmd,
            )
            .add_command(build("rename", "rename <name>", "rename this room"), rename_cmd)
            .add_command(build("desc", "desc <text>", "set this room's description"), desc_cmd)
            .add_command(
                build("link", "link <dir> <room>", "add a one-way exit to an existing room"),
                link_cmd,
            )
            .add_command(build("unlink", "unlink <dir>", "remove an exit"), unlink_cmd)
            .add_command(build("zones", "zones", "list all zones"), zones_cmd)
            .add_command(
                build(
                    "zone",
                    "zone create <id> <name> | zone rename <name>",
                    "create a zone, or rename the one you're in",
                ),
                zone_cmd,
            )
            .add_command(
                admin(
                    "role",
                    "role <account> <player|builder|admin>",
                    "change an account's role",
                ),
                role_cmd,
            )
            .add_command(
                admin(
                    "setattr",
                    "setattr <player> <attribute> <rating>",
                    "set an online player's attribute (ignores Karma)",
                ),
                setattr_cmd,
            );
    }
}

/// Zones created in game whose files haven't been picked up by the file watcher yet, and the
/// builder to tell when they're ready.
#[derive(Resource, Default)]
struct PendingZones(Vec<(String, Entity)>);

/// The builder running a command and the room they're standing in.
fn locate(players: &PlayerQuery, rooms: &RoomQuery, entity: Entity) -> Option<(Actor, RoomKey)> {
    let actor = Actor::get(players, entity)?;
    let (_, here, _) = rooms.get(actor.room).ok()?;
    let here = here.clone();
    Some((actor, here))
}

fn where_cmd(In(inv): In<Invocation>, mut players: PlayerQuery, rooms: RoomQuery) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    deliver(vec![Outgoing::line(inv.entity, format!("You are in [{here}]."))], &mut players);
}

fn rooms_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    zones: Res<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = list_rooms(&here, &zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn goto_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    db: Res<Database>,
) {
    let Some((actor, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    if let Err(err) = goto(&actor, &here, &inv, &mut players, &rooms, &index, &db, &mut out) {
        out.push(reply_line(inv.entity, Err(err)));
    }
    deliver(out, &mut players);
}

/// A builder command that edits the zone they're standing in, by calling `$edit`.
macro_rules! zone_edit_command {
    ($name:ident, $edit:ident) => {
        fn $name(
            In(inv): In<Invocation>,
            mut players: PlayerQuery,
            rooms: RoomQuery,
            mut zones: ResMut<Assets<ZoneDef>>,
        ) {
            let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
                return;
            };
            let reply = $edit(&here, &inv, &mut zones);
            deliver(vec![reply_line(inv.entity, reply)], &mut players);
        }
    };
}

zone_edit_command!(dig_cmd, dig);
zone_edit_command!(rename_cmd, rename);
zone_edit_command!(desc_cmd, describe);
zone_edit_command!(unlink_cmd, unlink);

fn link_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    mut zones: ResMut<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = link(&here, &inv, &index, &mut zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn zones_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    zones: Res<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = list_zones(&here, &zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn zone_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut zones: ResMut<Assets<ZoneDef>>,
    mut pending: ResMut<PendingZones>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = match inv.args.split_once(' ').unwrap_or((&inv.args, "")) {
        ("create", rest) => create_zone(inv.entity, rest.trim(), &zones, &mut pending),
        ("rename", rest) => rename_zone(&here, rest.trim(), &mut zones),
        _ => Err(inv.usage_error()),
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn role_cmd(In(inv): In<Invocation>, mut players: PlayerQuery, db: Res<Database>) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = set_role(&actor, &inv, &db, &mut players, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut players);
}

fn setattr_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    keywords: Query<&Keywords>,
    mut sheets: Query<&mut Attributes>,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    let reply = set_attribute(&inv, &db, &players, &keywords, &mut sheets, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut players);
}

/// Apply an edit to a zone and save it to disk. `edit` should validate before changing anything.
fn edit_zone(
    zones: &mut Assets<ZoneDef>,
    zone_id: &str,
    edit: impl FnOnce(&mut ZoneDef) -> Reply,
) -> Reply {
    let asset_id = zones
        .iter()
        .find(|(_, zone)| zone.id == zone_id)
        .map(|(id, _)| id)
        .ok_or_else(|| format!("Zone '{zone_id}' isn't loaded."))?;
    let mut zone = zones.get_mut(asset_id).expect("zone asset id was just looked up");
    let reply = edit(&mut zone)?;
    if let Err(err) = zone.save() {
        error!("failed to save zone '{zone_id}': {err}");
        return Err(format!("{reply}\r\nWarning: the change is live but could not be saved: {err}"));
    }
    Ok(reply)
}

/// The room being edited, from inside an `edit_zone` closure.
fn room_mut<'a>(zone: &'a mut ZoneDef, here: &RoomKey) -> Result<&'a mut RoomDef, String> {
    zone.rooms
        .get_mut(&here.room)
        .ok_or_else(|| "This room no longer exists in its zone file.".into())
}

fn list_rooms(here: &RoomKey, zones: &Assets<ZoneDef>) -> Reply {
    let zone = zones
        .iter()
        .map(|(_, zone)| zone)
        .find(|zone| zone.id == here.zone)
        .ok_or_else(|| format!("Zone '{}' isn't loaded.", here.zone))?;
    let mut text = format!("Rooms in {} [{}]:", zone.name, zone.id);
    for (id, room) in &zone.rooms {
        let exits: Vec<_> = room
            .exits
            .iter()
            .map(|(dir, to)| format!("{dir}->{to}"))
            .collect();
        text += &format!("\r\n  {id:<16} {}  ({})", room.name, exits.join(", "));
    }
    Ok(text)
}

#[allow(clippy::too_many_arguments)]
fn goto(
    actor: &Actor,
    here: &RoomKey,
    inv: &Invocation,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    index: &RoomIndex,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Result<(), String> {
    if inv.args.is_empty() {
        return Err(inv.usage_error());
    }
    let (entity, name, from) = (actor.entity, &actor.name, actor.room);
    let target = RoomKey::parse(&inv.args, &here.zone);
    let &to = index
        .0
        .get(&target)
        .ok_or_else(|| format!("There is no room [{target}]."))?;

    out.push(Outgoing::Room {
        room: from,
        except: entity,
        text: format!("{name} vanishes.\r\n"),
    });
    move_player(entity, actor.character_id, to, players, rooms, db, out);
    out.push(Outgoing::Room {
        room: to,
        except: entity,
        text: format!("{name} appears out of thin air.\r\n"),
    });
    Ok(())
}

fn dig(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let Some((dir, name)) = inv.args.split_once(' ').map(|(d, n)| (d, n.trim())) else {
        return Err(inv.usage_error());
    };
    if name.is_empty() {
        return Err(inv.usage_error());
    }
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();

    edit_zone(zones, &here.zone, |zone| {
        if room_mut(zone, here)?.exits.contains_key(&dir) {
            return Err(format!("There is already an exit {dir}. Use 'unlink {dir}' first."));
        }

        let id = unique_room_id(&slugify(name), zone);
        let mut exits = BTreeMap::new();
        let way_back = opposite_direction(&dir);
        if let Some(back) = way_back {
            exits.insert(back.to_string(), here.room.clone());
        }
        zone.rooms.insert(
            id.clone(),
            RoomDef {
                name: name.into(),
                description: "An unfinished room. (Use 'desc' to describe it.)".into(),
                exits,
            },
        );
        room_mut(zone, here)?.exits.insert(dir.clone(), id.clone());

        let mut reply = format!("You dig {dir}, creating {name} [{}:{id}].", here.zone);
        if way_back.is_none() {
            reply += " It has no exit back; add one with 'link'.";
        }
        Ok(reply)
    })
}

fn rename(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let args = inv.args.as_str();
    if args.is_empty() {
        return Err(inv.usage_error());
    }
    edit_zone(zones, &here.zone, |zone| {
        room_mut(zone, here)?.name = args.into();
        Ok(format!("Room renamed to {args}."))
    })
}

fn describe(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let args = inv.args.as_str();
    if args.is_empty() {
        return Err(inv.usage_error());
    }
    edit_zone(zones, &here.zone, |zone| {
        room_mut(zone, here)?.description = args.into();
        Ok("Description updated.".into())
    })
}

fn link(here: &RoomKey, inv: &Invocation, index: &RoomIndex, zones: &mut Assets<ZoneDef>) -> Reply {
    let Some((dir, target)) = inv.args.split_once(' ').map(|(d, t)| (d, t.trim())) else {
        return Err(inv.usage_error());
    };
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let target = RoomKey::parse(target, &here.zone);
    if !index.0.contains_key(&target) {
        return Err(format!("There is no room [{target}]."));
    }
    // Same-zone exits are stored without the zone prefix, matching hand-written files.
    let stored = if target.zone == here.zone {
        target.room.clone()
    } else {
        target.to_string()
    };

    edit_zone(zones, &here.zone, |zone| {
        let replaced = room_mut(zone, here)?.exits.insert(dir.clone(), stored);
        let mut reply = format!("Exit {dir} now leads to [{target}] (one-way).");
        if let Some(old) = replaced {
            reply += &format!(" It used to lead to [{old}].");
        }
        Ok(reply)
    })
}

fn unlink(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    if inv.args.is_empty() {
        return Err(inv.usage_error());
    }
    let dir = full_direction(&inv.args.to_ascii_lowercase()).to_string();
    edit_zone(zones, &here.zone, |zone| {
        let room = room_mut(zone, here)?;
        if !room.exits.contains_key(&dir) {
            return Err(format!("There is no exit {dir}."));
        }
        room.exits.remove(&dir);
        Ok(format!("Removed the exit {dir}. Exits back to this room are unchanged."))
    })
}

fn list_zones(here: &RoomKey, zones: &Assets<ZoneDef>) -> Reply {
    let mut list: Vec<_> = zones.iter().map(|(_, zone)| zone).collect();
    list.sort_by(|a, b| a.id.cmp(&b.id));
    let mut text = String::from("Zones:");
    for zone in list {
        let marker = if zone.id == here.zone { " (you are here)" } else { "" };
        let count = zone.rooms.len();
        let plural = if count == 1 { "" } else { "s" };
        text += &format!(
            "\r\n  {:<16} {}  [{count} room{plural}]{marker}",
            zone.id,
            zone.name,
        );
    }
    Ok(text)
}

fn create_zone(
    entity: Entity,
    args: &str,
    zones: &Assets<ZoneDef>,
    pending: &mut PendingZones,
) -> Reply {
    let usage = || "Usage: zone create <id> <name>, e.g. zone create caves The Damp Caves".to_string();
    let (id, name) = args.split_once(' ').ok_or_else(usage)?;
    let (id, name) = (id.to_ascii_lowercase(), name.trim());
    if name.is_empty() {
        return Err(usage());
    }
    let valid = (2..=32).contains(&id.len())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !valid {
        return Err("Zone ids are 2-32 lowercase letters, digits, or underscores.".into());
    }
    if zones.iter().any(|(_, zone)| zone.id == id) || pending.0.iter().any(|(p, _)| *p == id) {
        return Err(format!("A zone named '{id}' already exists."));
    }

    let zone = ZoneDef::new(&id, name);
    if let Err(err) = zone.create() {
        error!("failed to create zone '{id}': {err}");
        return Err(format!("Could not create the zone file: {err}"));
    }
    info!("created zone '{id}' ({name})");
    pending.0.push((id.clone(), entity));
    Ok(format!(
        "Created zone {name} [{id}]. It will load in a moment, starting with the room [{id}:{ZONE_ENTRANCE}]."
    ))
}

fn rename_zone(here: &RoomKey, name: &str, zones: &mut Assets<ZoneDef>) -> Reply {
    if name.is_empty() {
        return Err("Usage: zone rename <name>".into());
    }
    edit_zone(zones, &here.zone, |zone| {
        zone.name = name.into();
        Ok(format!("Zone [{}] renamed to {name}.", zone.id))
    })
}

/// Tell builders when a zone they created has been loaded and can be visited.
fn announce_new_zones(
    mut pending: ResMut<PendingZones>,
    index: Res<RoomIndex>,
    mut connections: Query<&mut Connection>,
) {
    pending.0.retain(|(id, builder)| {
        let entrance = RoomKey {
            zone: id.clone(),
            room: ZONE_ENTRANCE.into(),
        };
        if !index.0.contains_key(&entrance) {
            return true;
        }
        if let Ok(mut conn) = connections.get_mut(*builder) {
            conn.send_raw(format!(
                "\r\nZone [{id}] is ready. Visit it with 'goto {entrance}', or connect it to \
                 this room with 'link <dir> {entrance}'.\r\n> "
            ));
        }
        false
    });
}

fn set_role(
    actor: &Actor,
    inv: &Invocation,
    db: &Database,
    players: &mut PlayerQuery,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let usage = || inv.usage_error();
    let (account, role) = inv.args.split_once(' ').ok_or_else(usage)?;
    let role = Role::parse(role.trim()).ok_or_else(usage)?;
    let entity = actor.entity;
    if account.eq_ignore_ascii_case(&actor.account) {
        return Err("You can't change your own role.".into());
    }

    match db.set_role(account, role) {
        Ok(true) => {}
        Ok(false) => return Err(format!("There is no account named '{account}'.")),
        Err(err) => {
            error!("failed to set role: {err}");
            return Err("Database error; the role was not changed.".into());
        }
    }
    info!("account '{account}' is now {}", role.as_str());

    // Apply to anyone online from that account right away.
    for (other, _, player, _) in players.iter_mut() {
        if let Some(mut player) = player
            && player.account.eq_ignore_ascii_case(account)
        {
            player.role = role;
            if other != entity {
                out.push(Outgoing::line(
                    other,
                    format!("\r\nYour account is now a {}. Type 'help' to see your commands.", role.as_str()),
                ));
                out.push(Outgoing::prompt(other));
            }
        }
    }
    Ok(format!("Account '{account}' is now a {}.", role.as_str()))
}

/// `setattr <player> <attribute> <rating>`: set an online character's attribute directly, for
/// testing. This bypasses Karma, so the character's unspent Karma may drop to zero.
fn set_attribute(
    inv: &Invocation,
    db: &Database,
    players: &PlayerQuery,
    keywords: &Query<&Keywords>,
    sheets: &mut Query<&mut Attributes>,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let usage = || inv.usage_error();
    let mut parts = inv.args.split_whitespace();
    let (Some(target), Some(attribute), Some(rating), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(usage());
    };
    let attribute = Attribute::parse(attribute)?;
    let rating: i32 = rating
        .parse()
        .ok()
        .filter(|r| (0..=1000).contains(r))
        .ok_or("Ratings are whole numbers from 0 to 1000.")?;

    let target = Target::parse(target).ok_or_else(usage)?;
    let Actor {
        entity,
        character_id,
        name,
        ..
    } = target
        .find_one(inv.entity, targets_online(players, keywords))
        .and_then(|e| Actor::get(players, e))
        .ok_or_else(|| format!("No one matching {} is online.", target.describe()))?;
    let mut attributes = sheets
        .get_mut(entity)
        .map_err(|_| format!("{name} has no attributes loaded."))?;

    if let Err(err) = db.set_attribute(character_id, attribute.name(), rating) {
        error!("failed to set attribute: {err}");
        return Err("Database error; the attribute was not changed.".into());
    }
    attributes.set(attribute, rating);
    info!("{name}'s {} set to {rating}", attribute.name());
    if entity != inv.entity {
        out.push(Outgoing::line(
            entity,
            format!("\r\nYour {} is now {rating}.", attribute.name()),
        ));
        out.push(Outgoing::prompt(entity));
    }
    Ok(format!("{name}'s {} is now {rating}.", attribute.name()))
}

/// Turn a room name into an id: "The Old Mill" -> "the_old_mill".
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_end_matches('_');
    if slug.is_empty() { "room".into() } else { slug.into() }
}

fn unique_room_id(base: &str, zone: &ZoneDef) -> String {
    if !zone.rooms.contains_key(base) {
        return base.into();
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !zone.rooms.contains_key(id))
        .unwrap()
}

fn opposite_direction(dir: &str) -> Option<&'static str> {
    Some(match dir {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        "up" => "down",
        "down" => "up",
        "northeast" => "southwest",
        "southwest" => "northeast",
        "northwest" => "southeast",
        "southeast" => "northwest",
        "in" => "out",
        "out" => "in",
        _ => return None,
    })
}
