//! Mobs: monsters, animals and townsfolk, made from templates in zone files.
//!
//! Mobs in play come from spawns (see `spawns.rs`), which can also let them wander and push them
//! outward when a room gets crowded. Builders can `summon` a mob that belongs to no spawn. Mobs
//! aren't saved: a restart brings them back fresh at their spawns.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::characters::{Races, article};
use crate::combat::{Aggressive, AutoAttack, Conditions, Fighting, Inanimate, Tactics, the};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::{Permission, Role};
use crate::dice::Dice;
use crate::items::{self, HANDS, ItemLocation, ItemTemplates, capitalized, counted};
use crate::money::Purse;
use crate::net::Connection;
use crate::output::{Actor, Characters, Outbox, Outgoing, PuppetedBy, RoomQuery, deliver};
use crate::skills::{SkillDefs, SkillState, Skills};
use crate::spawns::{SpawnedBy, Spawns};
use crate::spells::Spells;
use crate::targeting::{Keywords, Target};
use crate::time::heartbeat;
use crate::vitals::{BaseHealth, Maxima, Vitals};
use crate::world::{
    Aggression, Door, DoorState, InRoom, ItemKey, MobDef, MobKey, RoomKey, SpawnKey, Spawned,
    Wander, ZoneDef,
};

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
                    .permission(Permission::Stage),
                summon,
            )
            .add_command(
                CommandSpec::new("purge", Category::Building)
                    .usage("purge [<mob>]")
                    .summary("remove a mob here, or every mob here; spawns refill as usual")
                    .permission(Permission::Stage),
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
    /// Its race, if it's one of the peoples.
    pub race: Option<String>,
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
type Idle = (Without<Connection>, Without<Fighting>, Without<PuppetedBy>);

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

/// Make a mob from its template in `room`, with its gear.
#[allow(clippy::too_many_arguments)]
pub fn spawn_mob(
    commands: &mut Commands,
    templates: &MobTemplates,
    item_templates: &ItemTemplates,
    defs: &SkillDefs,
    key: &MobKey,
    room: Entity,
    races: &Races,
    dice: &mut Dice,
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
        race: def.race.clone(),
    };
    // Mobs fight by the same rules as players, with what their template gives them: a race's
    // attributes, unless the template sets one.
    let race = def.race.as_deref().and_then(|id| races.get(id));
    if let (Some(id), None) = (&def.race, race) {
        warn!("mob [{key}]: no race '{id}'");
    }
    let attributes = Attributes::starting(
        |attribute| {
            def.attributes.get(&attribute).copied().unwrap_or_else(|| {
                // A race's shift, which never takes an attribute below 1.
                match race.and_then(|r| r.attributes.get(&attribute)) {
                    Some(&shift) => (attribute.base_rating() + shift).max(1),
                    None => attribute.base_rating(),
                }
            })
        },
        0,
    );
    let base = def.health.map(BaseHealth);
    let mut skills = Skills(
        def.skills
            .iter()
            .map(|(key, level)| {
                let mut state = SkillState::new("Template");
                state.level = *level;
                (key.clone(), state)
            })
            .collect(),
    );
    // Its race's skills, as a player of that race has them.
    for racial in race.iter().flat_map(|r| &r.skills) {
        if defs.skills.contains_key(racial) && !skills.0.contains_key(racial) {
            skills.0.insert(racial.clone(), SkillState::new("Starting"));
        }
    }
    let vitals = Vitals::full(Maxima::of(&attributes, &skills, defs, base.as_ref()));
    let mut entity = commands.spawn((
        mob,
        def.keywords(),
        InRoom(room),
        attributes,
        vitals,
        skills,
        AutoAttack(def.attack.clone()),
        Tactics(def.tactics.unwrap_or(crate::rules::rules().mob_tactics)),
        Spells(def.spells.iter().cloned().collect()),
        // What it's born with, hidden in its lair or proof against blades, until something ends it.
        // And what it is: its own traits, and its race's.
        Conditions::born_with(
            def.conditions
                .iter()
                .map(|c| (c.condition.clone(), c.strength)),
        )
        .with_traits(def.traits.iter().chain(race.iter().flat_map(|r| &r.traits))),
    ));
    if let Some(base) = base {
        entity.insert(base);
    }
    match def.aggressive {
        _ if def.inanimate() => {}
        Aggression::Never => {}
        Aggression::Always => {
            entity.insert(Aggressive::ANYWHERE);
        }
        Aggression::Home => {
            entity.insert(Aggressive { home: Some(room) });
        }
    }
    if def.inanimate() {
        entity.insert(Inanimate);
    }
    // The coins it carries: what a pickpocket can lift, and what's left falls to its slayer.
    if let Some((low, high)) = def.coins {
        let (low, high) = (low.min(high), low.max(high));
        let coins = dice.range(0, i64::from(low)..=i64::from(high)) as u64;
        entity.insert(Purse(coins));
    }
    let id = entity.id();
    // Its gear: what can be worn is worn, the rest held while it has hands free.
    let (mut held, mut worn) = (0, Vec::new());
    for item in &def.equipment {
        let item = ItemKey::parse(item, &key.zone);
        let Some(item_def) = item_templates.0.get(&item) else {
            warn!("mob [{key}]: no item template [{item}]");
            continue;
        };
        let location = match item_def.wear {
            Some(slot) if !worn.contains(&slot) => {
                worn.push(slot);
                ItemLocation::Worn(id, slot)
            }
            _ if held < HANDS => {
                held += 1;
                ItemLocation::Held(id)
            }
            _ => {
                warn!("mob [{key}]: no room to carry [{item}]");
                continue;
            }
        };
        // Its gear is of whatever quality it happened to find.
        let quality = items::Quality::roll(dice, 0, item_def);
        items::spawn_item(commands, item_templates, item, None, location, quality);
    }
    Some(id)
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
/// only once its room holds too many. A spawn that can `breach` breaks through a closed door
/// when its room is packed.
#[allow(clippy::too_many_arguments)]
pub fn wander(
    mut commands: Commands,
    time: Res<Time>,
    spawns: Res<Spawns>,
    rooms: RoomQuery,
    mut doors: Query<&mut Door>,
    mut dice: ResMut<Dice>,
    mut mobs: Query<(Entity, &SpawnedBy, &InRoom, &mut Wanderer), Idle>,
    everyone: Query<(&Mob, &InRoom)>,
    templates: Res<MobTemplates>,
    registry: Res<CommandRegistry>,
    mut outbox: ResMut<Outbox>,
) {
    // Mobs walk the way players do, with the same command.
    let Some((go, id)) = registry
        .find("go", Role::Player)
        .map(|(spec, id)| (spec.clone(), *id))
    else {
        return;
    };
    let elapsed = time.elapsed_secs_f64();
    let mut crowds: HashMap<(SpawnKey, Entity), u32> = HashMap::new();
    for (_, spawned_by, in_room, _) in &mobs {
        *crowds.entry((spawned_by.0.clone(), in_room.0)).or_default() += 1;
    }
    let crowd = |crowds: &HashMap<(SpawnKey, Entity), u32>, key: &SpawnKey, room: Entity| {
        crowds.get(&(key.clone(), room)).copied().unwrap_or(0)
    };
    // Which kinds of mob are in each room, for those who avoid some.
    let mut kinds: HashMap<Entity, Vec<&MobKey>> = HashMap::new();
    for (mob, in_room) in &everyone {
        kinds.entry(in_room.0).or_default().push(&mob.key);
    }
    for (entity, spawned_by, in_room, mut wanderer) in &mut mobs {
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
        // Never into a room holding what it avoids.
        let me = everyone.get(entity).ok().map(|(m, _)| &m.key);
        let mine = me.and_then(|k| Some((k, templates.0.get(k)?)));
        let shunned = |room: Entity| {
            let Some((own, def)) = mine.filter(|(_, d)| !d.avoids.is_empty()) else {
                return false;
            };
            kinds.get(&room).is_some_and(|here| {
                here.iter().any(|key| {
                    templates
                        .0
                        .get(*key)
                        .is_some_and(|other| def.avoids_mob(own, key, other))
                })
            })
        };
        // Packed in, they'll break through a closed door to somewhere emptier.
        let breach = def
            .breach
            .as_ref()
            .filter(|b| crowd(&crowds, key, in_room.0) >= b.packed);
        let passable = |door: Option<Entity>| {
            door.and_then(|d| doors.get(d).ok())
                .is_none_or(|d| d.is_open() || (breach.is_some() && d.can_breach()))
        };
        // Each way they might go, and the closed door they'd break to go that way.
        let mut choices: Vec<(&String, Entity, Option<Entity>)> = exits
            .0
            .iter()
            // Mobs keep to where they can walk: no climbing or swimming.
            .filter(|(_, exit)| exit.needs.is_none() && passable(exit.door) && !shunned(exit.to))
            .filter(|(_, exit)| {
                rooms.get(exit.to).is_ok_and(|(room, key, _)| {
                    may_enter(&def.wander, &spawned_by.0, key, room.no_wander)
                })
            })
            .map(|(dir, exit)| {
                let closed = exit
                    .door
                    .filter(|d| doors.get(*d).is_ok_and(|d| !d.is_open()));
                (dir, exit.to, closed)
            })
            .collect();
        if def.crowd.is_some()
            && let Some(least) = choices
                .iter()
                .map(|(_, to, _)| crowd(&crowds, key, *to))
                .min()
        {
            choices.retain(|(_, to, _)| crowd(&crowds, key, *to) == least);
        }
        // A door is broken only when what's beyond is emptier than every open way.
        if choices.iter().any(|(_, _, closed)| closed.is_none()) {
            choices.retain(|(_, _, closed)| closed.is_none());
        }
        if choices.is_empty() {
            continue;
        }
        let pick = dice.range(0, 0..=(choices.len() as i64 - 1)) as usize;
        let (dir, to, closed) = choices[pick];
        // A closed door on the way gives, and stays open.
        if let (Some(breach), Some(door)) = (breach, closed)
            && let Ok(mut door) = doors.get_mut(door)
        {
            door.state = DoorState::Open;
            let name = everyone
                .get(entity)
                .map_or("something", |(m, _)| m.name.as_str());
            let fill =
                |text: &str| text.replace("{actor}", name).replace("{door}", &door.the()) + "\r\n";
            outbox.0.push(Outgoing::Room {
                room: in_room.0,
                except: entity,
                text: fill(&breach.room),
            });
            outbox.0.push(Outgoing::Room {
                room: to,
                except: entity,
                text: fill(&breach.beyond),
            });
        }
        *crowds.entry((key.clone(), in_room.0)).or_default() -= 1;
        *crowds.entry((key.clone(), to)).or_default() += 1;
        commands.run_system_with(
            id,
            Invocation {
                entity,
                command: go.name,
                usage: go.usage,
                args: dir.clone(),
            },
        );
    }
}

/// The lines a room shows for the mobs in it, with repeats counted.
pub fn here_lines(room: Entity, mobs: &MobQuery, shown: impl Fn(Entity) -> bool) -> Vec<String> {
    let mut here: Vec<_> = mobs
        .iter()
        .filter(|(e, _, r)| r.0 == room && shown(*e))
        .collect();
    here.sort_by_key(|(entity, _, _)| *entity);
    counted(here.iter().map(|(_, mob, _)| mob.here.as_str()))
}

/// Mob names in a room, with repeats counted, for a glimpse through an exit.
pub fn names_in(room: Entity, mobs: &MobQuery, shown: impl Fn(Entity) -> bool) -> Vec<String> {
    let mut here: Vec<_> = mobs
        .iter()
        .filter(|(e, _, r)| r.0 == room && shown(*e))
        .collect();
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

/// What `look <mob>` shows: what race it is, if it's one of the peoples, and its description.
pub fn describe(mob: &Mob, races: &Races) -> String {
    let mut text = String::new();
    if let Some(race) = &mob.race {
        let race = races.name(race).to_lowercase();
        text += &format!(
            "{} is {} {race}.\r\n",
            capitalized(&the(&mob.name)),
            article(&race)
        );
    }
    if mob.description.is_empty() {
        text + &format!("You see nothing special about {}.", the(&mob.name))
    } else {
        text + &mob.description
    }
}

/// `summon <mob>`: a builder brings a mob from a template into the room. It belongs to no spawn.
#[allow(clippy::too_many_arguments)]
fn summon(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    rooms: RoomQuery,
    templates: Res<MobTemplates>,
    item_templates: Res<ItemTemplates>,
    (defs, races): (Res<SkillDefs>, Res<Races>),
    mut dice: ResMut<Dice>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let zone = rooms
        .get(actor.room)
        .map(|(_, key, _)| key.zone.clone())
        .unwrap_or_default();
    let found = crate::world::find_template(
        &inv.args,
        &zone,
        &templates.0,
        (MobKey::parse, |k: &MobKey| k.zone.as_str()),
        MobDef::keywords,
        "mob",
    );
    let found = found.and_then(|key| {
        let def = templates.0.get(&key).ok_or_else(String::new)?;
        Ok((def, key))
    });
    match found {
        _ if inv.args.trim().is_empty() => out.push(Outgoing::line(inv.entity, inv.usage_error())),
        Err(refusal) => out.push(Outgoing::line(inv.entity, refusal)),
        Ok((def, key)) => {
            spawn_mob(
                &mut commands,
                &templates,
                &item_templates,
                &defs,
                &key,
                actor.room,
                &races,
                &mut dice,
            );
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
    deliver(out, &mut outbox);
}

/// `purge [<mob>]`: a builder removes one mob here, or all of them.
fn purge(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
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
    deliver(out, &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::Item;
    use crate::world::Slot;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn mobs_are_born_with_their_conditions() {
        let mut world = World::new();
        let key = MobKey::parse("sewers:grey_slime", "");
        let mut slime = MobDef::named("a grey slime");
        slime.conditions = vec![
            crate::world::BornWith {
                condition: "shielded".into(),
                strength: 3,
            },
            crate::world::BornWith {
                condition: crate::combat::HIDDEN.into(),
                strength: 12,
            },
        ];
        // And what it is: here a thing, and an elf, though no slime is either.
        slime.traits = vec![crate::combat::INANIMATE.into()];
        slime.race = Some("elf".into());
        world.insert_resource(MobTemplates(HashMap::from([(key.clone(), slime)])));
        world.insert_resource(ItemTemplates::default());
        world.insert_resource(SkillDefs::default());
        world.insert_resource(Dice::seeded(1));
        world.insert_resource(Races(vec![crate::characters::RaceDef {
            id: "elf".into(),
            name: "Elf".into(),
            summary: String::new(),
            attributes: Default::default(),
            karma: 0,
            skills: Vec::new(),
            traits: vec!["fey_mind".into()],
        }]));
        let room = world.spawn_empty().id();
        let mob = world
            .run_system_once(
                move |mut commands: Commands,
                      mobs: Res<MobTemplates>,
                      items: Res<ItemTemplates>,
                      defs: Res<SkillDefs>,
                      races: Res<Races>,
                      mut dice: ResMut<Dice>| {
                    spawn_mob(
                        &mut commands,
                        &mobs,
                        &items,
                        &defs,
                        &key,
                        room,
                        &races,
                        &mut dice,
                    )
                },
            )
            .unwrap()
            .unwrap();
        let conditions = world.get::<Conditions>(mob).unwrap();
        assert!(conditions.has("shielded"));
        assert_eq!(conditions.0[0].strength, 3);
        assert_eq!(conditions.0[0].lasts, crate::combat::Lasts::Indefinitely);
        // Born hidden, it lurks unseen until someone notices it.
        let passer_by = Entity::from_raw_u32(99).unwrap();
        assert!(!conditions.seen_by(passer_by));
        let defs = crate::combat::defs_on_disk();
        assert_eq!(
            conditions.names(&defs, true),
            vec!["inanimate".to_string(), "Fey Mind".to_string()],
            "its traits, and its race's"
        );
        assert!(world.get::<crate::combat::Inanimate>(mob).is_some());
    }

    #[test]
    fn mobs_are_made_with_their_gear() {
        let mut world = World::new();
        let key = MobKey::parse("town:guard", "");
        let mut guard = MobDef::named("a town guard");
        // A dwarf, but one the template makes quick.
        guard.race = Some("dwarf".into());
        guard
            .attributes
            .insert(crate::attributes::Attribute::Reaction, 5);
        // The helm is worn, the sword and shield fill both hands, so a second helm has nowhere to
        // go, and there's no such thing as a torch here.
        guard.equipment = ["sword", "helm", "shield", "helm", "torch"]
            .map(String::from)
            .to_vec();
        let mut items = ItemTemplates::default();
        for (id, wear) in [
            ("sword", None),
            ("helm", Some(Slot::Head)),
            ("shield", None),
        ] {
            let mut def = crate::world::ItemDef::named(id);
            def.wear = wear;
            items.0.insert(ItemKey::parse(id, "town"), def);
        }
        world.insert_resource(MobTemplates(HashMap::from([(key.clone(), guard)])));
        world.insert_resource(items);
        world.insert_resource(SkillDefs::default());
        world.insert_resource(Dice::seeded(1));
        world.insert_resource(Races(vec![crate::characters::RaceDef {
            id: "dwarf".into(),
            name: "Dwarf".into(),
            summary: String::new(),
            attributes: [(crate::attributes::Attribute::Body, 2)].into(),
            karma: 0,
            skills: Vec::new(),
            traits: Vec::new(),
        }]));
        let room = world.spawn_empty().id();
        let mob = world
            .run_system_once(
                move |mut commands: Commands,
                      mobs: Res<MobTemplates>,
                      items: Res<ItemTemplates>,
                      defs: Res<SkillDefs>,
                      races: Res<Races>,
                      mut dice: ResMut<Dice>| {
                    spawn_mob(
                        &mut commands,
                        &mobs,
                        &items,
                        &defs,
                        &key,
                        room,
                        &races,
                        &mut dice,
                    )
                },
            )
            .unwrap()
            .unwrap();
        // Its race shifts what the template leaves alone.
        let attributes = world.get::<Attributes>(mob).unwrap();
        assert_eq!(attributes.get(crate::attributes::Attribute::Body), 5);
        assert_eq!(attributes.get(crate::attributes::Attribute::Reaction), 5);
        assert_eq!(
            attributes.get(crate::attributes::Attribute::Magic),
            0,
            "no shift, the base"
        );
        let mut gear: Vec<(String, ItemLocation)> = world
            .query::<(&Item, &ItemLocation)>()
            .iter(&world)
            .map(|(item, location)| (item.name.clone(), *location))
            .collect();
        gear.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            gear,
            [
                ("helm".to_string(), ItemLocation::Worn(mob, Slot::Head)),
                ("shield".to_string(), ItemLocation::Held(mob)),
                ("sword".to_string(), ItemLocation::Held(mob)),
            ]
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
    /// A nest of `rats` below a closed grate, wandering now, with crowding at 6 and a breach at
    /// 8. Returns the world, the grate and the nest.
    fn nest(rats: usize) -> (World, Entity, Entity) {
        use crate::command::CommandSpec;
        use crate::world::{Breach, Every, Exit, Exits, Refill, Room, SpawnDef, Terrain};

        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Dice::seeded(3));
        world.insert_resource(Outbox::default());
        world.insert_resource(MobTemplates::default());
        let go = world.register_system(|In(_): In<Invocation>| {});
        world.insert_resource(CommandRegistry::of(vec![(
            CommandSpec::new("go", Category::Movement),
            go,
        )]));
        let key = SpawnKey::parse("sewers:rats", "");
        world.insert_resource(Spawns(HashMap::from([(
            key.clone(),
            SpawnDef {
                mob: Some("rat".into()),
                item: None,
                contents: Vec::new(),
                room: "nest".into(),
                every: Every::Minutes(20),
                refill: Refill::Keep(1),
                wander: Wander::Anywhere { secs: (30, 120) },
                crowd: Some(6),
                pack: None,
                breach: Some(Breach {
                    packed: 8,
                    room: "{actor} gnaws through {door}!".into(),
                    beyond: "{door} gives way!".into(),
                }),
                when: None,
            },
        )])));
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
        let below = room(&mut world, "sewers:nest");
        let above = room(&mut world, "town:alley");
        let grate = world
            .spawn(Door {
                name: "a rusted grate".into(),
                state: DoorState::Closed,
                default: DoorState::Closed,
                lock: None,
            })
            .id();
        let exit = |to| Exit {
            to,
            door: Some(grate),
            needs: None,
        };
        world
            .entity_mut(below)
            .insert(Exits([("up".to_string(), exit(above))].into()));
        world
            .entity_mut(above)
            .insert(Exits([("down".to_string(), exit(below))].into()));
        for _ in 0..rats {
            world.spawn((
                Mob {
                    key: MobKey::parse("sewers:rat", ""),
                    name: "a sewer rat".into(),
                    here: String::new(),
                    description: String::new(),
                    race: None,
                },
                SpawnedBy(key.clone()),
                InRoom(below),
                Wanderer { next: 0.0 },
            ));
        }
        (world, grate, below)
    }

    #[test]
    fn a_packed_room_breaks_through_a_closed_door() {
        let (mut world, grate, _) = nest(8);
        world.run_system_once(wander).unwrap();
        assert!(world.get::<Door>(grate).unwrap().is_open());
        let told: Vec<String> = world
            .resource::<Outbox>()
            .0
            .iter()
            .filter_map(|out| match out {
                Outgoing::Room { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            told,
            [
                "a sewer rat gnaws through the rusted grate!\r\n",
                "the rusted grate gives way!\r\n"
            ]
        );
    }

    #[test]
    fn a_crowded_room_that_isnt_packed_stays_behind_a_closed_door() {
        let (mut world, grate, _) = nest(7);
        world.run_system_once(wander).unwrap();
        assert!(!world.get::<Door>(grate).unwrap().is_open());
        assert!(world.resource::<Outbox>().0.is_empty());
    }

    #[test]
    fn a_packed_room_takes_an_open_way_before_breaking_a_door() {
        use crate::world::{Exit, Exits, Room, Terrain};
        // A tunnel as empty as the street above: they go that way, and the grate holds.
        let (mut world, grate, below) = nest(8);
        let tunnel = world
            .spawn((
                Room {
                    name: "tunnel".into(),
                    description: String::new(),
                    no_wander: false,
                    indoors: true,
                    terrain: Terrain::Paved,
                },
                RoomKey::parse("sewers:tunnel", ""),
                Exits::default(),
            ))
            .id();
        world.get_mut::<Exits>(below).unwrap().0.insert(
            "east".into(),
            Exit {
                to: tunnel,
                door: None,
                needs: None,
            },
        );
        world.run_system_once(wander).unwrap();
        assert!(!world.get::<Door>(grate).unwrap().is_open());
    }

    /// Where wandering mobs went: the direction each `go` was given.
    #[derive(Resource, Default)]
    struct Went(Vec<String>);

    #[test]
    fn crowded_mobs_keep_out_of_rooms_holding_what_they_avoid() {
        use crate::command::CommandSpec;
        use crate::world::{Every, Exit, Exits, Refill, Room, SpawnDef, Terrain};

        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Dice::seeded(3));
        world.insert_resource(Outbox::default());
        world.insert_resource(Went::default());
        let go = world.register_system(|In(inv): In<Invocation>, mut went: ResMut<Went>| {
            went.0.push(inv.args);
        });
        world.insert_resource(CommandRegistry::of(vec![(
            CommandSpec::new("go", Category::Movement),
            go,
        )]));
        let rats = SpawnKey::parse("sewers:rats", "");
        world.insert_resource(Spawns(HashMap::from([(
            rats.clone(),
            SpawnDef {
                mob: Some("rat".into()),
                item: None,
                contents: Vec::new(),
                room: "middle".into(),
                every: Every::Minutes(20),
                refill: Refill::Keep(1),
                wander: Wander::Anywhere { secs: (30, 120) },
                crowd: Some(2),
                pack: None,
                breach: None,
                when: None,
            },
        )])));
        let (rat_key, slime_key) = (
            MobKey::parse("sewers:rat", ""),
            MobKey::parse("sewers:slime", ""),
        );
        let mut rat = MobDef::named("a sewer rat");
        rat.avoids = vec!["slime".into()];
        let mut slime = MobDef::named("a grey slime");
        slime.tags = vec!["slime".into()];
        world.insert_resource(MobTemplates(HashMap::from([
            (rat_key.clone(), rat),
            (slime_key.clone(), slime),
        ])));
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
        let (middle, east, west) = (
            room(&mut world, "sewers:middle"),
            room(&mut world, "sewers:east"),
            room(&mut world, "sewers:west"),
        );
        let exit = |to| Exit {
            to,
            door: None,
            needs: None,
        };
        world.entity_mut(middle).insert(Exits(
            [
                ("east".to_string(), exit(east)),
                ("west".to_string(), exit(west)),
            ]
            .into(),
        ));
        for here in [east, west] {
            world.entity_mut(here).insert(Exits::default());
        }
        let mob = |world: &mut World, key: &MobKey, name: &str, at: Entity| {
            world
                .spawn((
                    Mob {
                        key: key.clone(),
                        name: name.into(),
                        here: String::new(),
                        description: String::new(),
                        race: None,
                    },
                    InRoom(at),
                ))
                .id()
        };
        // A slime sits in the east room; six rats crowd the middle, three over their limit.
        mob(&mut world, &slime_key, "a grey slime", east);
        for _ in 0..6 {
            let rat = mob(&mut world, &rat_key, "a sewer rat", middle);
            world
                .entity_mut(rat)
                .insert((SpawnedBy(rats.clone()), Wanderer { next: 0.0 }));
        }
        world.run_system_once(wander).unwrap();
        world.flush();
        let went = &world.resource::<Went>().0;
        assert!(!went.is_empty(), "crowded, they move on");
        assert!(went.iter().all(|dir| dir == "west"), "never east: {went:?}");
    }

    #[test]
    fn only_doors_a_crowd_could_force_give_way() {
        let door = |state, bash| Door {
            name: "a door".into(),
            state,
            default: state,
            lock: Some(crate::world::LockDef {
                key: None,
                pick: None,
                bash,
            }),
        };
        assert!(door(DoorState::Closed, None).can_breach());
        assert!(door(DoorState::Locked, Some(6)).can_breach());
        assert!(!door(DoorState::Locked, None).can_breach(), "too sturdy");
        assert!(!door(DoorState::Open, None).can_breach(), "already open");
    }
}
