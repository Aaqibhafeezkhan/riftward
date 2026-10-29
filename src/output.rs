//! Shared plumbing for characters and what they're told: who is where, and queuing text for one
//! character or a whole room.
//!
//! **Characters are players and mobs alike.** `Characters` reads both, and `Actor` is whichever
//! one is running a command: a player typing, or a mob a builder has puppeted (see
//! `puppet.rs`). Only players are saved, so `Actor::character_id` is `None` for a mob, and
//! anything that saves skips it.
//!
//! Text goes into the `Outbox` while systems work, and `flush_outbox` delivers it once a tick:
//! to a player's connection, or to whoever is puppeting a mob. A line starting with a name gets
//! its capital letter there, so "the town guard says" reads right wherever it lands.

use bevy::prelude::*;

use crate::combat::Conditions;
use crate::combat::the;
use crate::db::Role;
use crate::mobs::Mob;
use crate::net::Connection;
use crate::stealth::sees;
use crate::targeting::Keywords;
use crate::world::{Exits, InRoom, Player, Room, RoomKey};

/// Players and mobs: who they are and where.
pub type Characters<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        Option<&'static Player>,
        Option<&'static Mob>,
        &'static InRoom,
    ),
    Or<(With<Player>, With<Mob>)>,
>;

pub type RoomQuery<'w, 's> = Query<'w, 's, (&'static Room, &'static RoomKey, &'static Exits)>;

/// The character running a command, copied out of `Characters` so handlers can keep using it.
pub struct Actor {
    pub entity: Entity,
    /// As others say it: "Grok", "the town guard".
    pub name: String,
    /// Their saved character, if they're a player. Mobs aren't saved.
    pub character_id: Option<i64>,
    pub account: String,
    pub role: Role,
    /// Their race id, for requirements; empty for a beast.
    pub race: String,
    pub room: Entity,
}

impl Actor {
    /// The character behind `entity`, player or mob, if there is one.
    pub fn get(characters: &Characters, entity: Entity) -> Option<Self> {
        let (_, player, mob, in_room) = characters.get(entity).ok()?;
        Some(match (player, mob) {
            (Some(player), _) => Self {
                entity,
                name: player.name.clone(),
                character_id: Some(player.character_id),
                account: player.account.clone(),
                role: player.role,
                race: player.race.clone(),
                room: in_room.0,
            },
            (None, Some(mob)) => Self {
                entity,
                name: the(&mob.name),
                character_id: None,
                account: String::new(),
                role: Role::Player,
                race: mob.race.clone().unwrap_or_default(),
                room: in_room.0,
            },
            (None, None) => return None,
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
    /// A full line of text for one character.
    pub fn line(entity: Entity, text: impl Into<String>) -> Self {
        Outgoing::To(entity, text.into() + "\r\n")
    }
}

/// What everyone is to be told this tick, in order.
#[derive(Resource, Default)]
pub struct Outbox(pub Vec<Outgoing>);

/// A builder puppeting a mob: their input goes to it, and what it's told goes to them.
#[derive(Component, Debug, Clone, Copy)]
pub struct Puppeting(pub Entity);

/// A mob being puppeted, by the builder whose entity this is.
#[derive(Component, Debug, Clone, Copy)]
pub struct PuppetedBy(pub Entity);

/// A command's reply: `Ok` for success, `Err` for a problem, shown in red.
pub type Reply = Result<String, String>;

/// Turn a `Reply` into a line for the character who ran the command.
pub fn reply_line(entity: Entity, reply: Reply) -> Outgoing {
    match reply {
        Ok(text) => Outgoing::line(entity, text),
        Err(text) => Outgoing::line(entity, format!("\x1b[31m{text}\x1b[0m")),
    }
}

/// Queue `out` to be told.
pub fn deliver(out: Vec<Outgoing>, outbox: &mut Outbox) {
    outbox.0.extend(out);
}

/// `text` with the first letter of each line capitalised, past any colour codes: a line that
/// starts with a mob's name, "the town guard says", reads "The town guard says".
fn capital_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut start = true;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // A colour code: copy it through to its closing letter.
            out.push(c);
            for code in chars.by_ref() {
                out.push(code);
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        start = c == '\n';
    }
    out
}

/// Characters' rooms, for delivering what's said in one.
type Present<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static InRoom,
        Option<&'static PuppetedBy>,
        Has<Puppeting>,
    ),
    Or<(With<Player>, With<Mob>)>,
>;

/// Deliver everything in the outbox: to players' connections, and what a puppeted mob is told
/// to the builder puppeting it. A builder puppeting something hears nothing of their own body,
/// only their puppet.
pub fn flush_outbox(
    mut outbox: ResMut<Outbox>,
    mut connections: Query<&mut Connection>,
    present: Present,
    puppeted: Query<&PuppetedBy>,
    puppeting: Query<(), With<Puppeting>>,
    hidden: Query<&Conditions>,
) {
    let mut send = |to: Entity, text: &str| {
        if puppeting.get(to).is_ok() {
            return;
        }
        let to = puppeted.get(to).map_or(to, |by| by.0);
        if let Ok(mut conn) = connections.get_mut(to) {
            conn.send_raw(capital_lines(text));
        }
    };
    for msg in std::mem::take(&mut outbox.0) {
        match msg {
            Outgoing::To(entity, text) => send(entity, &text),
            Outgoing::Room { room, except, text } => {
                for entity in audience(room, except, &present, &hidden) {
                    send(entity, &text);
                }
            }
        }
    }
}

/// Who's told what `who` does in `room`: everyone there but them, and while they're still hidden
/// (doing something quiet, like reading tracks from the shadows), only those who've noticed them.
fn audience(
    room: Entity,
    who: Entity,
    present: &Present,
    hidden: &Query<&Conditions>,
) -> Vec<Entity> {
    present
        .iter()
        .filter(|(entity, in_room, _, puppeting)| {
            *entity != who && in_room.0 == room && !puppeting && sees(hidden, *entity, who)
        })
        .map(|(entity, ..)| entity)
        .collect()
}

/// Logged-in players in `room` with their keywords, in a stable order, for resolving targets.
pub fn targets_in_room<'a>(
    room: Entity,
    characters: &Characters,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    characters
        .iter()
        .filter(|(_, player, _, in_room)| player.is_some() && in_room.0 == room)
        .filter_map(|(entity, ..)| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}

/// Everyone in `room`, players and mobs, with their keywords: someone to hand something to.
pub fn characters_in_room<'a>(
    room: Entity,
    characters: &Characters,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    let mut here: Vec<(Entity, &Keywords)> = characters
        .iter()
        .filter(|(.., in_room)| in_room.0 == room)
        .filter_map(|(entity, ..)| Some((entity, keywords.get(entity).ok()?)))
        .collect();
    here.sort_by_key(|(entity, _)| *entity);
    here
}

/// Every logged-in player with their keywords, for commands that reach anyone online.
pub fn targets_online<'a>(
    characters: &Characters,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    characters
        .iter()
        .filter(|(_, player, _, _)| player.is_some())
        .filter_map(|(entity, ..)| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashSet;

    #[test]
    fn what_a_hidden_character_does_is_seen_only_by_those_who_noticed() {
        let mut world = World::new();
        let room = world.spawn_empty().id();
        let mob = |world: &mut World| {
            world
                .spawn((
                    Mob {
                        key: crate::world::MobKey::parse("test:mob", ""),
                        name: "a mob".into(),
                        here: String::new(),
                        description: String::new(),
                        race: None,
                    },
                    InRoom(room),
                ))
                .id()
        };
        let (tracker, noticer, other) = (mob(&mut world), mob(&mut world), mob(&mut world));
        let told = |world: &mut World| {
            let mut told = world
                .run_system_once(move |present: Present, hidden: Query<&Conditions>| {
                    audience(room, tracker, &present, &hidden)
                })
                .unwrap();
            told.sort();
            told
        };
        let mut everyone = vec![noticer, other];
        everyone.sort();
        assert_eq!(told(&mut world), everyone, "in plain sight");
        let defs = crate::combat::defs_on_disk();
        let mut conditions = Conditions::default();
        conditions.apply(
            &defs,
            crate::combat::HIDDEN,
            10,
            crate::combat::Lasts::Indefinitely,
        );
        conditions.hidden_mut().unwrap().seen_by = HashSet::from([noticer]);
        world.entity_mut(tracker).insert(conditions);
        assert_eq!(told(&mut world), vec![noticer]);
    }

    #[test]
    fn lines_start_with_a_capital() {
        assert_eq!(
            capital_lines("the town guard says hello.\r\nthe rat squeaks.\r\n"),
            "The town guard says hello.\r\nThe rat squeaks.\r\n"
        );
        assert_eq!(
            capital_lines("\x1b[1;31mthe rat attacks you!\x1b[0m"),
            "\x1b[1;31mThe rat attacks you!\x1b[0m"
        );
        assert_eq!(
            capital_lines("  a knife\r\n"),
            "  a knife\r\n",
            "indented stays"
        );
    }
}
