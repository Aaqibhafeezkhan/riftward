//! Shared plumbing for sending text to players: who is where, and queuing text for one player or a
//! whole room. Text is collected as `Outgoing` while a system works, then delivered in one pass.

use bevy::prelude::*;

use crate::db::Role;
use crate::net::Connection;
use crate::targeting::Keywords;
use crate::world::{Exits, InRoom, Player, Room, RoomKey};

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

/// The player running a command, copied out of `PlayerQuery` so handlers can keep using it.
pub struct Actor {
    pub entity: Entity,
    pub name: String,
    pub character_id: i64,
    pub account: String,
    pub role: Role,
    pub room: Entity,
}

impl Actor {
    /// The logged-in player behind `entity`, if there is one.
    pub fn get(players: &PlayerQuery, entity: Entity) -> Option<Self> {
        let (_, _, player, in_room) = players.get(entity).ok()?;
        let (player, in_room) = (player?, in_room?);
        Some(Self {
            entity,
            name: player.name.clone(),
            character_id: player.character_id,
            account: player.account.clone(),
            role: player.role,
            room: in_room.0,
        })
    }
}

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
}

/// A command's reply: `Ok` for success, `Err` for a problem, shown in red.
pub type Reply = Result<String, String>;

/// Turn a `Reply` into a line for the player who ran the command.
pub fn reply_line(entity: Entity, reply: Reply) -> Outgoing {
    match reply {
        Ok(text) => Outgoing::line(entity, text),
        Err(text) => Outgoing::line(entity, format!("\x1b[31m{text}\x1b[0m")),
    }
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

/// Logged-in players in `room` with their keywords, in a stable order, for resolving targets.
pub fn targets_in_room<'a>(
    room: Entity,
    players: &PlayerQuery,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    players
        .iter()
        .filter(|(_, _, player, in_room)| player.is_some() && in_room.is_some_and(|r| r.0 == room))
        .filter_map(|(entity, ..)| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}

/// Every logged-in player with their keywords, for commands that reach anyone online.
pub fn targets_online<'a>(
    players: &PlayerQuery,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    players
        .iter()
        .filter(|(_, _, player, _)| player.is_some())
        .filter_map(|(entity, ..)| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}
