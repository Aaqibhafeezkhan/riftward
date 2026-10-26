//! Trade: Haggling and Appraisal.
//!
//! **Haggling** works on its own whenever its holder deals with a shopkeeper (see `shops.rs`).
//! The first dealing strikes a deal: their Haggling and Charisma against the shopkeeper's, by
//! opposed roll, and how surely they win sets how far prices move their way (a twentieth to a
//! fifth), for `DEAL_SECS`. Buying prices come down by that share of an item's value, and selling
//! prices go up by it, so the two never cross. Without Haggling, a character pays list price.
//!
//! **Appraisal** is `appraise <item>`: what something is worth, judged against a difficulty that
//! grows with its value.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::attributes::Attribute;
use crate::combat::{Blow, the};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::dice::Contest;
use crate::items::{self, ItemQuery, ItemTemplates, worth};
use crate::money;
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};
use crate::skills::{Learner, Practice, fill};
use crate::targeting::Keywords;

/// The skill that moves prices.
pub const HAGGLING: &str = "trade:haggling";
/// The skill `appraise` uses.
const APPRAISAL: &str = "trade:appraisal";
/// How long a deal with a shopkeeper lasts, in seconds.
const DEAL_SECS: f64 = 10.0 * 60.0;
/// What a shop pays for an item, before haggling: this share of its value.
const BUYING_SHARE: f64 = 1.0 / 3.0;

pub struct TradePlugin;

impl Plugin for TradePlugin {
    fn build(&self, app: &mut App) {
        app.add_command(
            CommandSpec::new("appraise", Category::Interaction)
                .usage("appraise <item>")
                .summary("judge what something is worth, if you know Appraisal")
                .quiet()
                .action(),
            appraise,
        );
    }
}

/// The deals a character has struck with shopkeepers: how far prices move their way, and until
/// when, in seconds since the server started. Not saved.
#[derive(Component, Debug, Default)]
pub struct Bargains(pub HashMap<Entity, (f64, f64)>);

/// How far a haggle moves prices, from how surely it was won: nothing if lost, then a twentieth,
/// a tenth, three twentieths or a fifth of an item's value.
pub fn deal(contest: Contest) -> f64 {
    if !contest.won {
        return 0.0;
    }
    match Blow::of(contest.margin) {
        Blow::Glancing => 0.05,
        Blow::Solid => 0.10,
        Blow::Strong => 0.15,
        Blow::Crushing => 0.20,
    }
}

/// How a deal reads: "a little", "a tenth".
fn deal_words(share: f64) -> &'static str {
    match share {
        s if s < 0.075 => "a little",
        s if s < 0.125 => "a tenth",
        s if s < 0.175 => "a good way",
        _ => "a fifth",
    }
}

/// What a shop asks for something worth `value`, with a deal of `share`: at least a copper.
pub fn asking_price(value: u64, share: f64) -> u64 {
    ((value as f64 * (1.0 - share)).round() as u64).max(1)
}

/// What a shop pays for something worth `value`, with a deal of `share`: a third of its value,
/// plus the deal, and at least a copper.
pub fn offer(value: u64, share: f64) -> u64 {
    ((value as f64 * (BUYING_SHARE + share)).floor() as u64).max(1)
}

/// The deal `actor` has with the shopkeeper `keeper` at `now`: one struck already, or, if they
/// know Haggling, a new one haggled for, told in `out`. Without Haggling, none.
#[allow(clippy::too_many_arguments)]
pub fn bargain(
    practice: &mut Practice,
    bargains: &mut Query<&mut Bargains>,
    commands: &mut Commands,
    actor: &Actor,
    keeper: Entity,
    keeper_name: &str,
    now: f64,
    out: &mut Vec<Outgoing>,
) -> f64 {
    if let Ok(struck) = bargains.get(actor.entity)
        && let Some((share, until)) = struck.0.get(&keeper)
        && *until > now
    {
        return *share;
    }
    let Some(skill) = practice.defs.skills.get(HAGGLING).cloned() else {
        return 0.0;
    };
    // Their Haggling and its attribute, and the shopkeeper's.
    let mut rating = |who: Entity| {
        practice.sheet(who).and_then(|(skills, _, attributes)| {
            let level = skills.0.get(HAGGLING)?.level as i32;
            Some((
                f64::from((level + attributes.get(skill.attribute)).max(1)),
                attributes.get(Attribute::Luck),
            ))
        })
    };
    let Some((mine, luck)) = rating(actor.entity) else {
        return 0.0;
    };
    let (theirs, their_luck) = rating(keeper).unwrap_or((1.0, 0));
    let share = deal(practice.opposed(mine, luck, theirs, their_luck));

    let learner = Learner {
        character_id: actor.character_id,
        race: &actor.race,
        boost: 1.0,
    };
    let gains = practice.exercise(actor.entity, learner, HAGGLING);
    if let Some(text) = &skill.def.text {
        let lines = match (share > 0.0, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let keeper_name = the(keeper_name);
        let with = [
            ("actor", actor.name.as_str()),
            ("target", keeper_name.as_str()),
            ("deal", deal_words(share)),
        ];
        let mut told = fill(&lines.actor, &with);
        for line in gains {
            told += &format!("\r\n{line}");
        }
        out.push(Outgoing::line(actor.entity, told));
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: fill(&lines.room, &with) + "\r\n",
        });
    }
    let struck = (share, now + DEAL_SECS);
    match bargains.get_mut(actor.entity) {
        Ok(mut bargains) => {
            bargains.0.insert(keeper, struck);
        }
        Err(_) => {
            commands
                .entity(actor.entity)
                .insert(Bargains(HashMap::from([(keeper, struck)])));
        }
    }
    share
}

