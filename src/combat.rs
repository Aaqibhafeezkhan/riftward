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

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;

use crate::attributes::{Attribute, Attributes};
use crate::command::{ActionQueue, AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::commands::movement::{View, travel};
use crate::db::{Database, ItemPlace};
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::items::{
    self, ItemLocation, ItemMutQuery, ItemQuery, ItemTemplates, MOB_CORPSE_SECS,
    PLAYER_CORPSE_SECS, capitalized,
};
use crate::mobs::{self, Mob, MobQuery};
use crate::output::{Actor, Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::skills::{Bonus, Cost, SkillDefs, SkillKind, Skills, count_use};
use crate::spawns::{SpawnedBy, Spawns};
use crate::targeting::{Keywords, Target};
use crate::vitals::{Resting, Vitals};
use crate::world::{InRoom, MobKey, Player, RoomIndex, RoomKey, without_article};

/// Seconds in a combat round, and between a player's actions.
pub const ROUND_SECS: f64 = 3.0;
/// Everyone's most basic attack, used until they choose another.
pub const BASIC_ATTACK: &str = "brawling:basics";
/// The skill that helps avoid attacks.
const DODGE: &str = "athletics:dodge";
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
                    handle_deaths,
                    deliver_log,
                )
                    .chain(),
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
            );
    }
}

/// Who someone is fighting.
#[derive(Component, Debug, Clone, Copy)]
pub struct Fighting(pub Entity);

/// The attack someone makes each round: `None` means the basic attack.
#[derive(Component, Debug, Clone, Default)]
pub struct AutoAttack(pub Option<String>);

/// A mob's health, when its template sets one instead of the usual 8 + Body / 2.
#[derive(Component, Debug, Clone, Copy)]
pub struct MaxHealth(pub i32);

/// A mob that attacks players who come near.
#[derive(Component, Debug)]
pub struct Aggressive;

/// Killed this round, waiting for `handle_deaths`.
#[derive(Component, Debug)]
pub struct Dead;

/// What combat has to say, delivered after each round.
#[derive(Resource, Default)]
pub struct CombatLog(Vec<Outgoing>);

/// The most health someone can have.
pub fn max_health(attributes: &Attributes, override_: Option<&MaxHealth>) -> i32 {
    override_.map_or(attributes.health(), |m| m.0)
}

/// How hurt someone looks, from their health and their most.
pub fn condition(health: f32, max: i32) -> &'static str {
    match health / max.max(1) as f32 {
        f if f >= 1.0 => "unhurt",
        f if f >= 0.75 => "scratched",
        f if f >= 0.5 => "hurt",
        f if f >= 0.25 => "badly wounded",
        _ => "barely standing",
    }
}

/// "the sewer rat" for "a sewer rat"; names stay as they are.
pub fn the(name: &str) -> String {
    let bare = without_article(name);
    if bare.len() == name.trim().len() {
        name.to_string()
    } else {
        format!("the {bare}")
    }
}

/// "punches" for "punch", "slashes" for "slash".
fn third_person(verb: &str) -> String {
    if ["s", "x", "z", "sh", "ch"]
        .iter()
        .any(|end| verb.ends_with(end))
    {
        format!("{verb}es")
    } else {
        format!("{verb}s")
    }
}

/// The chance an attack hits: the attacker's skill against the defender's evasion, as a ratio.
pub fn hit_chance(attack: i32, defence: i32) -> f64 {
    let (attack, defence) = (f64::from(attack.max(1)), f64::from(defence.max(1)));
    attack / (attack + defence)
}

type Fighters<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static InRoom,
        &'static mut Attributes,
        &'static mut Vitals,
        &'static mut Skills,
        Option<&'static Fighting>,
        Option<&'static AutoAttack>,
        Option<&'static MaxHealth>,
        Option<&'static mut ActionQueue>,
        Has<Aggressive>,
        Has<Dead>,
        Option<&'static Player>,
        Option<&'static Mob>,
    ),
>;

