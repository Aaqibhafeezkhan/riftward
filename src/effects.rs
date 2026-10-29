//! Effects: what spells and techniques *do*, in one vocabulary (`docs/effects.md#effects`).
//!
//! A spell and a technique list their effects the same way, and the combat round carries them
//! out the same way (`combat/effects.rs`): conditions go on whoever they're `on`, damage goes
//! through the target's armour and resistances. Effects on the user happen whether or not the
//! spell or blow lands; effects on the target only when it does.
//!
//! Any effect works in either: a technique can do fire damage on top of its blow, and a spell can
//! cleave.

use serde::{Deserialize, Serialize};

use crate::attributes::Attribute;
use crate::combat::{ConditionDefs, Lasts};
use crate::formula::Formula;
use crate::skills::Lines;
use crate::time::REAL_SECS_PER_GAME_DAY;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Effect {
    /// Damage of a type (`assets/damage_types.ron`), `amount` a [formula](crate::formula), landing
    /// harder the more decisively it won: `Damage(amount: "roll(2, 5) + power / 3", type: "force")`.
    /// Armour takes its share only of what armour stops; resistances and weaknesses count.
    Damage {
        amount: Formula,
        #[serde(rename = "type", default = "crate::world::physical")]
        damage_type: String,
    },
    /// Health back, up to the most: `amount` is a formula.
    Heal { amount: Formula },
    /// Puts a condition (`assets/conditions`) on someone, at `strength` (a formula, 1 unless it
    /// says: a ward's `"4 + power / 3"` takes more from a stronger caster), for as long as it
    /// `lasts`.
    Apply {
        condition: String,
        #[serde(default = "one")]
        strength: Formula,
        lasts: Lasting,
        #[serde(default)]
        on: Who,
    },
    /// Ends a condition early.
    Remove {
        condition: String,
        #[serde(default)]
        on: Who,
    },
    /// Ends every condition with a tag: `Cure(tag: "poison")`.
    Cure {
        tag: String,
        #[serde(default)]
        on: Who,
    },
    /// Puts a condition on the weapon in hand of whoever it's `on`: a venom coating, a flaming
    /// blade. Its modifiers pass to whoever holds it, and its `on_hit` effects land on whatever
    /// it strikes, with `strength` in their formulas the item condition's: flaming's
    /// `"roll(1, 3) * strength"` burns twice as hot at 2. `strength` is a formula itself, 1
    /// unless it says. It lasts for `lasts` of game time or `charges` hits, whichever runs out
    /// first; with neither, until something takes it off. Saved with the item.
    Imbue {
        condition: String,
        #[serde(default = "one")]
        strength: Formula,
        #[serde(default)]
        lasts: Option<Lasting>,
        #[serde(default)]
        charges: Option<u32>,
        #[serde(default)]
        on: Who,
    },
    /// This much of the target's armour doesn't help: a point finding a gap.
    Pierce(i32),
    /// The target's armour doesn't help at all.
    IgnoreArmour,
    /// The blow carries on into a second foe fighting the user.
    Cleave,
    /// An animal stops fighting and stops attacking those who come near.
    Calm,
    /// Something found in the room, of a `kind` its finds list (`"forage"`, `"herbs"`): picked
    /// from the room's table with the user's Luck, the better made the more surely it was found,
    /// and into their hand. Only in an active skill aimed at the room.
    Find { kind: String },
    /// Something lifted from the target and given to the user, if it lands: `Take(Coins("..."))`
    /// for Pickpocketing. See [`Taken`].
    Take(Taken),
    /// Something the user learns, and nothing else changes: an item's worth, a room's tracks. Only
    /// in an active skill aimed at what it reveals. See [`Revealed`].
    Reveal(Revealed),
    /// The user's allies within `rooms` of them who aren't busy come running, and join the fight
    /// against the target: a losing guard shouting for help. Who answers is whoever counts the
    /// user an ally (`allies` on their templates, and always their own kind).
    CallForHelp { rooms: u32 },
    /// `count` (a formula, 1 unless it says) more mobs of a template come into being where the
    /// user is, and join the fight against the target, unless that's the user: an ooze splitting
    /// when struck hard, the Slime Lord shedding slimes. `mob` is a template id in the user's own
    /// zone, or `zone:id`. They belong to no spawn, so they never come back once killed. At most
    /// `SPAWN_MOST` at once.
    Spawn {
        mob: String,
        #[serde(default = "one")]
        count: Formula,
    },
    /// The user runs: in a fight, they try to break away as `flee` does (its stamina, its
    /// chance, held fast by `CantFlee`); out of one, they bolt through a random way anyone could
    /// walk, away from rooms holding what they `avoid`. A startled deer, a beaten wolf.
    Flee,
}

