//! Talking: to the room, to one person, to the zone, to everyone.

use bevy::prelude::*;

use crate::characters::Races;
use crate::combat::Conditions;
use crate::command::Invocation;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver, targets_in_room};
use crate::stealth::{sees, visible};
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

/// `emote <action>`: everyone in the room, the actor too, sees it done by name: "Grok waves."
pub fn emote(In(inv): In<Invocation>, players: Characters, mut outbox: ResMut<Outbox>) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let out = if inv.args.is_empty() {
        vec![Outgoing::line(actor.entity, "Emote what?")]
    } else {
        let line = emoted(&actor.name, &inv.args);
        vec![
            Outgoing::line(actor.entity, line.clone()),
            Outgoing::Room {
                room: actor.room,
                except: actor.entity,
                text: line + "\r\n",
            },
        ]
    };
    deliver(out, &mut outbox);
}

/// `action` done by `name`, as a sentence: "Grok waves.". A possessive or a comma joins on
/// without a space ("Grok's eyes narrow."), and a full stop is added unless it ends already.
fn emoted(name: &str, action: &str) -> String {
    let action = action.trim();
    let joined = if action.starts_with(['\'', ',']) {
        format!("{name}{action}")
    } else {
        format!("{name} {action}")
    };
    if joined.ends_with(['.', '!', '?', '"', '\'', ')']) {
        joined
    } else {
        joined + "."
    }
}

/// Only the target hears the words; everyone else in the room just sees that a whisper happened.
pub fn whisper(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    keywords: Query<&Keywords>,
    hidden: Query<&Conditions>,
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
    shown: Query<&crate::guild::ShownRank>,
) {
    // A guild member's rank shows while they wear their tag.
    let mut names: Vec<_> = players
        .iter()
        .filter_map(|(e, p, _, _)| {
            let p = p?;
            let rank = crate::guild::shown_rank_name(e, &shown)
                .map_or(String::new(), |rank| format!(" [{rank}]"));
            Some(format!("{} ({}){rank}", p.name, races.name(&p.race)))
        })
        .collect();
    names.sort();
    let text = format!(
        "Players online ({}):\r\n  {}",
        names.len(),
        names.join("\r\n  ")
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_emote_reads_as_a_sentence() {
        assert_eq!(emoted("Grok", "waves"), "Grok waves.");
        assert_eq!(emoted("Grok", "'s eyes narrow"), "Grok's eyes narrow.");
        assert_eq!(emoted("Grok", "laughs!"), "Grok laughs!");
        assert_eq!(emoted("the town guard", "nods"), "the town guard nods.");
    }

    #[test]
    fn a_colon_is_an_emote() {
        assert_eq!(
            crate::command::split_verb(":waves hello"),
            ("emote".into(), "waves hello")
        );
    }
}