/// A fighter's name, as others say it: "Grok", "the sewer rat".
fn name_of(player: Option<&Player>, mob: Option<&Mob>) -> String {
    match (player, mob) {
        (Some(player), _) => player.name.clone(),
        (_, Some(mob)) => the(&mob.name),
        _ => "someone".into(),
    }
}

/// The bonuses auras give right now, by zone and mob template: every living mob holding an aura
/// skill lends its bonus to the mobs it affects in its zone.
pub type Auras = HashMap<(String, MobKey), Bonus>;

fn auras(fighters: &Fighters, rooms: &RoomQuery, defs: &SkillDefs) -> Auras {
    let mut auras = Auras::new();
    for (_, in_room, _, _, skills, _, _, _, _, _, dead, _, mob) in fighters {
        if dead || mob.is_none() {
            continue;
        }
        let Ok((_, room, _)) = rooms.get(in_room.0) else {
            continue;
        };
        for key in skills.0.keys() {
            if let Some(SkillKind::Aura { affects, bonus }) =
                defs.skills.get(key).map(|s| &s.def.kind)
            {
                let affected = MobKey::parse(affects, &room.zone);
                *auras.entry((room.zone.clone(), affected)).or_default() += *bonus;
            }
        }
    }
    auras
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
    resting: Query<(), With<Resting>>,
    rooms: RoomQuery,
) {
    let now = time.elapsed_secs_f64();
    let auras = auras(&fighters, &rooms, &defs);
    // The aura a fighter fights under, if any.
    let aura_on = |entity: Entity, fighters: &Fighters| -> Bonus {
        let Ok((_, in_room, .., Some(mob))) = fighters.get(entity) else {
            return Bonus::default();
        };
        rooms
            .get(in_room.0)
            .ok()
            .and_then(|(_, room, _)| auras.get(&(room.zone.clone(), mob.key.clone())))
            .map_or_else(Bonus::default, |bonus: &Bonus| *bonus)
    };
    let mut targets: HashMap<Entity, Entity> = HashMap::new();
    let mut alive_players: Vec<(Entity, Entity)> = Vec::new();
    for (entity, in_room, _, _, _, fighting, _, _, _, _, dead, player, _) in &fighters {
        if dead {
            continue;
        }
        if let Some(Fighting(target)) = fighting {
            targets.insert(entity, *target);
        }
        if player.is_some() {
            alive_players.push((entity, in_room.0));
        }
    }

    // Aggressive mobs pick on a player in their room, but no more of a spawn's mobs gang up on
    // one player than its `pack` allows; the rest wait their turn.
    let spawn_of = |entity: Entity| spawned.get(entity).ok().map(|s| &s.0);
    for (entity, in_room, _, _, _, fighting, _, _, _, aggressive, dead, _, mob) in &fighters {
        if !aggressive || dead || fighting.is_some() {
            continue;
        }
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
            .find(|(victim, room)| *room == in_room.0 && room_for(*victim))
        else {
            continue;
        };
        targets.insert(entity, victim);
        commands.entity(entity).insert(Fighting(victim));
        let name = name_of(None, mob);
        log.0.push(Outgoing::line(
            victim,
            format!("\x1b[1;31m{} attacks you!\x1b[0m", capitalized(&name)),
        ));
    }

    // Fights end when the target is gone, dead or elsewhere; defenders fight back.
    let place = |entity: Entity| fighters.get(entity).ok().filter(|f| !f.10).map(|f| f.1.0);
    let pairs: Vec<(Entity, Entity)> = targets.iter().map(|(a, t)| (*a, *t)).collect();
    for (attacker, target) in pairs {
        if place(target).is_none() || place(target) != place(attacker) {
            targets.remove(&attacker);
            commands.entity(attacker).remove::<Fighting>();
        }
    }
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
            let (_, _, attributes, ..) = fighters.get(*entity).ok()?;
            let luck = attributes.get(Attribute::Luck);
            let roll = dice.range(luck, 1..=6);
            Some((i64::from(attributes.initiative()) + roll, *entity))
        })
        .collect();
    order.sort_by(|a, b| b.cmp(a));

    let mut fallen: HashSet<Entity> = HashSet::new();
    for (_, attacker) in order {
        let Some(&target) = targets.get(&attacker) else {
            continue;
        };
        if fallen.contains(&attacker) || fallen.contains(&target) {
            continue;
        }
        // Someone caught resting spends their first turn getting up.
        if resting.get(attacker).is_ok() {
            commands.entity(attacker).remove::<Resting>();
            log.0.push(Outgoing::line(
                attacker,
                "\x1b[1mYou scramble to your feet!\x1b[0m",
            ));
            continue;
        }
        // A player's queued action takes their turn.
        if let Ok((.., Some(mut queue), _, _, Some(_), _)) = fighters.get_mut(attacker)
            && let Some((id, invocation)) = queue.pop()
        {
            queue.next_at = now + ROUND_SECS;
            commands.run_system_with(id, invocation);
            continue;
        }
        let bonuses = (aura_on(attacker, &fighters), aura_on(target, &fighters));
        if let Some(died) = attack(
            attacker,
            target,
            bonuses,
            &mut fighters,
            &items,
            &templates,
            &defs,
            &mut dice,
            &db,
            &mut events,
            &mut log,
            &alive_players,
            resting.get(target).is_ok(),
        ) {
            fallen.insert(died);
            commands.entity(died).insert(Dead).remove::<Fighting>();
            for (who, whom) in &targets {
                if *whom == died {
                    commands.entity(*who).remove::<Fighting>();
                }
            }
        }
    }
}

