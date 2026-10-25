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
use crate::commands::movement::{View, full_direction, move_player, toward};
use crate::db::{Database, Permission, Role};
use crate::mobs::SpawnedBy;
use crate::net::Connection;
use crate::output::{
    Actor, Outgoing, PlayerQuery, Reply, RoomQuery, deliver, reply_line, targets_online,
};
use crate::targeting::{Keywords, Target};
use crate::world::{
    Bulk, Capacity, ContainerDef, DoorDef, DoorKey, DoorState, Every, ExitDef, ItemDef, ItemKey,
    LockDef, MobDef, MobKey, Refill, RoomDef, RoomIndex, RoomKey, Slot, SpawnDef, SpawnKey, Wander,
    ZONE_ENTRANCE, ZoneDef, format_bulk, without_article,
};

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
            .add_command(
                build("where", "where", "show the current room's id"),
                where_cmd,
            )
            .add_command(
                build("rooms", "rooms", "list the rooms in this zone"),
                rooms_cmd,
            )
            .add_command(
                build(
                    "goto",
                    "goto <room>",
                    "teleport to a room (`room` or `zone:room`)",
                ),
                goto_cmd,
            )
            .add_command(
                build(
                    "dig",
                    "dig <dir> <name>",
                    "create a room through a new exit, with a way back",
                ),
                dig_cmd,
            )
            .add_command(
                build("rename", "rename <name>", "rename this room"),
                rename_cmd,
            )
            .add_command(
                build("desc", "desc <text>", "set this room's description"),
                desc_cmd,
            )
            .add_command(
                build(
                    "link",
                    "link <dir> <room>",
                    "add a one-way exit to an existing room",
                ),
                link_cmd,
            )
            .add_command(
                build("unlink", "unlink <dir>", "remove an exit"),
                unlink_cmd,
            )
            .add_command(
                build(
                    "door",
                    "door <dir> [<name> | lock | nolock | key <item> | open | closed | remove]",
                    "show, add, rename or remove a door, give it a lock and key, or set how it starts",
                ),
                door_cmd,
            )
            .add_command(
                build(
                    "item",
                    "item [<id> | new <id> <name> | <field> <id> <value>]",
                    "list, show, create or edit item templates; fields: name, desc, keywords, bulk, tags, wear, fixed, container",
                ),
                item_cmd,
            )
            .add_command(
                build(
                    "mob",
                    "mob [<id> | new <id> <name> | <field> <id> <value>]",
                    "list, show, create or edit mob templates; fields: name, here, desc, keywords",
                ),
                mob_cmd,
            )
            .add_command(
                build(
                    "spawn",
                    "spawn [<id> | new <id> <mob> [<count>] | remove <id> | <field> <id> <value>]",
                    "list, show, add or edit this zone's spawns; fields: mob, count, every, wander, room",
                ),
                spawn_cmd,
            )
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
    deliver(
        vec![Outgoing::line(inv.entity, format!("You are in [{here}]."))],
        &mut players,
    );
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
    view: View,
    index: Res<RoomIndex>,
    db: Res<Database>,
) {
    let Some((actor, here)) = locate(&players, &view.rooms, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    if let Err(err) = goto(
        &actor,
        &here,
        &inv,
        &mut players,
        &view,
        &index,
        &db,
        &mut out,
    ) {
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

fn door_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut zones: ResMut<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = door(&here, &inv, &mut zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn item_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut zones: ResMut<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = item_template(&here, &inv, &mut zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn mob_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut zones: ResMut<Assets<ZoneDef>>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let reply = mob_template(&here, &inv, &mut zones);
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

fn spawn_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut zones: ResMut<Assets<ZoneDef>>,
    mobs: Query<&SpawnedBy>,
) {
    let Some((_, here)) = locate(&players, &rooms, inv.entity) else {
        return;
    };
    let alive = |key: &SpawnKey| mobs.iter().filter(|s| s.0 == *key).count();
    let reply = spawn(&here, &inv, &mut zones, alive);
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
    let mut zone = zones
        .get_mut(asset_id)
        .expect("zone asset id was just looked up");
    let reply = edit(&mut zone)?;
    if let Err(err) = zone.save() {
        error!("failed to save zone '{zone_id}': {err}");
        return Err(format!(
            "{reply}\r\nWarning: the change is live but could not be saved: {err}"
        ));
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
            .map(|(dir, exit)| match exit.door() {
                Some(door) => format!("{dir}->{} [door {door}]", exit.to()),
                None => format!("{dir}->{}", exit.to()),
            })
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
    view: &View,
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
    move_player(entity, actor.character_id, to, players, view, db, out);
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
            return Err(format!(
                "There is already an exit {dir}. Use 'unlink {dir}' first."
            ));
        }

        let id = unique_room_id(&slugify(name), zone);
        let mut exits = BTreeMap::new();
        let way_back = opposite_direction(&dir);
        if let Some(back) = way_back {
            exits.insert(back.to_string(), ExitDef::To(here.room.clone()));
        }
        zone.rooms.insert(
            id.clone(),
            RoomDef {
                name: name.into(),
                description: "An unfinished room. (Use 'desc' to describe it.)".into(),
                exits,
                no_wander: false,
            },
        );
        room_mut(zone, here)?
            .exits
            .insert(dir.clone(), ExitDef::To(id.clone()));

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
        let exits = &mut room_mut(zone, here)?.exits;
        if exits.get(&dir).and_then(ExitDef::door).is_some() {
            return Err(format!(
                "The {dir} exit has a door. Remove it first with 'door {dir} remove'."
            ));
        }
        let replaced = exits.insert(dir.clone(), ExitDef::To(stored));
        let mut reply = format!("Exit {dir} now leads to [{target}] (one-way).");
        if let Some(old) = replaced {
            reply += &format!(" It used to lead to [{}].", old.to());
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
        match room.exits.get(&dir) {
            None => return Err(format!("There is no exit {dir}.")),
            Some(exit) if exit.door().is_some() => {
                return Err(format!(
                    "The {dir} exit has a door. Remove it first with 'door {dir} remove'."
                ));
            }
            Some(_) => {}
        }
        room.exits.remove(&dir);
        Ok(format!(
            "Removed the exit {dir}. Exits back to this room are unchanged."
        ))
    })
}

fn list_zones(here: &RoomKey, zones: &Assets<ZoneDef>) -> Reply {
    let mut list: Vec<_> = zones.iter().map(|(_, zone)| zone).collect();
    list.sort_by(|a, b| a.id.cmp(&b.id));
    let mut text = String::from("Zones:");
    for zone in list {
        let marker = if zone.id == here.zone {
            " (you are here)"
        } else {
            ""
        };
        let count = zone.rooms.len();
        let plural = if count == 1 { "" } else { "s" };
        text += &format!(
            "\r\n  {:<16} {}  [{count} room{plural}]{marker}",
            zone.id, zone.name,
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
    let usage =
        || "Usage: zone create <id> <name>, e.g. zone create caves The Damp Caves".to_string();
    let (id, name) = args.split_once(' ').ok_or_else(usage)?;
    let (id, name) = (id.to_ascii_lowercase(), name.trim());
    if name.is_empty() {
        return Err(usage());
    }
    let valid = (2..=32).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
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
                "Zone [{id}] is ready. Visit it with 'goto {entrance}', or connect it to \
                 this room with 'link <dir> {entrance}'.\r\n"
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
                    format!(
                        "Your account is now a {}. Type 'help' to see your commands.",
                        role.as_str()
                    ),
                ));
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
            format!("Your {} is now {rating}.", attribute.name()),
        ));
    }
    Ok(format!("{name}'s {} is now {rating}.", attribute.name()))
}

