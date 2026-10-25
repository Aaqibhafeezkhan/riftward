//! Mobs: monsters, animals and townsfolk, made from templates in zone files.
//!
//! Every mob in play comes from a spawn, which keeps a room stocked: on each reset it counts the
//! mobs it made, wherever they've wandered, and makes up the difference. Builders can also
//! `summon` a mob that belongs to no spawn. Mobs aren't saved: a restart brings them back fresh
//! at their spawns, except that a spawn waiting to return after its last mob died (`AfterDeath`)
//! remembers when, so the dragon doesn't return early.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::dice::Dice;
use crate::items::{capitalized, counted};
use crate::net::Connection;
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::targeting::{Keywords, Target};
use crate::world::{
    Door, Every, InRoom, MobDef, MobKey, RoomIndex, RoomKey, SpawnDef, SpawnKey, Wander, ZoneDef,
};

/// How often, in seconds, spawns check whether they're due.
const SPAWN_CHECK_SECS: f64 = 1.0;

pub struct MobsPlugin;

impl Plugin for MobsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MobTemplates>()
            .init_resource::<Spawns>()
            .init_resource::<SpawnTimers>()
            .add_systems(Startup, load_spawn_timers)
            .add_systems(Update, (sync_mob_templates, run_spawns, wander).chain())
            .add_command(
                CommandSpec::new("summon", Category::Building)
                    .usage("summon <mob>")
                    .summary("bring a mob here from a template (`mob` or `zone:mob`)")
                    .permission(Permission::Build),
                summon,
            )
            .add_command(
                CommandSpec::new("purge", Category::Building)
                    .usage("purge [<mob>]")
                    .summary("remove a mob here, or every mob here; spawns refill as usual")
                    .permission(Permission::Build),
                purge,
            );
    }
}

/// A mob in the world.
#[derive(Component, Debug)]
pub struct Mob {
    pub key: MobKey,
    pub name: String,
    /// The line a room shows for it.
    pub here: String,
    pub description: String,
}

/// The spawn a mob came from, which counts it wherever it goes.
#[derive(Component, Debug)]
pub struct SpawnedBy(pub SpawnKey);

/// When a wandering mob next moves, in seconds since the server started.
#[derive(Component, Debug)]
pub struct Wanderer {
    pub next: f64,
}

/// Every mob and the room it's in, for looking and targeting.
pub type MobQuery<'w, 's> =
    Query<'w, 's, (Entity, &'static Mob, &'static InRoom), Without<Connection>>;

/// Mobs, and never players, who also have an `InRoom`.
type OnlyMobs = (With<Mob>, Without<Connection>);

/// Every mob template from every loaded zone.
#[derive(Resource, Default)]
pub struct MobTemplates(pub HashMap<MobKey, MobDef>);

/// Every spawn from every loaded zone.
#[derive(Resource, Default)]
pub struct Spawns(pub HashMap<SpawnKey, SpawnDef>);