/// The attack a fighter makes this round: which skill, how much damage, and the verb.
#[derive(Debug, Clone, PartialEq)]
pub struct Strike {
    /// The skill it's made with, and practises.
    pub skill: String,
    pub damage: (u32, u32),
    pub verb: String,
    /// Extra damage from training in the weapon.
    pub bonus: i32,
    /// Stamina it takes.
    pub cost: u32,
}

/// Levels of a weapon skill for each extra point of damage with that weapon.
const LEVELS_PER_DAMAGE: i32 = 10;

/// The attack a fighter makes this round: their chosen attack if they can make it now, otherwise
/// the weapon in their hand, otherwise the basic attack.
///
/// A weapon does its own damage in anyone's hands. Trained in its skill, a fighter attacks with
/// that skill and does a point more for every ten levels; untrained, they swing it with only
/// their Brawling to guide them, and learn nothing of the weapon by it. Players must know an attack
/// to make it; mobs fight with whatever their template gives them. An attack they haven't the
/// stamina for falls back to the next, ending at the free basic attack.
#[allow(clippy::too_many_arguments)]
fn choose_attack(
    entity: Entity,
    chosen: Option<&str>,
    is_player: bool,
    stamina: f32,
    skills: &Skills,
    items: &ItemQuery,
    templates: &ItemTemplates,
    defs: &SkillDefs,
) -> Option<Strike> {
    let weapon = items.iter().find_map(|(_, item, location)| {
        (*location == ItemLocation::Held(entity))
            .then(|| templates.0.get(&item.key)?.weapon.as_ref())
            .flatten()
    });
    let strike = |key: &str| {
        let skill = defs.skills.get(key)?;
        let SkillKind::Attack { verb, damage, cost } = &skill.def.kind else {
            return None;
        };
        if is_player && !skills.0.contains_key(key) {
            return None;
        }
        let cost = match cost {
            Cost::Free => 0,
            Cost::Stamina(n) => *n,
        };
        if stamina < cost as f32 {
            return None;
        }
        let level = skills.0.get(key).map_or(0, |s| s.level as i32);
        match weapon {
            Some(w) if w.skill == key => Some(Strike {
                skill: key.to_string(),
                damage: w.damage,
                verb: w.verb.clone().unwrap_or_else(|| verb.clone()),
                bonus: level / LEVELS_PER_DAMAGE,
                cost,
            }),
            _ => damage.map(|damage| Strike {
                skill: key.to_string(),
                damage,
                verb: verb.clone(),
                bonus: 0,
                cost,
            }),
        }
    };
    chosen
        .and_then(strike)
        .or_else(|| weapon.and_then(|w| strike(&w.skill)))
        .or_else(|| {
            let brawl = strike(BASIC_ATTACK)?;
            Some(match weapon {
                // Swung untrained: Brawling's aim, the weapon's damage.
                Some(w) => Strike {
                    damage: w.damage,
                    verb: "hit".into(),
                    ..brawl
                },
                None => brawl,
            })
        })
}

