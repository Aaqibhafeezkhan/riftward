//! Game events: one vocabulary of things that happen in the world, written as Bevy messages by
//! whichever system makes them happen. Anything that cares listens, rather than each system
//! telling the others: today that's the announcements below; later, skill triggers, quests and
//! world events.

use bevy::prelude::*;

use crate::net;
use crate::output::{Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::time::Phase;
use crate::world::{ItemKey, MobKey};

pub struct EventsPlugin;

impl Plugin for EventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<GameEvent>()
            .init_resource::<crate::output::Outbox>()
            // Room announcements join what systems queued, and all of it goes out before the
            // prompt is drawn.
            .add_systems(
                PostUpdate,
                (announce, crate::output::flush_outbox)
                    .chain()
                    .before(net::flush_output),
            );
    }
}

#[derive(Message, Clone, Debug)]
pub enum GameEvent {
    /// A player or mob left a room. `witnesses` are the only ones who saw, for someone sneaking;
    /// `None` means everyone there.
    Left {
        who: Entity,
        name: String,
        room: Entity,
        way: Way,
        witnesses: Option<Vec<Entity>>,
    },
    /// A player or mob arrived in a room, seen by `witnesses` if they were sneaking.
    Entered {
        who: Entity,
        name: String,
        room: Entity,
        way: Way,
        witnesses: Option<Vec<Entity>>,
    },
    /// Someone hidden was seen again, by acting or moving openly.
    Revealed {
        who: Entity,
        name: String,
        room: Entity,
    },
    /// A new part of the day began.
    PhaseChanged(Phase),
    /// A character spent stamina, as on an active skill.
    StaminaSpent { who: Entity, amount: f32 },
    /// A player or mob dodged an attack.
    Dodged { who: Entity },
    /// A player or mob turned an attack aside with their shield.
    Blocked { who: Entity },
    /// A player or mob was hit while wearing armour.
    HitInArmour { who: Entity },
    /// A player or mob took damage.
    Damaged { who: Entity, amount: f32 },
    /// A player or mob was killed; `mob` is the template of a mob. `killer` struck the blow, or
    /// put on the bleeding or poison that did it; `None` when it was no one else (their own
    /// curse). The killer's side is `groups::side(killer)`. World events listen; quests will.
    #[allow(dead_code)]
    Died {
        who: Entity,
        name: String,
        room: Entity,
        mob: Option<MobKey>,
        killer: Option<Entity>,
    },
    /// A condition took hold of `who`, put on by `by` (themselves, if nobody). `reacting` if a
    /// reaction put it on, which sets off no more.
    ConditionApplied {
        who: Entity,
        by: Entity,
        condition: String,
        strength: i32,
        reacting: bool,
    },
    /// A condition on `who` ran out, in time or turns.
    ConditionFaded {
        who: Entity,
        condition: String,
        strength: i32,
    },
    /// Someone sat down to rest.
    Rested { who: Entity },
    /// Someone ate (or, with `drink`, drank) something.
    Consumed { who: Entity, drink: bool },
    /// `who` shouted for help against `foe`, heard within `rooms` (`combat/help.rs`).
    CalledForHelp {
        who: Entity,
        foe: Entity,
        rooms: u32,
    },
    /// `who` is to run, by an effect (`combat/fleeing.rs`).
    Flees { who: Entity },
    /// `count` mobs of `mob` are to come into being in `room`, by `who`'s doing, and fight `foe`
    /// unless that's `who` (`combat/spawning.rs`).
    Spawns {
        who: Entity,
        foe: Entity,
        mob: MobKey,
        count: u32,
        room: Entity,
    },
    /// `who` came to have an item of template `key`: picked it up, was given it, or bought it.
    /// What's inside it comes too, each its own event. Quests' `Obtain` will listen.
    #[allow(dead_code)]
    Obtained {
        who: Entity,
        item: Entity,
        key: ItemKey,
        how: Got,
    },
}

/// How someone came to have an item.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Got {
    /// From the ground, or out of something not theirs, like a corpse.
    PickedUp,
    /// Handed over by this player or mob.
    Given(Entity),
    /// From this shopkeeper.
    Bought(Entity),
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
pub fn from(dir: &str) -> String {
    match dir {
        "up" => "from above".into(),
        "down" => "from below".into(),
        other => format!("from the {other}"),
    }
}

/// What the people nearby see of each event, if anything.
fn text(event: &GameEvent) -> Option<String> {
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
        GameEvent::Revealed { name, .. } => format!("{name} steps out of the shadows."),
        GameEvent::StaminaSpent { .. }
        | GameEvent::Dodged { .. }
        | GameEvent::Blocked { .. }
        | GameEvent::HitInArmour { .. }
        | GameEvent::Damaged { .. }
        | GameEvent::Died { .. }
        | GameEvent::ConditionApplied { .. }
        | GameEvent::ConditionFaded { .. }
        | GameEvent::Rested { .. }
        | GameEvent::Consumed { .. }
        | GameEvent::CalledForHelp { .. }
        | GameEvent::Spawns { .. }
        | GameEvent::Obtained { .. }
        | GameEvent::Flees { .. } => return None,
    };
    Some(line + "\r\n")
}

/// Tell players what they see happen around them.
fn announce(
    mut events: MessageReader<GameEvent>,
    rooms: RoomQuery,
    players: Characters,
    mut outbox: ResMut<Outbox>,
) {
    let mut out = Vec::new();
    for event in events.read() {
        let Some(text) = text(event) else {
            continue;
        };
        match event {
            // Someone sneaking is seen only by those who noticed.
            GameEvent::Left {
                witnesses: Some(witnesses),
                ..
            }
            | GameEvent::Entered {
                witnesses: Some(witnesses),
                ..
            } => {
                for &witness in witnesses {
                    if players.get(witness).is_ok_and(|(_, _, p, _)| p.is_some()) {
                        out.push(Outgoing::To(witness, text.clone()));
                    }
                }
            }
            GameEvent::Left { who, room, .. }
            | GameEvent::Entered { who, room, .. }
            | GameEvent::Revealed { who, room, .. } => {
                out.push(Outgoing::Room {
                    room: *room,
                    except: *who,
                    text,
                });
            }
            GameEvent::StaminaSpent { .. }
            | GameEvent::Dodged { .. }
            | GameEvent::Blocked { .. }
            | GameEvent::HitInArmour { .. }
            | GameEvent::Damaged { .. }
            | GameEvent::Died { .. }
            | GameEvent::ConditionApplied { .. }
            | GameEvent::ConditionFaded { .. }
            | GameEvent::Rested { .. }
            | GameEvent::Consumed { .. }
            | GameEvent::CalledForHelp { .. }
            | GameEvent::Spawns { .. }
            | GameEvent::Obtained { .. }
            | GameEvent::Flees { .. } => {}
            GameEvent::PhaseChanged(_) => {
                for (entity, _, _, in_room) in &players {
                    let outdoors = rooms.get(in_room.0).is_ok_and(|(room, _, _)| !room.indoors);
                    if outdoors {
                        out.push(Outgoing::To(entity, text.clone()));
                    }
                }
            }
        }
    }
    deliver(out, &mut outbox);
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
            witnesses: None,
        };
        let entered = |way| GameEvent::Entered {
            who,
            name: "Gandalf".into(),
            room: who,
            way,
            witnesses: None,
        };
        let says = |event| text(&event).unwrap();
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
