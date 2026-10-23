//! Commands for logged-in characters (look, go, say, who, quit, ...), plus the shared plumbing
//! for sending text to players and rooms. Builder and admin verbs are forwarded to `build.rs`
//! as `BuildCommand` messages once the player's permissions have been checked.

use bevy::prelude::*;

use crate::db::{Database, Permission, Role};
use crate::net::{Connection, PlayerInput};
use crate::world::{Exits, InRoom, Player, Room, RoomKey};

pub struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<BuildCommand>()
            .add_systems(Update, (announce_departures, handle_commands).chain());
    }
}

const HELP: &str = "Commands:
  look (l)                 describe your surroundings
  go <dir> / n s e w u d   move through an exit
  say <text> / '<text>     speak to the room
  whisper <player> <text>  speak privately to someone in the room
  yell <text>              shout to everyone in this zone
  gossip <text> (gos)      chat with everyone online
  who                      list players online
  help                     show this message
  quit                     leave the game";

const BUILDER_HELP: &str = "Builder commands:
  where                    show the current room's id
  rooms                    list the rooms in this zone
  goto <room>              teleport to a room (`room` or `zone:room`)
  dig <dir> <name>         create a room through a new exit, with a way back
  rename <name>            rename this room
  desc <text>              set this room's description
  link <dir> <room>        add a one-way exit to an existing room
  unlink <dir>             remove an exit
  zones                    list all zones
  zone create <id> <name>  create a new zone, starting with the room <id>:entrance
  zone rename <name>       rename the zone you're in";

const ADMIN_HELP: &str = "Admin commands:
  role <account> <player|builder|admin>   change an account's role";

/// Verbs handled by `build.rs`, and the permission each needs.
const RESTRICTED_VERBS: &[(&str, Permission)] = &[
    ("where", Permission::Build),
    ("rooms", Permission::Build),
    ("goto", Permission::Build),
    ("dig", Permission::Build),
    ("rename", Permission::Build),
    ("desc", Permission::Build),
    ("link", Permission::Build),
    ("unlink", Permission::Build),
    ("zones", Permission::Build),
    ("zone", Permission::Build),
    ("role", Permission::ManageRoles),
];

/// A builder/admin command from a player who has permission to run it.
#[derive(Message)]
pub struct BuildCommand {
    pub entity: Entity,
    pub verb: String,
    pub args: String,
}

pub type PlayerQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Connection,
        Option<&'static mut Player>,
        Option<&'static mut InRoom>,
    ),
>;

