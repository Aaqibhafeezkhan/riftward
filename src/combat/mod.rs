//! Combat: fights between players and mobs, made entirely of skills.
//!
//! Fights run in rounds, every `ROUND_SECS`. Each round, everyone fighting acts once, in order of
//! initiative (Reaction + Intuition, rolled): a player's next queued action (see `ActionQueue`), or
//! otherwise their auto-attack, an attack skill they choose with `autoattack` (the basic attack,
//! Brawling, until they do). Mobs fight with attack skills from their templates, by the same rules.
//!
//! An attack pits the attack skill (level + its attribute) against the defender's Reaction plus
//! Dodge, as a ratio, each side with its Luck. Someone caught resting defends at half, and spends
//! their first turn getting up. Mobs fight better under an aura (see `SkillKind::Aura`): while the
//! Rat King lives, every sewer rat in the sewers is tougher. A hit does the skill's damage plus Strength / 2.
//! At 0 health a mob dies and leaves a corpse; a player wakes at the Wayside Shrine, hurt, and
//! everything they carried stays on their corpse where they fell.
//!
//! Players can't fight each other yet.

mod states;
mod strike;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;

use crate::attributes::{Attribute, Attributes};
use crate::command::{
    self, ActionQueue, AddCommand, Category, CommandRegistry, CommandSpec, Invocation,
};
use crate::commands::movement::{View, travel};
use crate::db::{Database, ItemPlace};
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::items::{
    self, ItemLocation, ItemMutQuery, ItemQuery, ItemTemplates, MOB_CORPSE_SECS,
    PLAYER_CORPSE_SECS, capitalized,
};
use crate::mobs::{self, Mob, MobQuery, MobTemplates};
use crate::net::Connection;
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::skills::{Bonus, SkillDefs, SkillKind, Skills, boosts};
use crate::spawns::{SpawnedBy, Spawns};
use crate::targeting::{Keywords, Target};
use crate::vitals::{Maxima, Resting, Vitals};
use crate::world::{InRoom, ItemKey, MobKey, Player, RoomIndex, RoomKey, without_article};

pub use states::{Conditions, State};
pub use strike::{Fighters, Move, Round, name_of};

/// Seconds in a combat round, and between a player's actions.
pub const ROUND_SECS: f64 = 3.0;
/// Everyone's most basic attack, used until they choose another.
pub const BASIC_ATTACK: &str = "brawling:basics";
/// Where players wake after they die.
const RESPAWN_ROOM: &str = "riftwatch:shrine";
/// Stamina it takes to flee.
const FLEE_COST: f32 = 2.0;
/// The chance of getting away, before Luck.
const FLEE_CHANCE: f64 = 0.6;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CombatLog>()
            .add_systems(
                Update,
                (
                    combat_round.run_if(on_timer(Duration::from_secs_f64(ROUND_SECS))),
                    abandon_fights,
                    handle_deaths,
                    deliver_log,
                )
                    .chain()
                    .after(command::dispatch),
            )
            .add_command(
                CommandSpec::new("kill", Category::Interaction)
                    .shortcuts(&["attack", "k"])
                    .usage("kill <target>")
                    .summary("attack something; you fight each round until one of you falls")
                    .action(),
                kill,
            )
            .add_command(
                CommandSpec::new("flee", Category::Interaction)
                    .summary("try to escape a fight through a random exit (costs stamina)")
                    .action(),
                flee,
            )
            .add_command(
                CommandSpec::new("autoattack", Category::Character)
                    .usage("autoattack [<skill>]")
                    .summary("see or choose the attack you make each round of a fight"),
                autoattack,
            )
            .add_command(
                CommandSpec::new("stop", Category::Character)
                    .summary("forget the actions you've queued up"),
                stop,
            )
            // Run by `use` for a technique; players don't type it themselves.
            .add_command(
                CommandSpec::new("technique", Category::Interaction)
                    .usage("use <technique> [<target>]")
                    .summary("make a technique: in a fight, or an opener to start one")
                    .unlisted(),
                technique,
            );
    }
}

/// Who someone is fighting.
#[derive(Component, Debug, Clone, Copy)]
pub struct Fighting(pub Entity);

/// The attack someone makes each round: `None` means the basic attack.
#[derive(Component, Debug, Clone, Default)]
pub struct AutoAttack(pub Option<String>);

/// A mob that attacks players who come near.
#[derive(Component, Debug)]
pub struct Aggressive;

/// How far a player is outmatched by the foe they're fighting (see [`strike::outmatched`]),
/// updated each round. A tough fight makes their skills quicker to improve and reveal.
#[derive(Component, Debug, Clone, Copy)]
pub struct Outmatched(pub f64);

/// The chance, in percent, that a mob makes one of its techniques on its turn instead of its
/// auto-attack.
#[derive(Component, Debug, Clone, Copy)]
pub struct Tactics(pub u32);

/// Killed this round, waiting for `handle_deaths`.
#[derive(Component, Debug)]
pub struct Dead;

/// Who struck the killing blow, whose Luck the loot is rolled with.
#[derive(Component, Debug, Clone, Copy)]
pub struct Slayer(pub Entity);

/// What combat has to say, delivered after each round.
#[derive(Resource, Default)]
pub struct CombatLog(Vec<Outgoing>);

/// "the sewer rat" for "a sewer rat"; names stay as they are.
pub fn the(name: &str) -> String {
    let bare = without_article(name);
    if bare.len() == name.trim().len() {
        name.to_string()
    } else {
        format!("the {bare}")
    }
}

