//! World events: the world reacting to what happens in it, from data in zone files.
//!
//! The world keeps named **flags** (`rats_broke_out`), saved in the `world_state` table. A zone's
//! `events` each have a `when` trigger and a list of `then` actions: when a sewer rat reaches the
//! city for the first time, set a flag and tell the town. Spawns can be gated on the same triggers
//! (`when: WorldFlag("rats_broke_out")`), which is how the Rat King appears only after a breakout.
//!
//! This is the first slice of the design in `docs/world.md`: triggers on mobs entering a zone or
//! dying, and on flags; actions that set and clear flags and make announcements.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::events::GameEvent;
use crate::mobs::Mob;
use crate::output::{Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::world::{MobKey, RoomIndex, RoomKey, ZoneDef};

pub struct WorldEventsPlugin;

impl Plugin for WorldEventsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WorldFlags>()
            .add_systems(Startup, load_world_flags)
            .add_systems(Update, run_world_events.after(crate::combat::deliver_log))
            .add_command(
                CommandSpec::new("worldflag", Category::Admin)
                    .usage("worldflag [<flag> on|off]")
                    .summary("list the world's flags, or set or clear one, to run events by hand")
                    .permission(Permission::ManageRoles),
                worldflag,
            );
    }
}

/// A world event: when its trigger is met, its actions happen.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorldEventDef {
    pub when: When,
    pub then: Vec<Action>,
}

/// A trigger, in the shared condition language's shape.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum When {
    /// Something just happened. Only true while that happening is being handled.
    Event(Happening),
    WorldFlag(String),
    All(Vec<When>),
    Any(Vec<When>),
    Not(Box<When>),
}

