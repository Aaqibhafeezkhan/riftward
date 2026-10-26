//! Healing and consumables: eating and drinking what you carry, and First Aid with bandages.
//!
//! Food, drink and draughts are items with a `consumable` section: `eat` or `drink` uses one up.
//! A draught gives back health or stamina at once; food and drink only speed recovery for a
//! while. `bandage [<someone>]` is First Aid's command:
//! it uses up a bandage you carry, and, if your skill is up to how hurt they are, heals them and
//! stops their bleeding. All three are actions, so in a fight each takes your turn.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::combat::{Conditions, State};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Database;
use crate::dice::Dice;
use crate::items::{self, Item, ItemLocation, ItemQuery, ItemTemplates, Snapshot, capitalized};
use crate::mobs::{self, MobQuery};
use crate::output::{Actor, Outgoing, PlayerQuery, deliver, targets_in_room};
use crate::skills::{Learner, Practice, SkillDefs, Skills, fill};
use crate::targeting::{Keywords, Target};
use crate::vitals::{BaseHealth, Maxima, Nourished, Vitals};
use crate::world::{Consume, Player};

/// The skill `bandage` uses.
const FIRST_AID: &str = "healing:first_aid";
/// The tag that makes an item a bandage.
const BANDAGE: &str = "bandage";

pub struct HealingPlugin;

impl Plugin for HealingPlugin {
    fn build(&self, app: &mut App) {
        app.add_command(
            CommandSpec::new("eat", Category::Interaction)
                .usage("eat <item>")
                .summary("eat something you carry")
                .action(),
            eat,
        )
        .add_command(
            CommandSpec::new("drink", Category::Interaction)
                .shortcuts(&["quaff"])
                .usage("drink <item>")
                .summary("drink something you carry, like a healing draught")
                .action(),
            drink,
        )
        .add_command(
            CommandSpec::new("bandage", Category::Interaction)
                .usage("bandage [<someone>]")
                .summary("bind wounds with a bandage you carry, if you know First Aid")
                .action(),
            bandage,
        );
    }
}

/// What eating and drinking need.
#[derive(SystemParam)]
struct Pantry<'w, 's> {
    commands: Commands<'w, 's>,
    players: PlayerQuery<'w, 's>,
    items: ItemQuery<'w, 's>,
    keywords: Query<'w, 's, &'static Keywords>,
    templates: Res<'w, ItemTemplates>,
    sheets: Query<
        'w,
        's,
        (
            &'static mut Vitals,
            &'static Attributes,
            &'static Skills,
            Option<&'static BaseHealth>,
        ),
    >,
    defs: Res<'w, SkillDefs>,
    dice: ResMut<'w, Dice>,
    db: Res<'w, Database>,
    time: Res<'w, Time>,
    fed: Query<'w, 's, &'static Nourished>,
}

/// `eat <item>`.
fn eat(In(inv): In<Invocation>, pantry: Pantry) {
    consume(inv, Consume::Eat, pantry);
}

/// `drink <item>`.
fn drink(In(inv): In<Invocation>, pantry: Pantry) {
    consume(inv, Consume::Drink, pantry);
}

/// Eat or drink something you carry: it's used up, and gives back what it rolls.
fn consume(inv: Invocation, how: Consume, mut p: Pantry) {
    let Some(actor) = Actor::get(&p.players, inv.entity) else {
        return;
    };
    let (verb, verbs) = match how {
        Consume::Eat => ("eat", "eats"),
        Consume::Drink => ("drink", "drinks"),
    };
    let mut out = Vec::new();
    let reply = (|| {
        if inv.args.trim().is_empty() {
            return Err(inv.usage_error());
        }
        let entity = items::find_item(&inv.args, &actor, &p.items, &p.keywords, &p.templates)?;
        let snapshot = Snapshot::new(p.items.iter().map(|(e, i, l)| (e, i, *l)), &p.templates);
        let (_, item, _) = p.items.get(entity).map_err(|_| String::new())?;
        if snapshot.root(entity).and_then(ItemLocation::holder) != Some(actor.entity) {
            return Err(format!("You'd have to pick up {} first.", item.name));
        }
        let Some(food) = p
            .templates
            .0
            .get(&item.key)
            .and_then(|def| def.consumable)
            .filter(|food| food.by == how)
        else {
            return Err(format!("You can't {verb} {}.", item.name));
        };
        let (mut vitals, attributes, skills, base) =
            p.sheets.get_mut(actor.entity).map_err(|_| String::new())?;
        let luck = attributes.get(Attribute::Luck);
        let max = Maxima::of(attributes, skills, &p.defs, base);
        let mut roll = |range: Option<(u32, u32)>| {
            range.map_or(0, |(low, high)| {
                let (low, high) = (low.min(high), low.max(high));
                p.dice.range(luck, i64::from(low)..=i64::from(high)) as i32
            })
        };
        let (health, stamina) = (roll(food.health), roll(food.stamina));
        vitals.health += health as f32;
        vitals.stamina += stamina as f32;
        vitals.clamp(max);
        let gained: Vec<String> = [(health, "health"), (stamina, "stamina")]
            .into_iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("+{n} {what}"))
            .collect();
        // A meal: faster recovery for a while, the better of this and any meal still working.
        if let Some(meal) = food.recovery {
            let now = p.time.elapsed_secs_f64();
            let until = now + f64::from(meal.minutes) * 60.0;
            let fed = match p.fed.get(actor.entity) {
                Ok(fed) if fed.until > now => fed.with(meal.speed, until),
                _ => Nourished {
                    speed: meal.speed,
                    until,
                },
            };
            p.commands.entity(actor.entity).insert(fed);
        }
        let name = item.name.clone();
        used_up(&mut p.commands, &p.db, entity, item);
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} {verbs} {name}.\r\n", actor.name),
        });
        let mut text = if gained.is_empty() {
            format!("You {verb} {name}.")
        } else {
            format!("You {verb} {name}. ({})", gained.join(", "))
        };
        if food.recovery.is_some() {
            text += " You feel refreshed, and will recover faster for a while.";
        }
        Ok(text)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    deliver(out, &mut p.players);
}

