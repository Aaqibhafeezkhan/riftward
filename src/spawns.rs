//! Spawns: what keeps the world stocked with mobs and items.
//!
//! On each reset a spawn counts what it made, wherever it has gone, and makes up the difference,
//! or, for a breeding spawn, grows the population. A spawned item belongs to its spawn until a
//! player touches it and it becomes real; a fixed item, like a chest, also counts once it's real,
//! since it can't leave the room. A container's spawn tops up its contents on each reset, and a
//! unique item is never made again once a player has found it.
//!
//! Mobs and untouched items aren't saved: a restart makes them fresh. Two things are saved so a
//! restart changes nothing important: when a spawn waiting on its dead returns (the dragon's
//! month, the rats' week), and how many mobs each breeding spawn has.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::characters::Races;
use crate::db::Database;
use crate::dice::Dice;
use crate::facts::Facts;
use crate::items::{self, Item, ItemLocation, ItemTemplates, Quality};
use crate::mobs::{self, MobTemplates, Wanderer};
use crate::output::RoomQuery;
use crate::skills::SkillDefs;
use crate::time::heartbeat;
use crate::world::{
    Every, InRoom, ItemKey, MobKey, Refill, RoomIndex, RoomKey, SpawnDef, SpawnKey, Spawned,
    ZoneDef,
};

pub struct SpawnsPlugin;

impl Plugin for SpawnsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Spawns>()
            .init_resource::<SpawnTimers>()
            .add_systems(Startup, load_spawn_timers)
            .add_systems(
                Update,
                (
                    sync_spawns.before(mobs::sync_mob_templates),
                    run_spawns
                        .run_if(heartbeat())
                        .after(mobs::sync_mob_templates)
                        .after(items::restore_room_items)
                        .before(mobs::wander),
                ),
            );
    }
}

/// The spawn a mob or item came from, which counts it wherever it goes. An item stops counting
/// once a player touches it.
#[derive(Component, Debug)]
pub struct SpawnedBy(pub SpawnKey);

/// Every spawn from every loaded zone.
#[derive(Resource, Default)]
pub struct Spawns(pub HashMap<SpawnKey, SpawnDef>);

/// Each spawn's timers and counts. Times are unix seconds.
#[derive(Resource, Default)]
pub struct SpawnTimers {
    /// When a timed spawn next resets.
    next: HashMap<SpawnKey, i64>,
    /// When a spawn waiting on its dead returns: `AfterDeath`, or a breeding spawn's reseed.
    /// Saved.
    waiting: HashMap<SpawnKey, i64>,
    /// How many mobs each breeding spawn had at its last check. Saved.
    counts: HashMap<SpawnKey, u32>,
    /// When each spawn last reset, for how much a breeding spawn has grown since.
    last: HashMap<SpawnKey, i64>,
    /// Spawns that have had their first check since the server started.
    seen: HashSet<SpawnKey>,
    /// Spawns a builder has asked to reset now.
    force: HashSet<SpawnKey>,
    /// How many each spawn had at its last check.
    alive: HashMap<SpawnKey, u32>,
}

impl SpawnTimers {
    /// How many mobs, or items still in place, a spawn had at its last check.
    pub fn alive(&self, key: &SpawnKey) -> u32 {
        self.alive.get(key).copied().unwrap_or(0)
    }

