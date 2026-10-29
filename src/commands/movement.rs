//! Looking around and moving between rooms.

use std::collections::HashSet;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::attributes::Attribute;
use crate::characters::{Races, article};
use crate::combat::{ConditionDefs, Conditions};
use crate::combat::{Fighting, the};
use crate::command::Invocation;
use crate::commands::doors::{DoorLookup, find_door};
use crate::db::{Database, Permission};
use crate::events::{GameEvent, Way, way_back};
use crate::items::{
    ItemLocation, ItemQuery, ItemTemplates, capitalized, counted, describe_item, find_item,
    gear_lines,
};
use crate::items::{Snapshot, bulk_limits, coloured, counted_with};
use crate::mobs::{self, MobQuery};
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver, targets_in_room};
use crate::skills::{Learner, Practice, fill};
use crate::stealth::{STEALTH, Sneaking, hide_as, stealth_rating, unhide, visible};
use crate::targeting::{Keywords, Target};
use crate::vitals::Resting;
use crate::world::{Door, DoorState, InRoom, Obstacle, Player};

/// Everything a room shows a player besides other players: the room itself, its doors, and the
/// items lying in it.
#[derive(SystemParam)]
pub struct View<'w, 's> {
    pub rooms: RoomQuery<'w, 's>,
    pub doors: Query<'w, 's, &'static Door>,
    pub items: ItemQuery<'w, 's>,
    pub templates: Res<'w, ItemTemplates>,
    pub mobs: MobQuery<'w, 's>,
    /// Who is hidden, and from whom: their conditions.
    pub hidden: Query<'w, 's, &'static Conditions>,
    pub sneaking: Query<'w, 's, (), With<Sneaking>>,
    /// What's on everyone, for what stops them moving and what others see of them.
    pub conditions: Query<'w, 's, &'static Conditions>,
    pub condition_defs: Res<'w, ConditionDefs>,
    /// How rooms are lit, and which something lights now.
    pub room_light: Query<'w, 's, &'static crate::light::RoomLight>,
    pub lit: Res<'w, crate::light::LitRooms>,
}

impl View<'_, '_> {
    /// Whether `room` is too dark for `viewer` to see in right now: it needs light and has none,
    /// not even one they carry, and they can't see in the dark. Worked out afresh, so someone
    /// walking in sees it as it is.
    pub fn too_dark(&self, room: Entity, viewer: Entity) -> bool {
        let Ok((info, _, _)) = self.rooms.get(room) else {
            return false;
        };
        crate::light::Light::of(info, self.room_light.get(room).ok())
            .needs_light(crate::time::GameTime::now().phase())
            && !self.lit.0.contains(&room)
            && !crate::light::carries_light(viewer, &self.items, &self.templates)
            && !self
                .conditions
                .get(viewer)
                .is_ok_and(|c| c.sees_in_dark(&self.condition_defs))
    }

    /// What others see of `who`'s conditions, one hint a line: "A faint shimmer surrounds Grok."
    pub fn hints(&self, who: Entity, name: &str) -> String {
        self.conditions
            .get(who)
            .map(|c| c.hints(&self.condition_defs, name))
            .unwrap_or_default()
            .into_iter()
            .map(|hint| hint + "\r\n")
            .collect()
    }
}