/// One attack. Returns who died, if anyone.
#[allow(clippy::too_many_arguments)]
fn attack(
    attacker: Entity,
    target: Entity,
    (a_bonus, d_bonus): (Bonus, Bonus),
    fighters: &mut Fighters,
    items: &ItemQuery,
    templates: &ItemTemplates,
    defs: &SkillDefs,
    dice: &mut Dice,
    db: &Database,
    events: &mut MessageWriter<GameEvent>,
    log: &mut CombatLog,
    watchers: &[(Entity, Entity)],
    caught_resting: bool,
) -> Option<Entity> {
    let (room, a_name, a_player, chosen, a_attributes, a_skills, a_stamina) = {
        let (_, in_room, attributes, vitals, skills, _, auto, _, _, _, _, player, mob) =
            fighters.get(attacker).ok()?;
        (
            in_room.0,
            name_of(player, mob),
            player.map(|p| p.character_id),
            auto.and_then(|a| a.0.clone()),
            attributes.clone(),
            skills.clone(),
            vitals.stamina,
        )
    };
    let Strike {
        skill: key,
        damage,
        verb,
        bonus,
        cost,
    } = choose_attack(
        attacker,
        chosen.as_deref(),
        a_player.is_some(),
        a_stamina,
        &a_skills,
        items,
        templates,
        defs,
    )?;
    let skill = defs.skills.get(&key)?;
    if cost > 0 {
        if let Ok((_, _, _, mut vitals, ..)) = fighters.get_mut(attacker) {
            vitals.stamina -= cost as f32;
        }
        events.write(GameEvent::StaminaSpent {
            who: attacker,
            amount: cost as f32,
        });
    }
    let level = a_skills.0.get(&key).map_or(0, |s| s.level as i32);
    let attack_rating = level + a_attributes.get(skill.attribute) + a_bonus.attack;

    let (d_name, d_attributes, d_skills, d_mob) = {
        let (_, _, attributes, _, skills, _, _, _, _, _, _, player, mob) =
            fighters.get(target).ok()?;
        (
            name_of(player, mob),
            attributes.clone(),
            skills.clone(),
            mob.map(|m| m.key.clone()),
        )
    };
    let dodge = d_skills.0.get(DODGE).map_or(0, |s| s.level as i32);
    let mut defence = d_attributes.get(Attribute::Reaction) + dodge + d_bonus.defence;
    // Someone sitting down is much easier to hit.
    if caught_resting {
        defence /= 2;
    }
    let (a_luck, d_luck) = (
        a_attributes.get(Attribute::Luck),
        d_attributes.get(Attribute::Luck),
    );
    let hit = dice.contest(a_luck, d_luck, hit_chance(attack_rating, defence));

    let mut died = None;
    let verbs = third_person(&verb);
    let mut lines: Vec<(Entity, String)> = Vec::new();
    let mut room_line: String;
    if hit {
        let (low, high) = (damage.0.min(damage.1), damage.0.max(damage.1));
        let rolled = dice.range(a_luck, i64::from(low)..=i64::from(high)) as i32;
        let dealt =
            (rolled + a_attributes.get(Attribute::Strength) / 2 + bonus - d_bonus.armour).max(1);
        lines.push((attacker, format!("You {verb} {d_name}. ({dealt})")));
        lines.push((
            target,
            format!(
                "\x1b[31m{} {verbs} you. ({dealt})\x1b[0m",
                capitalized(&a_name)
            ),
        ));
        room_line = format!("{} {verbs} {d_name}.", capitalized(&a_name));
        if let Ok((_, _, attributes, mut vitals, _, _, _, most, ..)) = fighters.get_mut(target) {
            vitals.health -= dealt as f32;
            if vitals.health <= 0.0 {
                vitals.health = 0.0;
                died = Some(target);
            } else {
                let looks = condition(vitals.health, max_health(&attributes, most));
                lines.push((attacker, format!("{} is {looks}.", capitalized(&d_name))));
            }
        }
    } else {
        lines.push((attacker, format!("You miss {d_name}.")));
        lines.push((
            target,
            format!("{} {verbs} at you, but misses.", capitalized(&a_name)),
        ));
        room_line = format!("{} misses {d_name}.", capitalized(&a_name));
        events.write(GameEvent::Dodged { who: target });
    }
    if died.is_some() {
        lines.push((attacker, format!("\x1b[1mYou have killed {d_name}!\x1b[0m")));
        room_line += &format!(" {} falls, dead.", capitalized(&d_name));
        events.write(GameEvent::Died {
            who: target,
            name: d_name.clone(),
            room,
            mob: d_mob,
        });
    }

    // Players practise their attack every round they make one.
    if let Some(character_id) = a_player
        && let Ok((_, _, mut attributes, _, mut skills, ..)) = fighters.get_mut(attacker)
    {
        for line in count_use(
            defs,
            &mut skills,
            &mut attributes,
            dice,
            db,
            character_id,
            &key,
        ) {
            lines.push((attacker, line));
        }
    }

    for (who, line) in lines {
        log.0.push(Outgoing::line(who, line));
    }
    // Everyone else in the room sees it too; the two fighters have already been told.
    for &(player, at) in watchers {
        if at == room && player != attacker && player != target {
            log.0.push(Outgoing::line(player, room_line.clone()));
        }
    }
    died
}