    /// Reset a spawn at its next check, whatever its timers say.
    pub fn force(&mut self, key: SpawnKey) {
        self.force.insert(key);
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// What a spawn does at one check: how many to make now, and when it next resets.
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

/// How big a breeding population should be after `elapsed` seconds: logistic growth that doubles
/// every `doubling` seconds while small and levels off at `max`. It always grows by at least one
/// while any survive and there's room, so small populations don't stall.
fn grow(alive: u32, elapsed: i64, doubling: i64, max: u32) -> u32 {
    if alive == 0 || alive >= max {
        return alive;
    }
    let (n, k) = (f64::from(alive), f64::from(max));
    let shrink = 2f64.powf(-(elapsed.max(0) as f64) / doubling.max(1) as f64);
    let target = k / (1.0 + (k / n - 1.0) * shrink);
    (target.floor() as u32).clamp(alive + 1, max)
}

/// A breeding spawn's settings.
struct Breeding<'a> {
    every: &'a Every,
    doubles_every: &'a Every,
    max: u32,
    reseed: Option<&'a Every>,
    start: u32,
}

/// What a breeding spawn does at one check.
#[derive(Debug, PartialEq)]
struct BreedStep {
    make: u32,
    next: Option<i64>,
    waiting: Option<i64>,
}

/// What a breeding spawn does at one check, given its timers (see [`SpawnTimers`]).
///
/// On its first check after a restart it brings back its saved count, or `start` the first time
/// the world runs. After that it grows on its schedule. When the last of its mobs is gone it stops,
/// and waits for its reseed, if it has one, to bring back a single mob.
#[allow(clippy::too_many_arguments)]
fn plan_breed(
    breeding: &Breeding,
    alive: u32,
    saved: Option<u32>,
    next: Option<i64>,
    waiting: Option<i64>,
    last: Option<i64>,
    first: bool,
    now: i64,
) -> BreedStep {
    let every = breeding.every.secs();
    let reset = |make| BreedStep {
        make,
        next: Some(now + every),
        waiting: None,
    };
    if first {
        let target = saved.unwrap_or(breeding.start).min(breeding.max);
        if target > alive {
            return reset(target - alive);
        }
    }
    if alive == 0 {
        return match waiting {
            Some(at) if now >= at => reset(1),
            Some(at) => BreedStep {
                make: 0,
                next: None,
                waiting: Some(at),
            },
            None => BreedStep {
                make: 0,
                next: None,
                waiting: breeding.reseed.map(|reseed| now + reseed.secs()),
            },
        };
    }
    match next {
        Some(at) if now < at => BreedStep {
            make: 0,
            next: Some(at),
            waiting: None,
        },
        _ => {
            let elapsed = last.map_or(every, |last| now - last);
            let doubling = breeding.doubles_every.secs();
            reset(grow(alive, elapsed, doubling, breeding.max) - alive)
        }
    }
}

/// Which content templates to add so a container holds at least what `wanted` lists, given what
/// it `has` directly inside.
fn missing<'a>(wanted: &'a [ItemKey], has: &[&ItemKey]) -> Vec<&'a ItemKey> {
    let mut has: Vec<&ItemKey> = has.to_vec();
    wanted
        .iter()
        .filter(|key| match has.iter().position(|h| h == key) {
            Some(found) => {
                has.swap_remove(found);
                false
            }
            None => true,
        })
        .collect()
}

fn load_spawn_timers(db: Res<Database>, mut timers: ResMut<SpawnTimers>) {
    match db.spawn_timers() {
        Ok(rows) => {
            for (spawn, at) in rows {
                timers.waiting.insert(SpawnKey::parse(&spawn, ""), at);
            }
        }
        Err(err) => error!("failed to load spawn timers: {err}"),
    }
    match db.spawn_counts() {
        Ok(rows) => {
            for (spawn, count) in rows {
                timers.counts.insert(SpawnKey::parse(&spawn, ""), count);
            }
        }
        Err(err) => error!("failed to load spawn counts: {err}"),
    }
}

/// Rebuild the spawn table when zones change. Untouched items whose spawn was removed go with it;
/// `mobs::sync_mob_templates` does the same for mobs.
fn sync_spawns(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<ZoneDef>>,
    zones: Res<Assets<ZoneDef>>,
    mut spawns: ResMut<Spawns>,
    mut timers: ResMut<SpawnTimers>,
    items: Query<(Entity, &Item, &SpawnedBy)>,
) {
    if events.read().count() == 0 {
        return;
    }
    let old = std::mem::take(&mut spawns.0);
    for (_, zone) in zones.iter() {
        for (id, def) in &zone.spawns {
            let key = SpawnKey {
                zone: zone.id.clone(),
                spawn: id.clone(),
            };
            // A new schedule takes effect at once, rather than after the old one's next reset.
            if old.get(&key).is_some_and(|old| old.every != def.every) {
                timers.next.remove(&key);
            }
            spawns.0.insert(key, def.clone());
        }
    }
    for (entity, item, spawned_by) in &items {
        let gone = spawns.0.get(&spawned_by.0).is_none_or(|def| {
            def.what()
                .is_none_or(|what| !matches!(what, Spawned::Item(_)))
        });
        if item.id.is_none() && gone {
            commands.entity(entity).despawn();
        }
    }
}