/// A technique made ready to use on the fighter's next turn: an opener that starts a fight.
#[derive(Component, Debug, Clone)]
pub struct Readied {
    pub skill: String,
}

/// The aura each living mob fights under: every living mob holding an aura skill lends its bonus
/// to the mobs it affects in its zone.
fn auras(fighters: &Fighters, rooms: &RoomQuery, defs: &SkillDefs) -> HashMap<Entity, Bonus> {
    let zone_of = |room: Entity| rooms.get(room).ok().map(|(_, key, _)| key.zone.clone());
    let mut by_zone: HashMap<(String, MobKey), Bonus> = HashMap::new();
    for fighter in fighters {
        let (false, Some(_), Some(zone)) = (fighter.dead, fighter.mob, zone_of(fighter.in_room.0))
        else {
            continue;
        };
        for key in fighter.skills.0.keys() {
            if let Some(SkillKind::Aura { affects, bonus }) =
                defs.skills.get(key).map(|s| &s.def.kind)
            {
                let affected = MobKey::parse(affects, &zone);
                *by_zone.entry((zone.clone(), affected)).or_default() += *bonus;
            }
        }
    }
    fighters
        .iter()
        .filter_map(|fighter| {
            let zone = zone_of(fighter.in_room.0)?;
            let bonus = by_zone.get(&(zone, fighter.mob?.key.clone()))?;
            Some((fighter.entity, *bonus))
        })
        .collect()
}

/// The technique a queued `use` names, if it is one the player knows.
fn queued_technique(queue: &ActionQueue, skills: &Skills, defs: &SkillDefs) -> Option<String> {
    let (_, invocation) = queue.peek()?;
    if invocation.command != "use" {
        return None;
    }
    let (skill, _) = defs.find_known_in(skills, &invocation.args)?;
    matches!(skill.def.kind, SkillKind::Technique(_)).then(|| skill.key.clone())
}