/// `look` describes the room; `look <target>` looks along an exit, at a door, at someone here, or
/// at an item here or in your pack.
#[allow(clippy::too_many_arguments)]
pub fn look(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    view: View,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    races: Res<Races>,
    mut events: MessageWriter<GameEvent>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let lookup = view
        .rooms
        .get(actor.room)
        .ok()
        .filter(|_| !inv.args.is_empty())
        .map(|(_, _, exits)| {
            let lookup = find_door(&inv.args, exits, |e| keywords.get(e).ok());
            (lookup, exits)
        });
    let text = match (Target::parse(&inv.args), lookup) {
        (None, _) => describe_room(
            actor.room,
            actor.entity,
            &players,
            &view,
            actor.role.can(Permission::Stage),
        ),
        (_, Some((DoorLookup::Door { door, dir }, exits))) => match view.doors.get(door) {
            Ok(door) if door.is_open() => format!(
                "Through the open {} {} you see {}",
                door.the().trim_start_matches("the "),
                toward(&dir),
                glimpse(exits.0[&dir].to, actor.entity, &players, &view)
            ),
            Ok(door) => format!("You see {} {}. It is closed.\r\n", door.name, toward(&dir)),
            Err(_) => String::new(),
        },
        (_, Some((DoorLookup::NoDoor { dir }, exits))) => {
            let mut there = toward(&dir);
            there[..1].make_ascii_uppercase();
            format!(
                "{there} you see {}",
                glimpse(exits.0[&dir].to, actor.entity, &players, &view)
            )
        }
        (_, Some((DoorLookup::NotHere, _))) if is_direction(&inv.args.to_lowercase()) => {
            let dir = full_direction(&inv.args.to_lowercase()).to_string();
            format!("You see nothing special {}.\r\n", toward(&dir))
        }
        (Some(target), _) => {
            let mut candidates = targets_in_room(actor.room, &players, &keywords);
            candidates.extend(mobs::targets_in(actor.room, &view.mobs, &keywords));
            let candidates = visible(actor.entity, candidates, &view.hidden);
            match target.find_one(actor.entity, candidates).map(|e| {
                (
                    e,
                    players.get(e).ok().and_then(|p| p.1),
                    view.mobs.get(e).ok(),
                )
            }) {
                Some((e, Some(me), _)) if e == actor.entity => {
                    format!("You look yourself over. {}", describe_player(me, &races))
                }
                Some((e, Some(other), _)) => {
                    describe_player(other, &races)
                        + &view.hints(e, &other.name)
                        + &gear_lines(e, &view.items, &other.name)
                }
                Some((e, _, Some((_, mob, _)))) => {
                    mobs::describe(mob, &races)
                        + "\r\n"
                        + &view.hints(e, &the(&mob.name))
                        + &gear_lines(e, &view.items, &the(&mob.name))
                }
                _ => match find_item(&inv.args, &actor, &view.items, &keywords, &templates) {
                    Ok(item) => {
                        events.write_batch(crate::items::examined(actor.entity, item, &view.items));
                        describe_item(item, &view.items, &templates) + "\r\n"
                    }
                    Err(err) => err + "\r\n",
                },
            }
        }
    };
    deliver(vec![Outgoing::To(actor.entity, text)], &mut outbox);
}

/// `go <direction>`.
#[allow(clippy::too_many_arguments)]
pub fn go(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    view: View,
    db: Res<Database>,
    fighting: Query<(), With<Fighting>>,
    mut commands: Commands,
    mut practice: Practice,
) {
    commands.entity(inv.entity).remove::<Resting>();
    let out = if inv.args.is_empty() {
        vec![Outgoing::line(inv.entity, "Go where?")]
    } else if fighting.get(inv.entity).is_ok() {
        vec![Outgoing::line(inv.entity, IN_A_FIGHT)]
    } else {
        travel(
            inv.entity,
            &inv.args,
            &players,
            &view,
            &db,
            &mut practice,
            &mut commands,
            true,
        )
    };
    deliver(out, &mut outbox);
}

/// `north`, `south`, ... and their short forms: the command's name is the direction.
#[allow(clippy::too_many_arguments)]
pub fn walk(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    view: View,
    db: Res<Database>,
    fighting: Query<(), With<Fighting>>,
    mut commands: Commands,
    mut practice: Practice,
) {
    commands.entity(inv.entity).remove::<Resting>();
    if fighting.get(inv.entity).is_ok() {
        deliver(vec![Outgoing::line(inv.entity, IN_A_FIGHT)], &mut outbox);
        return;
    }
    let out = travel(
        inv.entity,
        inv.command,
        &players,
        &view,
        &db,
        &mut practice,
        &mut commands,
        true,
    );
    deliver(out, &mut outbox);
}

/// What someone in a fight is told when they try to walk away.
const IN_A_FIGHT: &str = "You're fighting for your life! Type 'flee' to try to escape.";