/// Lay out the dead: mobs leave a corpse, and players leave one with everything they carried,
/// then wake at the shrine.
#[allow(clippy::too_many_arguments)]
fn handle_deaths(
    mut commands: Commands,
    dead_mobs: Query<(Entity, &Mob, &InRoom), (With<Dead>, mobs::OnlyMobs)>,
    dead_players: Query<Entity, (With<Dead>, With<Player>)>,
    mut players: PlayerQuery,
    mut sheets: Query<(&mut Vitals, &Attributes, Option<&mut ActionQueue>)>,
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
    for (entity, mob, in_room) in &dead_mobs {
        items::spawn_corpse(
            &mut commands,
            &templates,
            &mob.name,
            None,
            in_room.0,
            now + MOB_CORPSE_SECS,
        );
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
        if let Ok((mut vitals, attributes, queue)) = sheets.get_mut(entity) {
            vitals.health = (attributes.health() as f32 / 4.0).max(1.0);
            vitals.stamina = attributes.stamina() as f32 / 4.0;
            if let Some(mut queue) = queue {
                queue.clear();
            }
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
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply = |text: &str| vec![Outgoing::line(inv.entity, text)];
    // Where they run, once they've paid for it and broken away.
    let mut escape = None;
    let mut out = if fighting.get(inv.entity).is_err() {
        reply("You aren't fighting anyone.")
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

    #[test]
    fn names_and_verbs_read_naturally() {
        assert_eq!(the("a sewer rat"), "the sewer rat");
        assert_eq!(the("an old wolf"), "the old wolf");
        assert_eq!(the("Grok"), "Grok");
        assert_eq!(the("the drill sergeant"), "the drill sergeant");
        assert_eq!(third_person("punch"), "punches");
        assert_eq!(third_person("bite"), "bites");
        assert_eq!(third_person("slash"), "slashes");
    }

    #[test]
    fn hits_follow_the_ratio_rule() {
        assert!((hit_chance(6, 3) - 2.0 / 3.0).abs() < 1e-9);
        assert!((hit_chance(20, 10) - hit_chance(40, 20)).abs() < 1e-9);
        assert!(hit_chance(0, 0) > 0.0, "nothing is impossible");
    }
}
