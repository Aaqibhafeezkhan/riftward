//! Shops: mobs who sell and buy. A mob template's `shop` lists what it always has for sale and
//! the kinds of thing it buys (see `ShopDef`). Next to a shopkeeper, `list` shows their goods,
//! `buy` and `sell` trade with them, and `value` asks what they'd pay.
//!
//! A shop sells at an item's value and buys at a third of it, and resells what it buys at full
//! value until someone buys it back (unless it stocks that anyway). What a shop has bought isn't
//! saved: a restart, or the shopkeeper's death, clears it. A character who knows Haggling strikes
//! a deal with each shopkeeper that moves both prices their way (see `trade.rs`).

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, ItemPlace};
use crate::events::{GameEvent, Got};
use crate::items::{
    self, HANDS, ItemLocation, ItemQuery, ItemTemplates, Quality, Snapshot, capitalized, worth,
};
use crate::mobs::{MobQuery, MobTemplates};
use crate::money::{self, Purse};
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::skills::Practice;
use crate::targeting::Keywords;
use crate::trade::{self, Bargains};
use crate::world::{Consume, ItemDef, ItemKey, ShopDef};

/// The most a shop resells at once; the oldest goes first.
const MAX_RESALE: usize = 20;

pub struct ShopsPlugin;

impl Plugin for ShopsPlugin {
    fn build(&self, app: &mut App) {
        app.add_command(
            CommandSpec::new("list", Category::Interaction)
                .summary("see what a shopkeeper here sells, and for how much")
                .quiet(),
            list,
        )
        .add_command(
            CommandSpec::new("buy", Category::Interaction)
                .usage("buy <item>")
                .summary("buy something from a shopkeeper here"),
            buy,
        )
        .add_command(
            CommandSpec::new("sell", Category::Interaction)
                .usage("sell <item>")
                .summary("sell something you carry to a shopkeeper here"),
            sell,
        )
        .add_command(
            CommandSpec::new("value", Category::Interaction)
                .usage("value <item>")
                .summary("ask a shopkeeper here what they'd pay for something you carry")
                .quiet(),
            value,
        );
    }
}

/// What a shopkeeper has bought from players and will sell again, oldest first, with each one's
/// quality.
#[derive(Component, Debug, Default)]
pub struct Resale(pub Vec<(ItemKey, Quality)>);

/// Whether a shop buys this kind of item: `anything`, one of its tags, or what it is (`weapon`,
/// `armour`, `food`, `drink`, `container`).
pub fn buys(shop: &ShopDef, def: &ItemDef) -> bool {
    let mut kinds: Vec<&str> = def.tags.iter().map(String::as_str).collect();
    if def.weapon.is_some() {
        kinds.push("weapon");
    }
    if def.armour.is_some() {
        kinds.push("armour");
    }
    if def.container.is_some() {
        kinds.push("container");
    }
    match def.consumable.map(|c| c.by) {
        Some(Consume::Eat) => kinds.push("food"),
        Some(Consume::Drink) => kinds.push("drink"),
        None => {}
    }
    worth(def, Quality::Standard).is_some()
        && shop
            .buys
            .iter()
            .any(|kind| kind == "anything" || kinds.contains(&kind.as_str()))
}

/// What trading needs.
#[derive(SystemParam)]
struct Counter<'w, 's> {
    commands: Commands<'w, 's>,
    players: Characters<'w, 's>,
    outbox: ResMut<'w, Outbox>,
    mobs: MobQuery<'w, 's>,
    mob_templates: Res<'w, MobTemplates>,
    templates: Res<'w, ItemTemplates>,
    items: ItemQuery<'w, 's>,
    keywords: Query<'w, 's, &'static Keywords>,
    purses: Query<'w, 's, &'static mut Purse>,
    resale: Query<'w, 's, &'static mut Resale>,
    rooms: RoomQuery<'w, 's>,
    db: Res<'w, Database>,
    practice: Practice<'w, 's>,
    bargains: Query<'w, 's, &'static mut Bargains>,
    time: Res<'w, Time>,
}

/// Something a player could sell: the item, its template, its name and its quality.
type Sellable = (Entity, ItemKey, String, Quality);

/// A shopkeeper and their shop.
struct Keeper {
    entity: Entity,
    name: String,
    zone: String,
    shop: ShopDef,
}

/// One line of a shop's goods.
struct Ware {
    key: ItemKey,
    def: ItemDef,
    quality: Quality,
    /// Its name, with its quality.
    name: String,
    /// Its value: the price before any deal.
    price: u64,
    /// Bought from a player, at this place in the resale list.
    resale: Option<usize>,
}