/// Move someone through the exit in direction `dir`, telling both rooms. Walking into a room costs
/// the stamina its terrain asks (doubled, and never free, for someone carrying too much); without
/// it they can't go. An exit to climb or swim has to be crossed first (see [`cross`]). `pay` is
/// false when something else has already paid, as fleeing does.
#[allow(clippy::too_many_arguments)]
pub fn travel(
    entity: Entity,
    dir: &str,
    players: &Characters,
    view: &View,
    db: &Database,
    practice: &mut Practice,
    commands: &mut Commands,
    pay: bool,
) -> Vec<Outgoing> {
    let mut out = Vec::new();
    let Some(actor) = Actor::get(players, entity) else {
        return out;
    };
    // A rooted character goes nowhere.
    if view
        .conditions
        .get(entity)
        .is_ok_and(|c| !c.can_move(&view.condition_defs))
    {
        out.push(Outgoing::line(entity, "You can't move!"));
        return out;
    }
    let dir = full_direction(&dir.to_ascii_lowercase()).to_string();
    let Some(&exit) = view
        .rooms
        .get(actor.room)
        .ok()
        .and_then(|(_, _, exits)| exits.0.get(&dir))
    else {
        out.push(Outgoing::line(entity, "You can't go that way."));
        return out;
    };
    if let Some(door) = exit.door.and_then(|d| view.doors.get(d).ok())
        && !door.is_open()
    {
        out.push(Outgoing::line(
            entity,
            format!("{} is closed.", door.the_capitalized()),
        ));
        return out;
    }
    let to = exit.to;
    if let Some(needs) = exit.needs
        && !cross(&actor, needs, &dir, practice, &mut out)
    {
        return out;
    }

    if pay
        && let Ok(terrain) = view.rooms.get(to).map(|(room, _, _)| room.terrain)
        && let Some((_, mut vitals, attributes)) = practice.sheet(entity)
    {
        let snapshot = Snapshot::new(
            view.items.iter().map(|(e, i, l)| (e, i, *l)),
            &view.templates,
        );
        let (encumbered_at, _) = bulk_limits(attributes.get(Attribute::Strength));
        let mut cost = terrain.cost();
        if snapshot.burden(entity) > encumbered_at {
            cost = (cost * 2.0).max(0.5);
        }
        if cost > 0.0 {
            if vitals.stamina < cost {
                out.push(Outgoing::line(
                    entity,
                    format!(
                        "You're too exhausted {}. Rest a while.",
                        terrain.too_tired()
                    ),
                ));
                return out;
            }
            vitals.stamina -= cost;
            practice.events.write(GameEvent::StaminaSpent {
                who: entity,
                amount: cost,
            });
        }
    }

    // Sneaking, they stay hidden: only those who'd noticed them see them go, and only those in
    // the next room who notice them see them come. Walking openly shows them.
    let (left_seen_by, entered_seen_by) = if view.sneaking.get(entity).is_ok() {
        sneak_through(&actor, to, players, view, practice, commands, &mut out)
    } else {
        if view.hidden.get(entity).is_ok_and(|c| c.hidden().is_some()) {
            unhide(commands, entity);
        }
        (None, None)
    };

    practice.events.write(GameEvent::Left {
        who: entity,
        name: actor.name.clone(),
        room: actor.room,
        way: Way::Walked(Some(dir)),
        witnesses: left_seen_by,
    });
    move_player(
        entity,
        actor.character_id,
        to,
        commands,
        players,
        view,
        db,
        &mut out,
    );
    practice.events.write(GameEvent::Entered {
        who: entity,
        name: actor.name.clone(),
        room: to,
        way: Way::Walked(way_back(&view.rooms, to, actor.room)),
        witnesses: entered_seen_by,
    });
    out
}

