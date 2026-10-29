//! Quests: written as data, one RON file each, in a folder named for the zone they belong to
//! (`assets/quests/riftwatch/rats_in_the_drains.quest.ron` is `riftwatch:rats_in_the_drains`).
//! A quest is posted on a board, taken with the guild's clerk, kept in each character's journal,
//! counted from game events as its own facts, and reported to be paid; claimed ones run out, and
//! a group does them together. See `docs/quests.md`.
//!
//! - `def`: the quest file, checked and loaded.
//! - `board`: `board` and `accept`, and offers to a group.
//! - `journal`: `QuestLog` and `journal`.
//! - `progress`: counting kills, and moving through stages.
//! - `report`: `report`, and paying out.
//! - `claims`: deadlines, and `abandon`.
//! - `failures`: what a quest's `fails_if` fails it on.
//! - `loot`: what a boss drops for those on its quest.
//! - `ask`: asking mobs about the topics a quest gives them.
//! - `spawned`: mobs a quest brings into the world, and clearing them away.
//! - `why`: GMs' account of where someone stands in a quest, and why.

mod actions;
mod ask;
mod board;
mod claims;
mod def;
mod failures;
mod journal;
mod loot;
mod progress;
mod report;
mod spawned;
mod why;

use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec};
use crate::db::Permission;
use ask::ask;
use board::{Offers, accept, board};
use claims::{Claims, abandon, expire_claims, load_claims};
use def::{QuestDef, QuestLoader, load_quests, sync_quests};
use failures::{fail_on_deaths, fail_on_losses, fail_pending};
use journal::journal;
use loot::{deaths, want_loot};
use progress::count_progress;
use report::report;
use spawned::{QuestMobs, clear_quest_mobs};
use why::why;

pub use def::Quests;
pub use journal::QuestLog;

pub struct QuestsPlugin;

impl Plugin for QuestsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<QuestDef>()
            .register_asset_loader(QuestLoader)
            .init_resource::<Quests>()
            .init_resource::<Claims>()
            .init_resource::<Offers>()
            .init_resource::<QuestMobs>()
            .init_resource::<actions::PendingFailures>()
            .add_systems(Startup, (load_quests, load_claims))
            .add_systems(Update, (sync_quests, count_progress, fail_on_deaths))
            // What a stage's `Fail` failed, written off once whatever ran it is done.
            .add_systems(
                Update,
                fail_pending
                    .after(count_progress)
                    .after(crate::command::dispatch),
            )
            .add_systems(
                Update,
                (
                    expire_claims,
                    fail_on_losses,
                    clear_quest_mobs.after(count_progress),
                )
                    .run_if(crate::time::heartbeat()),
            )
            // Just before loot is rolled for what died, what holders still need from it.
            .add_systems(
                Update,
                want_loot
                    .run_if(deaths)
                    .after(crate::combat::abandon_fights)
                    .before(crate::combat::handle_deaths),
            )
            .add_command(
                CommandSpec::new("board", Category::Interaction)
                    .shortcuts(&["contracts"])
                    .usage("board [<n>]")
                    .summary("read the notices posted on a quest board here, or one in full"),
                board,
            )
            .add_command(
                CommandSpec::new("accept", Category::Interaction)
                    .usage("accept <n>")
                    .summary("take a notice from the board, with the guild's clerk here"),
                accept,
            )
            .add_command(
                CommandSpec::new("report", Category::Interaction)
                    .usage("report [<quest>]")
                    .summary("report a finished quest to whoever it's for, here, and be paid"),
                report,
            )
            .add_command(
                CommandSpec::new("ask", Category::Communication)
                    .usage("ask <someone> about <topic>")
                    .summary("ask someone here about something, as a quest may want you to"),
                ask,
            )
            .add_command(
                CommandSpec::new("abandon", Category::Interaction)
                    .usage("abandon <quest>")
                    .summary("give up a quest you're on; a contract costs some merit"),
                abandon,
            )
            .add_command(
                CommandSpec::new("why", Category::Admin)
                    .usage("why <player> <quest>")
                    .summary("where a player stands in a quest, and why: every objective and test")
                    .permission(Permission::Events),
                why,
            )
            .add_command(
                CommandSpec::new("journal", Category::Character)
                    .shortcuts(&["quests"])
                    .summary("the quests you're on, where you are in each, and those you've done"),
                journal,
            );
    }
}

#[cfg(test)]
pub(super) mod testing {
    use super::def::{Availability, MobMatch, Objective, Posted, QuestDef, Repeat, Reward, Stage};
    use crate::world::Every;

    /// A quest to change for tests.
    pub(super) fn rats() -> QuestDef {
        QuestDef {
            id: "riftwatch:rats".into(),
            name: "Rats".into(),
            summary: "Rats.".into(),
            description: "Rats, at length.".into(),
            posted: Posted::Board("contract_hall".into()),
            posted_when: None,
            contract: None,
            availability: Availability::Claimed {
                lasts: Every::Days(1),
            },
            requires: None,
            repeat: Repeat::Every(Every::Days(1)),
            stages: vec![Stage {
                text: "Kill rats.".into(),
                objectives: vec![Objective::Kill(MobMatch::Mob("rat".into()), 3)],
                ..Default::default()
            }],
            report_to: None,
            gives: Vec::new(),
            endings: Default::default(),
            fails_if: Vec::new(),
            rewards: vec![Reward::Coin(250)],
        }
    }
}