fn zone_def<'a>(zones: &'a Assets<ZoneDef>, zone_id: &str) -> Result<&'a ZoneDef, String> {
    zones
        .iter()
        .map(|(_, zone)| zone)
        .find(|zone| zone.id == zone_id)
        .ok_or_else(|| format!("Zone '{zone_id}' isn't loaded."))
}

/// `door <dir> ...`: everything about the door on one of this room's exits.
fn door(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let (dir, rest) = inv
        .args
        .split_once(' ')
        .map(|(d, r)| (d, r.trim()))
        .unwrap_or((&inv.args, ""));
    if dir.is_empty() {
        return Err(inv.usage_error());
    }
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let exit = zone_def(zones, &here.zone)?
        .rooms
        .get(&here.room)
        .and_then(|room| room.exits.get(&dir))
        .cloned()
        .ok_or_else(|| format!("There is no exit {dir}."))?;
    let existing = exit.door().map(|d| DoorKey::parse(d, &here.zone));

    let Some(key) = existing else {
        return match rest {
            "" => Ok(format!(
                "There's no door {}. Add one with 'door {dir} <name>'.",
                toward(&dir)
            )),
            "lock" | "nolock" | "open" | "closed" | "remove" | "key" => Err(format!(
                "There's no door {}. Add one first with 'door {dir} <name>'.",
                toward(&dir)
            )),
            name => add_door(here, &dir, &exit, name, zones),
        };
    };

    let def = zone_def(zones, &key.zone)?
        .doors
        .get(&key.door)
        .cloned()
        .ok_or_else(|| format!("The {dir} exit names a door [{key}] that doesn't exist."))?;
    match rest {
        "" => Ok(describe_door(&dir, &key, &def)),
        "remove" => remove_door(&key, &def, zones),
        _ if rest == "key" || rest.starts_with("key ") => {
            let item = rest["key".len()..].trim();
            if item.is_empty() {
                return Err(format!("Usage: door {dir} key <item>"));
            }
            if def.lock.is_none() {
                return Err(format!(
                    "The {dir} door has no lock. Add one first with 'door {dir} lock'."
                ));
            }
            let item_key = ItemKey::parse(item, &key.zone);
            if !zone_def(zones, &item_key.zone)?
                .items
                .contains_key(&item_key.item)
            {
                return Err(format!("There's no item template [{item_key}]."));
            }
            // Keys in the door's own zone are stored without the zone, like exits.
            let stored = if item_key.zone == key.zone {
                item_key.item.clone()
            } else {
                item_key.to_string()
            };
            edit_door(&key, zones, |door| {
                if let Some(lock) = door.lock.as_mut() {
                    lock.key = Some(stored);
                }
                Ok(describe_door(&dir, &key, door))
            })
        }
        "lock" | "nolock" | "open" | "closed" => edit_door(&key, zones, |door| {
            match rest {
                "lock" => {
                    door.lock = Some(LockDef {
                        key: None,
                        pick: Some(5),
                        bash: Some(10),
                    });
                    door.state = DoorState::Locked;
                }
                "nolock" => {
                    door.lock = None;
                    if door.state == DoorState::Locked {
                        door.state = DoorState::Closed;
                    }
                }
                "open" => door.state = DoorState::Open,
                _ => {
                    if door.state == DoorState::Open {
                        door.state = DoorState::Closed;
                    }
                }
            }
            Ok(describe_door(&dir, &key, door))
        }),
        name => edit_door(&key, zones, |door| {
            door.name = name.into();
            door.keywords.clear();
            Ok(format!("The {dir} door is now called {name}."))
        }),
    }
}