/// The most mobs one `Spawn` brings, whatever its count says.
pub const SPAWN_MOST: u32 = 10;

/// What `Reveal` tells the user. Each is read off what the skill is aimed at.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revealed {
    /// An item's worth, named as `{worth}` in the skill's text: Appraisal.
    Worth,
    /// The tracks in a room, each read by its own roll against `against`, which sees the
    /// ground's difficulty as `target.ground` and the track's age in minutes as `age`; those read
    /// are listed after the skill's line: Tracking.
    Tracks,
}

/// What `Take` lifts. Coins, for now; items (carried, held, by tag) and conditions (a stolen
/// blessing) would each be another kind here.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Taken {
    /// Coins from their purse into the user's: as many as the formula says (in copper), up to
    /// what they have. A skill's text names the haul as `{coins}`.
    Coins(Formula),
}

fn one() -> Formula {
    Formula::number(1.0)
}

/// What the target resists a spell or technique with, in an opposed roll against the user's
/// rating (`combat/resolve.rs`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resist {
    /// Getting out of the way as from a blow: Reaction and Dodge, and a shield's block. What a
    /// technique is resisted with unless it says otherwise.
    Evasion,
    /// Getting out of the way, but a shield doesn't help: a bolt of force.
    Dodge,
    /// An attribute: Strength against a grip, Willpower against a mind's touch.
    Attribute(Attribute),
}

/// A blow's resistance, for techniques that don't name another.
pub fn evasion() -> Option<Resist> {
    Some(Resist::Evasion)
}

/// Something a condition or passive skill does by itself when its bearer does or suffers
/// something: thorns that tear at whoever strikes its bearer. It's resolved like any spell or
/// blow, with the bearer as the user and the other one involved as the target, so its effects
/// (`on: Target` by default) land on the other one; where nobody else is involved (entering a
/// room, resting, the heartbeat), the target is the bearer. A reaction never sets off another.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Reaction {
    pub on: When,
    /// The odds it happens, 0 to 1, a formula (`"0.25"`, `"strength / 10"`); always, without.
    #[serde(default)]
    pub chance: Option<Formula>,
    /// Who the other one must be for it to happen: `only: Foes`. Anyone, without.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Only>,
    pub effects: Vec<Effect>,
    /// What everyone sees, `{actor}` the bearer and `{target}` the other one, with `{harm}` in
    /// every line if it does damage.
    pub text: Lines,
}

/// A check on who the other one in a reaction is.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Only {
    /// Not the bearer's ally: for a mob, not its own kind nor one its `allies` name
    /// (`MobDef::allied`); for a player, not another player. A deer startles at a wolf or a
    /// hunter coming in, not at another deer.
    Foes,
}

/// When a reaction happens.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum When {
    /// The bearer lands a blow, technique or spell meant to harm.
    Hits,
    /// The bearer is hit by one.
    Struck,
    /// The bearer dodges a blow (not a shield block).
    Dodges,
    /// The bearer kills someone.
    Kills,
    /// The bearer is killed, by whoever is the other one (themselves, for bleeding to death).
    /// The only reaction that answers from beyond the grave.
    Dies,
    /// A hit leaves the bearer below this share of their most health, having been above it:
    /// `Hurt(0.25)`.
    Hurt(f64),
    /// The start of each of the bearer's turns in a fight, at whoever they're fighting.
    TurnStarts,
    /// The bearer casts a spell, at whoever it's cast at.
    Casts,
    /// The bearer comes into a room.
    Enters,
    /// Someone the bearer sees comes into the bearer's room, who is the other one.
    Arrives,
    /// The bearer sits down to rest.
    Rests,
    /// The bearer eats something.
    Eats,
    /// The bearer drinks something.
    Drinks,
    /// The bearer tries to find something in a room, foraging or gathering herbs, whether or not
    /// they do. A room's own condition answers it too: the bees of a honey tree.
    Gathers,
    /// This condition takes hold, at whoever put it on. Only a condition's own reactions.
    Applied,
    /// This condition runs out, in time or turns; not when it's cured, removed or broken. Only a
    /// condition's own reactions.
    Fades,
    /// Every so often, in game time, in a fight or out of one: `Every(Minutes(10))`.
    Every(Lasting),
}

impl When {
    /// Whether only a condition's own reactions answer it: taking hold, running out.
    pub fn own(self) -> bool {
        matches!(self, When::Applied | When::Fades)
    }
}

