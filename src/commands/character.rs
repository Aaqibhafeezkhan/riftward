//! Commands about the player's own character: attributes, aliases, leaving.

use bevy::prelude::*;

use crate::aliases::{self, Aliases};
use crate::attributes::{self, Attributes};
use crate::command::{CommandRegistry, Invocation};
use crate::db::Database;
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};

pub fn score(In(inv): In<Invocation>, mut players: PlayerQuery, sheets: Query<&Attributes>) {
    let (Some(actor), Ok(attributes)) = (Actor::get(&players, inv.entity), sheets.get(inv.entity))
    else {
        return;
    };
    let text = attributes::score_text(&actor.name, attributes);
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
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
    let (Some(actor), Ok(mut aliases)) =
        (Actor::get(&players, inv.entity), aliases.get_mut(inv.entity))
    else {
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
    let (Some(actor), Ok(mut aliases)) =
        (Actor::get(&players, inv.entity), aliases.get_mut(inv.entity))
    else {
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