impl Counter<'_, '_> {
    /// The deal `actor` has with `keeper`, haggling for one if they can; how it went goes in `out`.
    fn deal(&mut self, actor: &Actor, keeper: &Keeper, out: &mut Vec<Outgoing>) -> f64 {
        let now = self.time.elapsed_secs_f64();
        trade::bargain(
            &mut self.practice,
            &mut self.bargains,
            &mut self.commands,
            actor,
            keeper.entity,
            &keeper.name,
            now,
            out,
        )
    }

    /// The first shopkeeper in `room`, if any.
    fn keeper(&self, room: Entity) -> Result<Keeper, String> {
        let mut here: Vec<_> = self
            .mobs
            .iter()
            .filter(|(_, _, r)| r.0 == room)
            .filter_map(|(entity, mob, _)| {
                let shop = self.mob_templates.0.get(&mob.key)?.shop.clone()?;
                Some(Keeper {
                    entity,
                    name: mob.name.clone(),
                    zone: mob.key.zone.clone(),
                    shop,
                })
            })
            .collect();
        here.sort_by_key(|k| k.entity);
        here.into_iter()
            .next()
            .ok_or_else(|| "There's no one here to trade with.".to_string())
    }

    /// Everything `keeper` has for sale: their stock, then what they've bought.
    fn wares(&self, keeper: &Keeper) -> Vec<Ware> {
        let stock = keeper
            .shop
            .sells
            .iter()
            .map(|id| (ItemKey::parse(id, &keeper.zone), Quality::Standard, None));
        let resale = self
            .resale
            .get(keeper.entity)
            .map(|r| r.0.clone())
            .unwrap_or_default();
        let bought = resale
            .into_iter()
            .enumerate()
            .map(|(i, (key, quality))| (key, quality, Some(i)));
        stock
            .chain(bought)
            .filter_map(|(key, quality, resale)| {
                let def = self.templates.0.get(&key)?.clone();
                let price = worth(&def, quality)?;
                Some(Ware {
                    key,
                    name: quality.named(&def),
                    def,
                    quality,
                    price,
                    resale,
                })
            })
            .collect()
    }

    /// An item `actor` carries, by name, ready to sell: not worn, and empty if it's a container.
    fn sellable(&self, actor: &Actor, words: &str) -> Result<Sellable, String> {
        if words.trim().is_empty() {
            return Err("Sell what?".into());
        }
        let entity = items::find_item(words, actor, &self.items, &self.keywords, &self.templates)?;
        let snapshot = Snapshot::new(
            self.items.iter().map(|(e, i, l)| (e, i, *l)),
            &self.templates,
        );
        let (_, item, location) = self.items.get(entity).map_err(|_| String::new())?;
        if snapshot.root(entity).and_then(ItemLocation::holder) != Some(actor.entity) {
            return Err(format!("You'd have to pick up {} first.", item.name));
        }
        if matches!(location, ItemLocation::Worn(..)) {
            return Err(format!("You'd have to take off {} first.", item.name));
        }
        if snapshot.at(ItemLocation::Inside(entity)).next().is_some() {
            return Err(format!("You'd have to empty {} first.", item.name));
        }
        Ok((entity, item.key.clone(), item.name.clone(), item.quality))
    }
}

