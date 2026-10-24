//! Doors: opening, closing, locking and forcing them. A door is targeted by the direction of its
//! exit (`open north`) or by name (`open trapdoor`); directions are checked first.
//!
//! Locking and unlocking needs a copy of the lock's key item in your pack. Builders can lock and
//! unlock any door without one, for testing.

use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::command::Invocation;
use crate::commands::movement::{full_direction, is_direction, toward};
use crate::db::Permission;
use crate::dice::Dice;
use crate::items::{ItemLocation, ItemQuery};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::targeting::{Keywords, Target};
use crate::world::{Door, DoorKey, DoorState, Exits, ItemKey};

pub type DoorQuery<'w, 's> = Query<'w, 's, (&'static mut Door, &'static Keywords)>;

/// What a door argument refers to, from inside one room.
pub enum DoorLookup {
    Door {
        door: Entity,
        dir: String,
    },
    /// There's an exit that way, but no door on it.
    NoDoor {
        dir: String,
    },
    NotHere,
}

/// Resolve `north`, `n`, `door`, `2.door` among a room's exits.
pub fn find_door<'a>(
    arg: &str,
    exits: &Exits,
    keywords: impl Fn(Entity) -> Option<&'a Keywords>,
) -> DoorLookup {
    let word = arg.trim().to_lowercase();
    let dir = full_direction(&word);
    if let Some(exit) = exits.0.get(dir) {
        return match exit.door {
            Some(door) => DoorLookup::Door {
                door,
                dir: dir.into(),
            },
            None => DoorLookup::NoDoor { dir: dir.into() },
        };
    }

    let Some(target) = Target::parse(arg) else {
        return DoorLookup::NotHere;
    };
    // Each door once, in exit order, even when two exits share it.
    let mut doors: Vec<(Entity, &Keywords)> = Vec::new();
    for exit in exits.0.values() {
        if let Some(door) = exit.door
            && !doors.iter().any(|(e, _)| *e == door)
            && let Some(keywords) = keywords(door)
        {
            doors.push((door, keywords));
        }
    }
    let found = target
        .find_one(Entity::PLACEHOLDER, doors)
        .filter(|e| *e != Entity::PLACEHOLDER);
    match found.and_then(|door| {
        exits
            .0
            .iter()
            .find(|(_, exit)| exit.door == Some(door))
            .map(|(dir, _)| (door, dir.clone()))
    }) {
        Some((door, dir)) => DoorLookup::Door { door, dir },
        None => DoorLookup::NotHere,
    }
}

/// The player, their door, the exit it's on, and the room on the other side.
struct Doorway {
    actor: Actor,
    door: Entity,
    other_side: Entity,
}

fn doorway(
    inv: &Invocation,
    players: &PlayerQuery,
    rooms: &RoomQuery,
    doors: &DoorQuery,
) -> Result<Doorway, String> {
    let actor = Actor::get(players, inv.entity).ok_or_else(String::new)?;
    if inv.args.is_empty() {
        return Err(inv.usage_error());
    }
    let (_, _, exits) = rooms.get(actor.room).map_err(|_| String::new())?;
    match find_door(&inv.args, exits, |e| doors.get(e).ok().map(|(_, k)| k)) {
        DoorLookup::Door { door, dir } => Ok(Doorway {
            other_side: exits.0[&dir].to,
            actor,
            door,
        }),
        DoorLookup::NoDoor { dir } => Err(format!("There's no door {}.", toward(&dir))),
        DoorLookup::NotHere if is_direction(&inv.args.to_lowercase()) => Err(format!(
            "There's no way out {}.",
            toward(full_direction(&inv.args.to_lowercase()))
        )),
        DoorLookup::NotHere => Err(format!("You don't see a door called '{}' here.", inv.args)),
    }
}

/// Tell the player, the room, and the room on the other side what happened to a door.
fn announce(way: &Doorway, you: String, room: String, beyond: String) -> Vec<Outgoing> {
    let mut out = vec![
        Outgoing::line(way.actor.entity, you),
        Outgoing::Room {
            room: way.actor.room,
            except: way.actor.entity,
            text: room + "\r\n",
        },
    ];
    if way.other_side != way.actor.room {
        out.push(Outgoing::Room {
            room: way.other_side,
            except: way.actor.entity,
            text: beyond + "\r\n",
        });
    }
    out
}

/// Run a door command: find the door, then let `act` change it and say what happened.
fn with_door(
    inv: &Invocation,
    players: &mut PlayerQuery,
    rooms: &RoomQuery,
    doors: &mut DoorQuery,
    act: impl FnOnce(&Doorway, &mut Door) -> Result<Vec<Outgoing>, String>,
) {
    let out = match doorway(inv, players, rooms, doors) {
        Ok(way) => match doors.get_mut(way.door) {
            Ok((mut door, _)) => act(&way, &mut door),
            Err(_) => Err(String::new()),
        },
        Err(err) => Err(err),
    };
    let out = match out {
        Ok(out) => out,
        Err(err) if err.is_empty() => Vec::new(),
        Err(err) => vec![Outgoing::line(inv.entity, err)],
    };
    deliver(out, players);
}