fn describe_door(dir: &str, key: &DoorKey, door: &DoorDef) -> String {
    let starts = match door.state {
        DoorState::Open => "open",
        DoorState::Closed => "closed",
        DoorState::Locked => "locked",
    };
    let lock = match &door.lock {
        None => "no lock".to_string(),
        Some(lock) => {
            let or_never = |n: Option<u32>| n.map_or("never".to_string(), |n| n.to_string());
            format!(
                "a lock (pick {}, bash {}, key {})",
                or_never(lock.pick),
                or_never(lock.bash),
                lock.key.as_deref().unwrap_or("none")
            )
        }
    };
    format!(
        "The {dir} door: {} [{key}], starts {starts}, {lock}.",
        door.name
    )
}

/// Apply an edit to a door's definition, wherever its zone is.
fn edit_door(
    key: &DoorKey,
    zones: &mut Assets<ZoneDef>,
    edit: impl FnOnce(&mut DoorDef) -> Reply,
) -> Reply {
    edit_zone(zones, &key.zone, |zone| {
        let door = zone
            .doors
            .get_mut(&key.door)
            .ok_or_else(|| format!("Door [{key}] no longer exists."))?;
        edit(door)
    })
}

/// Add a closed, unlocked door to an exit, and to the exit leading back if there is one.
fn add_door(
    here: &RoomKey,
    dir: &str,
    exit: &ExitDef,
    name: &str,
    zones: &mut Assets<ZoneDef>,
) -> Reply {
    let id = {
        let zone = zone_def(zones, &here.zone)?;
        unique_id(&slugify(without_article(name)), |id| {
            zone.doors.contains_key(id)
        })
    };
    let dest = RoomKey::parse(exit.to(), &here.zone);
    // The exit back from the destination, if it leads here and has no door of its own.
    let back = zone_def(zones, &dest.zone).ok().and_then(|zone| {
        zone.rooms.get(&dest.room)?.exits.iter().find_map(|(d, e)| {
            (RoomKey::parse(e.to(), &dest.zone) == *here && e.door().is_none())
                .then(|| (d.clone(), e.to().to_string()))
        })
    });
    let door_ref = |zone: &str| {
        if zone == here.zone {
            id.clone()
        } else {
            format!("{}:{id}", here.zone)
        }
    };

    edit_zone(zones, &here.zone, |zone| {
        zone.doors.insert(
            id.clone(),
            DoorDef {
                name: name.into(),
                keywords: Vec::new(),
                state: DoorState::Closed,
                lock: None,
            },
        );
        room_mut(zone, here)?.exits.insert(
            dir.into(),
            ExitDef::Door {
                to: exit.to().into(),
                door: id.clone(),
            },
        );
        if dest.zone == here.zone
            && let Some((back_dir, back_to)) = &back
            && let Some(room) = zone.rooms.get_mut(&dest.room)
        {
            room.exits.insert(
                back_dir.clone(),
                ExitDef::Door {
                    to: back_to.clone(),
                    door: door_ref(&dest.zone),
                },
            );
        }
        Ok(String::new())
    })?;
    if dest.zone != here.zone
        && let Some((back_dir, back_to)) = &back
    {
        edit_zone(zones, &dest.zone, |zone| {
            let room = zone
                .rooms
                .get_mut(&dest.room)
                .ok_or_else(|| format!("Room [{dest}] no longer exists."))?;
            room.exits.insert(
                back_dir.clone(),
                ExitDef::Door {
                    to: back_to.clone(),
                    door: door_ref(&dest.zone),
                },
            );
            Ok(String::new())
        })?;
    }

    let mut reply = format!(
        "Added {name} [{}:{id}] to the {dir} exit. It starts closed, with no lock.",
        here.zone
    );
    match back {
        Some((back_dir, _)) => reply += &format!(" The {back_dir} exit from [{dest}] shares it."),
        None => reply += &format!(" There's no exit back from [{dest}], so it has only this side."),
    }
    Ok(reply)
}