impl Reaction {
    /// Whether it's complete, so nothing is forgotten.
    pub fn check(&self) -> Result<(), String> {
        if self.effects.is_empty() {
            return Err("a reaction needs effects".into());
        }
        if let When::Every(every) = self.on
            && every.real_secs() <= 0.0
        {
            return Err("a reaction every so often needs a time, in minutes or hours".into());
        }
        check(&self.effects)?;
        let harms = self
            .effects
            .iter()
            .any(|e| matches!(e, Effect::Damage { .. }));
        let lines = [
            Some(&self.text.actor),
            self.text.target.as_ref(),
            Some(&self.text.room),
        ];
        if harms
            && lines
                .into_iter()
                .flatten()
                .any(|line| !line.contains("{harm}"))
        {
            return Err("a reaction that does damage needs {harm} in every line".into());
        }
        Ok(())
    }
}

/// Whom an effect is on.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Who {
    /// Whoever the spell or blow is aimed at: on them only if it lands.
    #[default]
    Target,
    /// The one using it, whether or not it lands: a technique that leaves you open.
    User,
}

/// How long a condition lasts: in game time (a game hour is 7.5 real minutes), counting down in a
/// fight or out of one, or in the bearer's turns, for what only matters in a fight.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lasting {
    Minutes(u32),
    Hours(u32),
    Turns(u32),
    /// So many combat rounds' worth of real time (`round_seconds` in `rules.ron` each), in a fight or out of one,
    /// unlike `Turns`: a startled deer's two rounds before it bolts.
    Rounds(u32),
    /// Until something ends it: hiding lasts until the hider acts openly or is found.
    Indefinitely,
}

impl Lasting {
    /// The same time in real seconds; nothing for turns.
    pub fn real_secs(self) -> f64 {
        let minutes = match self {
            Lasting::Minutes(n) => n,
            Lasting::Hours(n) => n * 60,
            Lasting::Rounds(n) => return f64::from(n) * crate::rules::rules().round_seconds,
            Lasting::Turns(_) | Lasting::Indefinitely => 0,
        };
        f64::from(minutes) * REAL_SECS_PER_GAME_DAY as f64 / (24.0 * 60.0)
    }

    /// How long a condition put on at `now` (seconds since the server started) lasts.
    pub fn lasts(self, now: f64) -> Lasts {
        match self {
            Lasting::Turns(n) => Lasts::Turns(n.max(1)),
            Lasting::Indefinitely => Lasts::Indefinitely,
            time => Lasts::Until(now + time.real_secs()),
        }
    }
}

impl Effect {
    /// Whom it's on.
    pub fn on(&self) -> Who {
        match self {
            Effect::Apply { on, .. }
            | Effect::Remove { on, .. }
            | Effect::Cure { on, .. }
            | Effect::Imbue { on, .. } => *on,
            // Wherever the user is, whether or not anything lands.
            Effect::Spawn { .. } | Effect::Flee => Who::User,
            _ => Who::Target,
        }
    }

    /// The condition it puts on or takes off, if any, so tests can check it's a real one.
    #[cfg(test)]
    pub fn condition(&self) -> Option<&str> {
        match self {
            Effect::Apply { condition, .. }
            | Effect::Remove { condition, .. }
            | Effect::Imbue { condition, .. } => Some(condition),
            _ => None,
        }
    }

    /// This effect as an item condition at strength `n` carries it: `strength` in its formulas
    /// is `n`.
    pub fn with_strength(&self, n: i32) -> Effect {
        let n = f64::from(n);
        let mut effect = self.clone();
        match &mut effect {
            Effect::Damage { amount, .. }
            | Effect::Heal { amount }
            | Effect::Take(Taken::Coins(amount)) => {
                *amount = amount.with_strength(n);
            }
            Effect::Apply { strength, .. } | Effect::Imbue { strength, .. } => {
                *strength = strength.with_strength(n);
            }
            _ => {}
        }
        effect
    }

    /// The damage type it names, if any.
    #[cfg(test)]
    pub fn damage_type(&self) -> Option<&str> {
        match self {
            Effect::Damage { damage_type, .. } => Some(damage_type),
            _ => None,
        }
    }
}

/// Whether any of `effects` is done to a room or an item rather than to someone: `Find`,
/// `Reveal`.
pub fn in_a_room(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Find { .. } | Effect::Reveal(_)))
}

