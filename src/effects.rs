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
use crate::time::REAL_SECS_PER_GAME_DAY;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Effect {
    /// Damage of a type (`assets/damage_types.ron`), landing harder the more decisively it won.
    /// Armour takes its share only of what armour stops; resistances and weaknesses count.
    Damage {
        amount: (u32, u32),
        #[serde(rename = "type", default = "crate::world::physical")]
        damage_type: String,
    },
    /// Health back, up to the most.
    Heal { amount: (u32, u32) },
    /// Puts a condition (`assets/conditions`) on someone, at `strength`, for as long as it
    /// `lasts`. `empowered` adds a third of the caster's power to the strength: a stronger
    /// caster's ward takes more.
    Apply {
        condition: String,
        #[serde(default = "one")]
        strength: i32,
        lasts: Lasting,
        #[serde(default)]
        on: Who,
        #[serde(default)]
        empowered: bool,
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
    /// This much of the target's armour doesn't help: a point finding a gap.
    Pierce(i32),
    /// The target's armour doesn't help at all.
    IgnoreArmour,
    /// The blow carries on into a second foe fighting the user.
    Cleave,
    /// An animal stops fighting and stops attacking those who come near.
    Calm,
}

fn one() -> i32 {
    1
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
}

impl Lasting {
    /// The same time in real seconds; nothing for turns.
    pub fn real_secs(self) -> f64 {
        let minutes = match self {
            Lasting::Minutes(n) => n,
            Lasting::Hours(n) => n * 60,
            Lasting::Turns(_) => 0,
        };
        f64::from(minutes) * REAL_SECS_PER_GAME_DAY as f64 / (24.0 * 60.0)
    }

    /// How long a condition put on at `now` (seconds since the server started) lasts.
    pub fn lasts(self, now: f64) -> Lasts {
        match self {
            Lasting::Turns(n) => Lasts::Turns(n.max(1)),
            time => Lasts::Until(now + time.real_secs()),
        }
    }
}

impl Effect {
    /// Whom it's on.
    pub fn on(&self) -> Who {
        match self {
            Effect::Apply { on, .. } | Effect::Remove { on, .. } | Effect::Cure { on, .. } => *on,
            _ => Who::Target,
        }
    }

    /// The condition it puts on or takes off, if any, so tests can check it's a real one.
    #[cfg(test)]
    pub fn condition(&self) -> Option<&str> {
        match self {
            Effect::Apply { condition, .. } | Effect::Remove { condition, .. } => Some(condition),
            _ => None,
        }
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
                Damage(amount: (2, 5)),
                Cure(tag: "poison"),
                Pierce(2),
            ]"#,
        )
        .unwrap();
        assert_eq!(
            effects[0],
            Effect::Apply {
                condition: "staggered".into(),
                strength: 1,
                lasts: Lasting::Turns(1),
                on: Who::Target,
                empowered: false,
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
                strength: 1,
                lasts: Lasting::Turns(2),
                on: Who::Target,
                empowered: false,
            },
            Effect::Damage {
                amount: (1, 2),
                damage_type: "force".into(),
            },
        ];
        assert_eq!(
            tags(&["charm".into()], &effects, &conditions),
            ["charm", "force", "mind", "sleep"]
        );
    }

    #[test]
    fn game_time_lasts_in_real_time() {
        assert_eq!(Lasting::Hours(12).real_secs(), 90.0 * 60.0);
        assert_eq!(Lasting::Minutes(80).real_secs(), 10.0 * 60.0);
    }
}