/// Remove a door's definition and every exit's reference to it, in any zone.
fn remove_door(key: &DoorKey, def: &DoorDef, zones: &mut Assets<ZoneDef>) -> Reply {
    let referencing: Vec<String> = zones
        .iter()
        .map(|(_, zone)| zone)
        .filter(|zone| {
            zone.rooms.values().any(|room| {
                room.exits.values().any(|e| {
                    e.door()
                        .is_some_and(|d| DoorKey::parse(d, &zone.id) == *key)
                })
            })
        })
        .map(|zone| zone.id.clone())
        .collect();
    for zone_id in &referencing {
        edit_zone(zones, zone_id, |zone| {
            let zone_id = zone.id.clone();
            for room in zone.rooms.values_mut() {
                for exit in room.exits.values_mut() {
                    if exit
                        .door()
                        .is_some_and(|d| DoorKey::parse(d, &zone_id) == *key)
                    {
                        *exit = ExitDef::To(exit.to().into());
                    }
                }
            }
            Ok(String::new())
        })?;
    }
    edit_zone(zones, &key.zone, |zone| {
        zone.doors.remove(&key.door);
        Ok(format!("Removed {} [{key}].", def.name))
    })
}

/// `item ...`: list, show, create and edit the current zone's item templates.
fn item_template(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let mut words = inv.args.splitn(3, ' ');
    let first = words.next().unwrap_or("");
    let id = words.next().unwrap_or("").to_ascii_lowercase();
    let value = words.next().unwrap_or("").trim();
    let zone = zone_def(zones, &here.zone)?;

    match first {
        "" => {
            if zone.items.is_empty() {
                return Ok(format!(
                    "Zone [{}] has no item templates. Make one with 'item new <id> <name>'.",
                    zone.id
                ));
            }
            let mut text = format!("Item templates in {} [{}]:", zone.name, zone.id);
            for (id, def) in &zone.items {
                text += &format!("\r\n  {id:<20} {}", def.name);
            }
            Ok(text)
        }
        "new" => {
            if !valid_id(&id) || value.is_empty() {
                return Err(format!("Usage: item new <id> <name>, with {ID_RULES}."));
            }
            if zone.items.contains_key(&id) {
                return Err(format!("[{}:{id}] already exists.", here.zone));
            }
            edit_zone(zones, &here.zone, |zone| {
                zone.items.insert(id.clone(), ItemDef::named(value));
                Ok(format!(
                    "Created item template {value} [{}:{id}]. Make one with 'create {id}'.",
                    here.zone
                ))
            })
        }
        "name" | "desc" | "keywords" | "bulk" | "tags" | "wear" | "fixed" | "container" => {
            if !zone.items.contains_key(&id) || value.is_empty() {
                return Err(format!(
                    "Usage: item {first} <id> <value>, for a template in [{}].{}",
                    here.zone,
                    match first {
                        "bulk" => " Bulk is negligible, light, or a number.",
                        "wear" => " Slots: head, neck, body, back, belt, feet, or none.",
                        "fixed" => " Use yes or no.",
                        "container" => {
                            " Use bulk <n>, light <n> or count <n>, then any tags it \
                             accepts; or none."
                        }
                        _ => "",
                    }
                ));
            }
            let change = template_change(first, value)?;
            edit_zone(zones, &here.zone, |zone| {
                let def = zone
                    .items
                    .get_mut(&id)
                    .ok_or_else(|| "That template no longer exists.".to_string())?;
                change(def);
                Ok(describe_item_template(&here.zone, &id, def))
            })
        }
        id => {
            let def = zone
                .items
                .get(id)
                .ok_or_else(|| format!("There's no item template [{}:{id}].", here.zone))?;
            Ok(describe_item_template(&here.zone, id, def))
        }
    }
}