/// When each spawn next resets, in unix seconds, and which spawns have had their first check
/// since the server started.
#[derive(Resource, Default)]
struct SpawnTimers {
    next: HashMap<SpawnKey, i64>,
    seen: HashSet<SpawnKey>,
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// What a spawn does at one check: how many mobs to make now, and when it next resets.
///
/// A timed spawn resets on its first check and then on its schedule. An `AfterDeath` spawn waits
/// until its last mob is gone, then waits its delay and refills completely. On its first check
/// after a restart it refills at once, unless a saved timer says it's still waiting.
fn plan(
    every: &Every,
    wanted: u32,
    alive: u32,
    timer: Option<i64>,
    first: bool,
    now: i64,
) -> (u32, Option<i64>) {
    let short = wanted.saturating_sub(alive);
    if let Every::AfterDeath(_) = every {
        if short == 0 {
            return (0, None);
        }
        return match timer {
            None if first => (short, None),
            None if alive > 0 => (0, None),
            None => (0, Some(now + every.secs())),
            Some(at) if now >= at => (short, None),
            Some(at) => (0, Some(at)),
        };
    }
    match timer {
        Some(at) if now < at => (0, Some(at)),
        _ => (short, Some(now + every.secs())),
    }
}

fn load_spawn_timers(db: Res<Database>, mut timers: ResMut<SpawnTimers>) {
    match db.spawn_timers() {
        Ok(rows) => {
            for (spawn, at) in rows {
                timers.next.insert(SpawnKey::parse(&spawn, ""), at);
            }
        }
        Err(err) => error!("failed to load spawn timers: {err}"),
    }
}

/// Rebuild the template and spawn tables when zones change, and refresh the mobs already in play.
/// Mobs whose template or spawn was removed go with it.
#[allow(clippy::too_many_arguments)]
fn sync_mob_templates(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<ZoneDef>>,
    zones: Res<Assets<ZoneDef>>,
    mut templates: ResMut<MobTemplates>,
    mut spawns: ResMut<Spawns>,
    time: Res<Time>,
    mut dice: ResMut<Dice>,
    mut mobs: Query<(Entity, &mut Mob, Option<&SpawnedBy>, Has<Wanderer>)>,
) {
    if events.read().count() == 0 {
        return;
    }
    templates.0.clear();
    spawns.0.clear();
    for (_, zone) in zones.iter() {
        for (id, def) in &zone.mobs {
            let key = MobKey {
                zone: zone.id.clone(),
                mob: id.clone(),
            };
            templates.0.insert(key, def.clone());
        }
        for (id, def) in &zone.spawns {
            let key = SpawnKey {
                zone: zone.id.clone(),
                spawn: id.clone(),
            };
            spawns.0.insert(key, def.clone());
        }
    }
    for (entity, mut mob, spawned_by, wanders) in &mut mobs {
        let spawn = spawned_by.map(|s| spawns.0.get(&s.0));
        match (templates.0.get(&mob.key), spawn) {
            (Some(def), None | Some(Some(_))) => {
                mob.name = def.name.clone();
                mob.here = def.here_line();
                mob.description = def.description.clone();
                let mut entity = commands.entity(entity);
                entity.insert(def.keywords());
                // A spawn's wandering can be switched on or off while its mobs are about.
                match spawn.flatten().and_then(|s| s.wander.secs()) {
                    Some(secs) if !wanders => {
                        let next = time.elapsed_secs_f64() + roll_secs(&mut dice, secs);
                        entity.insert(Wanderer { next });
                    }
                    None if wanders => {
                        entity.remove::<Wanderer>();
                    }
                    _ => {}
                }
            }
            _ => commands.entity(entity).despawn(),
        }
    }
}

/// Make a mob from its template in `room`.
pub fn spawn_mob(
    commands: &mut Commands,
    templates: &MobTemplates,
    key: &MobKey,
    room: Entity,
) -> Option<Entity> {
    let Some(def) = templates.0.get(key) else {
        warn!("no mob template [{key}]");
        return None;
    };
    let mob = Mob {
        key: key.clone(),
        name: def.name.clone(),
        here: def.here_line(),
        description: def.description.clone(),
    };
    Some(commands.spawn((mob, def.keywords(), InRoom(room))).id())
}

/// Check each spawn about once a second and refill the ones that are due.
#[allow(clippy::too_many_arguments)]
fn run_spawns(
    mut commands: Commands,
    mut last: Local<f64>,
    time: Res<Time>,
    index: Res<RoomIndex>,
    rooms: RoomQuery,
    templates: Res<MobTemplates>,
    spawns: Res<Spawns>,
    mut timers: ResMut<SpawnTimers>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mobs: Query<(Entity, &InRoom, Option<&SpawnedBy>), OnlyMobs>,
) {
    let elapsed = time.elapsed_secs_f64();
    if elapsed - *last < SPAWN_CHECK_SECS || index.0.is_empty() || templates.0.is_empty() {
        return;
    }
    *last = elapsed;

    // Mobs whose room was removed from its zone go away; their spawn will replace them.
    let mut alive: HashMap<&SpawnKey, u32> = HashMap::new();
    for (entity, in_room, spawned_by) in &mobs {
        if rooms.get(in_room.0).is_err() {
            commands.entity(entity).despawn();
        } else if let Some(SpawnedBy(key)) = spawned_by {
            *alive.entry(key).or_default() += 1;
        }
    }

    let now = unix_now();
    let mut keys: Vec<&SpawnKey> = spawns.0.keys().collect();
    keys.sort();
    for key in keys {
        let def = &spawns.0[key];
        let Some(&room) = index.0.get(&RoomKey::parse(&def.room, &key.zone)) else {
            continue;
        };
        let first = timers.seen.insert(key.clone());
        let timer = timers.next.get(key).copied();
        let wanted = def.refill.wanted();
        let count = alive.get(key).copied().unwrap_or(0);
        let (make, next) = plan(&def.every, wanted, count, timer, first, now);

        if next != timer {
            match next {
                Some(at) => timers.next.insert(key.clone(), at),
                None => timers.next.remove(key),
            };
            if let Every::AfterDeath(_) = def.every
                && let Err(err) = db.set_spawn_timer(&key.to_string(), next)
            {
                error!("failed to save the timer for spawn [{key}]: {err}");
            }
        }

        let mob = MobKey::parse(&def.mob, &key.zone);
        for _ in 0..make {
            let Some(entity) = spawn_mob(&mut commands, &templates, &mob, room) else {
                break;
            };
            let mut entity = commands.entity(entity);
            entity.insert(SpawnedBy(key.clone()));
            if let Some(secs) = def.wander.secs() {
                entity.insert(Wanderer {
                    next: elapsed + roll_secs(&mut dice, secs),
                });
            }
        }
    }
}

/// A random wait in the range `secs`, inclusive.
fn roll_secs(dice: &mut Dice, (min, max): (u32, u32)) -> f64 {
    let (min, max) = (i64::from(min.max(1)), i64::from(max.max(min.max(1))));
    dice.range(0, min..=max) as f64
}

/// Whether a wandering mob from `spawn` may step into `room`.
fn may_enter(wander: &Wander, spawn: &SpawnKey, room: &RoomKey, no_wander: bool) -> bool {
    if no_wander {
        return false;
    }
    match wander {
        Wander::Never => false,
        Wander::Rooms { rooms, .. } => rooms
            .iter()
            .any(|r| RoomKey::parse(r, &spawn.zone) == *room),
        Wander::Zone { .. } => room.zone == spawn.zone,
        Wander::Anywhere { .. } => true,
    }
}

/// Move each wandering mob whose time has come through a random open exit it's allowed to take.
fn wander(
    time: Res<Time>,
    spawns: Res<Spawns>,
    rooms: RoomQuery,
    doors: Query<&Door>,
    mut dice: ResMut<Dice>,
    mut mobs: Query<(&Mob, &SpawnedBy, &mut InRoom, &mut Wanderer), Without<Connection>>,
    mut players: PlayerQuery,
) {
    let elapsed = time.elapsed_secs_f64();
    let mut out = Vec::new();
    for (mob, spawned_by, mut in_room, mut wanderer) in &mut mobs {
        if wanderer.next > elapsed {
            continue;
        }
        let Some(def) = spawns.0.get(&spawned_by.0) else {
            continue;
        };
        let Some(secs) = def.wander.secs() else {
            continue;
        };
        wanderer.next = elapsed + roll_secs(&mut dice, secs);
        let Ok((_, _, exits)) = rooms.get(in_room.0) else {
            continue;
        };
        let choices: Vec<(&String, Entity)> = exits
            .0
            .iter()
            .filter(|(_, exit)| {
                exit.door
                    .and_then(|d| doors.get(d).ok())
                    .is_none_or(Door::is_open)
            })
            .filter(|(_, exit)| {
                rooms.get(exit.to).is_ok_and(|(room, key, _)| {
                    may_enter(&def.wander, &spawned_by.0, key, room.no_wander)
                })
            })
            .map(|(dir, exit)| (dir, exit.to))
            .collect();
        if choices.is_empty() {
            continue;
        }
        let pick = dice.range(0, 0..=(choices.len() as i64 - 1)) as usize;
        let (dir, to) = choices[pick];
        let name = capitalized(&mob.name);
        out.push(Outgoing::Room {
            room: in_room.0,
            except: Entity::PLACEHOLDER,
            text: format!("{name} leaves {dir}.\r\n"),
        });
        out.push(Outgoing::Room {
            room: to,
            except: Entity::PLACEHOLDER,
            text: format!("{name} arrives.\r\n"),
        });
        in_room.0 = to;
    }
    deliver(out, &mut players);
}

/// The lines a room shows for the mobs in it, with repeats counted.
pub fn here_lines(room: Entity, mobs: &MobQuery) -> Vec<String> {
    let mut here: Vec<_> = mobs.iter().filter(|(_, _, r)| r.0 == room).collect();
    here.sort_by_key(|(entity, _, _)| *entity);
    counted(here.iter().map(|(_, mob, _)| mob.here.as_str()))
}

/// Mob names in a room, with repeats counted, for a glimpse through an exit.
pub fn names_in(room: Entity, mobs: &MobQuery) -> Vec<String> {
    let mut here: Vec<_> = mobs.iter().filter(|(_, _, r)| r.0 == room).collect();
    here.sort_by_key(|(entity, _, _)| *entity);
    counted(here.iter().map(|(_, mob, _)| mob.name.as_str()))
}

/// Mobs in `room` with their keywords, in a stable order, for resolving targets.
pub fn targets_in<'a>(
    room: Entity,
    mobs: &MobQuery,
    keywords: &'a Query<&Keywords>,
) -> Vec<(Entity, &'a Keywords)> {
    let mut here: Vec<Entity> = mobs
        .iter()
        .filter(|(_, _, r)| r.0 == room)
        .map(|(entity, _, _)| entity)
        .collect();
    here.sort();
    here.into_iter()
        .filter_map(|entity| Some((entity, keywords.get(entity).ok()?)))
        .collect()
}