/// One round of every fight.
#[allow(clippy::too_many_arguments)]
fn combat_round(
    mut commands: Commands,
    mut fighters: Fighters,
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    defs: Res<SkillDefs>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    time: Res<Time>,
    mut events: MessageWriter<GameEvent>,
    mut log: ResMut<CombatLog>,
    spawned: Query<&SpawnedBy>,
    spawns: Res<Spawns>,
    resting_query: Query<Entity, With<Resting>>,
    readied: Query<&Readied>,
    rooms: RoomQuery,
) {
    let now = time.elapsed_secs_f64();
    let auras = auras(&fighters, &rooms, &defs);
    let resting: HashSet<Entity> = resting_query.iter().collect();
    let mut targets: HashMap<Entity, Entity> = HashMap::new();
    let mut alive_players: Vec<(Entity, Entity)> = Vec::new();
    for fighter in &fighters {
        if fighter.dead {
            continue;
        }
        if let Some(Fighting(target)) = fighter.fighting {
            targets.insert(fighter.entity, *target);
        }
        if fighter.player.is_some() {
            alive_players.push((fighter.entity, fighter.in_room.0));
        }
    }

    // Aggressive mobs pick on a player in their room, but no more of a spawn's mobs gang up on
    // one player than its `pack` allows; the rest wait their turn.
    let spawn_of = |entity: Entity| spawned.get(entity).ok().map(|s| &s.0);
    for fighter in &fighters {
        if !fighter.aggressive || fighter.dead || fighter.fighting.is_some() {
            continue;
        }
        let entity = fighter.entity;
        let pack = spawn_of(entity)
            .and_then(|key| spawns.0.get(key))
            .and_then(|def| def.pack);
        let room_for = |victim: Entity| {
            pack.is_none_or(|limit| {
                let ganging = targets
                    .iter()
                    .filter(|(a, t)| **t == victim && spawn_of(**a) == spawn_of(entity))
                    .count();
                (ganging as u32) < limit
            })
        };
        let Some(&(victim, _)) = alive_players
            .iter()
            .find(|(victim, room)| *room == fighter.in_room.0 && room_for(*victim))
        else {
            continue;
        };
        targets.insert(entity, victim);
        commands.entity(entity).insert(Fighting(victim));
        let name = name_of(None, fighter.mob);
        log.0.push(Outgoing::line(
            victim,
            format!("\x1b[1;31m{} attacks you!\x1b[0m", capitalized(&name)),
        ));
    }

    // Fights end when the target is gone, dead or elsewhere.
    let place = |entity: Entity| {
        fighters
            .get(entity)
            .ok()
            .filter(|f| !f.dead)
            .map(|f| f.in_room.0)
    };
    let pairs: Vec<(Entity, Entity)> = targets.iter().map(|(a, t)| (*a, *t)).collect();
    for (attacker, target) in pairs {
        if place(target).is_none() || place(target) != place(attacker) {
            targets.remove(&attacker);
            commands.entity(attacker).remove::<Fighting>();
        }
    }
    // Who was fighting whom before anyone fought back, for openers.
    let engaged = targets.clone();
    // Defenders fight back.
    let pairs: Vec<(Entity, Entity)> = targets.iter().map(|(a, t)| (*a, *t)).collect();
    for (attacker, target) in pairs {
        if let std::collections::hash_map::Entry::Vacant(entry) = targets.entry(target) {
            entry.insert(attacker);
            commands.entity(target).insert(Fighting(attacker));
        }
    }

    // Initiative: Reaction + Intuition, plus a roll.
    let mut order: Vec<(i64, Entity)> = targets
        .keys()
        .filter_map(|entity| {
            let fighter = fighters.get(*entity).ok()?;
            let luck = fighter.attributes.get(Attribute::Luck);
            let roll = dice.range(luck, 1..=6);
            let quick = boosts(fighter.skills, &defs).initiative;
            Some((
                i64::from(fighter.attributes.initiative() + quick) + roll,
                *entity,
            ))
        })
        .collect();
    order.sort_by(|a, b| b.cmp(a));

    let outmatched = strike::outmatched(&fighters, &targets, &auras, &items, &templates, &defs);
    for (who, _) in &alive_players {
        match outmatched.get(who) {
            Some(odds) => commands.entity(*who).insert(Outmatched(*odds)),
            None => commands.entity(*who).remove::<Outmatched>(),
        };
    }

    let mut fallen: HashSet<Entity> = HashSet::new();
    let mut round = Round {
        fighters: &mut fighters,
        items: &items,
        templates: &templates,
        defs: &defs,
        dice: &mut dice,
        db: &db,
        events: &mut events,
        log: &mut log,
        watchers: &alive_players,
        engaged: &engaged,
        targets: &targets,
        auras: &auras,
        resting: &resting,
        outmatched: &outmatched,
    };
    for (_, attacker) in order {
        let Some(&target) = targets.get(&attacker) else {
            continue;
        };
        if fallen.contains(&attacker) || fallen.contains(&target) {
            continue;
        }
        // Someone caught resting spends their first turn getting up.
        if resting.contains(&attacker) {
            commands.entity(attacker).remove::<Resting>();
            round.log.0.push(Outgoing::line(
                attacker,
                "\x1b[1mYou scramble to your feet!\x1b[0m",
            ));
            continue;
        }
        // Bleeding, staggers and falls come first.
        let (acts, bled_out) = round.start_turn(attacker);
        if bled_out {
            fallen.insert(attacker);
            commands.entity(attacker).insert(Dead).remove::<Fighting>();
            continue;
        }
        if !acts {
            continue;
        }
        // A readied opener, or a technique next in the queue, takes the turn.
        let mut technique = readied.get(attacker).ok().map(|r| r.skill.clone());
        if technique.is_some() {
            commands.entity(attacker).remove::<Readied>();
        } else if let Ok(mut fighter) = round.fighters.get_mut(attacker)
            && fighter.player.is_some()
            && let Some(queue) = fighter.queue.as_mut()
        {
            technique = queued_technique(queue, &fighter.skills, &defs);
            if technique.is_some() {
                queue.pop();
                queue.next_at = now + ROUND_SECS;
            } else if let Some((id, invocation)) = queue.pop() {
                // Any other action takes the turn too.
                queue.next_at = now + ROUND_SECS;
                commands.run_system_with(id, invocation);
                continue;
            }
        }
        // Mobs choose their own, now and then.
        if technique.is_none() {
            technique = round.mob_technique(attacker);
        }
        let moves = technique
            .as_ref()
            .and_then(|key| match defs.skills.get(key) {
                Some(skill) => match &skill.def.kind {
                    SkillKind::Technique(t) => Some(Move {
                        key: key.as_str(),
                        technique: t,
                    }),
                    _ => None,
                },
                None => None,
            });
        for died in round.strike(attacker, target, moves, false) {
            fallen.insert(died);
            // A riposte can fell the attacker; then the target struck the blow.
            let slayer = if died == attacker { target } else { attacker };
            commands
                .entity(died)
                .insert((Dead, Slayer(slayer)))
                .remove::<Fighting>();
            for (who, whom) in &targets {
                if *whom == died {
                    commands.entity(*who).remove::<Fighting>();
                }
            }
        }
    }
}

