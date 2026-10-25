//! Doors and closable containers: opening, closing, locking and forcing them. A door is targeted
//! by the direction of its exit (`open north`) or by name (`open trapdoor`); directions and doors
//! are checked first, then containers you carry or that lie here (`unlock chest`).
//!
//! Locking and unlocking needs a copy of the lock's key item on you. Builders can lock and unlock
//! anything without one, for testing.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::command::Invocation;
use crate::commands::movement::{full_direction, is_direction, toward};
use crate::db::Permission;
use crate::dice::Dice;
use crate::items::{
    Item, ItemLocation, ItemTemplates, Snapshot, capitalized, carries, with_keywords,
};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::skills::{Learner, Practice};
use crate::targeting::{Keywords, Target};
use crate::world::{Door, DoorKey, DoorState, Exits, ItemKey, LockDef, without_article};

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

/// Everything a lock command needs to find and change doors and containers.
#[derive(SystemParam)]
pub struct Lockables<'w, 's> {
    rooms: RoomQuery<'w, 's>,
    doors: Query<'w, 's, (&'static mut Door, &'static DoorKey)>,
    items: Query<'w, 's, (Entity, &'static mut Item, &'static ItemLocation)>,
    keywords: Query<'w, 's, &'static Keywords>,
    templates: Res<'w, ItemTemplates>,
}

/// The door or container a command acts on, as it is now.
struct Lockable {
    actor: Actor,
    kind: Kind,
    name: String,
    state: DoorState,
    lock: Option<LockDef>,
    /// The zone a key named without a zone belongs to.
    key_zone: String,
}

enum Kind {
    Door { entity: Entity, other_side: Entity },
    Container(Entity),
}

impl Lockable {
    /// "the heavy oak door", for the middle of a sentence.
    fn the(&self) -> String {
        format!("the {}", without_article(&self.name))
    }

    /// Tell the player and the room what happened; for a door, the room beyond hears too.
    fn announce(&self, you: String, room: String, beyond: String) -> Vec<Outgoing> {
        let mut out = vec![
            Outgoing::line(self.actor.entity, you),
            Outgoing::Room {
                room: self.actor.room,
                except: self.actor.entity,
                text: room + "\r\n",
            },
        ];
        if let Kind::Door { other_side, .. } = self.kind
            && other_side != self.actor.room
        {
            out.push(Outgoing::Room {
                room: other_side,
                except: self.actor.entity,
                text: beyond + "\r\n",
            });
        }
        out
    }
}

fn find_lockable(
    inv: &Invocation,
    players: &PlayerQuery,
    world: &Lockables,
) -> Result<Lockable, String> {
    let actor = Actor::get(players, inv.entity).ok_or_else(String::new)?;
    if inv.args.is_empty() {
        return Err(inv.usage_error());
    }
    let (_, _, exits) = world.rooms.get(actor.room).map_err(|_| String::new())?;
    match find_door(&inv.args, exits, |e| world.keywords.get(e).ok()) {
        DoorLookup::Door { door, dir } => {
            let (found, key) = world.doors.get(door).map_err(|_| String::new())?;
            Ok(Lockable {
                kind: Kind::Door {
                    entity: door,
                    other_side: exits.0[&dir].to,
                },
                name: found.name.clone(),
                state: found.state,
                lock: found.lock.clone(),
                key_zone: key.zone.clone(),
                actor,
            })
        }
        DoorLookup::NoDoor { dir } => Err(format!("There's no door {}.", toward(&dir))),
        DoorLookup::NotHere if is_direction(&inv.args.to_lowercase()) => Err(format!(
            "There's no way out {}.",
            toward(full_direction(&inv.args.to_lowercase()))
        )),
        DoorLookup::NotHere => {
            let snapshot = Snapshot::new(
                world.items.iter().map(|(e, i, l)| (e, i, *l)),
                &world.templates,
            );
            let reach: Vec<_> = snapshot
                .at_hand(actor.entity, actor.room)
                .into_iter()
                .filter(|e| snapshot.is_container(*e))
                .collect();
            let target = Target::parse(&inv.args).ok_or_else(String::new)?;
            let entity = target
                .find_one(actor.entity, with_keywords(reach, &world.keywords))
                .filter(|e| *e != actor.entity)
                .ok_or_else(|| format!("You don't see {} here.", target.describe()))?;
            let (_, item, _) = world.items.get(entity).map_err(|_| String::new())?;
            let spec = world
                .templates
                .0
                .get(&item.key)
                .and_then(|def| def.container.as_ref());
            let closable = spec.is_some_and(|c| c.lock.is_some() || c.state != DoorState::Open);
            if !closable {
                return Err(format!(
                    "{} doesn't open or close.",
                    capitalized(&item.name)
                ));
            }
            Ok(Lockable {
                kind: Kind::Container(entity),
                name: item.name.clone(),
                state: item.state,
                lock: spec.and_then(|c| c.lock.clone()),
                key_zone: item.key.zone.clone(),
                actor,
            })
        }
    }
}

