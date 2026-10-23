//! Builder and admin commands. Builder commands edit the zone definitions in memory and save them
//! back to their files; `world::sync_world` then applies the change to the room entities, exactly
//! as if the file had been edited by hand.
//!
//! Permissions are checked in `commands.rs` before a `BuildCommand` is sent.

use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::commands::{
    self, BuildCommand, Outgoing, PlayerQuery, RoomQuery, deliver, full_direction, move_player,
};
use crate::attributes::{Attribute, Attributes};
use crate::db::{Database, Role};
use crate::net::Connection;
use crate::world::{RoomDef, RoomIndex, RoomKey, ZONE_ENTRANCE, ZoneDef};

pub struct BuildPlugin;

impl Plugin for BuildPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingZones>().add_systems(
            Update,
            (handle_build.after(commands::handle_commands), announce_new_zones),
        );
    }
}

/// Zones created in game whose files haven't been picked up by the file watcher yet, and the
/// builder to tell when they're ready.
#[derive(Resource, Default)]
struct PendingZones(Vec<(String, Entity)>);

/// A message for the player: `Ok` on success, `Err` explaining what went wrong.
type Reply = Result<String, String>;

#[allow(clippy::too_many_arguments)]
fn handle_build(
    mut inputs: MessageReader<BuildCommand>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    mut zones: ResMut<Assets<ZoneDef>>,
    mut pending: ResMut<PendingZones>,
    mut sheets: Query<&mut Attributes>,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    for BuildCommand { entity, verb, args } in inputs.read() {
        let entity = *entity;
        let Ok((_, _, Some(player), Some(in_room))) = players.get(entity) else {
            continue;
        };
        let (name, account, character_id) = (
            player.name.clone(),
            player.account.clone(),
            player.character_id,
        );
        let room = in_room.0;
        let Ok((_, here, _)) = rooms.get(room) else {
            continue;
        };
        let here = here.clone();

        let reply = match verb.as_str() {
            "where" => Ok(format!("You are in [{here}].")),
            "rooms" => list_rooms(&here, &zones),
            "goto" => {
                match goto(entity, &name, character_id, room, &here, args, &mut players, &rooms, &index, &db, &mut out) {
                    Ok(()) => {
                        out.push(Outgoing::prompt(entity));
                        continue;
                    }
                    Err(err) => Err(err),
                }
            }
            "dig" => dig(&here, args, &mut zones),
            "rename" => rename(&here, args, &mut zones),
            "desc" => describe(&here, args, &mut zones),
            "link" => link(&here, args, &index, &mut zones),
            "unlink" => unlink(&here, args, &mut zones),
            "zones" => list_zones(&here, &zones),
            "zone" => match args.split_once(' ').unwrap_or((args, "")) {
                ("create", rest) => create_zone(entity, rest.trim(), &zones, &mut pending),
                ("rename", rest) => rename_zone(&here, rest.trim(), &mut zones),
                _ => Err("Usage: zone create <id> <name>, or zone rename <name>".into()),
            },
            "role" => set_role(entity, &account, args, &db, &mut players, &mut out),
            "setattr" => set_attribute(args, &db, &players, &mut sheets, &mut out),
            _ => Err("Huh?".into()),
        };

        let text = match reply {
            Ok(text) => text,
            Err(text) => format!("\x1b[31m{text}\x1b[0m"),
        };
        out.push(Outgoing::line(entity, text));
        out.push(Outgoing::prompt(entity));
    }
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
    entity: Entity,
    name: &str,
    character_id: i64,
    from: Entity,
    here: &RoomKey,
    args: &str,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    index: &RoomIndex,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Result<(), String> {
    if args.is_empty() {
        return Err("Usage: goto <room> or goto <zone:room>".into());
    }
    let target = RoomKey::parse(args, &here.zone);
    let &to = index
        .0
        .get(&target)
        .ok_or_else(|| format!("There is no room [{target}]."))?;

    out.push(Outgoing::Room {
        room: from,
        except: entity,
        text: format!("{name} vanishes.\r\n"),
    });
    move_player(entity, character_id, to, players, rooms, db, out);
    out.push(Outgoing::Room {
        room: to,
        except: entity,
        text: format!("{name} appears out of thin air.\r\n"),
    });
    Ok(())
}

fn dig(here: &RoomKey, args: &str, zones: &mut Assets<ZoneDef>) -> Reply {
    let Some((dir, name)) = args.split_once(' ').map(|(d, n)| (d, n.trim())) else {
        return Err("Usage: dig <direction> <room name>".into());
    };
    if name.is_empty() {
        return Err("Usage: dig <direction> <room name>".into());
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

fn rename(here: &RoomKey, args: &str, zones: &mut Assets<ZoneDef>) -> Reply {
    if args.is_empty() {
        return Err("Usage: rename <new room name>".into());
    }
    edit_zone(zones, &here.zone, |zone| {
        room_mut(zone, here)?.name = args.into();
        Ok(format!("Room renamed to {args}."))
    })
}

fn describe(here: &RoomKey, args: &str, zones: &mut Assets<ZoneDef>) -> Reply {
    if args.is_empty() {
        return Err("Usage: desc <description>".into());
    }
    edit_zone(zones, &here.zone, |zone| {
        room_mut(zone, here)?.description = args.into();
        Ok("Description updated.".into())
    })
}

fn link(here: &RoomKey, args: &str, index: &RoomIndex, zones: &mut Assets<ZoneDef>) -> Reply {
    let Some((dir, target)) = args.split_once(' ').map(|(d, t)| (d, t.trim())) else {
        return Err("Usage: link <direction> <room> (or <zone:room>)".into());
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

fn unlink(here: &RoomKey, args: &str, zones: &mut Assets<ZoneDef>) -> Reply {
    if args.is_empty() {
        return Err("Usage: unlink <direction>".into());
    }
    let dir = full_direction(&args.to_ascii_lowercase()).to_string();
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
    entity: Entity,
    own_account: &str,
    args: &str,
    db: &Database,
    players: &mut PlayerQuery,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let usage = || "Usage: role <account> <player|builder|admin>".to_string();
    let (account, role) = args.split_once(' ').ok_or_else(usage)?;
    let role = Role::parse(role.trim()).ok_or_else(usage)?;
    if account.eq_ignore_ascii_case(own_account) {
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
    args: &str,
    db: &Database,
    players: &PlayerQuery,
    sheets: &mut Query<&mut Attributes>,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let usage = || "Usage: setattr <player> <attribute> <rating>".to_string();
    let mut parts = args.split_whitespace();
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

    let (entity, character_id, name) = players
        .iter()
        .find_map(|(e, _, player, _)| {
            let player = player?;
            player
                .name
                .eq_ignore_ascii_case(target)
                .then(|| (e, player.character_id, player.name.clone()))
        })
        .ok_or_else(|| format!("No one named {target} is online."))?;
    let mut attributes = sheets
        .get_mut(entity)
        .map_err(|_| format!("{name} has no attributes loaded."))?;

    if let Err(err) = db.set_attribute(character_id, attribute.name(), rating) {
        error!("failed to set attribute: {err}");
        return Err("Database error; the attribute was not changed.".into());
    }
    attributes.set(attribute, rating);
    info!("{name}'s {} set to {rating}", attribute.name());
    out.push(Outgoing::line(
        entity,
        format!("\r\nYour {} is now {rating}.", attribute.name()),
    ));
    out.push(Outgoing::prompt(entity));
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
