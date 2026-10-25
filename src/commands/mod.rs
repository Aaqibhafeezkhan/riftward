//! Commands every logged-in player has. Each is a system registered with the command system in
//! `command.rs`; builder and admin commands live in `build.rs`.

mod character;
mod communication;
pub mod doors;
pub mod movement;

use bevy::prelude::*;

use crate::command::{self, AddCommand, Category, CommandSpec};
use crate::output::{Outgoing, PlayerQuery, deliver};

pub struct CommandsPlugin;

impl Plugin for CommandsPlugin {
    fn build(&self, app: &mut App) {
        use Category::*;
        app.add_systems(Update, announce_departures.before(command::dispatch))
            // Information
            .add_command(
                CommandSpec::new("look", Information)
                    .shortcuts(&["l"])
                    .usage("look [<target>]")
                    .summary("describe your surroundings, or someone here"),
                movement::look,
            )
            .add_command(
                CommandSpec::new("who", Information).summary("list players online"),
                communication::who,
            )
            // Movement
            .add_command(
                CommandSpec::new("go", Movement)
                    .usage("go <direction>")
                    .summary("move through an exit; or just n, s, e, w, u, d"),
                movement::go,
            )
            .add_command(direction("north", &["n"]), movement::walk)
            .add_command(direction("south", &["s"]), movement::walk)
            .add_command(direction("east", &["e"]), movement::walk)
            .add_command(direction("west", &["w"]), movement::walk)
            .add_command(direction("up", &["u"]), movement::walk)
            .add_command(direction("down", &["d"]), movement::walk)
            // Interaction
            .add_command(
                CommandSpec::new("open", Interaction)
                    .usage("open <door>")
                    .summary("open a door, by direction or name"),
                doors::open,
            )
            .add_command(
                CommandSpec::new("close", Interaction)
                    .usage("close <door>")
                    .summary("close a door"),
                doors::close,
            )
            .add_command(
                CommandSpec::new("lock", Interaction)
                    .usage("lock <door>")
                    .summary("lock a closed door, if you have the key"),
                doors::lock,
            )
            .add_command(
                CommandSpec::new("unlock", Interaction)
                    .usage("unlock <door>")
                    .summary("unlock a door, if you have the key"),
                doors::unlock,
            )
            .add_command(
                CommandSpec::new("pick", Interaction)
                    .usage("pick <door|container>")
                    .summary("pick a lock, if you know Lockpicking"),
                doors::pick,
            )
            .add_command(
                CommandSpec::new("bash", Interaction)
                    .usage("bash <door>")
                    .summary("try to force a locked door open with your Strength"),
                doors::bash,
            )
            // Communication
            .add_command(
                CommandSpec::new("say", Communication)
                    .usage("say <text>")
                    .summary("speak to the room; '<text> works too"),
                communication::say,
            )
            .add_command(
                CommandSpec::new("whisper", Communication)
                    .usage("whisper <player> <text>")
                    .summary("speak privately to someone in the room"),
                communication::whisper,
            )
            .add_command(
                CommandSpec::new("yell", Communication)
                    .usage("yell <text>")
                    .summary("shout to everyone in this zone"),
                communication::yell,
            )
            .add_command(
                CommandSpec::new("gossip", Communication)
                    .shortcuts(&["gos"])
                    .usage("gossip <text>")
                    .summary("chat with everyone online"),
                communication::gossip,
            )
            // Character
            .add_command(
                CommandSpec::new("score", Character).summary("show your attributes and Karma"),
                character::score,
            )
            .add_command(
                CommandSpec::new("describe", Character)
                    .usage("describe [<text>]")
                    .summary("see or change what others see when they look at you"),
                character::describe,
            )
            .add_command(
                CommandSpec::new("raise", Character)
                    .usage("raise <attribute>")
                    .summary("spend Karma to raise an attribute"),
                character::raise,
            )
            .add_command(
                CommandSpec::new("alias", Character)
                    .usage("alias [<name> [<command>]]")
                    .summary("list aliases, show one, or make <name> run <command>"),
                character::alias,
            )
            .add_command(
                CommandSpec::new("unalias", Character)
                    .usage("unalias <name>")
                    .summary("remove an alias"),
                character::unalias,
            )
            .add_command(
                CommandSpec::new("quit", Character).summary("leave the game"),
                character::quit,
            );
    }
}

fn direction(name: &'static str, shortcuts: &'static [&'static str]) -> CommandSpec {
    CommandSpec::new(name, Category::Movement)
        .shortcuts(shortcuts)
        .summary("move through that exit")
        .unlisted()
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