/// Run a lock command: find the door or container, let `act` decide its new state and what
/// everyone sees, then apply it.
fn with_lockable(
    inv: &Invocation,
    players: &mut PlayerQuery,
    world: &mut Lockables,
    act: impl FnOnce(&Lockable, &Lockables) -> Result<(DoorState, Vec<Outgoing>), String>,
) {
    let result = find_lockable(inv, players, world).and_then(|target| {
        let (state, out) = act(&target, world)?;
        match target.kind {
            Kind::Door { entity, .. } => {
                if let Ok((mut door, _)) = world.doors.get_mut(entity) {
                    door.state = state;
                }
            }
            Kind::Container(entity) => {
                if let Ok((_, mut item, _)) = world.items.get_mut(entity) {
                    item.state = state;
                }
            }
        }
        Ok(out)
    });
    let out = match result {
        Ok(out) => out,
        Err(err) if err.is_empty() => Vec::new(),
        Err(err) => vec![Outgoing::line(inv.entity, err)],
    };
    deliver(out, players);
}

pub fn open(In(inv): In<Invocation>, mut players: PlayerQuery, mut world: Lockables) {
    with_lockable(&inv, &mut players, &mut world, |it, _| {
        match it.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Locked => return Err("It's locked.".into()),
            DoorState::Closed => {}
        }
        let the = it.the();
        Ok((
            DoorState::Open,
            it.announce(
                format!("You open {the}."),
                format!("{} opens {the}.", it.actor.name),
                format!("{} opens.", capitalized(&the)),
            ),
        ))
    });
}

pub fn close(In(inv): In<Invocation>, mut players: PlayerQuery, mut world: Lockables) {
    with_lockable(&inv, &mut players, &mut world, |it, _| {
        if it.state != DoorState::Open {
            return Err("It's already closed.".into());
        }
        let the = it.the();
        Ok((
            DoorState::Closed,
            it.announce(
                format!("You close {the}."),
                format!("{} closes {the}.", it.actor.name),
                format!("{} closes.", capitalized(&the)),
            ),
        ))
    });
}

/// Whether the player can work this lock: they carry a copy of its key item. A key named
/// without a zone is in the door's or container's zone. Builders can always, for testing.
fn has_key(it: &Lockable, world: &Lockables) -> Result<(), String> {
    if it.actor.role.can(Permission::Build) {
        return Ok(());
    }
    let Some(key) = it.lock.as_ref().and_then(|lock| lock.key.as_ref()) else {
        return Err("It has no keyhole.".into());
    };
    let key = ItemKey::parse(key, &it.key_zone);
    let items = world.items.iter().map(|(e, i, l)| (e, i, *l));
    if carries(items, it.actor.entity, &key) {
        Ok(())
    } else {
        Err("You don't have the key.".into())
    }
}

pub fn lock(In(inv): In<Invocation>, mut players: PlayerQuery, mut world: Lockables) {
    with_lockable(&inv, &mut players, &mut world, |it, world| {
        if it.lock.is_none() {
            return Err("It has no lock.".into());
        }
        match it.state {
            DoorState::Locked => return Err("It's already locked.".into()),
            DoorState::Open => return Err("You'll have to close it first.".into()),
            DoorState::Closed => {}
        }
        has_key(it, world)?;
        let the = it.the();
        Ok((
            DoorState::Locked,
            it.announce(
                format!("You lock {the}."),
                format!("{} locks {the}.", it.actor.name),
                format!("You hear a click from {the}."),
            ),
        ))
    });
}

