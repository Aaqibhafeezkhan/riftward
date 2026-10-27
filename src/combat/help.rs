//! Calls for help: a guard losing a fight shouts (`CallForHelp`, an effect, usually in a reaction
//! on `Hurt`), and those within earshot who count them an ally come running and join the fight.
//!
//! Earshot is so many rooms through exits anyone could walk: open doors, no climbing or swimming.
//! Who answers is whoever counts the caller an ally (`MobDef::allied`: its own kind, and those
//! its `allies` name), and isn't already fighting, dead, or being played. They arrive as anyone walking in
//! does, so the room sees them come, tracks are laid and the hidden are noticed; defending an
//! ally, they fight whoever the caller is fighting, a player too.

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;

use super::{Dead, Fighting};
use crate::events::{GameEvent, Way};
use crate::mobs::{Mob, MobTemplates};
use crate::net::Connection;
use crate::output::{Outbox, Outgoing, PuppetedBy, RoomQuery};
use crate::world::{Door, InRoom, Player};

/// Mobs free to answer: alive, not fighting, not being played.
type Answerers<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Mob, &'static InRoom),
    (
        Without<Fighting>,
        Without<Dead>,
        Without<PuppetedBy>,
        Without<Connection>,
    ),
>;

/// Answer every call for help made since the last.
#[allow(clippy::too_many_arguments)]
pub(super) fn answer_calls(
    mut events: ParamSet<(MessageReader<GameEvent>, MessageWriter<GameEvent>)>,
    mut commands: Commands,
    rooms: RoomQuery,
    doors: Query<&Door>,
    answerers: Answerers,
    callers: Query<(&InRoom, Option<&Mob>, Has<Player>)>,
    templates: Res<MobTemplates>,
    mut outbox: ResMut<Outbox>,
) {
    let calls: Vec<(Entity, Entity, u32)> = events
        .p0()
        .read()
        .filter_map(|event| match event {
            GameEvent::CalledForHelp { who, foe, rooms } => Some((*who, *foe, *rooms)),
            _ => None,
        })
        .collect();
    let mut answered = Vec::new();
    let mut moves = Vec::new();
    for (caller, foe, reach) in calls {
        let Ok((at, caller_mob, caller_player)) = callers.get(caller) else {
            continue;
        };
        let caller_def = caller_mob
            .filter(|_| !caller_player)
            .and_then(|m| Some((&m.key, templates.0.get(&m.key)?)));
        let earshot = earshot(at.0, reach, &rooms, &doors);
        for (helper, mob, in_room) in &answerers {
            if helper == caller || helper == foe || answered.contains(&helper) {
                continue;
            }
            let Some(way) = earshot.get(&in_room.0) else {
                continue;
            };
            let Some(def) = templates.0.get(&mob.key) else {
                continue;
            };
            if !def.allied(&mob.key, caller_def) {
                continue;
            }
            answered.push(helper);
            commands.entity(helper).insert(Fighting(foe));
            // One already here steps in; one from elsewhere is seen arriving.
            if in_room.0 == at.0 {
                outbox.0.push(Outgoing::Room {
                    room: at.0,
                    except: helper,
                    text: format!(
                        "{} comes to help!\r\n",
                        crate::items::capitalized(&mob.name)
                    ),
                });
            } else {
                commands.entity(helper).insert(InRoom(at.0));
                moves.push((helper, mob.name.clone(), in_room.0, at.0, way.clone()));
            }
        }
    }
    let mut writer = events.p1();
    for (who, name, from, to, (out, came)) in moves {
        writer.write(GameEvent::Left {
            who,
            name: name.clone(),
            room: from,
            way: Way::Walked(Some(out)),
            witnesses: None,
        });
        writer.write(GameEvent::Entered {
            who,
            name,
            room: to,
            way: Way::Walked(Some(came)),
            witnesses: None,
        });
    }
}

/// Every room within `reach` of `from` through exits anyone could walk, with the way out of it
/// toward `from` and the way into `from` at the end. `from` itself is there, reached by no way.
fn earshot(
    from: Entity,
    reach: u32,
    rooms: &RoomQuery,
    doors: &Query<&Door>,
) -> HashMap<Entity, (String, String)> {
    let walkable = |door: Option<Entity>| {
        door.and_then(|d| doors.get(d).ok())
            .is_none_or(Door::is_open)
    };
    // Each room found, with the room before it on the way back to `from` and the exit there.
    let mut found: HashMap<Entity, (String, String)> =
        HashMap::from([(from, (String::new(), String::new()))]);
    let mut queue = VecDeque::from([(from, 0)]);
    while let Some((room, steps)) = queue.pop_front() {
        if steps == reach {
            continue;
        }
        let Ok((_, _, exits)) = rooms.get(room) else {
            continue;
        };
        for (dir, exit) in &exits.0 {
            if exit.needs.is_some() || !walkable(exit.door) || found.contains_key(&exit.to) {
                continue;
            }
            // The way back from the next room is the exit there leading here; the way in at the
            // end is the first step's, kept from where this room was reached.
            let Ok((_, _, back)) = rooms.get(exit.to) else {
                continue;
            };
            let out = back
                .0
                .iter()
                .find(|(_, e)| e.to == room)
                .map_or_else(String::new, |(d, _)| d.clone());
            let came = if room == from {
                dir.clone()
            } else {
                found[&room].1.clone()
            };
            found.insert(exit.to, (out, came));
            queue.push_back((exit.to, steps + 1));
        }
    }
    found
}
