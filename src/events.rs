//! Game events: one vocabulary of things that happen in the world, written as Bevy messages by
//! whichever system makes them happen. Anything that cares listens, rather than each system
//! telling the others: today that's the announcements below; later, skill triggers, quests and
//! world events.

use bevy::prelude::*;

use crate::net;
use crate::output::{Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::time::Phase;

pub struct EventsPlugin;

impl Plugin for EventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<GameEvent>()
            .add_systems(PostUpdate, announce.before(net::flush_output));
    }
}

#[derive(Message, Clone, Debug)]
pub enum GameEvent {
    /// A player or mob left a room.
    Left {
        who: Entity,
        name: String,
        room: Entity,
        way: Way,
    },
    /// A player or mob arrived in a room.
    Entered {
        who: Entity,
        name: String,
        room: Entity,
        way: Way,
    },
    /// A new part of the day began.
    PhaseChanged(Phase),
}

/// How someone moved.
#[derive(Clone, Debug, PartialEq)]
pub enum Way {
    /// Through an exit. Leaving, the direction they went; arriving, the direction they came from
    /// as seen from the new room, if an exit there leads back.
    Walked(Option<String>),
    /// A builder's `goto`.
    Teleported,
}

/// The direction of the exit in `room` that leads back to `from`, if there is one.
pub fn way_back(rooms: &RoomQuery, room: Entity, from: Entity) -> Option<String> {
    let (_, _, exits) = rooms.get(room).ok()?;
    exits
        .0
        .iter()
        .find(|(_, exit)| exit.to == from)
        .map(|(dir, _)| dir.clone())
}

/// Where a direction is, as somewhere someone comes from: "from the west", "from above".
fn from(dir: &str) -> String {
    match dir {
        "up" => "from above".into(),
        "down" => "from below".into(),
        other => format!("from the {other}"),
    }
}

/// What the people nearby see of each event.
fn text(event: &GameEvent) -> String {
    let line = match event {
        GameEvent::Left { name, way, .. } => match way {
            Way::Walked(Some(dir)) => format!("{name} leaves {dir}."),
            Way::Walked(None) => format!("{name} leaves."),
            Way::Teleported => format!("{name} vanishes."),
        },
        GameEvent::Entered { name, way, .. } => match way {
            Way::Walked(Some(dir)) => format!("{name} arrives {}.", from(dir)),
            Way::Walked(None) => format!("{name} arrives."),
            Way::Teleported => format!("{name} appears out of thin air."),
        },
        GameEvent::PhaseChanged(phase) => phase.arrives().into(),
    };
    line + "\r\n"
}

/// Tell players what they see happen around them.
fn announce(mut events: MessageReader<GameEvent>, rooms: RoomQuery, mut players: PlayerQuery) {
    let mut out = Vec::new();
    for event in events.read() {
        let text = text(event);
        match event {
            GameEvent::Left { who, room, .. } | GameEvent::Entered { who, room, .. } => {
                out.push(Outgoing::Room {
                    room: *room,
                    except: *who,
                    text,
                });
            }
            GameEvent::PhaseChanged(_) => {
                for (entity, _, player, in_room) in &players {
                    let outdoors = in_room
                        .and_then(|r| rooms.get(r.0).ok())
                        .is_some_and(|(room, _, _)| !room.indoors);
                    if player.is_some() && outdoors {
                        out.push(Outgoing::To(entity, text.clone()));
                    }
                }
            }
        }
    }
    deliver(out, &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_reads_naturally() {
        let who = Entity::PLACEHOLDER;
        let walked = |dir: Option<&str>| Way::Walked(dir.map(String::from));
        let left = |way| GameEvent::Left {
            who,
            name: "A stray dog".into(),
            room: who,
            way,
        };
        let entered = |way| GameEvent::Entered {
            who,
            name: "Gandalf".into(),
            room: who,
            way,
        };
        let says = |event| text(&event);
        assert_eq!(
            says(left(walked(Some("east")))),
            "A stray dog leaves east.\r\n"
        );
        assert_eq!(
            says(entered(walked(Some("west")))),
            "Gandalf arrives from the west.\r\n"
        );
        assert_eq!(
            says(entered(walked(Some("down")))),
            "Gandalf arrives from below.\r\n"
        );
        assert_eq!(says(entered(walked(None))), "Gandalf arrives.\r\n");
        assert_eq!(
            says(entered(Way::Teleported)),
            "Gandalf appears out of thin air.\r\n"
        );
    }
}