/// Sneak into `to`: everyone there rolls their Intuition against the sneaker's Stealth, and the
/// sneaker arrives hidden from those who failed. Practises Stealth. Returns who saw them leave
/// (whoever had noticed them, or everyone if they weren't hidden) and who saw them arrive.
#[allow(clippy::type_complexity)]
fn sneak_through(
    actor: &Actor,
    to: Entity,
    players: &Characters,
    view: &View,
    practice: &mut Practice,
    commands: &mut Commands,
    out: &mut Vec<Outgoing>,
) -> (Option<Vec<Entity>>, Option<Vec<Entity>>) {
    let Some((skills, _, attributes)) = practice.sheet(actor.entity) else {
        return (None, None);
    };
    let (stealth, luck) = (
        stealth_rating(&skills, &attributes),
        attributes.get(Attribute::Luck),
    );
    let left = view
        .hidden
        .get(actor.entity)
        .ok()
        .and_then(Conditions::hidden)
        .map(|h| h.seen_by.iter().copied().collect());
    let watchers: Vec<Entity> = players
        .iter()
        .filter(|(_, p, _, r)| p.is_some() && r.0 == to)
        .map(|(e, ..)| e)
        .chain(
            view.mobs
                .iter()
                .filter(|(_, _, r)| r.0 == to)
                .map(|(e, ..)| e),
        )
        .filter(|e| *e != actor.entity)
        .collect();
    let mut seen_by = HashSet::new();
    for watcher in watchers {
        let Some((_, _, attributes)) = practice.sheet(watcher) else {
            continue;
        };
        let (intuition, their_luck) = (
            f64::from(attributes.get(Attribute::Intuition).max(1)),
            attributes.get(Attribute::Luck),
        );
        if practice.opposed(intuition, their_luck, stealth, luck).won {
            seen_by.insert(watcher);
        }
    }
    let entered = seen_by.iter().copied().collect();
    hide_as(commands, actor.entity, stealth.round() as i32, seen_by);
    let learner = Learner {
        character_id: actor.character_id,
        race: &actor.race,
        boost: 1.0,
        fighting: false,
    };
    for line in practice.exercise(actor.entity, learner, STEALTH) {
        out.push(Outgoing::line(actor.entity, line));
    }
    (left, Some(entered))
}

/// The skill that climbs, and the one that swims.
const CLIMBING: &str = "athletics:climbing";
const SWIMMING: &str = "athletics:swimming";

/// Try to climb or swim through an exit: Climbing plus Strength, or Swimming plus Body, against
/// its difficulty, by the ratio rule. Anyone can try on their attribute alone; only the trained
/// practise. A fall hurts (never to death) and a flounder tires, and either way they stay put.
/// Told in the skill's words. Returns whether they made it.
fn cross(
    actor: &Actor,
    needs: Obstacle,
    dir: &str,
    practice: &mut Practice,
    out: &mut Vec<Outgoing>,
) -> bool {
    let (key, attribute, difficulty) = match needs {
        Obstacle::Climb(d) => (CLIMBING, Attribute::Strength, d),
        Obstacle::Swim(d) => (SWIMMING, Attribute::Body, d),
    };
    let Some((skills, _, attributes)) = practice.sheet(actor.entity) else {
        return false;
    };
    let known = skills.0.contains_key(key);
    let rating = skills.0.get(key).map_or(0, |s| s.level as i32) + attributes.get(attribute);
    let luck = attributes.get(Attribute::Luck);
    let rating = f64::from(rating.max(1));
    let made_it = practice
        .opposed(rating, luck, f64::from(difficulty.max(1)), 0)
        .won;

    // What a failure costs.
    let mut cost = String::new();
    if !made_it {
        let hurt = match needs {
            Obstacle::Climb(d) => practice.roll(0, 1..=i64::from((d / 2).max(1))) as f32,
            Obstacle::Swim(_) => 0.0,
        };
        let tired = match needs {
            Obstacle::Swim(d) => (d / 2).max(1) as f32,
            Obstacle::Climb(_) => 0.0,
        };
        if let Some((_, mut vitals, _)) = practice.sheet(actor.entity) {
            vitals.health = (vitals.health - hurt).max(1.0);
            vitals.stamina = (vitals.stamina - tired).max(0.0);
        }
        if hurt > 0.0 {
            cost = format!(" ({hurt})");
            practice.events.write(GameEvent::Damaged {
                who: actor.entity,
                amount: hurt,
            });
        }
        if tired > 0.0 {
            practice.events.write(GameEvent::StaminaSpent {
                who: actor.entity,
                amount: tired,
            });
        }
    }
    let gains = if known {
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
            fighting: false,
        };
        practice.exercise(actor.entity, learner, key)
    } else {
        Vec::new()
    };

    // A success is told to them alone, since leaving tells the room; a failure to everyone.
    if let Some(text) = practice
        .defs
        .skills
        .get(key)
        .and_then(|s| s.def.text.clone())
    {
        let lines = match (made_it, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let with = [("actor", actor.name.as_str()), ("direction", dir)];
        out.push(Outgoing::line(
            actor.entity,
            fill(&lines.actor, &with) + &cost,
        ));
        if !made_it {
            out.push(Outgoing::Room {
                room: actor.room,
                except: actor.entity,
                text: fill(&lines.room, &with) + "\r\n",
            });
        }
    }
    out.extend(
        gains
            .into_iter()
            .map(|line| Outgoing::line(actor.entity, line)),
    );
    made_it
}