pub fn unlock(In(inv): In<Invocation>, mut players: PlayerQuery, mut world: Lockables) {
    with_lockable(&inv, &mut players, &mut world, |it, world| {
        if it.lock.is_none() {
            return Err("It has no lock.".into());
        }
        if it.state != DoorState::Locked {
            return Err("It isn't locked.".into());
        }
        has_key(it, world)?;
        let the = it.the();
        Ok((
            DoorState::Closed,
            it.announce(
                format!("You unlock {the}."),
                format!("{} unlocks {the}.", it.actor.name),
                format!("You hear a click from {the}."),
            ),
        ))
    });
}

/// Force a locked door or container open: Strength against the lock's sturdiness, as a ratio,
/// with Luck. It only forces it open; the lock still works afterwards.
pub fn bash(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut world: Lockables,
    sheets: Query<&Attributes>,
    mut dice: ResMut<Dice>,
) {
    let (strength, luck) = sheets
        .get(inv.entity)
        .map(|a| (a.get(Attribute::Strength), a.get(Attribute::Luck)))
        .unwrap_or((1, 0));
    with_lockable(&inv, &mut players, &mut world, |it, _| {
        match it.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Closed => return Err("It isn't locked; just open it.".into()),
            DoorState::Locked => {}
        }
        let Some(sturdiness) = it.lock.as_ref().and_then(|lock| lock.bash) else {
            return Err("It's far too sturdy to force.".into());
        };
        let strength = f64::from(strength.max(1));
        let chance = strength / (strength + f64::from(sturdiness.max(1)));
        let the = it.the();
        if dice.chance(luck, chance) {
            Ok((
                DoorState::Open,
                it.announce(
                    format!("You bash {the} open!"),
                    format!("{} bashes {the} open!", it.actor.name),
                    format!("{} bursts open!", capitalized(&the)),
                ),
            ))
        } else {
            Ok((
                DoorState::Locked,
                it.announce(
                    format!("You throw yourself against {the}, but it holds."),
                    format!("{} slams into {the}.", it.actor.name),
                    format!("Something slams against {the}."),
                ),
            ))
        }
    });
}

/// The skill `pick` uses.
const LOCKPICKING: &str = "thievery:lockpicking";

/// Pick a locked door or container: Lockpicking (level plus Agility) against the lock's
/// difficulty, as a ratio, with Luck. Every attempt practises the skill.
pub fn pick(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut world: Lockables,
    mut practice: Practice,
) {
    let mut gains = Vec::new();
    with_lockable(&inv, &mut players, &mut world, |it, _| {
        match it.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Closed => return Err("It isn't locked.".into()),
            DoorState::Locked => {}
        }
        let Some(difficulty) = it.lock.as_ref().and_then(|lock| lock.pick) else {
            return Err("That lock can't be picked.".into());
        };
        let ready = practice.begin(it.actor.entity, LOCKPICKING)?;
        let skill = f64::from(ready.effectiveness.max(1));
        let chance = skill / (skill + f64::from(difficulty.max(1)));
        let picked = practice.chance(ready.luck, chance);
        let learner = Learner {
            character_id: it.actor.character_id,
            race: &it.actor.race,
            boost: 1.0,
        };
        gains = practice.exercise(it.actor.entity, learner, LOCKPICKING);
        let the = it.the();
        let (name, lock) = (&it.actor.name, format!("the lock of {the}"));
        Ok(if picked {
            (
                DoorState::Closed,
                it.announce(
                    format!("You work at {lock} until it gives with a click."),
                    format!("{name} picks {lock}."),
                    format!("You hear a faint click from {the}."),
                ),
            )
        } else {
            (
                DoorState::Locked,
                it.announce(
                    format!("You work at {lock}, but it won't give."),
                    format!("{name} fiddles with {lock}."),
                    format!("Something scratches at {the}."),
                ),
            )
        })
    });
    let out = gains
        .into_iter()
        .map(|line| Outgoing::line(inv.entity, line))
        .collect();
    deliver(out, &mut players);
}
