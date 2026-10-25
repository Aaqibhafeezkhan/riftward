//! Looking around and moving between rooms.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::command::Invocation;
use crate::commands::doors::{DoorLookup, find_door};
use crate::db::{Database, Permission};
use crate::events::{GameEvent, Way, way_back};
use crate::items::{
    ItemLocation, ItemQuery, ItemTemplates, capitalized, counted, describe_item, find_item,
};
use crate::mobs::{self, MobQuery};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver, targets_in_room};
use crate::targeting::{Keywords, Target};
use crate::world::{Door, DoorState};

/// Everything a room shows a player besides other players: the room itself, its doors, and the
/// items lying in it.
#[derive(SystemParam)]
pub struct View<'w, 's> {
    pub rooms: RoomQuery<'w, 's>,
    pub doors: Query<'w, 's, &'static Door>,
    pub items: ItemQuery<'w, 's>,
    pub templates: Res<'w, ItemTemplates>,
    pub mobs: MobQuery<'w, 's>,
}

/// `look` describes the room; `look <target>` looks along an exit, at a door, at someone here, or
/// at an item here or in your pack.
pub fn look(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    view: View,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let lookup = view
        .rooms
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
            &view,
            actor.role.can(Permission::Build),
        ),
        (_, Some((DoorLookup::Door { door, dir }, exits))) => match view.doors.get(door) {
            Ok(door) if door.is_open() => format!(
                "Through the open {} {} you see {}",
                door.the().trim_start_matches("the "),
                toward(&dir),
                glimpse(exits.0[&dir].to, actor.entity, &players, &view)
            ),
            Ok(door) => format!("You see {} {}. It is closed.\r\n", door.name, toward(&dir)),
            Err(_) => String::new(),
        },
        (_, Some((DoorLookup::NoDoor { dir }, exits))) => {
            let mut there = toward(&dir);
            there[..1].make_ascii_uppercase();
            format!(
                "{there} you see {}",
                glimpse(exits.0[&dir].to, actor.entity, &players, &view)
            )
        }
        (_, Some((DoorLookup::NotHere, _))) if is_direction(&inv.args.to_lowercase()) => {
            let dir = full_direction(&inv.args.to_lowercase()).to_string();
            format!("You see nothing special {}.\r\n", toward(&dir))
        }
        (Some(target), _) => {
            let mut candidates = targets_in_room(actor.room, &players, &keywords);
            candidates.extend(mobs::targets_in(actor.room, &view.mobs, &keywords));
            match target
                .find_one(actor.entity, candidates)
                .map(|e| (e, Actor::get(&players, e), view.mobs.get(e).ok()))
            {
                Some((e, _, _)) if e == actor.entity => {
                    "You look yourself over. You look ready for anything.\r\n".into()
                }
                Some((_, Some(other), _)) => format!("You see {}, standing here.\r\n", other.name),
                Some((_, _, Some((_, mob, _)))) => mobs::describe(mob) + "\r\n",
                _ => match find_item(&inv.args, &actor, &view.items, &keywords, &templates) {
                    Ok(item) => describe_item(item, &view.items, &templates) + "\r\n",
                    Err(err) => err + "\r\n",
                },
            }
        }
    };
    deliver(vec![Outgoing::To(actor.entity, text)], &mut players);
}

/// `go <direction>`.
pub fn go(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    view: View,
    db: Res<Database>,
    mut events: MessageWriter<GameEvent>,
) {
    let out = if inv.args.is_empty() {
        vec![Outgoing::line(inv.entity, "Go where?")]
    } else {
        travel(inv.entity, &inv.args, &mut players, &view, &db, &mut events)
    };
    deliver(out, &mut players);
}

/// `north`, `south`, ... and their short forms: the command's name is the direction.
pub fn walk(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    view: View,
    db: Res<Database>,
    mut events: MessageWriter<GameEvent>,
) {
    let out = travel(
        inv.entity,
        inv.command,
        &mut players,
        &view,
        &db,
        &mut events,
    );
    deliver(out, &mut players);
}