/// What someone sees looking at a player: their race, and their own description.
fn describe_player(player: &Player, races: &Races) -> String {
    let race = races.name(&player.race).to_lowercase();
    let mut text = format!("{} is {} {race}.\r\n", player.name, article(&race));
    if !player.description.is_empty() {
        text += &format!("{}\r\n", player.description);
    }
    text
}

/// Where a direction is, for the middle of a sentence: "to the north", "above", "below".
pub fn toward(dir: &str) -> String {
    match dir {
        "up" => "above".into(),
        "down" => "below".into(),
        other => format!("to the {other}"),
    }
}

/// Whether a word names a compass or vertical direction, in full or short form.
pub fn is_direction(word: &str) -> bool {
    matches!(
        full_direction(word),
        "north"
            | "south"
            | "east"
            | "west"
            | "up"
            | "down"
            | "northeast"
            | "northwest"
            | "southeast"
            | "southwest"
    )
}

/// Expand direction shorthands (`n` -> `north`).
pub fn full_direction(dir: &str) -> &str {
    match dir {
        "n" => "north",
        "s" => "south",
        "e" => "east",
        "w" => "west",
        "u" => "up",
        "d" => "down",
        other => other,
    }
}

/// Put a character in a room, remember it in the database if they're a player, and show them
/// where they are. The move happens as the command finishes.
#[allow(clippy::too_many_arguments)]
pub fn move_player(
    entity: Entity,
    character_id: Option<i64>,
    to: Entity,
    commands: &mut Commands,
    players: &Characters,
    view: &View,
    db: &Database,
    out: &mut Vec<Outgoing>,
) {
    commands.entity(entity).insert(InRoom(to));
    let show_ids = players
        .get(entity)
        .is_ok_and(|(_, player, _, _)| player.is_some_and(|p| p.role.can(Permission::Stage)));
    if let (Some(character_id), Ok((_, key, _))) = (character_id, view.rooms.get(to))
        && let Err(err) = db.save_location(character_id, &key.to_string())
    {
        error!("failed to save location for character {character_id}: {err}");
    }
    out.push(Outgoing::To(
        entity,
        describe_room(to, entity, players, view, show_ids),
    ));
}

/// What you see looking into a neighbouring room: its name and who's there.
fn glimpse(room: Entity, viewer: Entity, players: &Characters, view: &View) -> String {
    let name = view
        .rooms
        .get(room)
        .map(|(room, _, _)| room.name.clone())
        .unwrap_or_default();
    let mut text = format!("{name}.\r\n");
    if view.too_dark(room, viewer) {
        return text + "  It's too dark to make out anyone there.\r\n";
    }
    // Everyone the viewer can see but themselves, player or mob: the light there is what counts.
    let shown =
        |who: Entity| who != viewer && view.hidden.get(who).ok().is_none_or(|c| c.seen_by(viewer));
    for name in mobs::names_in(room, &view.mobs, shown) {
        text += &format!("  {} is there.\r\n", capitalized(&name));
    }
    for (other, player, _, in_room) in players.iter() {
        if let Some(player) = player
            && other != viewer
            && in_room.0 == room
            && shown(other)
        {
            text += &format!("  {} is there.\r\n", player.name);
        }
    }
    text
}

