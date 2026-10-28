//! Doors and closable containers: opening, closing, locking and forcing them. A door is targeted
//! by the direction of its exit (`open north`) or by name (`open trapdoor`); directions and doors
//! are checked first, then containers you carry or that lie here (`unlock chest`).
//!
//! Locking and unlocking needs a copy of the lock's key item on you. Builders can lock and unlock
//! anything without one, for testing.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::command::{CommandRegistry, Invocation};
use crate::commands::movement::{full_direction, is_direction, toward};
use crate::db::{Permission, Role};
use crate::dice::Dice;
use crate::effects::Effect;
use crate::formula::Side;
use crate::items::{
    Item, ItemLocation, ItemTemplates, Snapshot, capitalized, carries, with_keywords,
};
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::skills::{Learner, Practice, SkillKind, fill};
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
            && !beyond.is_empty()
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
    players: &Characters,
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
    players: &Characters,
    outbox: &mut Outbox,
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
    deliver(out, outbox);
}

pub fn open(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
) {
    with_lockable(&inv, &players, &mut outbox, &mut world, |it, _| {
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

pub fn close(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
) {
    with_lockable(&inv, &players, &mut outbox, &mut world, |it, _| {
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

pub fn lock(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
) {
    with_lockable(&inv, &players, &mut outbox, &mut world, |it, world| {
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

pub fn unlock(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
) {
    with_lockable(&inv, &players, &mut outbox, &mut world, |it, world| {
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
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
    sheets: Query<&Attributes>,
    mut dice: ResMut<Dice>,
) {
    let (strength, luck) = sheets
        .get(inv.entity)
        .map(|a| (a.get(Attribute::Strength), a.get(Attribute::Luck)))
        .unwrap_or((1, 0));
    with_lockable(&inv, &players, &mut outbox, &mut world, |it, _| {
        match it.state {
            DoorState::Open => return Err("It's already open.".into()),
            DoorState::Closed => return Err("It isn't locked; just open it.".into()),
            DoorState::Locked => {}
        }
        let Some(sturdiness) = it.lock.as_ref().and_then(|lock| lock.bash) else {
            return Err("It's far too sturdy to force.".into());
        };
        let the = it.the();
        // Strength against the lock's sturdiness, by opposed roll.
        let forced = dice.opposed(
            f64::from(strength.max(1)),
            luck,
            f64::from(sturdiness.max(1)),
            0,
        );
        if forced.won {
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

/// `pick <door|container>`: Lockpicking, by its old name. It's an active skill made of effects
/// (`thievery.skill.ron`), so this is `use lockpicking`.
pub fn pick(In(inv): In<Invocation>, mut commands: Commands, registry: Res<CommandRegistry>) {
    let Some((spec, id)) = registry.find("lockwork", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: format!("{LOCKPICKING} {}", inv.args.trim()),
        },
    );
}

/// `lockwork <tree:skill> <door|container>`: what `use` runs for an active skill aimed at a lock,
/// like Lockpicking. It's the user's skill (level plus attribute) against its `against` formula,
/// which reads the lock's numbers (`target.pick`, `target.sturdiness`): a lock without the one it
/// reads can't be worked that way. Worked, it takes `locked` off or puts it on, as its effects
/// say. Every try practises the skill.
pub fn lockwork(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut world: Lockables,
    mut practice: Practice,
) {
    let (key, words) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let Some(skill) = practice.defs.skills.get(key).cloned() else {
        return;
    };
    let SkillKind::Active {
        against, effects, ..
    } = &skill.def.kind
    else {
        return;
    };
    let unlocks = effects.iter().any(|e| matches!(e, Effect::Remove { .. }));
    let on = Invocation {
        entity: inv.entity,
        command: inv.command,
        usage: "use <skill> <door|container>",
        args: words.trim().to_string(),
    };
    let mut gains = Vec::new();
    with_lockable(&on, &players, &mut outbox, &mut world, |it, _| {
        let lock = it.lock.clone().ok_or("It has no lock.")?;
        match (it.state, unlocks) {
            (DoorState::Open, true) => return Err("It's already open.".into()),
            (DoorState::Open, false) => return Err("You'll have to close it first.".into()),
            (DoorState::Closed, true) => return Err("It isn't locked.".into()),
            (DoorState::Locked, false) => return Err("It's already locked.".into()),
            _ => {}
        }
        let numbers = Side {
            pick: lock.pick.map_or(0.0, f64::from),
            sturdiness: lock.bash.map_or(0.0, f64::from),
            ..Side::default()
        };
        if let Some(against) = against {
            if against.reads("target.pick") && lock.pick.is_none() {
                return Err("That lock can't be picked.".into());
            }
            if against.reads("target.sturdiness") && lock.bash.is_none() {
                return Err("It's far too sturdy to force.".into());
            }
        }
        let ready = practice.begin(it.actor.entity, key)?;
        let worked = match against {
            None => true,
            Some(against) => {
                let inputs = practice.inputs(it.actor.entity, ready.effectiveness, numbers);
                let difficulty = practice.eval(against, &inputs).max(1.0);
                let skill = f64::from(ready.effectiveness.max(1));
                practice.opposed(skill, ready.luck, difficulty, 0).won
            }
        };
        let learner = Learner {
            character_id: it.actor.character_id,
            race: &it.actor.race,
            boost: 1.0,
        };
        gains = practice.exercise(it.actor.entity, learner, key);
        let text = skill.def.text.as_ref().ok_or_else(String::new)?;
        let lines = match (worked, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let the = it.the();
        let lock = format!("the lock of {the}");
        let with = [
            ("actor", it.actor.name.as_str()),
            ("target", lock.as_str()),
            ("door", the.as_str()),
        ];
        let beyond = lines
            .beyond
            .as_ref()
            .map_or_else(String::new, |b| fill(b, &with));
        let told = it.announce(fill(&lines.actor, &with), fill(&lines.room, &with), beyond);
        Ok(match (worked, unlocks) {
            (false, _) => (it.state, told),
            (true, true) => (DoorState::Closed, told),
            (true, false) => (DoorState::Locked, told),
        })
    });
    let out = gains
        .into_iter()
        .map(|line| Outgoing::line(inv.entity, line))
        .collect();
    deliver(out, &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::GameEvent;
    use crate::formula::Formula;
    use crate::mobs::Mob;
    use crate::skills::{
        Cost, Lines, Skill, SkillDef, SkillDefs, SkillKind, SkillState, SkillText, Skills,
    };
    use crate::spells::Aim;
    use crate::vitals::Vitals;
    use crate::world::{Exit, InRoom, MobKey, Room, RoomKey, Terrain};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::{BTreeMap, HashMap};

    /// A room with a locked door north, a thief who knows Lockpicking at `level`, and a lock
    /// with this pick difficulty. Returns the world, the thief and the door.
    fn vault(level: u32, pick: Option<u32>) -> (World, Entity, Entity) {
        let mut world = World::new();
        let lines = |actor: &str| Lines {
            actor: actor.into(),
            target: None,
            room: "{actor} works at {target}.".into(),
            beyond: Some("Something scratches at {door}.".into()),
        };
        let lockpicking = Skill {
            key: LOCKPICKING.into(),
            tree: "thievery".into(),
            attribute: Attribute::Agility,
            def: SkillDef {
                name: "Lockpicking".into(),
                description: "Picking locks.".into(),
                kind: SkillKind::Active {
                    command: None,
                    quiet: false,
                    provokes: false,
                    cost: Cost::Stamina(1),
                    cooldown: 0.0,
                    aim: Aim::Lock,
                    uses: None,
                    resist: None,
                    against: Some(Formula::parse("target.pick").unwrap()),
                    tags: Vec::new(),
                    effects: vec![Effect::Remove {
                        condition: "locked".into(),
                        on: crate::effects::Who::Target,
                    }],
                },
                text: Some(SkillText {
                    hit: lines("You pick {target}."),
                    miss: Some(lines("You can't pick {target}.")),
                }),
                mastery: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires: None,
                karma: None,
            },
        };
        world.insert_resource(SkillDefs {
            skills: HashMap::from([(LOCKPICKING.to_string(), lockpicking)]),
            trees: HashMap::new(),
        });
        world.insert_resource(ItemTemplates::default());
        world.insert_resource(Dice::seeded(5));
        world.init_resource::<crate::facts::Facts>();
        world.insert_resource(crate::db::Database::open(":memory:").unwrap());
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Outbox::default());
        world.init_resource::<Messages<GameEvent>>();
        let room = |world: &mut World, id: &str| {
            world
                .spawn((
                    Room {
                        name: id.into(),
                        description: String::new(),
                        no_wander: false,
                        indoors: true,
                        terrain: Terrain::Paved,
                    },
                    RoomKey::parse(id, ""),
                ))
                .id()
        };
        let hall = room(&mut world, "test:hall");
        let vault = room(&mut world, "test:vault");
        let door = world
            .spawn((
                Door {
                    name: "an iron door".into(),
                    state: DoorState::Locked,
                    default: DoorState::Locked,
                    lock: Some(LockDef {
                        key: None,
                        pick,
                        bash: Some(6),
                    }),
                },
                DoorKey::parse("test:vault_door", ""),
                Keywords::from_name("iron door"),
            ))
            .id();
        let exit = |to| Exit {
            to,
            door: Some(door),
            needs: None,
        };
        world
            .entity_mut(hall)
            .insert(Exits(BTreeMap::from([("north".to_string(), exit(vault))])));
        world
            .entity_mut(vault)
            .insert(Exits(BTreeMap::from([("south".to_string(), exit(hall))])));
        let mut skill = SkillState::new("Template");
        skill.level = level;
        let thief = world
            .spawn((
                Mob {
                    key: MobKey::parse("test:thief", ""),
                    name: "a thief".into(),
                    here: String::new(),
                    description: String::new(),
                    race: None,
                },
                InRoom(hall),
                Attributes::starting(|_| 1, 0),
                Vitals {
                    health: 10.0,
                    stamina: 10.0,
                },
                Skills(BTreeMap::from([(LOCKPICKING.to_string(), skill)])),
            ))
            .id();
        (world, thief, door)
    }

    fn work(world: &mut World, who: Entity, args: &str) {
        let inv = Invocation {
            entity: who,
            command: "lockwork",
            usage: "",
            args: format!("{LOCKPICKING} {args}"),
        };
        world.run_system_once_with(lockwork, inv).unwrap();
    }

    fn told(world: &World, text: &str) -> bool {
        world.resource::<Outbox>().0.iter().any(|out| match out {
            Outgoing::To(_, line) | Outgoing::Room { text: line, .. } => line.contains(text),
        })
    }

    #[test]
    fn lockpicking_takes_the_lock_off_a_door() {
        let (mut world, thief, door) = vault(1000, Some(4));
        work(&mut world, thief, "north");
        assert_eq!(world.get::<Door>(door).unwrap().state, DoorState::Closed);
        assert!(told(&world, "You pick the lock of the iron door."));
        assert!(
            told(&world, "Something scratches at the iron door."),
            "beyond"
        );
        assert_eq!(world.get::<Vitals>(thief).unwrap().stamina, 9.0);
        // Now there's nothing to pick.
        work(&mut world, thief, "door");
        assert!(told(&world, "It isn't locked."));
    }

    #[test]
    fn a_hard_lock_holds_but_the_try_is_paid_for() {
        let (mut world, thief, door) = vault(0, Some(1000));
        work(&mut world, thief, "north");
        assert_eq!(world.get::<Door>(door).unwrap().state, DoorState::Locked);
        assert!(told(&world, "You can't pick the lock of the iron door."));
        assert_eq!(world.get::<Vitals>(thief).unwrap().stamina, 9.0);
    }

    #[test]
    fn a_lock_without_a_pick_difficulty_cant_be_picked() {
        let (mut world, thief, door) = vault(1000, None);
        work(&mut world, thief, "north");
        assert_eq!(world.get::<Door>(door).unwrap().state, DoorState::Locked);
        assert!(told(&world, "That lock can't be picked."));
        assert_eq!(
            world.get::<Vitals>(thief).unwrap().stamina,
            10.0,
            "nothing spent"
        );
    }
}
