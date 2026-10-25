//! Riftward: a classless telnet MUD built on a headless Bevy app.
//!
//! Run with `cargo run`, then connect with `nc localhost 4000` (or any telnet/MUD client).

mod aliases;
mod attributes;
mod build;
mod characters;
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
mod spawns;
mod targeting;
mod time;
mod vitals;
mod world;

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
        .add_plugins((
            db::DbPlugin,
            characters::CharactersPlugin,
            dice::DicePlugin,
            net::NetPlugin,
            world::WorldPlugin,
            login::LoginPlugin,
            command::CommandPlugin,
            commands::CommandsPlugin,
            build::BuildPlugin,
            items::ItemsPlugin,
            mobs::MobsPlugin,
            spawns::SpawnsPlugin,
            events::EventsPlugin,
            time::TimePlugin,
            vitals::VitalsPlugin,
        ))
        .run();
}
