//! Looking around and moving between rooms.

use bevy::prelude::*;

use crate::command::Invocation;
use crate::db::{Database, Permission};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver, targets_in_room};
use crate::targeting::{Keywords, Target};

/// `look` describes the room; `look <target>` looks at someone in it.
pub fn look(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    keywords: Query<&Keywords>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = match Target::parse(&inv.args) {
        None => describe_room(
            actor.room,
            actor.entity,
            &players,
            &rooms,
            actor.role.can(Permission::Build),
        ),
        Some(target) => {
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
pub fn go(In(inv): In<Invocation>, mut players: PlayerQuery, rooms: RoomQuery, db: Res<Database>) {
    let out = if inv.args.is_empty() {
        vec![Outgoing::line(inv.entity, "Go where?")]
    } else {
        travel(inv.entity, &inv.args, &mut players, &rooms, &db)
    };
    deliver(out, &mut players);
}

/// `north`, `south`, ... and their short forms: the command's name is the direction.
pub fn walk(In(inv): In<Invocation>, mut players: PlayerQuery, rooms: RoomQuery, db: Res<Database>) {
    let out = travel(inv.entity, inv.command, &mut players, &rooms, &db);
    deliver(out, &mut players);
}

fn travel(
    entity: Entity,
    dir: &str,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    db: &Database,
) -> Vec<Outgoing> {
    let mut out = Vec::new();
    let Some(actor) = Actor::get(players, entity) else {
        return out;
    };
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let Some(&to) = rooms
        .get(actor.room)
        .ok()
        .and_then(|(_, _, exits)| exits.0.get(&dir))
    else {
        out.push(Outgoing::line(entity, "You can't go that way."));
        return out;
    };

    out.push(Outgoing::Room {
        room: actor.room,
        except: entity,
        text: format!("{} leaves {dir}.\r\n", actor.name),
    });
    move_player(entity, actor.character_id, to, players, rooms, db, &mut out);
    out.push(Outgoing::Room {
        room: to,
        except: entity,
        text: format!("{} arrives.\r\n", actor.name),
    });
    out
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
pub fn move_player(
    entity: Entity,
    character_id: i64,
    to: Entity,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
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
        describe_room(to, entity, players, rooms, show_ids),
    ));
}

/// The text shown by `look`. Builders also see the room's id.
pub fn describe_room(
    room: Entity,
    viewer: Entity,
    players: &PlayerQuery,
    rooms: &RoomQuery,
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

    let exit_list: Vec<_> = exits.0.keys().map(String::as_str).collect();
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