/// `technique <skill> [<target>]`, run by `use` for a technique outside a fight: an opener starts
/// one, readied for the user's first turn; anything else needs a fight already going.
#[allow(clippy::too_many_arguments)]
fn technique(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    fighting: Query<&Fighting>,
    dead: Query<(), With<Dead>>,
    defs: Res<SkillDefs>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let (key, target) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let Some(skill) = defs.skills.get(key) else {
        return;
    };
    let SkillKind::Technique(t) = &skill.def.kind else {
        return;
    };
    let text = if fighting.get(inv.entity).is_ok() {
        // Readied for their next turn.
        commands.entity(inv.entity).insert(Readied {
            skill: key.to_string(),
        });
        format!("You ready yourself to {}.", skill.def.name)
    } else if !t.opener {
        format!("You need to be in a fight to {}.", skill.def.name)
    } else {
        let candidates: Vec<_> = mobs::targets_in(actor.room, &mobs, &keywords)
            .into_iter()
            .filter(|(entity, _)| dead.get(*entity).is_err())
            .collect();
        match Target::parse(target)
            .and_then(|t| t.find_one(inv.entity, candidates))
            .and_then(|e| mobs.get(e).ok())
        {
            None if target.trim().is_empty() => format!("{} whom?", skill.def.name),
            None => format!("You don't see {} here.", target.trim()),
            Some((mob_entity, mob, _)) => {
                commands
                    .entity(inv.entity)
                    .insert((
                        Fighting(mob_entity),
                        Readied {
                            skill: key.to_string(),
                        },
                    ))
                    .remove::<Resting>();
                format!(
                    "You slip toward {}, ready to {}.",
                    the(&mob.name),
                    skill.def.name
                )
            }
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// Fighters who haven't fallen yet.
type StillFighting = (With<Fighting>, Without<Dead>);

/// Anyone who quits or disconnects in the middle of a fight is cut down as they go: no one
/// escapes a fight by leaving the game. They die like anyone else, so their gear stays on their
/// corpse and they wake at the shrine next time.
fn abandon_fights(
    mut commands: Commands,
    leaving: Query<(Entity, &Connection, &Player, &InRoom), StillFighting>,
    mut log: ResMut<CombatLog>,
) {
    for (entity, conn, player, in_room) in &leaving {
        if !conn.closing {
            continue;
        }
        commands.entity(entity).insert(Dead).remove::<Fighting>();
        log.0.push(Outgoing::line(
            entity,
            "\x1b[1;31mYou turn to flee the fight, and are cut down as you go.\x1b[0m",
        ));
        log.0.push(Outgoing::Room {
            room: in_room.0,
            except: entity,
            text: format!(
                "{} tries to slip away from the fight, and is cut down!\r\n",
                player.name
            ),
        });
        info!("{} left in the middle of a fight, and died", player.name);
    }
}

/// Mobs killed this round, with who killed them.
type DeadMobs<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Mob,
        &'static InRoom,
        Option<&'static Slayer>,
    ),
    (With<Dead>, mobs::OnlyMobs),
>;

/// Lay out the dead: mobs leave a corpse, and players leave one with everything they carried,
/// then wake at the shrine.
#[allow(clippy::too_many_arguments)]
fn handle_deaths(
    mut commands: Commands,
    dead_mobs: DeadMobs,
    dead_players: Query<Entity, (With<Dead>, With<Player>)>,
    (mob_templates, mut dice): (Res<MobTemplates>, ResMut<Dice>),
    mut players: PlayerQuery,
    mut sheets: Query<(
        &mut Vitals,
        &Attributes,
        &Skills,
        Option<&mut ActionQueue>,
        &mut Conditions,
    )>,
    defs: Res<SkillDefs>,
    mut items: ItemMutQuery,
    templates: Res<ItemTemplates>,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    registry: Res<CommandRegistry>,
    db: Res<Database>,
    mut log: ResMut<CombatLog>,
) {
    // Say how the fight ended before the dead wake up somewhere else.
    if !log.0.is_empty() {
        deliver(std::mem::take(&mut log.0), &mut players);
    }
    let now = unix_now();
    for (entity, mob, in_room, slayer) in &dead_mobs {
        let corpse = items::spawn_corpse(
            &mut commands,
            &templates,
            &mob.name,
            None,
            in_room.0,
            now + MOB_CORPSE_SECS,
        );
        // Its gear falls with it, for whoever wants it, and its loot, one find from each table
        // with its slayer's Luck.
        if let Some(corpse) = corpse {
            for (_, _, mut location) in &mut items {
                if location.holder() == Some(entity) {
                    *location = ItemLocation::Inside(corpse);
                }
            }
            let luck = slayer
                .and_then(|s| sheets.get(s.0).ok())
                .map_or(0, |(_, attributes, ..)| attributes.get(Attribute::Luck));
            let tables = mob_templates.0.get(&mob.key).map(|def| &def.loot);
            for table in tables.into_iter().flatten() {
                if let Some(Some(id)) = dice.pick(luck, table) {
                    let key = ItemKey::parse(id, &mob.key.zone);
                    items::spawn_item(
                        &mut commands,
                        &templates,
                        key,
                        None,
                        ItemLocation::Inside(corpse),
                    );
                }
            }
        }
        commands.entity(entity).despawn();
    }

    for entity in &dead_players {
        let Some(actor) = Actor::get(&players, entity) else {
            continue;
        };
        let room_key = rooms
            .get(actor.room)
            .map(|(_, key, _)| key.to_string())
            .unwrap_or_default();

        // Their corpse is real, so it and what's in it survive a restart.
        let corpse_id = match db.create_item(
            &items::corpse_key().to_string(),
            &ItemPlace::Room(room_key.clone()),
        ) {
            Ok(id) => Some(id),
            Err(err) => {
                error!("failed to save {}'s corpse: {err}", actor.name);
                None
            }
        };
        let name = format!("the corpse of {}", actor.name);
        if let Some(id) = corpse_id
            && let Err(err) = db.set_item_name(id, &name)
        {
            error!("failed to name {}'s corpse: {err}", actor.name);
        }
        if let Some(corpse) = items::spawn_corpse(
            &mut commands,
            &templates,
            &actor.name,
            corpse_id,
            actor.room,
            now + PLAYER_CORPSE_SECS,
        ) {
            for (_, mut item, mut location) in &mut items {
                if location.holder() != Some(entity) {
                    continue;
                }
                *location = ItemLocation::Inside(corpse);
                let Some(corpse_id) = corpse_id else {
                    continue;
                };
                let place = ItemPlace::Inside(corpse_id);
                let saved = match item.id {
                    Some(id) => db.move_item(id, &place),
                    None => db
                        .create_item(&item.key.to_string(), &place)
                        .map(|id| item.id = Some(id)),
                };
                if let Err(err) = saved {
                    error!("failed to move an item to {}'s corpse: {err}", actor.name);
                }
            }
        }

        // They wake at the shrine, hurt and tired.
        let shrine = index
            .0
            .get(&RoomKey::parse(RESPAWN_ROOM, ""))
            .copied()
            .or_else(|| index.start())
            .unwrap_or(actor.room);
        if let Ok((_, _, _, Some(mut in_room))) = players.get_mut(entity) {
            in_room.0 = shrine;
        }
        if let Ok((_, key, _)) = rooms.get(shrine)
            && let Err(err) = db.save_location(actor.character_id, &key.to_string())
        {
            error!("failed to save {}'s location: {err}", actor.name);
        }
        if let Ok((mut vitals, attributes, skills, queue, mut conditions)) = sheets.get_mut(entity)
        {
            let max = Maxima::of(attributes, skills, &defs, None);
            vitals.health = (max.health as f32 / 4.0).max(1.0);
            vitals.stamina = max.stamina as f32 / 4.0;
            if let Some(mut queue) = queue {
                queue.clear();
            }
            conditions.clear();
        }
        commands.entity(entity).remove::<(Dead, Fighting)>();

        let mut out = vec![Outgoing::line(
            entity,
            "\x1b[1;31mYou have been killed!\x1b[0m\r\n\r\nDarkness takes you... and then candlelight. \
             You wake on the cold stones of the Wayside Shrine, aching in every bone. Everything \
             you carried lies with your body where you fell.",
        )];
        out.push(Outgoing::Room {
            room: shrine,
            except: entity,
            text: format!(
                "{} stirs on the shrine's floor, pale and shaken.\r\n",
                actor.name
            ),
        });
        deliver(out, &mut players);
        if let Some((spec, id)) = registry.find("look", actor.role) {
            commands.run_system_with(
                *id,
                Invocation {
                    entity,
                    command: spec.name,
                    usage: spec.usage,
                    args: String::new(),
                },
            );
        }
    }
}

/// Tell everyone what happened in the round. World events run after this, so news of a death
/// follows the blow that caused it.
pub fn deliver_log(mut log: ResMut<CombatLog>, mut players: PlayerQuery) {
    if !log.0.is_empty() {
        deliver(std::mem::take(&mut log.0), &mut players);
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// `kill <target>`: start a fight with a mob here.
fn kill(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    fighting: Query<&Fighting>,
    dead: Query<(), With<Dead>>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let candidates: Vec<_> = mobs::targets_in(actor.room, &mobs, &keywords)
        .into_iter()
        .filter(|(entity, _)| dead.get(*entity).is_err())
        .collect();
    let target = Target::parse(&inv.args).and_then(|t| t.find_one(inv.entity, candidates));
    match target.and_then(|e| mobs.get(e).ok()) {
        _ if inv.args.trim().is_empty() => out.push(Outgoing::line(inv.entity, "Kill what?")),
        None if crate::output::targets_in_room(actor.room, &players, &keywords)
            .iter()
            .any(|(_, k)| k.matches(inv.args.trim())) =>
        {
            out.push(Outgoing::line(
                inv.entity,
                "You can't attack other players.",
            ));
        }
        None => out.push(Outgoing::line(
            inv.entity,
            format!("You don't see {} here.", inv.args.trim()),
        )),
        Some((mob_entity, mob, _)) => {
            let name = the(&mob.name);
            if fighting.get(inv.entity).is_ok_and(|f| f.0 == mob_entity) {
                out.push(Outgoing::line(
                    inv.entity,
                    format!("You're already fighting {name}!"),
                ));
            } else {
                commands
                    .entity(inv.entity)
                    .insert(Fighting(mob_entity))
                    .remove::<Resting>();
                if fighting.get(mob_entity).is_err() {
                    commands.entity(mob_entity).insert(Fighting(inv.entity));
                }
                out.push(Outgoing::line(
                    inv.entity,
                    format!("\x1b[1mYou attack {name}!\x1b[0m"),
                ));
                out.push(Outgoing::Room {
                    room: actor.room,
                    except: inv.entity,
                    text: format!("{} attacks {name}!\r\n", actor.name),
                });
            }
        }
    }
    deliver(out, &mut players);
}

/// `flee`: try to run from a fight through a random open exit.
#[allow(clippy::too_many_arguments)]
fn flee(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    view: View,
    fighting: Query<&Fighting>,
    mut sheets: Query<(&mut Vitals, &Attributes)>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mut events: MessageWriter<GameEvent>,
    conditions: Query<&Conditions>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply = |text: &str| vec![Outgoing::line(inv.entity, text)];
    // Where they run, once they've paid for it and broken away.
    let mut escape = None;
    let mut out = if fighting.get(inv.entity).is_err() {
        reply("You aren't fighting anyone.")
    } else if conditions.get(inv.entity).is_ok_and(|c| !c.can_flee()) {
        reply("You're held fast and can't break away!")
    } else if let Ok((mut vitals, attributes)) = sheets.get_mut(inv.entity) {
        if vitals.stamina < FLEE_COST {
            reply("You're too exhausted to run!")
        } else {
            vitals.stamina -= FLEE_COST;
            events.write(GameEvent::StaminaSpent {
                who: inv.entity,
                amount: FLEE_COST,
            });
            let exits: Vec<String> = view
                .rooms
                .get(actor.room)
                .map(|(_, _, exits)| {
                    exits
                        .0
                        .iter()
                        .filter(|(_, exit)| {
                            exit.door
                                .and_then(|d| view.doors.get(d).ok())
                                .is_none_or(|d| d.is_open())
                        })
                        .map(|(dir, _)| dir.clone())
                        .collect()
                })
                .unwrap_or_default();
            let luck = attributes.get(Attribute::Luck);
            if exits.is_empty() {
                reply("There's nowhere to run!")
            } else if !dice.chance(luck, FLEE_CHANCE) {
                let mut out = reply("You try to break away, but can't get clear!");
                out.push(Outgoing::Room {
                    room: actor.room,
                    except: inv.entity,
                    text: format!("{} tries to flee, but can't get away.\r\n", actor.name),
                });
                out
            } else {
                let pick = dice.range(0, 0..=(exits.len() as i64 - 1)) as usize;
                commands.entity(inv.entity).remove::<Fighting>();
                escape = Some(exits[pick].clone());
                reply("You flee in a panic!")
            }
        }
    } else {
        Vec::new()
    };
    // Fleeing has already paid its way.
    if let Some(dir) = escape {
        out.extend(travel(
            inv.entity,
            &dir,
            &mut players,
            &view,
            &db,
            &mut events,
            &mut sheets,
            false,
        ));
    }
    deliver(out, &mut players);
}

/// `autoattack [<skill>]`: see or choose the attack made each round.
fn autoattack(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut sheets: Query<(&Skills, &mut AutoAttack)>,
    defs: Res<SkillDefs>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let Ok((skills, mut auto)) = sheets.get_mut(inv.entity) else {
        return;
    };
    let attacks: Vec<&str> = skills
        .0
        .keys()
        .filter_map(|key| defs.skills.get(key))
        .filter(|skill| matches!(skill.def.kind, SkillKind::Attack { .. }))
        .map(|skill| skill.def.name.as_str())
        .collect();
    let name_of = |key: &str| {
        defs.skills
            .get(key)
            .map_or(key.to_string(), |s| s.def.name.clone())
    };
    let current = match auto.0.as_deref() {
        Some(key) => name_of(key),
        None => format!(
            "the weapon in your hand, or your fists ({}) without one",
            name_of(BASIC_ATTACK)
        ),
    };
    let text = if inv.args.trim().is_empty() {
        format!(
            "Each round of a fight you attack with {current}. Your attacks: {}. \
             ('autoattack default' goes back to fighting with what you hold.)",
            attacks.join(", ")
        )
    } else if inv.args.trim().eq_ignore_ascii_case("default") {
        auto.0 = None;
        if let Err(err) = db.set_autoattack(actor.character_id, None) {
            error!("failed to save {}'s auto-attack: {err}", actor.name);
        }
        "You'll fight with the weapon in your hand, or your fists.".into()
    } else {
        match defs.find_known(skills, inv.args.trim()) {
            Some(skill) if matches!(skill.def.kind, SkillKind::Attack { .. }) => {
                let key = skill.key.clone();
                if let Err(err) = db.set_autoattack(actor.character_id, Some(&key)) {
                    error!("failed to save {}'s auto-attack: {err}", actor.name);
                }
                let weapon = match &skill.def.kind {
                    SkillKind::Attack { damage: None, .. } => {
                        " It needs a weapon for it in your hand; without one, you'll fight with \
                         what you hold, or brawl."
                            .to_string()
                    }
                    _ => String::new(),
                };
                auto.0 = Some(key);
                format!("You'll attack with {} each round.{weapon}", skill.def.name)
            }
            Some(skill) => format!("{} isn't an attack.", skill.def.name),
            None => format!("You don't know an attack called '{}'.", inv.args.trim()),
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// `stop`: forget queued actions.
fn stop(In(inv): In<Invocation>, mut players: PlayerQuery, mut queues: Query<&mut ActionQueue>) {
    let text = match queues.get_mut(inv.entity) {
        Ok(mut queue) if queue.len() > 0 => {
            let n = queue.len();
            queue.clear();
            format!("You put aside your plans ({n} forgotten).")
        }
        _ => "You have nothing planned.".into(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::Item;
    use crate::skills::{
        Cost, Effect, Lines, Skill, SkillDef, SkillKind, SkillState, SkillText, StrikeKind,
        Technique,
    };
    use crate::vitals::BaseHealth;
    use crate::world::{ArmourDef, DoorState, Exits, ItemDef, ItemKey, Room, Slot, Terrain};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;

    /// A skill whose text is made from its id: "You cleave {target}.", "{actor} cleaves you."
    fn skill(key: &str, kind: SkillKind) -> (String, Skill) {
        let (tree, id) = key.split_once(':').unwrap();
        let lines = |verb: &str| Lines {
            actor: format!("You {verb} {{target}}."),
            target: Some(format!("{{actor}} {verb}s you.")),
            room: format!("{{actor}} {verb}s {{target}}."),
            beyond: None,
        };
        let def = SkillDef {
            name: key.into(),
            description: key.into(),
            kind,
            text: Some(SkillText {
                hit: lines(id),
                miss: Some(lines(&format!("miss with a {id} at"))),
            }),
            max_level: 50,
            attribute: None,
            rarity: None,
            difficulty: 1.0,
            requires: None,
        };
        let skill = Skill {
            key: key.into(),
            tree: tree.into(),
            attribute: Attribute::Strength,
            def,
        };
        (key.into(), skill)
    }

    /// A world with one room and the skills these tests use: fists, a riposte with them, and a
    /// cleave made with them.
    fn world() -> (World, Entity) {
        let mut world = World::new();
        let defs = SkillDefs {
            skills: HashMap::from([
                skill(
                    BASIC_ATTACK,
                    SkillKind::Attack {
                        damage: Some((2, 2)),
                        cost: Cost::Free,
                    },
                ),
                skill(
                    "brawling:riposte",
                    SkillKind::Passive {
                        triggers: Vec::new(),
                        bonuses: Vec::new(),
                        counter: Some(BASIC_ATTACK.into()),
                    },
                ),
                skill(
                    "brawling:cleave",
                    SkillKind::Technique(Technique {
                        with: BASIC_ATTACK.into(),
                        cost: Cost::Free,
                        accuracy: 0,
                        damage: 0,
                        strike: StrikeKind::Hit,
                        effects: vec![Effect::Cleave],
                        opener: false,
                    }),
                ),
            ]),
            trees: HashMap::new(),
        };
        world.insert_resource(defs);
        world.insert_resource(ItemTemplates::default());
        world.insert_resource(Dice::seeded(7));
        world.insert_resource(Database::open(":memory:").unwrap());
        world.insert_resource(Time::<()>::default());
        world.insert_resource(CombatLog::default());
        world.insert_resource(Spawns::default());
        world.init_resource::<Messages<GameEvent>>();
        let room = world
            .spawn((
                Room {
                    name: "Arena".into(),
                    description: String::new(),
                    no_wander: false,
                    indoors: true,
                    terrain: Terrain::Paved,
                },
                RoomKey::parse("test:arena", ""),
                Exits::default(),
            ))
            .id();
        (world, room)
    }

    /// A mob fighter with this Strength (its punch) and Reaction (its dodge), knowing these
    /// skills. Every other attribute is 1.
    fn fighter(
        world: &mut World,
        room: Entity,
        strength: i32,
        reaction: i32,
        skills: &[&str],
    ) -> Entity {
        let attributes = Attributes::starting(
            |attribute| match attribute {
                Attribute::Strength => strength,
                Attribute::Reaction => reaction,
                _ => 1,
            },
            0,
        );
        let vitals = Vitals {
            health: 100.0,
            stamina: 100.0,
        };
        let skills = Skills(
            skills
                .iter()
                .map(|key| (key.to_string(), SkillState::new("Template")))
                .collect(),
        );
        let mob = Mob {
            key: MobKey::parse("test:mob", ""),
            name: "a brawler".into(),
            here: String::new(),
            description: String::new(),
        };
        world
            .spawn((
                mob,
                InRoom(room),
                attributes,
                vitals,
                skills,
                BaseHealth(100),
            ))
            .id()
    }

    fn said(world: &World, who: Entity, text: &str) -> bool {
        world.resource::<CombatLog>().0.iter().any(|out| match out {
            Outgoing::To(entity, line) => *entity == who && line.contains(text),
            _ => false,
        })
    }

    #[test]
    fn a_dodge_can_be_riposted() {
        let (mut world, room) = world();
        // The attacker can barely hit; the defender dodges almost everything.
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let defender = fighter(
            &mut world,
            room,
            1,
            1000,
            &[BASIC_ATTACK, "brawling:riposte"],
        );
        world.entity_mut(attacker).insert(Fighting(defender));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, defender, "You riposte"));
    }

    /// Put an armour piece on `who`, at `location`.
    fn armour(
        world: &mut World,
        id: &str,
        wear: Option<Slot>,
        armour: ArmourDef,
        at: ItemLocation,
    ) {
        let key = ItemKey::parse(id, "test");
        let def = ItemDef {
            wear,
            armour: Some(armour),
            ..ItemDef::named(id)
        };
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(key.clone(), def);
        world.spawn((
            Item {
                key,
                name: id.into(),
                id: None,
                state: DoorState::Open,
            },
            at,
        ));
    }

    /// Someone whose fists never miss.
    fn sure_hands(world: &mut World, who: Entity) {
        let mut skills = world.get_mut::<Skills>(who).unwrap();
        skills.0.get_mut(BASIC_ATTACK).unwrap().level = 1000;
    }

    #[test]
    fn armour_counts_where_it_belongs() {
        let (mut world, room) = world();
        let who = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let plate = ArmourDef {
            reduction: 4,
            penalty: 3,
            block: 0,
        };
        let shield = ArmourDef {
            reduction: 0,
            penalty: 1,
            block: 4,
        };
        armour(
            &mut world,
            "plate",
            Some(Slot::Body),
            plate,
            ItemLocation::Worn(who, Slot::Body),
        );
        armour(&mut world, "shield", None, shield, ItemLocation::Held(who));
        // Carried, not worn: a spare helm in hand counts for nothing.
        let helm = ArmourDef {
            reduction: 1,
            penalty: 1,
            block: 0,
        };
        armour(
            &mut world,
            "helm",
            Some(Slot::Head),
            helm,
            ItemLocation::Held(who),
        );
        let total = world
            .run_system_once(move |items: ItemQuery, templates: Res<ItemTemplates>| {
                strike::gear(who, &items, &templates)
            })
            .unwrap();
        assert_eq!(
            total,
            ArmourDef {
                reduction: 4,
                penalty: 4,
                block: 4
            }
        );
    }

    #[test]
    fn armour_can_turn_a_blow_to_nothing_and_a_point_can_find_the_gaps() {
        let (mut world, room) = world();
        world.resource_mut::<SkillDefs>().skills.extend([skill(
            "brawling:jab",
            SkillKind::Technique(Technique {
                with: BASIC_ATTACK.into(),
                cost: Cost::Free,
                accuracy: 0,
                damage: 0,
                strike: StrikeKind::Hit,
                effects: vec![Effect::Pierce(3)],
                opener: false,
            }),
        )]);
        // Fists do 2; a breastplate takes 4.
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "brawling:jab"]);
        let knight = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, attacker);
        let plate = ArmourDef {
            reduction: 4,
            penalty: 3,
            block: 0,
        };
        armour(
            &mut world,
            "plate",
            Some(Slot::Body),
            plate,
            ItemLocation::Worn(knight, Slot::Body),
        );
        world.entity_mut(attacker).insert(Fighting(knight));
        world.entity_mut(knight).insert(Fighting(attacker));
        world.get_mut::<Skills>(knight).unwrap().0.clear();
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, attacker, "glances off the brawler's armour"));
        assert_eq!(world.get::<Vitals>(knight).unwrap().health, 100.0);
        // Piercing 3 of its 4 lets 1 through.
        world.entity_mut(attacker).insert(Readied {
            skill: "brawling:jab".into(),
        });
        world.run_system_once(combat_round).unwrap();
        assert_eq!(world.get::<Vitals>(knight).unwrap().health, 99.0);
    }

    #[test]
    fn a_shield_takes_the_credit_for_what_it_stops() {
        let (mut world, room) = world();
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        // Reaction 1 and the shield's penalty of 1 leave no dodging at all: every miss is the
        // shield's.
        let defender = fighter(&mut world, room, 1, 1, &[]);
        let shield = ArmourDef {
            reduction: 0,
            penalty: 1,
            block: 50,
        };
        armour(
            &mut world,
            "shield",
            None,
            shield,
            ItemLocation::Held(defender),
        );
        world.entity_mut(attacker).insert(Fighting(defender));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, defender, "You get your shield in the way"));
        assert!(said(
            &world,
            attacker,
            "gets a shield in the way of your attack"
        ));
        let events = world.resource::<Messages<GameEvent>>();
        let mut cursor = events.get_cursor();
        let blocked = cursor
            .read(events)
            .filter(|e| matches!(e, GameEvent::Blocked { who } if *who == defender))
            .count();
        assert!(blocked >= 1);
    }

    #[test]
    fn a_cleave_carries_on_into_a_second_foe() {
        let (mut world, room) = world();
        // The cleaver can't miss, and hits hard.
        let cleaver = fighter(
            &mut world,
            room,
            1000,
            1,
            &[BASIC_ATTACK, "brawling:cleave"],
        );
        let first = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let second = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(cleaver).insert((
            Fighting(first),
            Readied {
                skill: "brawling:cleave".into(),
            },
        ));
        world.entity_mut(second).insert(Fighting(cleaver));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, cleaver, "carries on into"));
        assert!(world.get::<Vitals>(second).unwrap().health < 100.0);
    }

    #[test]
    fn mobs_make_their_techniques_by_their_tactics() {
        for (tactics, cleaves) in [(100, true), (0, false)] {
            let (mut world, room) = world();
            let cleaver = fighter(
                &mut world,
                room,
                1000,
                1,
                &[BASIC_ATTACK, "brawling:cleave"],
            );
            let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
            world
                .entity_mut(cleaver)
                .insert((Fighting(foe), Tactics(tactics)));
            world.run_system_once(combat_round).unwrap();
            assert_eq!(said(&world, foe, "cleaves"), cleaves, "tactics {tactics}");
        }
    }

    #[test]
    fn players_know_when_they_are_outmatched() {
        let (mut world, room) = world();
        let player = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        let giant = fighter(&mut world, room, 8, 8, &[BASIC_ATTACK]);
        let equal = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        let challenger = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        for who in [player, challenger] {
            world.entity_mut(who).insert(Player {
                name: "Grok".into(),
                character_id: 1,
                account: "grok".into(),
                role: crate::db::Role::Player,
                race: "human".into(),
                background: String::new(),
                description: String::new(),
            });
        }
        world.entity_mut(player).insert(Fighting(giant));
        world.entity_mut(challenger).insert(Fighting(equal));
        world.run_system_once(combat_round).unwrap();
        // Attack and defence 2 against the giant's 8: four times outmatched. An equal foe is
        // even.
        let odds = |who| world.get::<Outmatched>(who).map(|o| o.0);
        assert!(
            odds(player).is_some_and(|o| (o - 4.0).abs() < 1e-9),
            "{:?}",
            odds(player)
        );
        assert!(odds(challenger).is_some_and(|o| (o - 1.0).abs() < 1e-9));
        // Mobs don't learn, so they aren't counted.
        assert!(odds(giant).is_none());
        assert_eq!(crate::skills::tough_fight_boost(odds(player)), 2.0);
        assert_eq!(crate::skills::tough_fight_boost(odds(challenger)), 1.0);
        assert_eq!(crate::skills::tough_fight_boost(Some(0.5)), 1.0);
    }

    #[test]
    fn names_read_naturally() {
        assert_eq!(the("a sewer rat"), "the sewer rat");
        assert_eq!(the("an old wolf"), "the old wolf");
        assert_eq!(the("Grok"), "Grok");
        assert_eq!(the("the drill sergeant"), "the drill sergeant");
    }
}