/// `list`: what the shopkeeper here sells.
fn list(In(inv): In<Invocation>, mut counter: Counter) {
    let Some(actor) = Actor::get(&counter.players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let text = match counter.keeper(actor.room) {
        Err(err) => err,
        Ok(keeper) => {
            let share = counter.deal(&actor, &keeper, &mut out);
            let wares = counter.wares(&keeper);
            if wares.is_empty() {
                format!("{} has nothing for sale.", capitalized(&keeper.name))
            } else {
                let mut text = format!("{} sells:", capitalized(&keeper.name));
                for ware in wares {
                    let used = if ware.resale.is_some() { " (used)" } else { "" };
                    let name = format!("{}{used}", ware.name);
                    let price = trade::asking_price(ware.price, share);
                    // Padded before it's coloured, so the colour doesn't count as width.
                    let pad = " ".repeat(34usize.saturating_sub(name.chars().count()));
                    let name = items::coloured(&name, Some(&ware.def));
                    text += &format!("\r\n  {name}{pad} {}", money::coins(price));
                }
                text + "\r\nType 'buy <item>' to buy, 'sell <item>' to sell, or 'value <item>' to \
                        ask what they'd pay."
            }
        }
    };
    // How a haggle went comes first.
    let at = out
        .iter()
        .take_while(|o| matches!(o, Outgoing::To(..)))
        .count();
    out.insert(at, Outgoing::line(inv.entity, text));
    deliver(out, &mut counter.outbox);
}

/// `buy <item>`: buy something from the shopkeeper here. It goes into a free hand, or onto the
/// ground if your hands are full.
fn buy(In(inv): In<Invocation>, mut counter: Counter) {
    let Some(actor) = Actor::get(&counter.players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| {
        let keeper = counter.keeper(actor.room)?;
        let wanted = inv.args.trim();
        if wanted.is_empty() {
            return Err("Buy what? Type 'list' to see what's for sale.".to_string());
        }
        let ware = counter
            .wares(&keeper)
            .into_iter()
            .find(|ware| {
                // Every word must fit: `buy leather jerkin`.
                let keywords = ware.def.keywords();
                wanted.split_whitespace().all(|word| keywords.matches(word))
            })
            .ok_or_else(|| {
                format!(
                    "{} doesn't sell '{wanted}'. Type 'list' to see what's for sale.",
                    capitalized(&keeper.name)
                )
            })?;
        let price = trade::asking_price(ware.price, counter.deal(&actor, &keeper, &mut out));
        let mut purse = counter
            .purses
            .get_mut(actor.entity)
            .map_err(|_| String::new())?;
        if !purse.pay(price) {
            return Err(format!(
                "{} costs {}, and you have {}.",
                capitalized(&ware.name),
                money::coins(price),
                money::coins(purse.0)
            ));
        }
        purse.save(actor.character_id, &counter.db);
        if let Some(i) = ware.resale
            && let Ok(mut resale) = counter.resale.get_mut(keeper.entity)
        {
            resale.0.remove(i);
        }
        // Into a free hand, or set down if both are full. Either way it's a player's now, so real.
        let hands = counter
            .items
            .iter()
            .filter(|(_, _, l)| **l == ItemLocation::Held(actor.entity))
            .count();
        let (location, place, where_to) = if hands < HANDS {
            (
                ItemLocation::Held(actor.entity),
                actor.character_id.map(ItemPlace::Held),
                "",
            )
        } else {
            let room = counter
                .rooms
                .get(actor.room)
                .map(|(_, key, _)| key.to_string())
                .unwrap_or_default();
            (
                ItemLocation::Room(actor.room),
                Some(ItemPlace::Room(room)),
                " Your hands are full, so it's set down at your feet.",
            )
        };
        // A mob's things aren't saved.
        let id = place.and_then(|place| {
            counter
                .db
                .create_item(&ware.key.to_string(), &place, ware.quality)
                .map_err(|err| error!("failed to save a bought item [{}]: {err}", ware.key))
                .ok()
        });
        if let Some(item) = items::spawn_item(
            &mut counter.commands,
            &counter.templates,
            ware.key.clone(),
            id,
            location,
            ware.quality,
        ) {
            counter.practice.events.write(GameEvent::Obtained {
                who: actor.entity,
                item,
                key: ware.key.clone(),
                how: Got::Bought(keeper.entity),
            });
        }
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!(
                "{} buys {} from {}.\r\n",
                actor.name,
                ware.def.name,
                crate::combat::the(&keeper.name)
            ),
        });
        Ok(format!(
            "You buy {} from {} for {}.{where_to}",
            ware.def.name,
            crate::combat::the(&keeper.name),
            money::coins(price)
        ))
    })();
    // How a haggle went comes first.
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            let at = out
                .iter()
                .take_while(|o| matches!(o, Outgoing::To(..)))
                .count();
            out.insert(at, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    deliver(out, &mut counter.outbox);
}