fn travel(
    entity: Entity,
    dir: &str,
    players: &mut PlayerQuery,
    view: &View,
    db: &Database,
    events: &mut MessageWriter<GameEvent>,
) -> Vec<Outgoing> {
    let mut out = Vec::new();
    let Some(actor) = Actor::get(players, entity) else {
        return out;
    };
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let Some(&exit) = view
        .rooms
        .get(actor.room)
        .ok()
        .and_then(|(_, _, exits)| exits.0.get(&dir))
    else {
        out.push(Outgoing::line(entity, "You can't go that way."));
        return out;
    };
    if let Some(door) = exit.door.and_then(|d| view.doors.get(d).ok())
        && !door.is_open()
    {
        out.push(Outgoing::line(
            entity,
            format!("{} is closed.", door.the_capitalized()),
        ));
        return out;
    }
    let to = exit.to;

    events.write(GameEvent::Left {
        who: entity,
        name: actor.name.clone(),
        room: actor.room,
        way: Way::Walked(Some(dir)),
    });
    move_player(entity, actor.character_id, to, players, view, db, &mut out);
    events.write(GameEvent::Entered {
        who: entity,
        name: actor.name.clone(),
        room: to,
        way: Way::Walked(way_back(&view.rooms, to, actor.room)),
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
pub fn move_player(
    entity: Entity,
    character_id: i64,
    to: Entity,
    players: &mut PlayerQuery,
    view: &View,
    db: &Database,
    out: &mut Vec<Outgoing>,
) {
    let mut show_ids = false;
    if let Ok((_, _, player, Some(mut in_room))) = players.get_mut(entity) {
        in_room.0 = to;
        show_ids = player.is_some_and(|p| p.role.can(Permission::Build));
    }
    if let Ok((_, key, _)) = view.rooms.get(to)
        && let Err(err) = db.save_location(character_id, &key.to_string())
    {
        error!("failed to save location for character {character_id}: {err}");
    }
    out.push(Outgoing::To(
        entity,
        describe_room(to, entity, players, view, show_ids),
    ));
}

/// What you see looking into a neighbouring room: its name and who's there.
fn glimpse(room: Entity, viewer: Entity, players: &PlayerQuery, view: &View) -> String {
    let name = view
        .rooms
        .get(room)
        .map(|(room, _, _)| room.name.clone())
        .unwrap_or_default();
    let mut text = format!("{name}.\r\n");
    for name in mobs::names_in(room, &view.mobs) {
        text += &format!("  {} is there.\r\n", capitalized(&name));
    }
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
    view: &View,
    show_id: bool,
) -> String {
    let Ok((info, key, exits)) = view.rooms.get(room) else {
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
        .map(|(dir, exit)| {
            let state = exit
                .door
                .and_then(|d| view.doors.get(d).ok())
                .map(|d| d.state);
            match state {
                Some(DoorState::Closed | DoorState::Locked) => format!("{dir} (closed)"),
                _ => dir.clone(),
            }
        })
        .collect();
    text += &format!(
        "\x1b[33mExits: {}\x1b[0m\r\n",
        if exit_list.is_empty() {
            "none".into()
        } else {
            exit_list.join(", ")
        }
    );

    // Fixed things, like a chest or a signpost, are part of the room; loose things lie in it.
    let fixed = |entity: Entity| {
        view.items
            .get(entity)
            .ok()
            .and_then(|(_, item, _)| view.templates.0.get(&item.key))
            .is_some_and(|def| def.fixed)
    };
    let mut here: Vec<_> = view
        .items
        .iter()
        .filter(|(_, _, loc)| **loc == ItemLocation::Room(room))
        .map(|(entity, _, _)| entity)
        .collect();
    here.sort();
    for entity in here.iter().filter(|e| fixed(**e)) {
        if let Ok((_, item, _)) = view.items.get(*entity) {
            text += &format!("{} is here.\r\n", capitalized(&item.name));
        }
    }
    let loose: Vec<_> = here
        .iter()
        .filter(|e| !fixed(**e))
        .filter_map(|e| {
            view.items
                .get(*e)
                .ok()
                .map(|(_, item, _)| item.name.as_str())
        })
        .collect();
    for name in counted(loose) {
        text += &format!("{} lies here.\r\n", capitalized(&name));
    }
    for line in mobs::here_lines(room, &view.mobs) {
        text += &format!("{line}\r\n");
    }

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
