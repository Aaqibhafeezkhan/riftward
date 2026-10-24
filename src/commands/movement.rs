//! Looking around and moving between rooms.

use bevy::prelude::*;

use crate::command::Invocation;
use crate::commands::doors::{DoorLookup, find_door};
use crate::db::{Database, Permission};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver, targets_in_room};
use crate::targeting::{Keywords, Target};
use crate::world::{Door, DoorState};

/// `look` describes the room; `look <target>` looks along an exit, at a door, or at someone here.
pub fn look(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    doors: Query<&Door>,
    keywords: Query<&Keywords>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let lookup = rooms
        .get(actor.room)
        .ok()
        .filter(|_| !inv.args.is_empty())
        .map(|(_, _, exits)| {
            let lookup = find_door(&inv.args, exits, |e| keywords.get(e).ok());
            (lookup, exits)
        });
    let text = match (Target::parse(&inv.args), lookup) {
        (None, _) => describe_room(
            actor.room,
            actor.entity,
            &players,
            &rooms,
            &doors,
            actor.role.can(Permission::Build),
        ),
        (_, Some((DoorLookup::Door { door, dir }, exits))) => match doors.get(door) {
            Ok(door) if door.is_open() => format!(
                "Through the open {} {} you see {}",
                door.the().trim_start_matches("the "),
                toward(&dir),
                glimpse(exits.0[&dir].to, actor.entity, &players, &rooms)
            ),
            Ok(door) => format!("You see {} {}. It is closed.\r\n", door.name, toward(&dir)),
            Err(_) => String::new(),
        },
        (_, Some((DoorLookup::NoDoor { dir }, exits))) => {
            let mut there = toward(&dir);
            there[..1].make_ascii_uppercase();
            format!(
                "{there} you see {}",
                glimpse(exits.0[&dir].to, actor.entity, &players, &rooms)
            )
        }
        (_, Some((DoorLookup::NotHere, _))) if is_direction(&inv.args.to_lowercase()) => {
            let dir = full_direction(&inv.args.to_lowercase()).to_string();
            format!("You see nothing special {}.\r\n", toward(&dir))
        }
        (Some(target), _) => {
            let candidates = targets_in_room(actor.room, &players, &keywords);
            match target
                .find_one(actor.entity, candidates)
                .and_then(|e| Actor::get(&players, e))
            {
                Some(other) if other.entity == actor.entity => {
                    "You look yourself over. You look ready for anything.\r\n".into()
                }
                Some(other) => format!("You see {}, standing here.\r\n", other.name),
                None => format!("You don't see {} here.\r\n", target.describe()),
            }
        }
    };
    deliver(vec![Outgoing::To(actor.entity, text)], &mut players);
}

/// `go <direction>`.
pub fn go(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    doors: Query<&Door>,
    db: Res<Database>,
) {
    let out = if inv.args.is_empty() {
        vec![Outgoing::line(inv.entity, "Go where?")]
    } else {
        travel(inv.entity, &inv.args, &mut players, &rooms, &doors, &db)
    };
    deliver(out, &mut players);
}

/// `north`, `south`, ... and their short forms: the command's name is the direction.
pub fn walk(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    doors: Query<&Door>,
    db: Res<Database>,
) {
    let out = travel(inv.entity, inv.command, &mut players, &rooms, &doors, &db);
    deliver(out, &mut players);
}

fn travel(
    entity: Entity,
    dir: &str,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    doors: &Query<&Door>,
    db: &Database,
) -> Vec<Outgoing> {
    let mut out = Vec::new();
    let Some(actor) = Actor::get(players, entity) else {
        return out;
    };
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let Some(&exit) = rooms
        .get(actor.room)
        .ok()
        .and_then(|(_, _, exits)| exits.0.get(&dir))
    else {
        out.push(Outgoing::line(entity, "You can't go that way."));
        return out;
    };
    if let Some(door) = exit.door.and_then(|d| doors.get(d).ok())
        && !door.is_open()
    {
        out.push(Outgoing::line(
            entity,
            format!("{} is closed.", door.the_capitalized()),
        ));
        return out;
    }
    let to = exit.to;

    out.push(Outgoing::Room {
        room: actor.room,
        except: entity,
        text: format!("{} leaves {dir}.\r\n", actor.name),
    });
    move_player(
        entity,
        actor.character_id,
        to,
        players,
        rooms,
        doors,
        db,
        &mut out,
    );
    out.push(Outgoing::Room {
        room: to,
        except: entity,
        text: format!("{} arrives.\r\n", actor.name),
    });
    out
}