/// An item used up: gone from the world, and from the database if it was real.
fn used_up(commands: &mut Commands, db: &Database, entity: Entity, item: &Item) {
    if let Some(id) = item.id
        && let Err(err) = db.delete_item(id)
    {
        error!("failed to delete used-up item {id}: {err}");
    }
    commands.entity(entity).despawn();
}

/// `bandage [<someone>]`: First Aid. Uses up a bandage you carry; whether it helps is your First
/// Aid (level plus Intuition) against half the health they've lost, by opposed roll.
/// If it does, it heals half your First Aid plus 1 to 3, and stops their bleeding.
#[allow(clippy::too_many_arguments)]
fn bandage(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    mobs: MobQuery,
    items: ItemQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    mut practice: Practice,
    mut conditions: Query<&mut Conditions>,
    bases: Query<&BaseHealth>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let mut gains = Vec::new();
    let reply = (|| {
        // Who: someone here, or yourself.
        let patient = if inv.args.trim().is_empty() {
            actor.entity
        } else {
            let target = Target::parse(inv.args.trim()).ok_or_else(|| inv.usage_error())?;
            let mut candidates = targets_in_room(actor.room, &players, &keywords);
            candidates.extend(mobs::targets_in(actor.room, &mobs, &keywords));
            target
                .find_one(actor.entity, candidates)
                .ok_or_else(|| format!("You don't see {} here.", target.describe()))?
        };
        let me = patient == actor.entity;
        let name = if me {
            "yourself".to_string()
        } else {
            players
                .get(patient)
                .ok()
                .and_then(|(_, _, p, _)| p.map(|p: &Player| p.name.clone()))
                .or_else(|| {
                    mobs.get(patient)
                        .ok()
                        .map(|(_, m, _)| crate::combat::the(&m.name))
                })
                .unwrap_or_else(|| "someone".into())
        };
        let (missing, bleeding) = {
            let (vitals, max) = practice
                .vitals(patient, bases.get(patient).ok())
                .ok_or_else(String::new)?;
            let bleeding = conditions
                .get(patient)
                .is_ok_and(|c| c.has(State::Bleeding(0)));
            (max.health as f32 - vitals.health, bleeding)
        };
        if missing < 1.0 && !bleeding {
            return Err(if me {
                "You aren't hurt.".to_string()
            } else {
                format!("{} isn't hurt.", capitalized(&name))
            });
        }
        // A bandage you carry.
        let snapshot = Snapshot::new(items.iter().map(|(e, i, l)| (e, i, *l)), &templates);
        let roll_of_cloth = items
            .iter()
            .find(|(entity, item, _)| {
                snapshot.root(*entity).and_then(ItemLocation::holder) == Some(actor.entity)
                    && templates
                        .0
                        .get(&item.key)
                        .is_some_and(|def| def.tags.iter().any(|t| t == BANDAGE))
            })
            .map(|(entity, item, _)| (entity, item))
            .ok_or("You have no bandages.")?;
        let ready = practice.begin(actor.entity, FIRST_AID)?;
        let skill = f64::from(ready.effectiveness.max(1));
        // Half the health they've lost is how hard the wound is to treat.
        let wound = f64::from((missing / 2.0).max(1.0));
        let helped = practice.opposed(skill, ready.luck, wound, 0).won;
        let healed = if helped {
            let healed = ready.effectiveness / 2 + practice.roll(ready.luck, 1..=3) as i32;
            if let Some((mut vitals, max)) = practice.vitals(patient, bases.get(patient).ok()) {
                vitals.health += healed as f32;
                vitals.clamp(max);
            }
            if let Ok(mut conditions) = conditions.get_mut(patient) {
                conditions.remove(State::Bleeding(0));
            }
            Some(healed)
        } else {
            None
        };
        used_up(&mut commands, &db, roll_of_cloth.0, roll_of_cloth.1);
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
        };
        gains = practice.exercise(actor.entity, learner, FIRST_AID);

        // Told in First Aid's words.
        let text = practice
            .defs
            .skills
            .get(FIRST_AID)
            .and_then(|skill| skill.def.text.clone())
            .ok_or_else(String::new)?;
        let lines = match (helped, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let healed = healed.map_or_else(String::new, |n| format!(" (+{n})"));
        // Everyone else here sees it; a patient who's a player is told their own way.
        let theirs = if me { "themselves" } else { name.as_str() };
        let seen = fill(&lines.room, &[("actor", &actor.name), ("target", theirs)]);
        for (watcher, _, _, at) in &players {
            if at.is_some_and(|at| at.0 == actor.room)
                && watcher != actor.entity
                && watcher != patient
            {
                out.push(Outgoing::line(watcher, seen.clone()));
            }
        }
        if !me && let Some(told) = &lines.target {
            out.push(Outgoing::line(
                patient,
                fill(told, &[("actor", &actor.name), ("target", "you")]) + &healed,
            ));
        }
        Ok(fill(&lines.actor, &[("actor", &actor.name), ("target", &name)]) + &healed)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    out.extend(
        gains
            .into_iter()
            .map(|line| Outgoing::line(actor.entity, line)),
    );
    deliver(out, &mut players);
}