/// A change to apply to an item template.
type TemplateEdit = Box<dyn FnOnce(&mut ItemDef)>;

/// Parse one `item <field> <id> <value>` edit into a change to apply to the template.
fn template_change(field: &str, value: &str) -> Result<TemplateEdit, String> {
    let value = value.to_string();
    Ok(match field {
        "name" => Box::new(move |def| def.name = value),
        "desc" => Box::new(move |def| def.description = value),
        "keywords" => Box::new(move |def| {
            def.keywords = value.split_whitespace().map(str::to_lowercase).collect()
        }),
        "tags" => Box::new(move |def| {
            def.tags = value.split_whitespace().map(str::to_lowercase).collect()
        }),
        "bulk" => {
            let bulk = Bulk::parse(&value).ok_or("Bulk is negligible, light, or a number.")?;
            Box::new(move |def| def.bulk = bulk)
        }
        "wear" => {
            let slot = match value.as_str() {
                "none" => None,
                other => Some(
                    Slot::parse(other)
                        .ok_or("Slots are head, neck, body, back, belt and feet, or none.")?,
                ),
            };
            Box::new(move |def| def.wear = slot)
        }
        "fixed" => {
            let fixed = match value.as_str() {
                "yes" | "true" | "on" => true,
                "no" | "false" | "off" => false,
                _ => return Err("Use 'yes' or 'no'.".into()),
            };
            Box::new(move |def| def.fixed = fixed)
        }
        _ => {
            if value == "none" {
                return Ok(Box::new(|def: &mut ItemDef| def.container = None));
            }
            let mut words = value.split_whitespace();
            let usage = "Use bulk <n>, light <n> or count <n>, then any tags it accepts.";
            let kind = words.next().unwrap_or("");
            let n: u32 = words.next().and_then(|n| n.parse().ok()).ok_or(usage)?;
            let capacity = match kind {
                "bulk" => Capacity::Bulk(n),
                "light" => Capacity::Light(n),
                "count" => Capacity::Count(n),
                _ => return Err(usage.into()),
            };
            let accepts: Vec<String> = words.map(str::to_lowercase).collect();
            Box::new(move |def| {
                let (state, lock, max_item) = def
                    .container
                    .take()
                    .map(|c| (c.state, c.lock, c.max_item))
                    .unwrap_or((DoorState::Open, None, None));
                def.container = Some(ContainerDef {
                    capacity,
                    accepts,
                    max_item,
                    state,
                    lock,
                });
            })
        }
    })
}

