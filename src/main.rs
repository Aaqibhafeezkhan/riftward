//! Riftward: a classless telnet MUD built on a headless Bevy app.
//!
//! Run with `cargo run`, then connect with `nc localhost 4000` (or any telnet/MUD client).

mod aliases;
mod attributes;
mod build;
mod characters;
mod combat;
mod command;
mod commands;
mod conditions;
mod db;
mod dice;
mod events;
mod items;
mod login;
mod mobs;
mod net;
mod output;
mod skills;
mod spawns;
mod targeting;
mod telnet;
mod time;
mod trainers;
mod vitals;
mod world;
mod world_events;

use std::time::Duration;

use bevy::app::ScheduleRunnerPlugin;
use bevy::log::LogPlugin;
use bevy::prelude::*;

/// How many times per second the server ticks.
const TICK_RATE: f64 = 30.0;

fn main() {
    App::new()
        .add_plugins((
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                1.0 / TICK_RATE,
            ))),
            LogPlugin::default(),
            // Loads the zone files in `assets/zones` and hot-reloads them when they change on disk.
            AssetPlugin {
                watch_for_changes_override: Some(true),
                ..default()
            },
        ))
        // The engine: storage, connections, commands, time and events.
        .add_plugins((
            db::DbPlugin,
            dice::DicePlugin,
            net::NetPlugin,
            login::LoginPlugin,
            command::CommandPlugin,
            commands::CommandsPlugin,
            build::BuildPlugin,
            events::EventsPlugin,
            time::TimePlugin,
        ))
        // The game: the world and what's in it, and characters.
        .add_plugins((
            world::WorldPlugin,
            items::ItemsPlugin,
            mobs::MobsPlugin,
            spawns::SpawnsPlugin,
            characters::CharactersPlugin,
            vitals::VitalsPlugin,
            skills::SkillsPlugin,
            combat::CombatPlugin,
            world_events::WorldEventsPlugin,
            trainers::TrainersPlugin,
        ))
        .run();
}
