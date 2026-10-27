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
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::{Database, Role};
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::items::{self, Item, ItemLocation, ItemQuery, ItemTemplates, Snapshot};
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::skills::{SkillDefs, Skills};
use crate::targeting::Keywords;
use crate::vitals::{BaseHealth, Maxima, Nourished, Vitals};
use crate::world::Consume;

/// The skill `bandage` uses.
const FIRST_AID: &str = "healing:first_aid";

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
    players: Characters<'w, 's>,
    outbox: ResMut<'w, Outbox>,
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
    events: MessageWriter<'w, GameEvent>,
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
        p.events.write(GameEvent::Consumed {
            who: actor.entity,
            drink: how == Consume::Drink,
        });
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
    deliver(out, &mut p.outbox);
}

/// An item used up: gone from the world, and from the database if it was real.
pub fn used_up(commands: &mut Commands, db: &Database, entity: Entity, item: &Item) {
    if let Some(id) = item.id
        && let Err(err) = db.delete_item(id)
    {
        error!("failed to delete used-up item {id}: {err}");
    }
    commands.entity(entity).despawn();
}

/// `bandage [<someone>]`: First Aid, by its old name. It's an active skill made of effects
/// (`healing.skill.ron`), so this is `use first aid`.
fn bandage(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut outbox: ResMut<Outbox>,
    sheets: Query<&Skills>,
    registry: Res<CommandRegistry>,
) {
    if !sheets
        .get(inv.entity)
        .is_ok_and(|s| s.0.contains_key(FIRST_AID))
    {
        outbox.0.push(Outgoing::line(
            inv.entity,
            "You don't know First Aid. A healer could teach you.",
        ));
        return;
    }
    let Some((spec, id)) = registry.find("ability", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: format!("{FIRST_AID} {}", inv.args.trim()),
        },
    );
}