/// What a spell or technique is, for what resists it: its own `tags`, the tags of every condition
/// it puts on, and the types of damage it does. A sleep spell is `sleep` and `mind` because its
/// condition is; Spark is `force`.
pub fn tags(own: &[String], effects: &[Effect], conditions: &ConditionDefs) -> Vec<String> {
    let mut tags = own.to_vec();
    for effect in effects {
        match effect {
            Effect::Apply { condition, .. } => {
                tags.extend(
                    conditions
                        .get(condition)
                        .into_iter()
                        .flat_map(|d| d.tags.clone()),
                );
            }
            Effect::Damage { damage_type, .. } => tags.push(damage_type.clone()),
            _ => {}
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

/// Whether `effects` make sense, so a mistake is caught at load.
pub fn check(effects: &[Effect]) -> Result<(), String> {
    for effect in effects {
        if let Effect::Imbue {
            lasts: Some(Lasting::Turns(_)),
            ..
        } = effect
        {
            return Err(
                "imbues an item for turns, but items don't take turns: give it \
                        Minutes, Hours or charges"
                    .into(),
            );
        }
        if let Effect::Imbue {
            lasts: Some(Lasting::Indefinitely),
            ..
        } = effect
        {
            return Err("imbues an item indefinitely: leave `lasts` out instead".into());
        }
    }
    Ok(())
}

/// The armour a blow or spell gets past: `Pierce`s added up.
pub fn pierce(effects: &[Effect]) -> i32 {
    effects
        .iter()
        .map(|e| match e {
            Effect::Pierce(n) => *n,
            _ => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_parse_with_their_defaults() {
        let effects: Vec<Effect> = ron::from_str(
            r#"[
                Apply(condition: "staggered", lasts: Turns(1)),
                Apply(condition: "guarding", strength: 4, lasts: Turns(2), on: User),
                Damage(amount: "roll(2, 5)"),
                Cure(tag: "poison"),
                Pierce(2),
            ]"#,
        )
        .unwrap();
        assert_eq!(
            effects[0],
            Effect::Apply {
                condition: "staggered".into(),
                strength: Formula::number(1.0),
                lasts: Lasting::Turns(1),
                on: Who::Target,
            }
        );
        assert_eq!(effects[1].on(), Who::User);
        assert_eq!(effects[2].damage_type(), Some("physical"));
        assert_eq!(pierce(&effects), 2);
    }

    #[test]
    fn tags_come_from_conditions_damage_and_the_ability_itself() {
        let conditions = crate::combat::defs_on_disk();
        let effects = [
            Effect::Apply {
                condition: "asleep".into(),
                strength: Formula::number(1.0),
                lasts: Lasting::Turns(2),
                on: Who::Target,
            },
            Effect::Damage {
                amount: Formula::parse("roll(1, 2)").unwrap(),
                damage_type: "force".into(),
            },
        ];
        assert_eq!(
            tags(&["charm".into()], &effects, &conditions),
            ["charm", "force", "mind", "sleep"]
        );
    }

    #[test]
    fn an_item_is_imbued_for_time_or_charges_not_turns() {
        let imbue = |lasts| Effect::Imbue {
            condition: "flaming".into(),
            strength: Formula::number(1.0),
            lasts,
            charges: Some(3),
            on: Who::Target,
        };
        assert!(check(&[imbue(Some(Lasting::Minutes(10)))]).is_ok());
        assert!(check(&[imbue(None)]).is_ok());
        assert!(check(&[imbue(Some(Lasting::Turns(2)))]).is_err());
    }

    #[test]
    fn an_item_conditions_strength_reaches_its_effects() {
        let poison = Effect::Apply {
            condition: "poisoned".into(),
            strength: Formula::parse("strength * 2").unwrap(),
            lasts: Lasting::Turns(4),
            on: Who::Target,
        };
        let Effect::Apply { strength, .. } = poison.with_strength(3) else {
            unreachable!()
        };
        let worked = strength.eval(
            &crate::formula::Inputs::default(),
            &mut crate::dice::Dice::seeded(1),
            0,
        );
        assert_eq!(worked, 6.0);
    }

    #[test]
    fn a_reaction_that_hurts_says_how_much() {
        let lines = |text: &str| Lines {
            actor: text.into(),
            target: Some(text.into()),
            room: text.into(),
            beyond: None,
        };
        let mut thorns = Reaction {
            on: When::Struck,
            chance: None,
            only: None,
            effects: vec![Effect::Damage {
                amount: Formula::number(2.0),
                damage_type: "physical".into(),
            }],
            text: lines("Thorns {harm} {target}."),
        };
        assert!(thorns.check().is_ok());
        thorns.text = lines("Thorns prick {target}.");
        assert!(thorns.check().unwrap_err().contains("{harm}"));
        thorns.effects.clear();
        assert!(thorns.check().is_err(), "a reaction does something");
        thorns.text = lines("Thorns {harm} {target}.");
        thorns.effects = vec![Effect::Heal {
            amount: Formula::number(1.0),
        }];
        thorns.on = When::Every(Lasting::Minutes(10));
        assert!(thorns.check().is_ok());
        thorns.on = When::Every(Lasting::Turns(1));
        assert!(thorns.check().is_err(), "every so often is in game time");
    }

    #[test]
    fn game_time_lasts_in_real_time() {
        assert_eq!(Lasting::Hours(12).real_secs(), 90.0 * 60.0);
        assert_eq!(Lasting::Minutes(80).real_secs(), 10.0 * 60.0);
    }
}