/// What a spawn has in the world right now.
#[derive(Default)]
struct Census {
    /// Mobs, or items that still count.
    alive: u32,
    /// The containers among them, for topping up contents.
    containers: Vec<Entity>,
}

/// Check each spawn, on the heartbeat, and reset the ones that are due.
#[allow(clippy::too_many_arguments)]
fn run_spawns(
    mut commands: Commands,
    time: Res<Time>,
    index: Res<RoomIndex>,
    rooms: RoomQuery,
    mob_templates: Res<MobTemplates>,
    item_templates: Res<ItemTemplates>,
    (defs, races): (Res<SkillDefs>, Res<Races>),
    spawns: Res<Spawns>,
    mut timers: ResMut<SpawnTimers>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mobs: Query<(Entity, &InRoom, Option<&SpawnedBy>), mobs::OnlyMobs>,
    items: Query<(Entity, &Item, &ItemLocation, Option<&SpawnedBy>)>,
    restored: Res<items::Restored>,
    facts: Res<Facts>,
) {
    let elapsed = time.elapsed_secs_f64();
    // Wait for real items to be back in their rooms, so a real chest isn't spawned twice.
    if index.0.is_empty() || !restored.0 {
        return;
    }

    let mut census: HashMap<&SpawnKey, Census> = HashMap::new();
    // Mobs whose room was removed from its zone go away; their spawn will replace them.
    for (entity, in_room, spawned_by) in &mobs {
        if rooms.get(in_room.0).is_err() {
            commands.entity(entity).despawn();
        } else if let Some(SpawnedBy(key)) = spawned_by {
            census.entry(key).or_default().alive += 1;
        }
    }
    // Untouched items count toward their spawn, and real fixed ones toward the spawn for their
    // room and template.
    let fixed_spawns: HashMap<(Entity, &ItemKey), &SpawnKey> = spawns
        .0
        .iter()
        .filter_map(|(key, def)| {
            let Some(Spawned::Item(item)) = def.what() else {
                return None;
            };
            let item = item_templates.find(&ItemKey::parse(item, &key.zone))?;
            let room = index.0.get(&RoomKey::parse(&def.room, &key.zone))?;
            item_templates.0[item].fixed.then_some(((*room, item), key))
        })
        .collect();
    for (entity, item, location, spawned_by) in &items {
        let key = match (spawned_by, item.id, location) {
            // Contents carry their container's spawn too, but don't count as copies of it.
            (_, _, ItemLocation::Inside(_)) => None,
            (Some(SpawnedBy(key)), None, _) => Some(key),
            (_, Some(_), ItemLocation::Room(room)) => {
                fixed_spawns.get(&(*room, &item.key)).copied()
            }
            _ => None,
        };
        if let Some(key) = key {
            let entry = census.entry(key).or_default();
            entry.alive += 1;
            entry.containers.push(entity);
        }
    }

    let now = unix_now();
    let mut keys: Vec<&SpawnKey> = spawns.0.keys().collect();
    keys.sort();
    for key in keys {
        let def = &spawns.0[key];
        let (Some(what), Some(&room)) = (
            def.what(),
            index.0.get(&RoomKey::parse(&def.room, &key.zone)),
        ) else {
            continue;
        };
        // A gated spawn waits, untouched, until its trigger is met.
        if def
            .when
            .as_ref()
            .is_some_and(|when| !when.met(&key.zone, &facts, None))
        {
            continue;
        }
        let first = timers.seen.insert(key.clone());
        let force = timers.force.remove(key);
        let here = census.remove(key).unwrap_or_default();
        let count = here.alive;
        timers.alive.insert(key.clone(), count);
        let after_death = matches!(def.every, Every::AfterDeath(_));
        let mut next = timers.next.get(key).copied();
        let mut waiting = timers.waiting.get(key).copied();
        if force {
            next = None;
            waiting = waiting.map(|_| now);
            if after_death || count == 0 {
                waiting = Some(now);
            }
        }

        let (mut make, new_next, new_waiting) = match &def.refill {
            Refill::Keep(wanted) if after_death => {
                let (make, waiting) = plan(&def.every, *wanted, count, waiting, first, now);
                (make, None, waiting)
            }
            Refill::Keep(wanted) => {
                let (make, next) = plan(&def.every, *wanted, count, next, first, now);
                (make, next, None)
            }
            Refill::Breed {
                doubles_every,
                max,
                reseed,
                start,
            } => {
                let breeding = Breeding {
                    every: &def.every,
                    doubles_every,
                    max: *max,
                    reseed: reseed.as_ref(),
                    start: *start,
                };
                let saved = timers.counts.get(key).copied();
                if !first && saved != Some(count) {
                    timers.counts.insert(key.clone(), count);
                    if let Err(err) = db.set_spawn_count(&key.to_string(), count) {
                        error!("failed to save the count for spawn [{key}]: {err}");
                    }
                }
                let last = timers.last.get(key).copied();
                let step = plan_breed(&breeding, count, saved, next, waiting, last, first, now);
                (step.make, step.next, step.waiting)
            }
        };

        let reset = new_next != timers.next.get(key).copied();
        if reset {
            timers.last.insert(key.clone(), now);
            match new_next {
                Some(at) => timers.next.insert(key.clone(), at),
                None => timers.next.remove(key),
            };
        }
        if new_waiting != timers.waiting.get(key).copied() {
            match new_waiting {
                Some(at) => timers.waiting.insert(key.clone(), at),
                None => timers.waiting.remove(key),
            };
            if let Err(err) = db.set_spawn_timer(&key.to_string(), new_waiting) {
                error!("failed to save the timer for spawn [{key}]: {err}");
            }
        }

        match what {
            Spawned::Mob(mob) => {
                let mob = MobKey::parse(mob, &key.zone);
                for _ in 0..make {
                    let Some(entity) = mobs::spawn_mob(
                        &mut commands,
                        &mob_templates,
                        &item_templates,
                        &defs,
                        &mob,
                        room,
                        &races,
                        &mut dice,
                    ) else {
                        break;
                    };
                    let mut entity = commands.entity(entity);
                    entity.insert(SpawnedBy(key.clone()));
                    if let Some(secs) = def.wander.secs() {
                        entity.insert(Wanderer {
                            next: elapsed + mobs::roll_secs(&mut dice, secs),
                        });
                    }
                }
            }
            Spawned::Item(item) => {
                let Some(item) = item_templates.find(&ItemKey::parse(item, &key.zone)) else {
                    warn!("spawn [{key}]: no item template [{item}]");
                    continue;
                };
                // A unique item is never made again once a player has found it.
                if item_templates.0[item].unique && make > 0 {
                    let found = items.iter().any(|(_, i, _, _)| i.key == *item)
                        || db.item_exists(&item.to_string()).unwrap_or(true);
                    if found {
                        make = 0;
                    }
                }
                let contents: Vec<ItemKey> = def
                    .contents
                    .iter()
                    .filter_map(|c| item_templates.find(&ItemKey::parse(c, &key.zone)).cloned())
                    .collect();
                let mut containers = if reset || make > 0 || first {
                    here.containers
                } else {
                    Vec::new()
                };
                for _ in 0..make {
                    let location = ItemLocation::Room(room);
                    let quality = item_templates
                        .0
                        .get(item)
                        .map_or(Quality::Standard, |def| Quality::roll(&mut dice, 0, def));
                    let Some(entity) = items::spawn_item(
                        &mut commands,
                        &item_templates,
                        item.clone(),
                        None,
                        location,
                        quality,
                    ) else {
                        break;
                    };
                    commands.entity(entity).insert(SpawnedBy(key.clone()));
                    containers.push(entity);
                }
                // Top up each container's contents.
                for container in containers {
                    let has: Vec<&ItemKey> = items
                        .iter()
                        .filter(|(_, _, loc, _)| **loc == ItemLocation::Inside(container))
                        .map(|(_, i, _, _)| &i.key)
                        .collect();
                    for inside in missing(&contents, &has) {
                        let location = ItemLocation::Inside(container);
                        let quality = item_templates
                            .0
                            .get(inside)
                            .map_or(Quality::Standard, |def| Quality::roll(&mut dice, 0, def));
                        if let Some(entity) = items::spawn_item(
                            &mut commands,
                            &item_templates,
                            inside.clone(),
                            None,
                            location,
                            quality,
                        ) {
                            commands.entity(entity).insert(SpawnedBy(key.clone()));
                        }
                    }
                }
            }
        }
    }
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
    fn breeding_doubles_while_small_and_levels_off() {
        let hour = 3600;
        // Doubling every 4 hours: after 4 hours, 8 rats are about 16.
        assert_eq!(grow(8, 4 * hour, 4 * hour, 10_000), 15);
        // Small steps still add one.
        assert_eq!(grow(3, 60, 4 * hour, 200), 4);
        // Near the cap, growth slows and never passes it.
        assert_eq!(grow(190, 4 * hour, 4 * hour, 200), 194);
        assert_eq!(grow(199, 100 * hour, 4 * hour, 200), 200);
        assert_eq!(grow(200, 100 * hour, 4 * hour, 200), 200);
        // Nothing breeds from nothing.
        assert_eq!(grow(0, 100 * hour, 4 * hour, 200), 0);
    }

    #[test]
    fn breeding_spawns_restore_grow_die_out_and_reseed() {
        let (every, doubles, reseed) = (Every::Minutes(20), Every::Hours(4), Every::Days(7));
        let rats = Breeding {
            every: &every,
            doubles_every: &doubles,
            max: 200,
            reseed: Some(&reseed),
            start: 8,
        };
        let step = |make, next, waiting| BreedStep {
            make,
            next,
            waiting,
        };
        let week = 7 * 86400;
        let back = 2000 + week;
        // The first run starts with `start`; a restart brings back the saved count.
        assert_eq!(
            plan_breed(&rats, 0, None, None, None, None, true, 0),
            step(8, Some(1200), None)
        );
        assert_eq!(
            plan_breed(&rats, 0, Some(150), None, None, None, true, 0),
            step(150, Some(1200), None)
        );
        // Between resets, nothing; at a reset, growth since the last one.
        assert_eq!(
            plan_breed(&rats, 8, Some(8), Some(1200), None, Some(0), false, 600),
            step(0, Some(1200), None)
        );
        assert_eq!(
            plan_breed(&rats, 8, Some(8), Some(1200), None, Some(0), false, 1200),
            step(1, Some(2400), None)
        );
        // Wiped out: the reseed week starts, then one rat returns.
        assert_eq!(
            plan_breed(&rats, 0, Some(0), Some(2400), None, Some(1200), false, 2000),
            step(0, None, Some(back))
        );
        assert_eq!(
            plan_breed(&rats, 0, Some(0), None, Some(back), None, false, 3000),
            step(0, None, Some(back))
        );
        // ... even across a restart.
        assert_eq!(
            plan_breed(&rats, 0, Some(0), None, Some(back), None, true, 3000),
            step(0, None, Some(back))
        );
        assert_eq!(
            plan_breed(&rats, 0, Some(0), None, Some(back), None, false, back),
            step(1, Some(back + 1200), None)
        );
        // Without a reseed, a wiped-out spawn stays gone.
        let gone = Breeding {
            reseed: None,
            ..rats
        };
        assert_eq!(
            plan_breed(&gone, 0, Some(0), None, None, None, false, 5000),
            step(0, None, None)
        );
    }

    #[test]
    fn containers_are_topped_up_to_their_list() {
        let key = |id: &str| ItemKey::parse(id, "town");
        let (bread, torch) = (key("bread"), key("torch"));
        let wanted = vec![bread.clone(), bread.clone(), torch.clone()];
        assert_eq!(missing(&wanted, &[]), vec![&bread, &bread, &torch]);
        assert_eq!(missing(&wanted, &[&bread]), vec![&bread, &torch]);
        assert_eq!(
            missing(&wanted, &[&torch, &bread, &bread]),
            Vec::<&ItemKey>::new()
        );
        // Extra things a player put in don't count against the list.
        assert_eq!(missing(&wanted, &[&torch, &torch]), vec![&bread, &bread]);
    }
}