/// Something that happens in the world, for `When::Event`. Mob ids are `mob` or `zone:mob`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Happening {
    /// A mob made from this template walked into a room in `zone`.
    Entered { mob: String, zone: String },
    /// A mob made from this template died.
    Died(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Action {
    SetWorldFlag(String),
    ClearWorldFlag(String),
    /// Tell a room, a zone, or everyone.
    Announce(Scope, String),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Scope {
    /// `room` or `zone:room`.
    Room(String),
    Zone(String),
    Everyone,
}

/// What actually happened, resolved to full ids.
#[derive(Debug, Clone, PartialEq)]
pub enum Occurred {
    Entered { mob: MobKey, zone: String },
    Died { mob: MobKey },
}

/// The world's flags. Saved.
#[derive(Resource, Default)]
pub struct WorldFlags(pub HashSet<String>);

impl When {
    /// Whether the trigger is met, written in zone `zone`, given the flags and what (if anything)
    /// just happened.
    pub fn met(&self, zone: &str, flags: &WorldFlags, now: Option<&Occurred>) -> bool {
        match self {
            When::Event(happening) => match (happening, now) {
                (
                    Happening::Entered { mob, zone: z },
                    Some(Occurred::Entered { mob: m, zone: at }),
                ) => MobKey::parse(mob, zone) == *m && z == at,
                (Happening::Died(mob), Some(Occurred::Died { mob: m })) => {
                    MobKey::parse(mob, zone) == *m
                }
                _ => false,
            },
            When::WorldFlag(flag) => flags.0.contains(flag),
            When::All(all) => all.iter().all(|w| w.met(zone, flags, now)),
            When::Any(any) => any.iter().any(|w| w.met(zone, flags, now)),
            When::Not(not) => !not.met(zone, flags, now),
        }
    }
}

fn load_world_flags(db: Res<Database>, mut flags: ResMut<WorldFlags>) {
    match db.world_flags() {
        Ok(saved) => flags.0.extend(saved),
        Err(err) => error!("failed to load world flags: {err}"),
    }
}

/// Check every zone's events against what just happened, and carry out the ones that fire.
#[allow(clippy::too_many_arguments)]
fn run_world_events(
    mut events: MessageReader<GameEvent>,
    zones: Res<Assets<ZoneDef>>,
    mut flags: ResMut<WorldFlags>,
    mobs: Query<&Mob>,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    db: Res<Database>,
    mut players: PlayerQuery,
) {
    let happened: Vec<Occurred> = events
        .read()
        .filter_map(|event| match event {
            GameEvent::Entered { who, room, .. } => Some(Occurred::Entered {
                mob: mobs.get(*who).ok()?.key.clone(),
                zone: rooms.get(*room).ok()?.1.zone.clone(),
            }),
            GameEvent::Died { mob: Some(mob), .. } => Some(Occurred::Died { mob: mob.clone() }),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for occurred in &happened {
        for (_, zone) in zones.iter() {
            for (id, event) in &zone.events {
                if !event.when.met(&zone.id, &flags, Some(occurred)) {
                    continue;
                }
                info!("world event {}:{id} fired", zone.id);
                for action in &event.then {
                    match action {
                        Action::SetWorldFlag(flag) => {
                            flags.0.insert(flag.clone());
                            if let Err(err) = db.set_world_flag(flag, true) {
                                error!("failed to save world flag {flag}: {err}");
                            }
                        }
                        Action::ClearWorldFlag(flag) => {
                            flags.0.remove(flag);
                            if let Err(err) = db.set_world_flag(flag, false) {
                                error!("failed to clear world flag {flag}: {err}");
                            }
                        }
                        Action::Announce(scope, text) => {
                            let line = format!("\x1b[1;35m{text}\x1b[0m\r\n");
                            for (entity, _, player, in_room) in &players {
                                let Some(room) = in_room.filter(|_| player.is_some()) else {
                                    continue;
                                };
                                let hears = match scope {
                                    Scope::Everyone => true,
                                    Scope::Zone(z) => {
                                        rooms.get(room.0).is_ok_and(|(_, key, _)| key.zone == *z)
                                    }
                                    Scope::Room(r) => index
                                        .0
                                        .get(&RoomKey::parse(r, &zone.id))
                                        .is_some_and(|e| *e == room.0),
                                };
                                if hears {
                                    out.push(Outgoing::To(entity, line.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    deliver(out, &mut players);
}

/// `worldflag [<flag> on|off]`: an admin lists, sets or clears world flags. Setting one by hand
/// fires nothing itself, but gated spawns see it at once.
fn worldflag(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut flags: ResMut<WorldFlags>,
    db: Res<Database>,
) {
    let words: Vec<&str> = inv.args.split_whitespace().collect();
    let text = match words.as_slice() {
        [] => {
            let mut set: Vec<&String> = flags.0.iter().collect();
            set.sort();
            if set.is_empty() {
                "No world flags are set.".to_string()
            } else {
                format!(
                    "World flags set: {}",
                    set.iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
        [flag, on @ ("on" | "off")] => {
            let set = *on == "on";
            let flag = flag.to_lowercase();
            if set {
                flags.0.insert(flag.clone());
            } else {
                flags.0.remove(&flag);
            }
            match db.set_world_flag(&flag, set) {
                Ok(()) => format!("World flag {flag} is now {on}."),
                Err(err) => {
                    error!("failed to save world flag {flag}: {err}");
                    format!(
                        "World flag {flag} is {on} until the next restart; it couldn't be saved."
                    )
                }
            }
        }
        _ => inv.usage_error(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_match_what_happened_and_the_flags() {
        let mut flags = WorldFlags::default();
        let breakout = When::All(vec![
            When::Event(Happening::Entered {
                mob: "sewer_rat".into(),
                zone: "riftwatch".into(),
            }),
            When::Not(Box::new(When::WorldFlag("rats_broke_out".into()))),
        ]);
        let rat = MobKey::parse("sewers:sewer_rat", "");
        let into_town = Occurred::Entered {
            mob: rat.clone(),
            zone: "riftwatch".into(),
        };
        let in_sewers = Occurred::Entered {
            mob: rat.clone(),
            zone: "sewers".into(),
        };
        assert!(breakout.met("sewers", &flags, Some(&into_town)));
        assert!(!breakout.met("sewers", &flags, Some(&in_sewers)));
        assert!(
            !breakout.met("sewers", &flags, None),
            "events only fire as they happen"
        );
        flags.0.insert("rats_broke_out".into());
        assert!(
            !breakout.met("sewers", &flags, Some(&into_town)),
            "only the first time"
        );

        let king_dies = When::Event(Happening::Died("rat_king".into()));
        let died = Occurred::Died {
            mob: MobKey::parse("sewers:rat_king", ""),
        };
        assert!(king_dies.met("sewers", &flags, Some(&died)));
        assert!(
            !king_dies.met("riftwatch", &flags, Some(&died)),
            "ids are zone-relative"
        );
    }
}
