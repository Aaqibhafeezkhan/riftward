//! Healing and consumables: eating and drinking what you carry, and First Aid with bandages.
//!
//! Food, drink and draughts are items with a `consumable` section: `eat` or `drink` uses one up,
//! and it does what its effects say to whoever took it in (a draught heals; food and drink put on
//! `nourished`, which speeds recovery for a while). `bandage [<someone>]` is First Aid's command:
//! it uses up a bandage you carry, and, if your skill is up to how hurt they are, heals them and
//! stops their bleeding. All three are actions, so in a fight each takes your turn.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::combat::Rounds;
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::{Database, Role};
use crate::events::GameEvent;
use crate::items::{self, Item, ItemLocation, Snapshot};
use crate::output::{Actor, Characters, Outbox, Outgoing};
use crate::skills::Skills;
use crate::targeting::Keywords;
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

/// What eating and drinking need: who's here and what they carry, and a round to carry out
/// what it does.
#[derive(SystemParam)]
struct Pantry<'w, 's> {
    commands: Commands<'w, 's>,
    players: Characters<'w, 's>,
    keywords: Query<'w, 's, &'static Keywords>,
    rounds: Rounds<'w, 's>,
}

/// `eat <item>`.
fn eat(In(inv): In<Invocation>, pantry: Pantry) {
    consume(inv, Consume::Eat, pantry);
}

/// `drink <item>`.
fn drink(In(inv): In<Invocation>, pantry: Pantry) {
    consume(inv, Consume::Drink, pantry);
}

/// Eat or drink something you carry: it's used up, and does what it does to you (its
/// `consumable`'s effects).
fn consume(inv: Invocation, how: Consume, mut p: Pantry) {
    let Some(actor) = Actor::get(&p.players, inv.entity) else {
        return;
    };
    let (verb, verbs) = match how {
        Consume::Eat => ("eat", "eats"),
        Consume::Drink => ("drink", "drinks"),
    };
    let found = (|| {
        if inv.args.trim().is_empty() {
            return Err(inv.usage_error());
        }
        let r = &p.rounds;
        let entity = items::find_item(&inv.args, &actor, &r.items, &p.keywords, &r.templates)?;
        let snapshot = Snapshot::new(r.items.iter().map(|(e, i, l)| (e, i, *l)), &r.templates);
        let (_, item, _) = r.items.get(entity).map_err(|_| String::new())?;
        if snapshot.root(entity).and_then(ItemLocation::holder) != Some(actor.entity) {
            return Err(format!("You'd have to pick up {} first.", item.name));
        }
        let food = r
            .templates
            .0
            .get(&item.key)
            .and_then(|def| def.consumable.clone())
            .filter(|food| food.by == how)
            .ok_or_else(|| format!("You can't {verb} {}.", item.name))?;
        Ok((entity, item.name.clone(), food))
    })();
    let (entity, name, food) = match found {
        Ok(found) => found,
        Err(text) => {
            if !text.is_empty() {
                p.rounds.log.0.push(Outgoing::line(actor.entity, text));
            }
            return;
        }
    };
    p.rounds.now(|round| {
        round.take_in(actor.entity, &food.effects, |healed| match healed {
            0 => format!("You {verb} {name}."),
            n => format!("You {verb} {name}. (+{n} health)"),
        });
    });
    if let Ok((_, item, _)) = p.rounds.items.get(entity) {
        used_up(&mut p.commands, &p.rounds.db, entity, item);
    }
    p.rounds.events.write(GameEvent::Consumed {
        who: actor.entity,
        drink: how == Consume::Drink,
    });
    p.rounds.log.0.push(Outgoing::Room {
        room: actor.room,
        except: actor.entity,
        text: format!("{} {verbs} {name}.\r\n", actor.name),
    });
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