/// `sell <item>`: sell something you carry to the shopkeeper here, for a third of its value.
fn sell(In(inv): In<Invocation>, mut counter: Counter) {
    let Some(actor) = Actor::get(&counter.players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| {
        let keeper = counter.keeper(actor.room)?;
        let (entity, key, name, quality) = counter.sellable(&actor, &inv.args)?;
        let def = counter.templates.0.get(&key).ok_or_else(String::new)?;
        let keeper_name = crate::combat::the(&keeper.name);
        if !buys(&keeper.shop, def) {
            return Err(format!(
                "{} isn't interested in {name}.",
                capitalized(&keeper_name)
            ));
        }
        let (value, unique) = (worth(def, quality).unwrap_or_default(), def.unique);
        let paid = trade::offer(value, counter.deal(&actor, &keeper, &mut out));
        let mut purse = counter
            .purses
            .get_mut(actor.entity)
            .map_err(|_| String::new())?;
        purse.0 += paid;
        purse.save(actor.character_id, &counter.db);
        // Gone from the player.
        if let Ok((_, item, _)) = counter.items.get(entity)
            && let Some(id) = item.id
            && let Err(err) = counter.db.delete_item(id)
        {
            error!("failed to delete sold item {id}: {err}");
        }
        counter.commands.entity(entity).despawn();
        // A shop resells what it bought, unless it stocks it anyway (of standard quality), or
        // there's only ever one.
        let stocked = quality == Quality::Standard
            && keeper
                .shop
                .sells
                .iter()
                .any(|id| ItemKey::parse(id, &keeper.zone) == key);
        if !unique && !stocked {
            match counter.resale.get_mut(keeper.entity) {
                Ok(mut resale) => {
                    resale.0.push((key, quality));
                    if resale.0.len() > MAX_RESALE {
                        resale.0.remove(0);
                    }
                }
                Err(_) => {
                    counter
                        .commands
                        .entity(keeper.entity)
                        .insert(Resale(vec![(key, quality)]));
                }
            }
        }
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} sells {name} to {keeper_name}.\r\n", actor.name),
        });
        Ok(format!(
            "You sell {name} to {keeper_name} for {}.",
            money::coins(paid)
        ))
    })();
    // How a haggle went comes first.
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            let at = out
                .iter()
                .take_while(|o| matches!(o, Outgoing::To(..)))
                .count();
            out.insert(at, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    deliver(out, &mut counter.outbox);
}

/// `value <item>`: what the shopkeeper here would pay for something you carry.
fn value(In(inv): In<Invocation>, mut counter: Counter) {
    let Some(actor) = Actor::get(&counter.players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let text = (|| {
        let keeper = counter.keeper(actor.room)?;
        let (_, key, name, quality) = counter.sellable(&actor, &inv.args)?;
        let def = counter.templates.0.get(&key).ok_or_else(String::new)?;
        let keeper_name = capitalized(&crate::combat::the(&keeper.name));
        let (value, buys_it) = (
            worth(def, quality).unwrap_or_default(),
            buys(&keeper.shop, def),
        );
        Ok(if buys_it {
            let paid = trade::offer(value, counter.deal(&actor, &keeper, &mut out));
            format!(
                "{keeper_name} would give you {} for {name}.",
                money::coins(paid)
            )
        } else {
            format!("{keeper_name} isn't interested in {name}.")
        })
    })()
    .unwrap_or_else(|err: String| err);
    let at = out
        .iter()
        .take_while(|o| matches!(o, Outgoing::To(..)))
        .count();
    out.insert(at, Outgoing::line(inv.entity, text));
    deliver(out, &mut counter.outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{ConsumableDef, WeaponDef};

    #[test]
    fn shops_buy_what_they_deal_in() {
        let mut sword = ItemDef::named("a short sword");
        sword.weapon = Some(WeaponDef {
            skill: "swordplay:basics".into(),
            damage: (2, 5),
            damage_type: "physical".into(),
        });
        sword.value = Some(100);
        let mut bread = ItemDef::named("a loaf of bread");
        bread.value = Some(3);
        bread.consumable = Some(ConsumableDef {
            by: Consume::Eat,
            health: None,
            stamina: None,
            recovery: None,
        });
        let mut herb = ItemDef::named("a sprig of yarrow");
        herb.tags = vec!["herb".into()];
        herb.value = Some(4);
        let worthless = ItemDef::named("a rusty key");

        let smith = ShopDef {
            sells: Vec::new(),
            buys: vec!["weapon".into(), "armour".into()],
        };
        let herbalist = ShopDef {
            sells: Vec::new(),
            buys: vec!["herb".into(), "food".into()],
        };
        let tinker = ShopDef {
            sells: Vec::new(),
            buys: vec!["anything".into()],
        };
        assert!(buys(&smith, &sword) && !buys(&smith, &bread));
        assert!(buys(&herbalist, &herb) && buys(&herbalist, &bread) && !buys(&herbalist, &sword));
        assert!(
            buys(&tinker, &sword) && !buys(&tinker, &worthless),
            "nothing for no value"
        );
    }
}
