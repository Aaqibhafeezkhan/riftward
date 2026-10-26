//! Puppeting: a builder playing a mob. `puppet <mob>` puts the builder behind a mob here, and
//! from then on every command they type runs as the mob (`release` and `quit` excepted), with
//! the builder's permissions, and everything the mob is told comes to them. Players and mobs are
//! the same kind of character, so a puppeted mob can do anything a player can: talk, fight,
//! cast, trade, move.
//!
//! While puppeted, a mob's own behaviour stops: it doesn't wander or pick fights, and in a fight
//! it acts on what the builder queues, like a player. Its body stays where it is. A builder is
//! released when they type `release`, when the mob dies or goes, and when they disconnect.

use bevy::prelude::*;

use crate::combat::the;
use crate::command::{ActionQueue, AddCommand, Category, CommandSpec, Invocation};
use crate::db::Permission;
use crate::mobs::{self, Mob, MobQuery};
use crate::net::Connection;
use crate::output::{Actor, Characters, Outbox, Outgoing, PuppetedBy, Puppeting, deliver};
use crate::targeting::{Keywords, Target};
use crate::world::Player;

pub struct PuppetPlugin;

impl Plugin for PuppetPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, release_lost)
            .add_command(
                CommandSpec::new("puppet", Category::Building)
                    .shortcuts(&["possess"])
                    .usage("puppet <mob>")
                    .summary("play a mob here: your commands run as it, and you hear what it hears")
                    .permission(Permission::Build),
                puppet,
            )
            .add_command(
                CommandSpec::new("release", Category::Building)
                    .summary("stop puppeting a mob, and go back to your own body")
                    .permission(Permission::Build),
                release,
            );
    }
}

/// Commands that always run as the builder, even while they puppet something.
pub const OWN_COMMANDS: &[&str] = &["release", "quit"];

/// `puppet <mob>`: take over a mob here.
#[allow(clippy::too_many_arguments)]
fn puppet(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    puppeting: Query<&Puppeting>,
    puppeted: Query<&PuppetedBy>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    // Told as the mob, once it's theirs: their own body hears nothing while they're away.
    let mut told = actor.entity;
    let reply = (|| {
        if puppeting.get(actor.entity).is_ok() {
            return Err("You're already puppeting something. Type 'release' first.".to_string());
        }
        let target = Target::parse(&inv.args).ok_or_else(|| inv.usage_error())?;
        let (mob, name) = target
            .find_one(actor.entity, mobs::targets_in(actor.room, &mobs, &keywords))
            .and_then(|e| mobs.get(e).ok())
            .map(|(e, mob, _)| (e, the(&mob.name)))
            .ok_or_else(|| format!("You don't see {} here.", target.describe()))?;
        if puppeted.get(mob).is_ok() {
            return Err(format!("Someone is already puppeting {name}."));
        }
        commands.entity(actor.entity).insert(Puppeting(mob));
        // It acts on what its puppeteer queues now, a round at a time, like a player.
        commands
            .entity(mob)
            .insert((PuppetedBy(actor.entity), ActionQueue::default()));
        told = mob;
        // Every session is logged, so the power is accountable.
        let key = mobs
            .get(mob)
            .map_or_else(|_| "?".into(), |(_, m, _)| m.key.to_string());
        info!("{} began puppeting {name} [{key}]", actor.name);
        Ok(format!(
            "You slip into {name}. Your commands are its now; type 'release' to come back."
        ))
    })();
    let text = reply.unwrap_or_else(|err| err);
    deliver(vec![Outgoing::line(told, text)], &mut outbox);
}

/// `release`: go back to your own body.
fn release(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut outbox: ResMut<Outbox>,
    puppeting: Query<&Puppeting>,
    mobs: Query<&Mob>,
    players: Query<&Player>,
) {
    let text = match puppeting.get(inv.entity) {
        Ok(Puppeting(mob)) => {
            let name = mobs.get(*mob).map_or("it".into(), |m| the(&m.name));
            let builder = players
                .get(inv.entity)
                .map_or("someone", |p| p.name.as_str());
            info!("{builder} stopped puppeting {name}");
            let_go(&mut commands, inv.entity, *mob);
            format!("You let go of {name}, and you're yourself again.")
        }
        Err(_) => "You aren't puppeting anything.".into(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// End a puppeting: the builder is themselves again, and the mob its own.
fn let_go(commands: &mut Commands, builder: Entity, mob: Entity) {
    commands.entity(builder).remove::<Puppeting>();
    if let Ok(mut mob) = commands.get_entity(mob) {
        mob.remove::<(PuppetedBy, ActionQueue)>();
    }
}

/// Release builders whose mob is gone, or who've disconnected.
fn release_lost(
    mut commands: Commands,
    builders: Query<(Entity, &Puppeting, &Connection)>,
    mobs: Query<(), With<PuppetedBy>>,
    mut outbox: ResMut<Outbox>,
) {
    for (builder, Puppeting(mob), conn) in &builders {
        if conn.closing {
            info!("a puppeteer disconnected, and their puppet was let go");
            let_go(&mut commands, builder, *mob);
        } else if mobs.get(*mob).is_err() {
            commands.entity(builder).remove::<Puppeting>();
            outbox.0.push(Outgoing::line(
                builder,
                "\x1b[1mWhat you were puppeting is gone. You're yourself again.\x1b[0m",
            ));
        }
    }
}