fn describe_item_template(zone: &str, id: &str, def: &ItemDef) -> String {
    let none = |s: String| {
        if s.is_empty() {
            "(none)".to_string()
        } else {
            s
        }
    };
    let container = def.container.as_ref().map(|c| {
        let capacity = match c.capacity {
            Capacity::Bulk(n) => format!("{n} Bulk"),
            Capacity::Light(n) => format!("{n} light"),
            Capacity::Count(n) => format!("{n} items"),
        };
        if c.accepts.is_empty() {
            capacity
        } else {
            format!("{capacity}, only {}", c.accepts.join(", "))
        }
    });
    format!(
        "{} [{zone}:{id}]\r\n  keywords: {}\r\n  bulk: {}\r\n  tags: {}\r\n  wear: {}\r\n  \
         container: {}\r\n  fixed: {}\r\n  description: {}",
        def.name,
        def.keywords().0.join(", "),
        format_bulk(def.bulk.tenths()),
        none(def.tags.join(", ")),
        def.wear.map_or("(none)", Slot::name),
        container.unwrap_or_else(|| "(none)".into()),
        if def.fixed { "yes" } else { "no" },
        none(def.description.clone())
    )
}

/// What a template or spawn id may be made of.
const ID_RULES: &str = "an id of 2-32 lowercase letters, digits or underscores";