/// The text shown by `look`. Builders also see the room's id.
pub fn describe_room(
    room: Entity,
    viewer: Entity,
    players: &Characters,
    view: &View,
    show_id: bool,
) -> String {
    let Ok((info, key, exits)) = view.rooms.get(room) else {
        return "You are nowhere.\r\n".into();
    };

    let mut text = format!("\x1b[1;36m{}\x1b[0m", info.name);
    if show_id {
        text += &format!(" \x1b[2m[{key}]\x1b[0m");
    }
    // In the dark, all they have is the way out.
    let dark = view.too_dark(room, viewer);
    if dark {
        text += "\r\nIt's too dark to see. You can feel your way to the exits.\r\n";
    } else {
        text += &format!("\r\n{}\r\n", info.description);
    }

    let exit_list: Vec<_> = exits
        .0
        .iter()
        .map(|(dir, exit)| {
            let state = exit
                .door
                .and_then(|d| view.doors.get(d).ok())
                .map(|d| d.state);
            match state {
                Some(DoorState::Closed | DoorState::Locked) => format!("{dir} (closed)"),
                _ => dir.clone(),
            }
        })
        .collect();
    text += &format!(
        "\x1b[33mExits: {}\x1b[0m\r\n",
        if exit_list.is_empty() {
            "none".into()
        } else {
            exit_list.join(", ")
        }
    );

    if dark {
        return text;
    }
    // Fixed things, like a chest or a signpost, are part of the room; loose things lie in it.
    let fixed = |entity: Entity| {
        view.items
            .get(entity)
            .ok()
            .and_then(|(_, item, _)| view.templates.0.get(&item.key))
            .is_some_and(|def| def.fixed)
    };
    let mut here: Vec<_> = view
        .items
        .iter()
        .filter(|(_, _, loc)| **loc == ItemLocation::Room(room))
        .map(|(entity, _, _)| entity)
        .collect();
    here.sort();
    let fixed_names: Vec<_> = here
        .iter()
        .filter(|e| fixed(**e))
        .filter_map(|e| {
            view.items
                .get(*e)
                .ok()
                .map(|(_, item, _)| item.name.as_str())
        })
        .collect();
    for name in counted(fixed_names) {
        text += &format!("{} is here.\r\n", capitalized(&name));
    }
    let loose: Vec<_> = here
        .iter()
        .filter(|e| !fixed(**e))
        .filter_map(|e| {
            view.items
                .get(*e)
                .ok()
                .map(|(_, item, _)| item.name.as_str())
        })
        .collect();
    // Names show in their rarity's colour.
    let paint = |name: &str, line: String| {
        let def = here.iter().find_map(|e| {
            let (_, item, _) = view.items.get(*e).ok()?;
            (item.name == name).then(|| view.templates.0.get(&item.key))?
        });
        coloured(&line, def)
    };
    for (name, line) in counted_with(loose) {
        text += &format!(
            "{}\r\n",
            paint(&name, format!("{} lies here.", capitalized(&line)))
        );
    }
    // Everyone the viewer can see but themselves, player or mob. It isn't dark here, whatever
    // their last room was like.
    let shown =
        |who: Entity| who != viewer && view.hidden.get(who).ok().is_none_or(|c| c.seen_by(viewer));
    for line in mobs::here_lines(room, &view.mobs, shown) {
        text += &format!("{line}\r\n");
    }

    for (other, player, _, in_room) in players.iter() {
        if let Some(player) = player
            && other != viewer
            && in_room.0 == room
            && shown(other)
        {
            text += &format!("{} is here.\r\n", player.name);
        }
    }
    text
}