pub fn open(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut doors: DoorQuery,
) {
    with_door(&inv, &mut players, &rooms, &mut doors, |way, door| {
        match door.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Locked => return Err("It's locked.".into()),
            DoorState::Closed => door.state = DoorState::Open,
        }
        Ok(announce(
            way,
            format!("You open {}.", door.the()),
            format!("{} opens {}.", way.actor.name, door.the()),
            format!("{} opens.", door.the_capitalized()),
        ))
    });
}

pub fn close(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut doors: DoorQuery,
) {
    with_door(&inv, &mut players, &rooms, &mut doors, |way, door| {
        if !door.is_open() {
            return Err("It's already closed.".into());
        }
        door.state = DoorState::Closed;
        Ok(announce(
            way,
            format!("You close {}.", door.the()),
            format!("{} closes {}.", way.actor.name, door.the()),
            format!("{} closes.", door.the_capitalized()),
        ))
    });
}

/// Whether the player can work this lock: they carry a copy of its key item. A key named without
/// a zone is in the door's zone. Builders can always, for testing.
fn has_key(
    way: &Doorway,
    door: &Door,
    door_keys: &Query<&DoorKey>,
    items: &ItemQuery,
) -> Result<(), String> {
    if way.actor.role.can(Permission::Build) {
        return Ok(());
    }
    let Some(key) = door.lock.as_ref().and_then(|lock| lock.key.as_ref()) else {
        return Err("It has no keyhole.".into());
    };
    let zone = door_keys
        .get(way.door)
        .map(|k| k.zone.as_str())
        .unwrap_or("");
    let key = ItemKey::parse(key, zone);
    let pack = ItemLocation::Carried(way.actor.entity);
    let held = items
        .iter()
        .any(|(_, item, location)| *location == pack && item.key == key);
    if held {
        Ok(())
    } else {
        Err("You don't have the key.".into())
    }
}

pub fn lock(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut doors: DoorQuery,
    door_keys: Query<&DoorKey>,
    items: ItemQuery,
) {
    with_door(&inv, &mut players, &rooms, &mut doors, |way, door| {
        if door.lock.is_none() {
            return Err("It has no lock.".into());
        }
        match door.state {
            DoorState::Locked => return Err("It's already locked.".into()),
            DoorState::Open => return Err("You'll have to close it first.".into()),
            DoorState::Closed => {}
        }
        has_key(way, door, &door_keys, &items)?;
        door.state = DoorState::Locked;
        Ok(announce(
            way,
            format!("You lock {}.", door.the()),
            format!("{} locks {}.", way.actor.name, door.the()),
            format!("You hear a click from {}.", door.the()),
        ))
    });
}

pub fn unlock(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut doors: DoorQuery,
    door_keys: Query<&DoorKey>,
    items: ItemQuery,
) {
    with_door(&inv, &mut players, &rooms, &mut doors, |way, door| {
        if door.lock.is_none() {
            return Err("It has no lock.".into());
        }
        if door.state != DoorState::Locked {
            return Err("It isn't locked.".into());
        }
        has_key(way, door, &door_keys, &items)?;
        door.state = DoorState::Closed;
        Ok(announce(
            way,
            format!("You unlock {}.", door.the()),
            format!("{} unlocks {}.", way.actor.name, door.the()),
            format!("You hear a click from {}.", door.the()),
        ))
    });
}

/// Force a locked door open: Strength against the lock's sturdiness, as a ratio, with Luck.
/// It only forces the door open; the lock still works afterwards.
pub fn bash(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    mut doors: DoorQuery,
    sheets: Query<&Attributes>,
    mut dice: ResMut<Dice>,
) {
    let (strength, luck) = sheets
        .get(inv.entity)
        .map(|a| (a.get(Attribute::Strength), a.get(Attribute::Luck)))
        .unwrap_or((1, 0));
    with_door(&inv, &mut players, &rooms, &mut doors, |way, door| {
        match door.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Closed => return Err("It isn't locked; just open it.".into()),
            DoorState::Locked => {}
        }
        let Some(sturdiness) = door.lock.as_ref().and_then(|lock| lock.bash) else {
            return Err("It's far too sturdy to force.".into());
        };
        let strength = f64::from(strength.max(1));
        let chance = strength / (strength + f64::from(sturdiness.max(1)));
        if dice.chance(luck, chance) {
            door.state = DoorState::Open;
            Ok(announce(
                way,
                format!("You bash {} open!", door.the()),
                format!("{} bashes {} open!", way.actor.name, door.the()),
                format!("{} bursts open!", door.the_capitalized()),
            ))
        } else {
            Ok(announce(
                way,
                format!("You throw yourself against {}, but it holds.", door.the()),
                format!("{} slams into {}.", way.actor.name, door.the()),
                format!("Something slams against {}.", door.the()),
            ))
        }
    });
}