/// Where a direction is, for the middle of a sentence: "to the north", "above", "below".
pub fn toward(dir: &str) -> String {
    match dir {
        "up" => "above".into(),
        "down" => "below".into(),
        other => format!("to the {other}"),
    }
}

/// Whether a word names a compass or vertical direction, in full or short form.
pub fn is_direction(word: &str) -> bool {
    matches!(
        full_direction(word),
        "north"
            | "south"
            | "east"
            | "west"
            | "up"
            | "down"
            | "northeast"
            | "northwest"
            | "southeast"
            | "southwest"
    )
}

/// Expand direction shorthands (`n` -> `north`).
pub fn full_direction(dir: &str) -> &str {
    match dir {
        "n" => "north",
        "s" => "south",
        "e" => "east",
        "w" => "west",
        "u" => "up",
        "d" => "down",
        other => other,
    }
}

/// Put a player in a room, remember it in the database, and show them where they are.
#[allow(clippy::too_many_arguments)]
pub fn move_player(
    entity: Entity,
    character_id: i64,
    to: Entity,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    doors: &Query<&Door>,
    db: &Database,
    out: &mut Vec<Outgoing>,
) {
    let mut show_ids = false;
    if let Ok((_, _, player, Some(mut in_room))) = players.get_mut(entity) {
        in_room.0 = to;
        show_ids = player.is_some_and(|p| p.role.can(Permission::Build));
    }
    if let Ok((_, key, _)) = rooms.get(to)
        && let Err(err) = db.save_location(character_id, &key.to_string())
    {
        error!("failed to save location for character {character_id}: {err}");
    }
    out.push(Outgoing::To(
        entity,
        describe_room(to, entity, players, rooms, doors, show_ids),
    ));
}

/// What you see looking into a neighbouring room: its name and who's there.
fn glimpse(room: Entity, viewer: Entity, players: &PlayerQuery, rooms: &RoomQuery) -> String {
    let name = rooms
        .get(room)
        .map(|(room, _, _)| room.name.clone())
        .unwrap_or_default();
    let mut text = format!("{name}.\r\n");
    for (other, _, player, in_room) in players.iter() {
        if let (Some(player), Some(in_room)) = (player, in_room)
            && other != viewer
            && in_room.0 == room
        {
            text += &format!("  {} is there.\r\n", player.name);
        }
    }
    text
}

/// The text shown by `look`. Builders also see the room's id.
pub fn describe_room(
    room: Entity,
    viewer: Entity,
    players: &PlayerQuery,
    rooms: &RoomQuery,
    doors: &Query<&Door>,
    show_id: bool,
) -> String {
    let Ok((info, key, exits)) = rooms.get(room) else {
        return "You are nowhere.\r\n".into();
    };

    let mut text = format!("\x1b[1;36m{}\x1b[0m", info.name);
    if show_id {
        text += &format!(" \x1b[2m[{key}]\x1b[0m");
    }
    text += &format!("\r\n{}\r\n", info.description);

    let exit_list: Vec<_> = exits
        .0
        .iter()
        .map(
            |(dir, exit)| match exit.door.and_then(|d| doors.get(d).ok()).map(|d| d.state) {
                Some(DoorState::Closed | DoorState::Locked) => format!("{dir} (closed)"),
                _ => dir.clone(),
            },
        )
        .collect();
    text += &format!(
        "\x1b[33mExits: {}\x1b[0m\r\n",
        if exit_list.is_empty() {
            "none".into()
        } else {
            exit_list.join(", ")
        }
    );

    for (other, _, player, in_room) in players.iter() {
        if let (Some(player), Some(in_room)) = (player, in_room)
            && other != viewer
            && in_room.0 == room
        {
            text += &format!("{} is here.\r\n", player.name);
        }
    }
    text
}