/// The difficulty of appraising something worth `value`: harder the more it's worth. Bread is
/// about 3, a short sword 6, a breastplate 8.
fn appraisal_difficulty(value: u64) -> f64 {
    2.0 + 2.0 * (value as f64 + 1.0).log10()
}

/// `appraise <item>`: what something you can reach is worth, if you can tell.
fn appraise(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    items: ItemQuery,
    keywords: Query<&Keywords>,
    templates: Res<ItemTemplates>,
    mut practice: Practice,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| {
        if inv.args.trim().is_empty() {
            return Err(inv.usage_error());
        }
        let entity = items::find_item(&inv.args, &actor, &items, &keywords, &templates)?;
        let (_, item, _) = items.get(entity).map_err(|_| String::new())?;
        let ready = practice.begin(actor.entity, APPRAISAL)?;
        let value = templates
            .0
            .get(&item.key)
            .and_then(|def| worth(def, item.quality));
        let skill = f64::from(ready.effectiveness.max(1));
        // Something with no value is easy to judge.
        let judged = match value {
            None => true,
            Some(value) => {
                practice
                    .opposed(skill, ready.luck, appraisal_difficulty(value), 0)
                    .won
            }
        };
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
        };
        let gains = practice.exercise(actor.entity, learner, APPRAISAL);
        let text = practice
            .defs
            .skills
            .get(APPRAISAL)
            .and_then(|skill| skill.def.text.clone())
            .ok_or_else(String::new)?;
        let lines = match (judged, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let worth = value.map_or("nothing a merchant would pay for".to_string(), money::coins);
        let with = [
            ("actor", actor.name.as_str()),
            ("item", item.name.as_str()),
            ("worth", worth.as_str()),
        ];
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: fill(&lines.room, &with) + "\r\n",
        });
        let mut told = fill(&lines.actor, &with);
        for line in gains {
            told += &format!("\r\n{line}");
        }
        Ok(told)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    deliver(out, &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surer_haggle_makes_a_better_deal() {
        let lost = Contest {
            won: false,
            margin: 3.0,
        };
        assert_eq!(deal(lost), 0.0);
        let won = |margin| deal(Contest { won: true, margin });
        assert!(won(1.05) < won(1.3) && won(1.3) < won(2.0) && won(2.0) < won(3.0));
        assert_eq!(won(3.0), 0.20);
    }

    #[test]
    fn prices_move_your_way_but_never_cross() {
        assert_eq!(asking_price(100, 0.0), 100);
        assert_eq!(asking_price(100, 0.20), 80);
        assert_eq!(offer(100, 0.0), 33);
        assert_eq!(offer(100, 0.20), 53);
        assert_eq!(asking_price(1, 0.20), 1, "always at least a copper");
        // Even the best deal can't buy low and sell high.
        for value in [1, 3, 10, 50, 100, 1000] {
            assert!(offer(value, 0.20) <= asking_price(value, 0.20));
        }
    }

    #[test]
    fn dearer_things_are_harder_to_judge() {
        assert!(appraisal_difficulty(3) < appraisal_difficulty(100));
        assert!(appraisal_difficulty(100) < appraisal_difficulty(1000));
        assert!((appraisal_difficulty(1000) - 8.0).abs() < 0.01);
    }
}