fn valid_id(id: &str) -> bool {
    (2..=32).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `mob ...`: list, show, create and edit the current zone's mob templates.
fn mob_template(here: &RoomKey, inv: &Invocation, zones: &mut Assets<ZoneDef>) -> Reply {
    let mut words = inv.args.splitn(3, ' ');
    let first = words.next().unwrap_or("");
    let id = words.next().unwrap_or("").to_ascii_lowercase();
    let value = words.next().unwrap_or("").trim().to_string();
    let zone = zone_def(zones, &here.zone)?;

    match first {
        "" => {
            if zone.mobs.is_empty() {
                return Ok(format!(
                    "Zone [{}] has no mob templates. Make one with 'mob new <id> <name>'.",
                    zone.id
                ));
            }
            let mut text = format!("Mob templates in {} [{}]:", zone.name, zone.id);
            for (id, def) in &zone.mobs {
                text += &format!("\r\n  {id:<20} {}", def.name);
            }
            Ok(text)
        }
        "new" => {
            if !valid_id(&id) || value.is_empty() {
                return Err(format!("Usage: mob new <id> <name>, with {ID_RULES}."));
            }
            if zone.mobs.contains_key(&id) {
                return Err(format!("[{}:{id}] already exists.", here.zone));
            }
            edit_zone(zones, &here.zone, |zone| {
                zone.mobs.insert(id.clone(), MobDef::named(&value));
                Ok(format!(
                    "Created mob template {value} [{}:{id}]. Bring one here with 'summon {id}', \
                     or keep some about with 'spawn new <spawn> {id} <count>'.",
                    here.zone
                ))
            })
        }
        "name" | "here" | "desc" | "keywords" => {
            if !zone.mobs.contains_key(&id) || value.is_empty() {
                return Err(format!(
                    "Usage: mob {first} <id> <value>, for a template in [{}].{}",
                    here.zone,
                    if first == "here" {
                        " That's the line rooms show, like 'A sewer rat scurries past.'; \
                         'none' for the plain one."
                    } else {
                        ""
                    }
                ));
            }
            edit_zone(zones, &here.zone, |zone| {
                let def = zone
                    .mobs
                    .get_mut(&id)
                    .ok_or_else(|| "That template no longer exists.".to_string())?;
                match first {
                    "name" => def.name = value,
                    "here" if value == "none" => def.here.clear(),
                    "here" => def.here = value,
                    "desc" => def.description = value,
                    _ => def.keywords = value.split_whitespace().map(str::to_lowercase).collect(),
                }
                Ok(describe_mob_template(&here.zone, &id, def))
            })
        }
        id => {
            let def = zone
                .mobs
                .get(id)
                .ok_or_else(|| format!("There's no mob template [{}:{id}].", here.zone))?;
            Ok(describe_mob_template(&here.zone, id, def))
        }
    }
}

fn describe_mob_template(zone: &str, id: &str, def: &MobDef) -> String {
    let description = if def.description.is_empty() {
        "(none)"
    } else {
        &def.description
    };
    format!(
        "{} [{zone}:{id}]\r\n  keywords: {}\r\n  here: {}\r\n  description: {description}",
        def.name,
        def.keywords().0.join(", "),
        def.here_line(),
    )
}

/// `spawn ...`: list, show, add, edit and remove the current zone's spawns. `alive` counts a
/// spawn's mobs in play.
fn spawn(
    here: &RoomKey,
    inv: &Invocation,
    zones: &mut Assets<ZoneDef>,
    alive: impl Fn(&SpawnKey) -> usize,
) -> Reply {
    let mut words = inv.args.splitn(3, ' ');
    let first = words.next().unwrap_or("");
    let id = words.next().unwrap_or("").to_ascii_lowercase();
    let value = words.next().unwrap_or("").trim().to_string();
    let zone = zone_def(zones, &here.zone)?;
    let key = |id: &str| SpawnKey::parse(id, &here.zone);

    match first {
        "" => {
            if zone.spawns.is_empty() {
                return Ok(format!(
                    "Zone [{}] has no spawns. Add one here with 'spawn new <id> <mob> <count>'.",
                    zone.id
                ));
            }
            let mut text = format!("Spawns in {} [{}]:", zone.name, zone.id);
            for (id, def) in &zone.spawns {
                text += &format!(
                    "\r\n  {id:<20} {}/{} {} in {}",
                    alive(&key(id)),
                    def.refill.wanted(),
                    def.mob,
                    def.room
                );
            }
            Ok(text)
        }
        "new" => {
            let mut rest = value.split_whitespace();
            let mob = rest.next().unwrap_or("").to_ascii_lowercase();
            let count = match rest.next() {
                None => Some(1),
                Some(n) => n.parse::<u32>().ok().filter(|n| *n > 0),
            };
            let (true, false, Some(count)) = (valid_id(&id), mob.is_empty(), count) else {
                return Err(format!(
                    "Usage: spawn new <id> <mob> [<count>], with {ID_RULES}."
                ));
            };
            if zone.spawns.contains_key(&id) {
                return Err(format!("[{}:{id}] already exists.", here.zone));
            }
            if !mob_exists(zones, &MobKey::parse(&mob, &here.zone)) {
                return Err(format!("There's no mob template [{mob}]."));
            }
            edit_zone(zones, &here.zone, |zone| {
                let def = SpawnDef {
                    mob,
                    room: here.room.clone(),
                    every: Every::Minutes(10),
                    refill: Refill::Keep(count),
                    wander: Wander::Never,
                };
                let text = describe_spawn(&key(&id), &def, 0);
                zone.spawns.insert(id.clone(), def);
                Ok(text)
            })
        }
        "remove" => {
            if !zone.spawns.contains_key(&id) {
                return Err(format!("There's no spawn [{}:{id}].", here.zone));
            }
            edit_zone(zones, &here.zone, |zone| {
                zone.spawns.remove(&id);
                Ok(format!(
                    "Removed spawn [{}:{id}], and its mobs with it.",
                    here.zone
                ))
            })
        }
        "mob" | "count" | "every" | "wander" | "room" => {
            if !zone.spawns.contains_key(&id) || (value.is_empty() && first != "room") {
                return Err(format!(
                    "Usage: spawn {first} <id> {}, for a spawn in [{}].",
                    match first {
                        "mob" => "<mob>",
                        "count" => "<n>",
                        "every" => "<10m | 2h | 1d | death 30d>",
                        "wander" => {
                            "<never | zone <min> <max> | anywhere <min> <max> | rooms <min> <max> \
                             <room>...>, in seconds"
                        }
                        _ => "(moves it to this room)",
                    },
                    here.zone
                ));
            }
            let change = spawn_change(first, &value, here)?;
            if let Some(mob) = change.mob()
                && !mob_exists(zones, &MobKey::parse(mob, &here.zone))
            {
                return Err(format!("There's no mob template [{mob}]."));
            }
            let alive = alive(&key(&id));
            edit_zone(zones, &here.zone, |zone| {
                let def = zone
                    .spawns
                    .get_mut(&id)
                    .ok_or_else(|| "That spawn no longer exists.".to_string())?;
                change.apply(def);
                Ok(describe_spawn(&key(&id), def, alive))
            })
        }
        id => {
            let def = zone
                .spawns
                .get(id)
                .ok_or_else(|| format!("There's no spawn [{}:{id}].", here.zone))?;
            Ok(describe_spawn(&key(id), def, alive(&key(id))))
        }
    }
}

fn mob_exists(zones: &Assets<ZoneDef>, key: &MobKey) -> bool {
    zone_def(zones, &key.zone).is_ok_and(|zone| zone.mobs.contains_key(&key.mob))
}

/// One `spawn <field> <id> <value>` edit.
enum SpawnChange {
    Mob(String),
    Count(u32),
    Every(Every),
    Wander(Wander),
    Room(String),
}

impl SpawnChange {
    fn mob(&self) -> Option<&str> {
        match self {
            SpawnChange::Mob(mob) => Some(mob),
            _ => None,
        }
    }

    fn apply(self, def: &mut SpawnDef) {
        match self {
            SpawnChange::Mob(mob) => def.mob = mob,
            SpawnChange::Count(n) => def.refill = Refill::Keep(n),
            SpawnChange::Every(every) => def.every = every,
            SpawnChange::Wander(wander) => def.wander = wander,
            SpawnChange::Room(room) => def.room = room,
        }
    }
}

fn spawn_change(field: &str, value: &str, here: &RoomKey) -> Result<SpawnChange, String> {
    Ok(match field {
        "mob" => SpawnChange::Mob(value.to_ascii_lowercase()),
        "count" => SpawnChange::Count(
            value
                .parse()
                .ok()
                .filter(|n| *n > 0)
                .ok_or("The count is a number, 1 or more.")?,
        ),
        "every" => {
            SpawnChange::Every(Every::parse(&value.to_ascii_lowercase()).ok_or(
                "Use a number with m, h or d (10m, 2h, 1d), or 'death' and one (death 30d).",
            )?)
        }
        "wander" => {
            let words: Vec<&str> = value.split_whitespace().collect();
            let usage = "Use never; zone, anywhere or rooms with the least and most seconds \
                         between moves (zone 30 90); then, for rooms, the rooms.";
            let secs = || -> Result<(u32, u32), String> {
                let n = |i: usize| words.get(i).and_then(|w| w.parse::<u32>().ok());
                match (n(1), n(2)) {
                    (Some(min), Some(max)) if min >= 1 && min <= max => Ok((min, max)),
                    _ => Err(usage.into()),
                }
            };
            SpawnChange::Wander(match words.first().copied() {
                Some("never") => Wander::Never,
                Some("zone") => Wander::Zone { secs: secs()? },
                Some("anywhere") => Wander::Anywhere { secs: secs()? },
                Some("rooms") if words.len() > 3 => Wander::Rooms {
                    secs: secs()?,
                    rooms: words[3..].iter().map(|r| r.to_string()).collect(),
                },
                _ => return Err(usage.into()),
            })
        }
        _ => SpawnChange::Room(here.room.clone()),
    })
}

fn describe_spawn(key: &SpawnKey, def: &SpawnDef, alive: usize) -> String {
    let wander = match &def.wander {
        Wander::Never => "never".to_string(),
        Wander::Zone { secs: (min, max) } => format!("in its zone, every {min}-{max}s"),
        Wander::Anywhere { secs: (min, max) } => format!("anywhere, every {min}-{max}s"),
        Wander::Rooms {
            rooms,
            secs: (min, max),
        } => format!("among {}, every {min}-{max}s", rooms.join(", ")),
    };
    format!(
        "Spawn [{key}]: {alive} of {} alive\r\n  mob: {}\r\n  room: {}\r\n  resets: {}\r\n  \
         wander: {wander}",
        def.refill.wanted(),
        def.mob,
        def.room,
        def.every,
    )
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
    if slug.is_empty() {
        "room".into()
    } else {
        slug.into()
    }
}

fn unique_room_id(base: &str, zone: &ZoneDef) -> String {
    unique_id(base, |id| zone.rooms.contains_key(id))
}

/// `base`, or `base_2`, `base_3`, ... whichever isn't `taken`.
fn unique_id(base: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.into();
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|id| !taken(id))
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
