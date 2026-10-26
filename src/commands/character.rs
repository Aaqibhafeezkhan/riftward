//! Commands about the player's own character: attributes, aliases, leaving.

use bevy::prelude::*;

use crate::aliases::{self, Aliases};
use crate::attributes::{self, Attributes};
use crate::characters::{Backgrounds, Races};
use crate::combat::Conditions;
use crate::command::{CommandRegistry, Invocation};
use crate::db::Database;
use crate::money::{self, Purse};
use crate::output::{Actor, Outgoing, PlayerQuery, Reply, deliver, reply_line};
use crate::skills::{SkillDefs, Skills};
use crate::trainers;
use crate::vitals::{Maxima, Vitals};

/// The longest description a character can have.
const MAX_DESCRIPTION_LEN: usize = 600;

/// What `score` shows of a character.
type Sheet = (
    &'static Attributes,
    &'static Vitals,
    &'static Skills,
    Option<&'static Conditions>,
    Option<&'static Purse>,
);

pub fn score(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    sheets: Query<Sheet>,
    races: Res<Races>,
    backgrounds: Res<Backgrounds>,
    defs: Res<SkillDefs>,
) {
    let (Ok((_, _, Some(player), _)), Ok((attributes, vitals, skills, conditions, purse))) =
        (players.get(inv.entity), sheets.get(inv.entity))
    else {
        return;
    };
    let background = backgrounds
        .get(&player.background)
        .map_or(player.background.clone(), |b| b.name.clone());
    let who = format!("{} {background}", races.name(&player.race));
    let current = (vitals.health.floor() as i32, vitals.stamina.floor() as i32);
    let tradition = trainers::awakened(skills);
    let max = Maxima::of(attributes, skills, &defs, None);
    let mut text = attributes::score_text(
        &player.name,
        who.trim(),
        attributes,
        current,
        (max.health, max.stamina),
        tradition,
    );
    if let Some(purse) = purse {
        text += &format!("    Purse: {}", money::coins(purse.0));
    }
    let affected: Vec<&str> = conditions
        .into_iter()
        .flat_map(|c| c.0.iter().map(|(state, _)| state.describe().0))
        .collect();
    if !affected.is_empty() {
        text += &format!(
            "\r\n  Affected by: {} (type 'affects' to see for how long)",
            affected.join(", ")
        );
    }
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// `describe [<text>]`: see or set what others see when they look at you.
pub fn describe(In(inv): In<Invocation>, mut players: PlayerQuery, db: Res<Database>) {
    let reply: Reply = match players.get_mut(inv.entity) {
        Ok((_, _, Some(mut player), _)) => {
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
        _ => return,
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

pub fn raise(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut sheets: Query<&mut Attributes>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut attributes)) =
        (Actor::get(&players, inv.entity), sheets.get_mut(inv.entity))
    else {
        return;
    };
    let text = attributes::raise_command(actor.character_id, &mut attributes, &inv.args, &db);
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

pub fn alias(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
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
    let text = aliases::alias_command(
        actor.character_id,
        &mut aliases,
        &inv.args,
        |name| registry.is_command(name, actor.role),
        &db,
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

pub fn unalias(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut aliases: Query<&mut Aliases>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut aliases)) = (
        Actor::get(&players, inv.entity),
        aliases.get_mut(inv.entity),
    ) else {
        return;
    };
    let text = aliases::unalias_command(actor.character_id, &mut aliases, &inv.args, &db);
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

pub fn quit(In(inv): In<Invocation>, mut players: PlayerQuery) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let out = vec![
        Outgoing::line(actor.entity, "Farewell!"),
        Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} has left the game.\r\n", actor.name),
        },
    ];
    deliver(out, &mut players);
    if let Ok((_, mut conn, _, _)) = players.get_mut(actor.entity) {
        conn.closing = true;
    }
}
