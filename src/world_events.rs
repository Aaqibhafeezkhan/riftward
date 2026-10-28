//! World events: the world reacting to what happens in it, from data in zone files.
//!
//! The world keeps named **flags** (`rats_broke_out`): `world.` facts (`facts.rs`). A zone's
//! `events` each have a `when` trigger and a list of `then` actions: when a sewer rat reaches the
//! city for the first time, set a flag and tell the town. Spawns can be gated on the same triggers
//! (`when: WorldFlag("rats_broke_out")`), which is how the Rat King appears only after a breakout.
//!
//! This is the first slice of the design in `docs/world.md`: triggers on mobs entering a zone or
//! dying, and on flags; actions that set and clear flags and make announcements.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::events::GameEvent;
use crate::facts::{Facts, Owner, Value};
use crate::mobs::Mob;
use crate::output::{Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::requirements::{Occurred, Requirement, Subject};
use crate::time::unix_now;
use crate::world::{RoomIndex, RoomKey, ZoneDef};

pub struct WorldEventsPlugin;

impl Plugin for WorldEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, move_old_flags.after(crate::facts::load_facts))
            .add_systems(Update, run_world_events.after(crate::combat::handle_deaths))
            .add_command(
                CommandSpec::new("worldflag", Category::Admin)
                    .usage("worldflag [<flag> on|off]")
                    .summary("list the world's flags, or set or clear one, to run events by hand")
                    .permission(Permission::Events),
                worldflag,
            );
    }
}

/// A world event: when its trigger is met, its actions happen.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct WorldEventDef {
    /// When it fires: a requirement, usually an `Event` and a flag or two (`requirements.rs`).
    pub when: Requirement,
    pub then: Vec<Action>,
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

/// World flags saved before they were facts become `world.` facts, once.
fn move_old_flags(db: Res<Database>, mut facts: ResMut<Facts>) {
    match db.take_old_world_flags() {
        Ok(flags) => {
            for flag in flags {
                info!("world flag {flag} is a fact now");
                facts.set(&Owner::World, &flag, Value::Bool(true), None, &db);
            }
        }
        Err(err) => error!("failed to move the old world flags: {err}"),
    }
}

/// Check every zone's events against what just happened, and carry out the ones that fire.
#[allow(clippy::too_many_arguments)]
fn run_world_events(
    mut events: MessageReader<GameEvent>,
    zones: Res<Assets<ZoneDef>>,
    mut facts: ResMut<Facts>,
    mobs: Query<&Mob>,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    db: Res<Database>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
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
                if !event
                    .when
                    .met(&Subject::world(&facts, &zone.id, Some(occurred)))
                {
                    continue;
                }
                info!("world event {}:{id} fired", zone.id);
                for action in &event.then {
                    match action {
                        Action::SetWorldFlag(flag) => {
                            facts.set(&Owner::World, flag, Value::Bool(true), None, &db);
                        }
                        Action::ClearWorldFlag(flag) => facts.clear(&Owner::World, flag, &db),
                        Action::Announce(scope, text) => {
                            let line = format!("\x1b[1;35m{text}\x1b[0m\r\n");
                            for (entity, _, _, room) in &players {
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
    deliver(out, &mut outbox);
}

/// `worldflag [<flag> on|off]`: an admin lists the world's facts, or sets or clears a flag.
/// Setting one by hand fires nothing itself, but gated spawns see it at once.
fn worldflag(
    In(inv): In<Invocation>,
    mut outbox: ResMut<Outbox>,
    mut facts: ResMut<Facts>,
    db: Res<Database>,
) {
    let words: Vec<&str> = inv.args.split_whitespace().collect();
    let text = match words.as_slice() {
        [] => {
            let set = facts.of(&Owner::World, unix_now());
            if set.is_empty() {
                "No world flags are set.".to_string()
            } else {
                let shown: Vec<String> = set
                    .iter()
                    .map(|(key, value)| match value {
                        Value::Bool(true) => key.to_string(),
                        Value::Bool(false) => format!("{key} = false"),
                        Value::Int(n) => format!("{key} = {n}"),
                        Value::Word(w) => format!("{key} = \"{w}\""),
                    })
                    .collect();
                format!("World flags set: {}", shown.join(", "))
            }
        }
        [flag, on @ ("on" | "off")] => {
            let flag = flag.to_lowercase();
            if *on == "on" {
                facts.set(&Owner::World, &flag, Value::Bool(true), None, &db);
            } else {
                facts.clear(&Owner::World, &flag, &db);
            }
            format!("World flag {flag} is now {on}.")
        }
        _ => inv.usage_error(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_saved_before_facts_become_facts() {
        use bevy::ecs::system::RunSystemOnce;
        let db = Database::open(":memory:").unwrap();
        db.conn_for_tests()
            .execute(
                "INSERT INTO world_state (flag) VALUES ('rats_broke_out')",
                [],
            )
            .unwrap();
        let mut world = World::new();
        world.insert_resource(db);
        world.init_resource::<Facts>();
        world.run_system_once(move_old_flags).unwrap();
        assert!(
            world
                .resource::<Facts>()
                .is_set(&Owner::World, "rats_broke_out", 0)
        );
        let db = world.resource::<Database>();
        assert!(db.take_old_world_flags().unwrap().is_empty(), "moved, once");
        // And saved as a fact, for the next start.
        assert_eq!(db.facts().unwrap().len(), 1);
    }

    #[test]
    fn triggers_match_what_happened_and_the_flags() {
        use crate::requirements::Happening;
        use crate::world::MobKey;
        let db = Database::open(":memory:").unwrap();
        let mut facts = Facts::default();
        let breakout: Requirement = crate::world::ron_options()
            .from_str(
                r#"All([Event(Entered(mob: "sewer_rat", zone: "riftwatch")), Not(WorldFlag("rats_broke_out"))])"#,
            )
            .unwrap();
        let rat = MobKey::parse("sewers:sewer_rat", "");
        let into_town = Occurred::Entered {
            mob: rat.clone(),
            zone: "riftwatch".into(),
        };
        let in_sewers = Occurred::Entered {
            mob: rat.clone(),
            zone: "sewers".into(),
        };
        let met = |when: &Requirement, facts: &Facts, zone: &str, now: Option<&Occurred>| {
            when.met(&Subject::world(facts, zone, now))
        };
        assert!(met(&breakout, &facts, "sewers", Some(&into_town)));
        assert!(!met(&breakout, &facts, "sewers", Some(&in_sewers)));
        assert!(
            !met(&breakout, &facts, "sewers", None),
            "events only fire as they happen"
        );
        facts.set(
            &Owner::World,
            "rats_broke_out",
            Value::Bool(true),
            None,
            &db,
        );
        assert!(
            !met(&breakout, &facts, "sewers", Some(&into_town)),
            "only the first time"
        );

        let king_dies = Requirement::Event(Happening::Died("rat_king".into()));
        let died = Occurred::Died {
            mob: MobKey::parse("sewers:rat_king", ""),
        };
        assert!(met(&king_dies, &facts, "sewers", Some(&died)));
        assert!(
            !met(&king_dies, &facts, "riftwatch", Some(&died)),
            "ids are zone-relative"
        );
        // A world event has no one, so asking about a character is false.
        let strong = Requirement::Stat(crate::attributes::Attribute::Strength, 1);
        assert!(!met(&strong, &facts, "sewers", None));
    }
}