/// What `look <mob>` shows.
pub fn describe(mob: &Mob) -> String {
    if mob.description.is_empty() {
        format!("You see nothing special about {}.", mob.name)
    } else {
        mob.description.clone()
    }
}

/// `summon <mob>`: a builder brings a mob from a template into the room. It belongs to no spawn.
fn summon(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    templates: Res<MobTemplates>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let zone = rooms
        .get(actor.room)
        .map(|(_, key, _)| key.zone.clone())
        .unwrap_or_default();
    let key = MobKey::parse(inv.args.trim(), &zone);
    match templates.0.get(&key) {
        _ if inv.args.trim().is_empty() => out.push(Outgoing::line(inv.entity, inv.usage_error())),
        None => out.push(Outgoing::line(
            inv.entity,
            format!("There's no mob template [{key}]."),
        )),
        Some(def) => {
            spawn_mob(&mut commands, &templates, &key, actor.room);
            out.push(Outgoing::line(
                inv.entity,
                format!("You summon {} [{key}].", def.name),
            ));
            out.push(Outgoing::Room {
                room: actor.room,
                except: inv.entity,
                text: format!("{} appears.\r\n", capitalized(&def.name)),
            });
        }
    }
    deliver(out, &mut players);
}

/// `purge [<mob>]`: a builder removes one mob here, or all of them.
fn purge(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let candidates = targets_in(actor.room, &mobs, &keywords);
    let targets = match Target::parse(&inv.args) {
        None => candidates.iter().map(|(entity, _)| *entity).collect(),
        Some(target) => target.find_all(inv.entity, candidates),
    };
    let names: Vec<&str> = targets
        .iter()
        .filter_map(|entity| mobs.get(*entity).ok())
        .map(|(_, mob, _)| mob.name.as_str())
        .collect();
    let mut out = Vec::new();
    if names.is_empty() {
        out.push(Outgoing::line(
            inv.entity,
            "There's nothing like that to purge.",
        ));
    } else {
        let list = counted(names).join(", ");
        out.push(Outgoing::line(inv.entity, format!("You purge {list}.")));
        out.push(Outgoing::Room {
            room: actor.room,
            except: inv.entity,
            text: format!("{} vanishes: {list}.\r\n", actor.name),
        });
        for entity in targets {
            commands.entity(entity).despawn();
        }
    }
    deliver(out, &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_spawns_reset_at_once_then_on_schedule() {
        let every = Every::Minutes(10);
        assert_eq!(plan(&every, 4, 1, None, true, 1000), (3, Some(1600)));
        assert_eq!(plan(&every, 4, 1, Some(1600), false, 1599), (0, Some(1600)));
        assert_eq!(plan(&every, 4, 4, Some(1600), false, 1600), (0, Some(2200)));
        assert_eq!(plan(&every, 4, 6, Some(1600), false, 1600), (0, Some(2200)));
    }

    #[test]
    fn after_death_spawns_wait_once_they_are_short() {
        let every = Every::AfterDeath(Box::new(Every::Days(30)));
        let month = 30 * 86400;
        // First check after a restart with nothing waiting: the dragon is there.
        assert_eq!(plan(&every, 1, 0, None, true, 0), (1, None));
        // Alive: no timer.
        assert_eq!(plan(&every, 1, 1, None, false, 5), (0, None));
        // Killed: the wait starts.
        assert_eq!(plan(&every, 1, 0, None, false, 10), (0, Some(10 + month)));
        assert_eq!(
            plan(&every, 1, 0, Some(10 + month), false, 20),
            (0, Some(10 + month))
        );
        // A boss and its guards return only once all of them are dead.
        assert_eq!(plan(&every, 3, 1, None, false, 10), (0, None));
        assert_eq!(plan(&every, 3, 0, None, false, 10), (0, Some(10 + month)));
        // A restart keeps waiting.
        assert_eq!(
            plan(&every, 1, 0, Some(10 + month), true, 20),
            (0, Some(10 + month))
        );
        assert_eq!(
            plan(&every, 1, 0, Some(10 + month), false, 10 + month),
            (1, None)
        );
    }

    #[test]
    fn wanderers_keep_to_their_range() {
        let spawn = SpawnKey::parse("town:dogs", "");
        let market = RoomKey::parse("town:market", "");
        let stalls = RoomKey::parse("town:stalls", "");
        let road = RoomKey::parse("forest:road", "");
        let secs = (10, 20);

        let rooms = Wander::Rooms {
            rooms: vec!["market".into(), "forest:road".into()],
            secs,
        };
        assert!(may_enter(&rooms, &spawn, &market, false));
        assert!(may_enter(&rooms, &spawn, &road, false));
        assert!(!may_enter(&rooms, &spawn, &stalls, false));

        let zone = Wander::Zone { secs };
        assert!(may_enter(&zone, &spawn, &stalls, false));
        assert!(!may_enter(&zone, &spawn, &road, false));
        assert!(
            !may_enter(&zone, &spawn, &stalls, true),
            "no_wander rooms refuse"
        );

        assert!(may_enter(&Wander::Anywhere { secs }, &spawn, &road, false));
        assert!(!may_enter(&Wander::Never, &spawn, &market, false));
    }
}