pub type RoomQuery<'w, 's> = Query<'w, 's, (&'static Room, &'static RoomKey, &'static Exits)>;

/// Text produced while handling input, delivered once all input is processed.
pub enum Outgoing {
    To(Entity, String),
    Room {
        room: Entity,
        except: Entity,
        text: String,
    },
}

impl Outgoing {
    /// A full line of text for one player.
    pub fn line(entity: Entity, text: impl Into<String>) -> Self {
        Outgoing::To(entity, text.into() + "\r\n")
    }

    pub fn prompt(entity: Entity) -> Self {
        Outgoing::To(entity, "> ".into())
    }
}

/// Split input into a lowercase verb and the rest. `'hello` is shorthand for `say hello`.
fn split_verb(line: &str) -> (String, &str) {
    if let Some(text) = line.strip_prefix('\'') {
        return ("say".into(), text.trim());
    }
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    (verb.to_ascii_lowercase(), rest.trim())
}

/// Tell the room when a player's connection drops.
fn announce_departures(mut players: PlayerQuery) {
    let out: Vec<_> = players
        .iter()
        .filter_map(|(entity, conn, player, in_room)| match (player, in_room) {
            (Some(player), Some(in_room)) if conn.closing => Some(Outgoing::Room {
                room: in_room.0,
                except: entity,
                text: format!("{} has left the game.\r\n", player.name),
            }),
            _ => None,
        })
        .collect();
    deliver(out, &mut players);
}

pub fn handle_commands(
    mut inputs: MessageReader<PlayerInput>,
    mut build: MessageWriter<BuildCommand>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    for PlayerInput { entity, line } in inputs.read() {
        run_command(*entity, line, &mut players, &rooms, &db, &mut build, &mut out);
    }
    deliver(out, &mut players);
}

fn run_command(
    entity: Entity,
    line: &str,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    db: &Database,
    build: &mut MessageWriter<BuildCommand>,
    out: &mut Vec<Outgoing>,
) {
    // Input from connections that haven't logged in yet is handled by `login.rs`, and anything
    // typed after `quit` (in the same frame) is dropped.
    let Ok((_, conn, Some(player), Some(in_room))) = players.get(entity) else {
        return;
    };
    if conn.closing {
        return;
    }
    let name = player.name.clone();
    let character_id = player.character_id;
    let role = player.role;
    let room = in_room.0;

    let (verb, rest) = split_verb(line);

    if let Some((_, permission)) = RESTRICTED_VERBS.iter().find(|(v, _)| *v == verb)
        && role.can(*permission)
    {
        build.write(BuildCommand {
            entity,
            verb,
            args: rest.into(),
        });
        return;
    }

    match verb.as_str() {
        "" => {}
        "look" | "l" => out.push(Outgoing::To(
            entity,
            describe_room(room, entity, players, rooms, role.can(Permission::Build)),
        )),
        "help" | "?" => out.push(Outgoing::line(entity, help_for(role))),
        "who" => {
            let mut names: Vec<_> = players
                .iter()
                .filter_map(|(_, _, p, _)| p.map(|p| p.name.clone()))
                .collect();
            names.sort();
            out.push(Outgoing::line(
                entity,
                format!(
                    "Players online ({}):\r\n  {}",
                    names.len(),
                    names.join("\r\n  ")
                ),
            ));
        }
        "say" if rest.is_empty() => out.push(Outgoing::line(entity, "Say what?")),
        "say" => {
            out.push(Outgoing::line(entity, format!("You say, \"{rest}\"")));
            out.push(Outgoing::Room {
                room,
                except: entity,
                text: format!("{name} says, \"{rest}\"\r\n"),
            });
        }
        "whisper" => whisper(entity, &name, room, rest, players, out),
        "yell" if rest.is_empty() => out.push(Outgoing::line(entity, "Yell what?")),
        "yell" => {
            out.push(Outgoing::line(
                entity,
                format!("\x1b[1;31mYou yell, \"{rest}\"\x1b[0m"),
            ));
            if let Ok((_, key, _)) = rooms.get(room) {
                let text = format!("\x1b[1;31m{name} yells, \"{rest}\"\x1b[0m\r\n");
                for other in players_in_zone(&key.zone, players, rooms) {
                    if other != entity {
                        out.push(Outgoing::To(other, text.clone()));
                    }
                }
            }
        }
        "gossip" | "gos" if rest.is_empty() => out.push(Outgoing::line(entity, "Gossip what?")),
        "gossip" | "gos" => {
            out.push(Outgoing::line(
                entity,
                format!("\x1b[35m[Gossip] You: {rest}\x1b[0m"),
            ));
            let text = format!("\x1b[35m[Gossip] {name}: {rest}\x1b[0m\r\n");
            for other in players_online(players) {
                if other != entity {
                    out.push(Outgoing::To(other, text.clone()));
                }
            }
        }
        "quit" => {
            out.push(Outgoing::line(entity, "Farewell!"));
            out.push(Outgoing::Room {
                room,
                except: entity,
                text: format!("{name} has left the game.\r\n"),
            });
            if let Ok((_, mut conn, _, _)) = players.get_mut(entity) {
                conn.closing = true;
            }
            return;
        }
        "go" if rest.is_empty() => out.push(Outgoing::line(entity, "Go where?")),
        "go" => go(entity, &name, character_id, room, rest, players, rooms, db, out),
        "n" | "s" | "e" | "w" | "u" | "d" | "north" | "south" | "east" | "west" | "up"
        | "down" => go(entity, &name, character_id, room, &verb, players, rooms, db, out),
        _ => out.push(Outgoing::line(
            entity,
            "Huh? Type 'help' for a list of commands.",
        )),
    }

    out.push(Outgoing::prompt(entity));
}

/// Only the target hears the words; everyone else in the room just sees that a whisper happened.
fn whisper(
    entity: Entity,
    name: &str,
    room: Entity,
    args: &str,
    players: &PlayerQuery,
    out: &mut Vec<Outgoing>,
) {
    let Some((target_name, text)) = args
        .split_once(' ')
        .map(|(t, text)| (t, text.trim()))
        .filter(|(_, text)| !text.is_empty())
    else {
        out.push(Outgoing::line(entity, "Usage: whisper <player> <text>"));
        return;
    };

    let target = players.iter().find_map(|(other, _, player, in_room)| {
        let player = player?;
        let here = in_room.is_some_and(|r| r.0 == room);
        (here && player.name.eq_ignore_ascii_case(target_name)).then(|| (other, player.name.clone()))
    });
    let Some((target, target_name)) = target else {
        out.push(Outgoing::line(entity, format!("You don't see {target_name} here.")));
        return;
    };
    if target == entity {
        out.push(Outgoing::line(entity, "You mutter something to yourself."));
        return;
    }

    out.push(Outgoing::line(
        entity,
        format!("\x1b[2mYou whisper to {target_name}, \"{text}\"\x1b[0m"),
    ));
    out.push(Outgoing::line(
        target,
        format!("\x1b[2m{name} whispers to you, \"{text}\"\x1b[0m"),
    ));
    let seen = format!("{name} whispers something into {target_name}'s ear.\r\n");
    for (other, _, player, in_room) in players.iter() {
        if player.is_some()
            && in_room.is_some_and(|r| r.0 == room)
            && other != entity
            && other != target
        {
            out.push(Outgoing::To(other, seen.clone()));
        }
    }
}

/// Every logged-in player.
fn players_online(players: &PlayerQuery) -> Vec<Entity> {
    players
        .iter()
        .filter(|(_, _, player, _)| player.is_some())
        .map(|(entity, ..)| entity)
        .collect()
}

/// Every logged-in player standing in a room of the given zone.
fn players_in_zone(zone: &str, players: &PlayerQuery, rooms: &RoomQuery) -> Vec<Entity> {
    players
        .iter()
        .filter_map(|(entity, _, player, in_room)| {
            let room = in_room.filter(|_| player.is_some())?.0;
            let (_, key, _) = rooms.get(room).ok()?;
            (key.zone == zone).then_some(entity)
        })
        .collect()
}

fn help_for(role: Role) -> String {
    let mut text = HELP.to_string();
    if role.can(Permission::Build) {
        text = text + "\n\n" + BUILDER_HELP;
    }
    if role.can(Permission::ManageRoles) {
        text = text + "\n\n" + ADMIN_HELP;
    }
    text.replace('\n', "\r\n")
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

#[allow(clippy::too_many_arguments)]
fn go(
    entity: Entity,
    name: &str,
    character_id: i64,
    from: Entity,
    dir: &str,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    db: &Database,
    out: &mut Vec<Outgoing>,
) {
    let dir = full_direction(dir);
    let Ok((_, _, exits)) = rooms.get(from) else {
        return;
    };
    let Some(&to) = exits.0.get(dir) else {
        out.push(Outgoing::line(entity, "You can't go that way."));
        return;
    };

    out.push(Outgoing::Room {
        room: from,
        except: entity,
        text: format!("{name} leaves {dir}.\r\n"),
    });
    move_player(entity, character_id, to, players, rooms, db, out);
    out.push(Outgoing::Room {
        room: to,
        except: entity,
        text: format!("{name} arrives.\r\n"),
    });
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

pub fn deliver(out: Vec<Outgoing>, players: &mut PlayerQuery) {
    for msg in out {
        match msg {
            Outgoing::To(entity, text) => {
                if let Ok((_, mut conn, _, _)) = players.get_mut(entity) {
                    conn.send_raw(text);
                }
            }
            Outgoing::Room { room, except, text } => {
                for (entity, mut conn, _, in_room) in players.iter_mut() {
                    if entity != except && in_room.is_some_and(|r| r.0 == room) {
                        conn.send_raw(&text);
                    }
                }
            }
        }
    }
}
