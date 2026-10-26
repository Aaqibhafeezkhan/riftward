//! Money: copper, silver and gold, kept in each character's purse as a count of copper. Ten
//! copper make a silver, and ten silver a gold.
//!
//! A purse weighs nothing and holds any amount. Coins come from the mobs a character kills (a
//! template's `coins`) and from selling to shops, and go on buying and lessons. Players can
//! `give` them to each other: `give 5 silver grok`.

use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::output::{Actor, Outgoing, PlayerQuery, deliver, targets_online};
use crate::targeting::{Keywords, Target};

/// Copper in a silver piece, and silver in a gold one.
pub const COPPER_PER_SILVER: u64 = 10;
pub const SILVER_PER_GOLD: u64 = 10;
const COPPER_PER_GOLD: u64 = COPPER_PER_SILVER * SILVER_PER_GOLD;

pub struct MoneyPlugin;

impl Plugin for MoneyPlugin {
    fn build(&self, app: &mut App) {
        app.add_command(
            CommandSpec::new("setcoins", Category::Admin)
                .usage("setcoins <player> <amount>")
                .summary("set an online player's money, like '3 gold 5 silver', for testing")
                .permission(Permission::ManageRoles),
            setcoins,
        );
    }
}

/// A character's money, in copper.
#[derive(Component, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Purse(pub u64);

impl Purse {
    /// Take `amount` out, if it's there. Returns whether it was.
    pub fn pay(&mut self, amount: u64) -> bool {
        if self.0 < amount {
            return false;
        }
        self.0 -= amount;
        true
    }

    /// Save it as `character_id`'s.
    pub fn save(self, character_id: i64, db: &Database) {
        if let Err(err) = db.save_coins(character_id, self.0) {
            error!("failed to save the purse of character {character_id}: {err}");
        }
    }
}

/// An amount of copper as it's said: "3 gold, 4 silver and 7 copper", "nothing".
pub fn coins(copper: u64) -> String {
    let parts: Vec<String> = [
        (copper / COPPER_PER_GOLD, "gold"),
        (copper % COPPER_PER_GOLD / COPPER_PER_SILVER, "silver"),
        (copper % COPPER_PER_SILVER, "copper"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, coin)| format!("{n} {coin}"))
    .collect();
    match parts.as_slice() {
        [] => "nothing".into(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The coin a word names, as copper: `gold`, `g`, `silver`, `s`, `copper`, `c`, and the like.
fn coin(word: &str) -> Option<u64> {
    let word = word.trim_end_matches(['.', ',']).to_lowercase();
    let is = |full: &str, short: &str| word == short || (word.len() > 1 && full.starts_with(&word));
    if is("gold", "g") || is("gp", "gp") {
        Some(COPPER_PER_GOLD)
    } else if is("silver", "s") || is("sp", "sp") {
        Some(COPPER_PER_SILVER)
    } else if is("copper", "c") || is("cp", "cp") || word == "coins" || word == "coin" {
        Some(1)
    } else {
        None
    }
}

/// An amount of money at the start of `text`, and the rest: `2 gold 5 silver grok` is 250
/// copper and `grok`; `5s grok` is 50. A bare number is copper. `None` if it doesn't start with
/// an amount.
pub fn amount_in(text: &str) -> Option<(u64, &str)> {
    let mut total = 0;
    let mut rest = text.trim_start();
    loop {
        let (word, after) = rest.split_once(' ').unwrap_or((rest, ""));
        // "5s" or "5 silver".
        let digits = word.trim_end_matches(|c: char| c.is_alphabetic());
        let Ok(n) = digits.parse::<u64>() else {
            break;
        };
        let (worth, after) = match &word[digits.len()..] {
            "" => match after.split_once(' ').unwrap_or((after, "")) {
                (next, more) if coin(next).is_some() => (coin(next)?, more),
                _ => (1, after),
            },
            suffix => (coin(suffix)?, after),
        };
        total += n.checked_mul(worth)?;
        rest = after.trim_start();
    }
    (rest.len() < text.trim_start().len()).then_some((total, rest))
}

/// `setcoins <player> <amount>`: set someone's money, for testing.
fn setcoins(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mut purses: Query<&mut Purse>,
    keywords: Query<&Keywords>,
    db: Res<Database>,
) {
    let text = (|| {
        let (who, amount) = inv
            .args
            .trim()
            .split_once(' ')
            .ok_or_else(|| inv.usage_error())?;
        let (amount, "") = amount_in(amount).ok_or_else(|| inv.usage_error())? else {
            return Err(inv.usage_error());
        };
        let target = Target::parse(who)
            .and_then(|t| t.find_one(inv.entity, targets_online(&players, &keywords)))
            .and_then(|e| Actor::get(&players, e))
            .ok_or_else(|| format!("No one called '{who}' is online."))?;
        let mut purse = purses.get_mut(target.entity).map_err(|_| String::new())?;
        purse.0 = amount;
        purse.save(target.character_id, &db);
        Ok(format!("{} now has {}.", target.name, coins(amount)))
    })()
    .unwrap_or_else(|err| err);
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// Give `amount` from `giver`'s purse to `receiver`'s, and say so. For `give`.
pub fn give_coins(
    giver: &Actor,
    receiver: &Actor,
    amount: u64,
    purses: &mut Query<&mut Purse>,
    db: &Database,
) -> Result<Vec<Outgoing>, String> {
    if amount == 0 {
        return Err("Give how much?".into());
    }
    if receiver.entity == giver.entity {
        return Err("It's already yours.".into());
    }
    let mut mine = purses.get_mut(giver.entity).map_err(|_| String::new())?;
    if !mine.pay(amount) {
        return Err(format!("You only have {}.", coins(mine.0)));
    }
    mine.save(giver.character_id, db);
    let mut theirs = purses.get_mut(receiver.entity).map_err(|_| String::new())?;
    theirs.0 += amount;
    theirs.save(receiver.character_id, db);
    let said = coins(amount);
    Ok(vec![
        Outgoing::line(
            giver.entity,
            format!("You give {said} to {}.", receiver.name),
        ),
        Outgoing::line(receiver.entity, format!("{} gives you {said}.", giver.name)),
        Outgoing::Room {
            room: giver.room,
            except: giver.entity,
            text: format!("{} gives {} some coins.\r\n", giver.name, receiver.name),
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_reads_in_coins() {
        assert_eq!(coins(0), "nothing");
        assert_eq!(coins(7), "7 copper");
        assert_eq!(coins(50), "5 silver");
        assert_eq!(coins(347), "3 gold, 4 silver and 7 copper");
        assert_eq!(coins(305), "3 gold and 5 copper");
    }

    #[test]
    fn amounts_are_read_from_words() {
        assert_eq!(amount_in("5 silver grok"), Some((50, "grok")));
        assert_eq!(amount_in("2 gold 5 silver"), Some((250, "")));
        assert_eq!(amount_in("2g 5s 3c wren"), Some((253, "wren")));
        assert_eq!(
            amount_in("12 grok"),
            Some((12, "grok")),
            "a bare number is copper"
        );
        assert_eq!(amount_in("3 coins"), Some((3, "")));
        assert_eq!(amount_in("sword grok"), None);
        assert_eq!(amount_in("2.sword grok"), None);
    }
}
