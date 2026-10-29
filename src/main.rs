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
mod crafting;
mod db;
mod dice;
mod effects;
mod events;
mod facts;
mod formula;
mod gathering;
mod groups;
mod guild;
mod healing;
mod items;
mod login;
mod mobs;
mod money;
mod net;
mod output;
mod puppet;
mod quests;
mod requirements;
mod rules;
mod shops;
mod skills;
mod spawns;
mod spells;
mod stealth;
mod targeting;
mod telnet;
mod time;
mod tracking;
mod trade;
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
    let path = std::env::var("MUD_DB").unwrap_or_else(|_| db::DEFAULT_DB_PATH.into());
    let address = std::env::var("MUD_ADDR").unwrap_or_else(|_| net::DEFAULT_ADDRESS.into());
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
            1.0 / TICK_RATE,
        ))),
        LogPlugin::default(),
    ));
    add_game(&mut app, path, address);
    app.run();
}

/// Everything but the clock and the log: the engine and the game, with the database at `path`
/// and listening on `address`.
fn add_game(app: &mut App, path: String, address: String) {
    // The progression numbers are read now, so a mistake in their file stops the server at once.
    let numbers = attributes::progression();
    info!(
        "progression: {} experience and {} Karma a level",
        numbers.experience_per_level, numbers.karma_per_level
    );
    // Loads the zone files in `assets/zones` and hot-reloads them when they change on disk.
    app.add_plugins(AssetPlugin {
        watch_for_changes_override: Some(true),
        ..default()
    })
    // The engine: storage, connections, commands, time and events.
    .add_plugins((
        db::DbPlugin { path },
        dice::DicePlugin,
        money::MoneyPlugin,
        net::NetPlugin { address },
        login::LoginPlugin,
        command::CommandPlugin,
        commands::CommandsPlugin,
        build::BuildPlugin,
        events::EventsPlugin,
        facts::FactsPlugin,
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
        healing::HealingPlugin,
        gathering::GatheringPlugin,
        tracking::TrackingPlugin,
        stealth::StealthPlugin,
        groups::GroupsPlugin,
    ))
    // Magic, trade, the guild and its quests.
    .add_plugins((
        spells::SpellsPlugin,
        shops::ShopsPlugin,
        trade::TradePlugin,
        crafting::CraftingPlugin,
        puppet::PuppetPlugin,
        guild::GuildPlugin,
        quests::QuestsPlugin,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the whole server and runs it a few ticks. Every system and command is set up along
    /// the way, so one whose parameters clash (Bevy's B0001) fails here rather than live.
    #[test]
    fn the_server_starts() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        add_game(&mut app, ":memory:".into(), "127.0.0.1:0".into());
        for _ in 0..3 {
            app.update();
        }
    }
}
