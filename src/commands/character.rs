//! Commands about the player's own character: attributes, aliases, leaving.

use bevy::prelude::*;

use crate::aliases::{self, Aliases};
use crate::attributes::{self, Attributes};
use crate::characters::{Backgrounds, Races};
use crate::combat::{ConditionDefs, Conditions};
use crate::combat::{Fights, in_danger, the};
use crate::command::{CommandRegistry, Invocation};
use crate::db::Database;
use crate::items::capitalized;
use crate::money::{self, Purse};
use crate::net::Connection;
use crate::output::{Actor, Characters, Outbox, Outgoing, Reply, deliver, reply_line};
use crate::skills::{SkillDefs, Skills};
use crate::trainers;
use crate::vitals::{BaseHealth, Maxima, Vitals};
use crate::world::Player;

/// What a puppeted mob is told when it tries something only a player's account can do.
const ONLY_PLAYERS: &str = "Only a player can do that.";

/// The longest description a character can have.
const MAX_DESCRIPTION_LEN: usize = 600;

/// What `score` shows of a character.
type Sheet = (
    &'static Attributes,
    &'static Vitals,
    &'static Skills,
    Option<&'static Conditions>,
    Option<&'static Purse>,
    Option<&'static BaseHealth>,
);

#[allow(clippy::too_many_arguments)]
pub fn score(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    sheets: Query<Sheet>,
    races: Res<Races>,
    backgrounds: Res<Backgrounds>,
    (defs, condition_defs): (Res<SkillDefs>, Res<ConditionDefs>),
    memberships: Query<&crate::guild::Membership>,
) {
    let (Ok((_, player, mob, _)), Ok((attributes, vitals, skills, conditions, purse, base))) =
        (players.get(inv.entity), sheets.get(inv.entity))
    else {
        return;
    };
    // A player is their race and past life; a mob, its race, if it has one, and its template.
    let (name, who) = match (player, mob) {
        (Some(player), _) => {
            let background = backgrounds
                .get(&player.background)
                .map_or(player.background.clone(), |b| b.name.clone());
            let race = races.name(&player.race);
            (player.name.clone(), format!("{race} {background}"))
        }
        (None, Some(mob)) => {
            let race = mob.race.as_deref().map(|r| races.name(r));
            let what = race.map_or(format!("[{}]", mob.key), |r| format!("{r} [{}]", mob.key));
            (capitalized(&the(&mob.name)), what)
        }
        (None, None) => return,
    };
    let current = (vitals.health.floor() as i32, vitals.stamina.floor() as i32);
    let tradition = trainers::awakened(skills);
    let max = Maxima::of(attributes, skills, &defs, base);
    let mut text = attributes::score_text(
        &name,
        who.trim(),
        attributes,
        current,
        (max.health, max.stamina),
        tradition,
    );
    if let Some(purse) = purse {
        text += &format!("    Purse: {}", money::coins(purse.0));
    }
    if let Ok(m) = memberships.get(inv.entity) {
        let rank = crate::guild::guild()
            .ranks
            .get(m.rank)
            .map_or("", |r| r.name.as_str());
        text += &if m.expelled {
            format!("\r\n  Guild: expelled, at {rank}")
        } else {
            format!("\r\n  Guild: {rank}, {} merit", m.merit)
        };
    }
    let traits = conditions.map_or_else(Vec::new, |c| c.names(&condition_defs, true));
    if !traits.is_empty() {
        text += &format!("\r\n  Traits: {}", traits.join(", "));
    }
    let affected = conditions.map_or_else(Vec::new, |c| c.names(&condition_defs, false));
    if !affected.is_empty() {
        text += &format!(
            "\r\n  Affected by: {} (type 'affects' to see for how long)",
            affected.join(", ")
        );
    }
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// `describe [<text>]`: see or set what others see when they look at you.
pub fn describe(
    In(inv): In<Invocation>,
    mut players: Query<&mut Player>,
    mut outbox: ResMut<Outbox>,
    db: Res<Database>,
) {
    let reply: Reply = match players.get_mut(inv.entity) {
        Ok(mut player) => {
            let text = inv.args.trim();
            if text.is_empty() {
                Ok(if player.description.is_empty() {
                    "You haven't described yourself. Use 'describe <text>'.".into()
                } else {
                    format!("Others see:\r\n{}", player.description)
                })
            } else if text.chars().count() > MAX_DESCRIPTION_LEN {
                Err(format!("Keep it under {MAX_DESCRIPTION_LEN} characters."))
            } else if let Err(err) = db.set_description(player.character_id, text) {
                error!("failed to save description for {}: {err}", player.name);
                Err("Something went wrong saving that. Please try again.".into())
            } else {
                player.description = text.into();
                Ok("Description updated.".into())
            }
        }
        _ => Err(ONLY_PLAYERS.into()),
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut outbox);
}

pub fn raise(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut sheets: Query<&mut Attributes>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut attributes)) =
        (Actor::get(&players, inv.entity), sheets.get_mut(inv.entity))
    else {
        return;
    };
    let text = match actor.character_id {
        Some(id) => attributes::raise_command(id, &mut attributes, &inv.args, &db),
        None => ONLY_PLAYERS.into(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

pub fn alias(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut aliases: Query<&mut Aliases>,
    registry: Res<CommandRegistry>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut aliases)) = (
        Actor::get(&players, inv.entity),
        aliases.get_mut(inv.entity),
    ) else {
        return;
    };
    let Some(character_id) = actor.character_id else {
        return deliver(vec![Outgoing::line(inv.entity, ONLY_PLAYERS)], &mut outbox);
    };
    let text = aliases::alias_command(
        character_id,
        &mut aliases,
        &inv.args,
        |name| registry.is_command(name, actor.role),
        &db,
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

pub fn unalias(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut aliases: Query<&mut Aliases>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut aliases)) = (
        Actor::get(&players, inv.entity),
        aliases.get_mut(inv.entity),
    ) else {
        return;
    };
    let text = match actor.character_id {
        Some(id) => aliases::unalias_command(id, &mut aliases, &inv.args, &db),
        None => ONLY_PLAYERS.into(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

pub fn quit(
    In(inv): In<Invocation>,
    players: Characters,
    (fights, inanimate): Fights,
    mut outbox: ResMut<Outbox>,
    mut connections: Query<&mut Connection>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    if in_danger(actor.entity, &fights, &inanimate) {
        let text = "You can't leave in the middle of a fight. Flee it first.";
        deliver(
            vec![reply_line(actor.entity, Err(text.into()))],
            &mut outbox,
        );
        return;
    }
    let out = vec![
        Outgoing::line(actor.entity, "You leave the world for now."),
        Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} has left the game.\r\n", actor.name),
        },
    ];
    deliver(out, &mut outbox);
    // Leave as if disconnecting, but back to the character menu (`login::return_to_menu`).
    if let Ok(mut conn) = connections.get_mut(actor.entity) {
        conn.closing = true;
        conn.to_menu = true;
    }
}
