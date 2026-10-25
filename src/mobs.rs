//! Mobs: monsters, animals and townsfolk, made from templates in zone files.
//!
//! Mobs in play come from spawns (see `spawns.rs`), which can also let them wander and push them
//! outward when a room gets crowded. Builders can `summon` a mob that belongs to no spawn. Mobs
//! aren't saved: a restart brings them back fresh at their spawns.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::combat::{Aggressive, AutoAttack, Fighting, MaxHealth};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Permission;
use crate::dice::Dice;
use crate::events::{GameEvent, Way, way_back};
use crate::items::{capitalized, counted};
use crate::net::Connection;
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::skills::{SkillState, Skills};
use crate::spawns::{SpawnedBy, Spawns};
use crate::targeting::{Keywords, Target};
use crate::time::heartbeat;
use crate::vitals::Vitals;
use crate::world::{Door, InRoom, MobDef, MobKey, RoomKey, SpawnKey, Spawned, Wander, ZoneDef};

pub struct MobsPlugin;

impl Plugin for MobsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MobTemplates>()
            .add_systems(
                Update,
                (sync_mob_templates, wander.run_if(heartbeat())).chain(),
            )
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

/// When a wandering mob next moves, in seconds since the server started.
#[derive(Component, Debug)]
pub struct Wanderer {
    pub next: f64,
}

/// Every mob and the room it's in, for looking and targeting.
pub type MobQuery<'w, 's> =
    Query<'w, 's, (Entity, &'static Mob, &'static InRoom), Without<Connection>>;

/// Mobs, and never players, who also have an `InRoom`.
pub type OnlyMobs = (With<Mob>, Without<Connection>);

/// Mobs that aren't in a fight, so they're free to wander.
type Idle = (Without<Connection>, Without<Fighting>);

/// Every mob template from every loaded zone.
#[derive(Resource, Default)]
pub struct MobTemplates(pub HashMap<MobKey, MobDef>);

/// Rebuild the template table when zones change, and refresh the mobs already in play. Mobs whose
/// template or spawn was removed go with it.
#[allow(clippy::too_many_arguments)]
pub fn sync_mob_templates(
    mut commands: Commands,
    mut events: MessageReader<AssetEvent<ZoneDef>>,
    zones: Res<Assets<ZoneDef>>,
    mut templates: ResMut<MobTemplates>,
    spawns: Res<Spawns>,
    time: Res<Time>,
    mut dice: ResMut<Dice>,
    mut mobs: Query<(Entity, &mut Mob, Option<&SpawnedBy>, Has<Wanderer>)>,
) {
    if events.read().count() == 0 {
        return;
    }
    templates.0.clear();
    for (_, zone) in zones.iter() {
        for (id, def) in &zone.mobs {
            let key = MobKey {
                zone: zone.id.clone(),
                mob: id.clone(),
            };
            templates.0.insert(key, def.clone());
        }
    }
    for (entity, mut mob, spawned_by, wanders) in &mut mobs {
        let spawn = spawned_by.map(|s| {
            spawns
                .0
                .get(&s.0)
                .filter(|def| matches!(def.what(), Some(Spawned::Mob(_))))
        });
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
    // Mobs fight by the same rules as players, with what their template gives them.
    let attributes = Attributes::starting(
        |attribute| {
            def.attributes
                .get(&attribute)
                .copied()
                .unwrap_or(attribute.base_rating())
        },
        0,
    );
    let health = def.health.unwrap_or(attributes.health());
    let vitals = Vitals {
        health: health as f32,
        stamina: attributes.stamina() as f32,
    };
    let skills = Skills(
        def.skills
            .iter()
            .map(|(key, level)| {
                let mut state = SkillState::new("Template");
                state.level = *level;
                (key.clone(), state)
            })
            .collect(),
    );
    let mut entity = commands.spawn((
        mob,
        def.keywords(),
        InRoom(room),
        attributes,
        vitals,
        skills,
        AutoAttack(def.attack.clone()),
        MaxHealth(health),
    ));
    if def.aggressive {
        entity.insert(Aggressive);
    }
    Some(entity.id())
}

/// A random wait in the range `secs`, inclusive.
pub fn roll_secs(dice: &mut Dice, (min, max): (u32, u32)) -> f64 {
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

/// Move each wandering mob whose time has come through an open exit it's allowed to take: a random
/// one, or, for a spawn that minds crowding, toward the room with the fewest of its kind, and
/// only once its room holds too many.
pub fn wander(
    time: Res<Time>,
    spawns: Res<Spawns>,
    rooms: RoomQuery,
    doors: Query<&Door>,
    mut dice: ResMut<Dice>,
    mut mobs: Query<(Entity, &Mob, &SpawnedBy, &mut InRoom, &mut Wanderer), Idle>,
    mut events: MessageWriter<GameEvent>,
) {
    let elapsed = time.elapsed_secs_f64();
    let mut crowds: HashMap<(SpawnKey, Entity), u32> = HashMap::new();
    for (_, _, spawned_by, in_room, _) in &mobs {
        *crowds.entry((spawned_by.0.clone(), in_room.0)).or_default() += 1;
    }
    let crowd = |crowds: &HashMap<(SpawnKey, Entity), u32>, key: &SpawnKey, room: Entity| {
        crowds.get(&(key.clone(), room)).copied().unwrap_or(0)
    };
    for (entity, mob, spawned_by, mut in_room, mut wanderer) in &mut mobs {
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
        let key = &spawned_by.0;
        if def
            .crowd
            .is_some_and(|limit| crowd(&crowds, key, in_room.0) <= limit)
        {
            continue;
        }
        let Ok((_, _, exits)) = rooms.get(in_room.0) else {
            continue;
        };
        let mut choices: Vec<(&String, Entity)> = exits
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
        if def.crowd.is_some()
            && let Some(least) = choices.iter().map(|(_, to)| crowd(&crowds, key, *to)).min()
        {
            choices.retain(|(_, to)| crowd(&crowds, key, *to) == least);
        }
        if choices.is_empty() {
            continue;
        }
        let pick = dice.range(0, 0..=(choices.len() as i64 - 1)) as usize;
        let (dir, to) = choices[pick];
        *crowds.entry((key.clone(), in_room.0)).or_default() -= 1;
        *crowds.entry((key.clone(), to)).or_default() += 1;
        let name = capitalized(&mob.name);
        events.write(GameEvent::Left {
            who: entity,
            name: name.clone(),
            room: in_room.0,
            way: Way::Walked(Some(dir.clone())),
        });
        events.write(GameEvent::Entered {
            who: entity,
            name,
            room: to,
            way: Way::Walked(way_back(&rooms, to, in_room.0)),
        });
        in_room.0 = to;
    }
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
