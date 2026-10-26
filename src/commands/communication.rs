//! Talking: to the room, to one person, to the zone, to everyone.

use bevy::prelude::*;

use crate::characters::Races;
use crate::command::Invocation;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver, targets_in_room};
use crate::stealth::{Hidden, sees, visible};
use crate::targeting::{Keywords, Target};

pub fn say(In(inv): In<Invocation>, players: Characters, mut outbox: ResMut<Outbox>) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = &inv.args;
    let out = if text.is_empty() {
        vec![Outgoing::line(actor.entity, "Say what?")]
    } else {
        vec![
            Outgoing::line(actor.entity, format!("You say, \"{text}\"")),
            Outgoing::Room {
                room: actor.room,
                except: actor.entity,
                text: format!("{} says, \"{text}\"\r\n", actor.name),
            },
        ]
    };
    deliver(out, &mut outbox);
}

/// Only the target hears the words; everyone else in the room just sees that a whisper happened.
pub fn whisper(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    keywords: Query<&Keywords>,
    hidden: Query<&Hidden>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let parsed = inv
        .args
        .split_once(' ')
        .map(|(t, text)| (Target::parse(t), text.trim()))
        .filter(|(_, text)| !text.is_empty());

    match parsed {
        Some((Some(target), text)) => {
            let candidates = targets_in_room(actor.room, &players, &keywords);
            let candidates = visible(actor.entity, candidates, &hidden);
            match target.find_one(actor.entity, candidates) {
                Some(e) if e == actor.entity => {
                    out.push(Outgoing::line(
                        actor.entity,
                        "You mutter something to yourself.",
                    ));
                }
                Some(e) => {
                    let target_name = Actor::get(&players, e).map(|a| a.name).unwrap_or_default();
                    out.push(Outgoing::line(
                        actor.entity,
                        format!("\x1b[2mYou whisper to {target_name}, \"{text}\"\x1b[0m"),
                    ));
                    out.push(Outgoing::line(
                        e,
                        format!("\x1b[2m{} whispers to you, \"{text}\"\x1b[0m", actor.name),
                    ));
                    let seen = format!(
                        "{} whispers something into {target_name}'s ear.\r\n",
                        actor.name
                    );
                    // Only those who can see the whisperer notice the whisper.
                    for (other, _) in targets_in_room(actor.room, &players, &keywords) {
                        if other != actor.entity && other != e && sees(&hidden, other, actor.entity)
                        {
                            out.push(Outgoing::To(other, seen.clone()));
                        }
                    }
                }
                None => out.push(Outgoing::line(
                    actor.entity,
                    format!("You don't see {} here.", target.describe()),
                )),
            }
        }
        _ => out.push(Outgoing::line(actor.entity, inv.usage_error())),
    }
    deliver(out, &mut outbox);
}

pub fn yell(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    rooms: RoomQuery,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = &inv.args;
    let mut out = Vec::new();
    if text.is_empty() {
        out.push(Outgoing::line(actor.entity, "Yell what?"));
    } else {
        out.push(Outgoing::line(
            actor.entity,
            format!("\x1b[1;31mYou yell, \"{text}\"\x1b[0m"),
        ));
        if let Ok((_, key, _)) = rooms.get(actor.room) {
            let heard = format!("\x1b[1;31m{} yells, \"{text}\"\x1b[0m\r\n", actor.name);
            // Everyone in the zone hears, player or mob: a mob only listens if puppeted.
            for (other, _, _, in_room) in players.iter() {
                let in_zone = rooms
                    .get(in_room.0)
                    .is_ok_and(|(_, k, _)| k.zone == key.zone);
                if in_zone && other != actor.entity {
                    out.push(Outgoing::To(other, heard.clone()));
                }
            }
        }
    }
    deliver(out, &mut outbox);
}

pub fn gossip(In(inv): In<Invocation>, players: Characters, mut outbox: ResMut<Outbox>) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = &inv.args;
    let mut out = Vec::new();
    if text.is_empty() {
        out.push(Outgoing::line(actor.entity, "Gossip what?"));
    } else {
        out.push(Outgoing::line(
            actor.entity,
            format!("\x1b[35m[Gossip] You: {text}\x1b[0m"),
        ));
        let heard = format!("\x1b[35m[Gossip] {}: {text}\x1b[0m\r\n", actor.name);
        for (other, player, _, _) in players.iter() {
            if player.is_some() && other != actor.entity {
                out.push(Outgoing::To(other, heard.clone()));
            }
        }
    }
    deliver(out, &mut outbox);
}

pub fn who(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    races: Res<Races>,
) {
    let mut names: Vec<_> = players
        .iter()
        .filter_map(|(_, p, _, _)| p.map(|p| format!("{} ({})", p.name, races.name(&p.race))))
        .collect();
    names.sort();
    let text = format!(
        "Players online ({}):\r\n  {}",
        names.len(),
        names.join("\r\n  ")
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}
