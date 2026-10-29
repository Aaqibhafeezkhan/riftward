//! One way to resolve anything done to someone in a fight: a plain blow, a technique, a spell.
//!
//! Whoever's doing it works out their own side first (`strike.rs` chooses the weapon and pays for
//! the technique; `casting.rs` checks the school and pays the Drain), then hands a [`Use`] here.
//! From there it's all the same: effects on the user, the contest against whatever the target
//! resists with (plus their skills made for it, by tag), and on a win the blow's damage and every
//! effect, through armour, resistances and wards, told in the user's words. A blow resisted by
//! evasion is dodged or blocked; anything else resisted just misses.

use bevy::prelude::*;

use super::states::{ConditionDefs, Conditions};
use super::strike::{Blow, Round, Telling, defence_rating, gear, harm, shield_block, told};
use crate::attributes::Attribute;
use crate::effects::{self, Effect, Only, Reaction, Resist, Taken, When, Who};
use crate::events::GameEvent;
use crate::items::capitalized;
use crate::mobs::MobTemplates;
use crate::money;
use crate::skills::{SkillDefs, SkillKind, SkillText, Skills, Trigger, boosts, fill};
use crate::vitals::Maxima;
use crate::world::{MobDef, MobKey};

/// A blow's own damage, rolled when it lands: the weapon's or attack's, plus what the user adds.
pub(super) struct BlowDamage {
    pub damage: (u32, u32),
    /// Strength, passive bonuses and the technique's own extra.
    pub bonus: f64,
    pub damage_type: String,
    /// Armour the user's skill gets past with this attack.
    pub pierce: f64,
}

/// Something done to someone, ready to resolve.
pub(super) struct Use<'a> {
    pub user: Entity,
    pub target: Entity,
    /// The user's side of the contest: an attack rating, or a caster's power.
    pub rating: f64,
    /// The target's side; without, it always lands, unless it's `against` a difficulty.
    pub resist: Option<Resist>,
    /// A fixed difficulty to beat instead of the target's resistance, rolled without Luck on its
    /// side: how bad a wound is to bind.
    pub against: Option<f64>,
    /// What it is (`effects::tags`), for the target's skills that resist it.
    pub tags: Vec<String>,
    /// A blow's damage, for an attack or a technique that strikes to hurt.
    pub blow: Option<BlowDamage>,
    pub effects: &'a [Effect],
    /// A caster's power, a third of which adds to damage, healing and empowered conditions;
    /// nothing for a blow.
    pub power: i32,
    pub text: &'a SkillText,
    /// Only works on the unaware: they defend at half, and from hiding it can't miss.
    pub opener: bool,
    /// Told to the target in red: an attack, not a blessing.
    pub harmful: bool,
    /// A reaction being resolved, which sets off no more.
    pub reacting: bool,
}

/// A reaction found on someone, with the strength or skill it comes from: (reaction, strength,
/// power, skill).
pub type Found = (Reaction, i32, i32, Option<String>);

/// Every reaction a character has, from their conditions (an item's among them) and their passive
/// skills.
pub fn reactions_of(
    conditions: &Conditions,
    skills: &Skills,
    condition_defs: &ConditionDefs,
    defs: &SkillDefs,
) -> Vec<Found> {
    let mut found = Vec::new();
    for active in &conditions.0 {
        if let Some(def) = condition_defs.get(&active.key) {
            for r in &def.reactions {
                found.push((r.clone(), active.strength, 0, None));
            }
        }
    }
    for (key, state) in &skills.0 {
        if let Some(SkillKind::Passive { reactions, .. }) =
            defs.skills.get(key).map(|s| &s.def.kind)
        {
            for r in reactions {
                found.push((r.clone(), 1, state.level as i32, Some(key.clone())));
            }
        }
    }
    found
}

/// What came of it.
#[derive(Debug, Default)]
pub(super) struct Outcome {
    pub hit: bool,
    /// Missed because the target got out of the way (not blocked, not resisted otherwise): what
    /// a riposte answers.
    pub dodged: bool,
    pub died: Vec<Entity>,
    /// An animal it calmed.
    pub calmed: bool,
}

impl Round<'_, '_, '_, '_, '_, '_> {
    pub(super) fn resolve(&mut self, u: Use) -> Outcome {
        let mut out = Outcome::default();
        let (user, target) = (u.user, u.target);
        let Ok(a) = self.fighters.get(user) else {
            return out;
        };
        let room = a.in_room.0;
        let a_name = super::name_of(a.player, a.mob);
        let a_numbers = !a.hide_numbers;
        let a_luck = a.attributes.get(Attribute::Luck);
        let a_times = a.conditions.damage_dealt(self.conditions);
        let d_name = self.name(target);
        let mut tell = Telling::default();

        // What it does to the user, it does whether or not it lands.
        self.affect(
            user,
            target,
            u.effects,
            Who::User,
            u.power,
            1.0,
            u.reacting,
            &mut tell,
        );

        let Ok(d) = self.fighters.get(target) else {
            return out;
        };
        let d_luck = d.attributes.get(Attribute::Luck);
        let d_numbers = !d.hide_numbers;
        let d_most = Maxima::of(d.attributes, d.skills, self.defs, d.base).health;
        let d_mob = d.mob.map(|m| m.key.clone());
        let d_skills = d.skills.clone();
        let d_boosts = boosts(d.skills, self.defs);
        let aura = self.auras.get(&target).copied().unwrap_or_default();
        let d_gear = gear(target, self.items, self.templates);
        let parts = defence_rating(d.attributes, aura, &d_boosts, &d_gear);
        // Someone sitting down, or caught unawares by an opener, is much easier to hit.
        let caught = self.resting.contains(&target) || u.opener;
        let halved = |n: f64| if caught { n / 2.0 } else { n };
        // Skills and traits made for it help resist what it is: an elf against sleep.
        let rating = u.resist.map(|resist| {
            let base = match resist {
                Resist::Evasion => halved(d.conditions.defence(parts.total(), self.conditions)),
                Resist::Dodge => halved(d.conditions.defence(parts.evasion, self.conditions)),
                Resist::Attribute(attribute) => f64::from(d.attributes.get(attribute)),
            };
            base + d_boosts.resisting(&u.tags) + d.conditions.withstanding(self.conditions, &u.tags)
        });

        // The user's rating against the target's, by opposed roll; how decisively it wins is how
        // hard it lands. An opener from hiding can't be dodged: Backstab from the shadows lands.
        let theirs = match (rating, u.against) {
            (Some(rating), _) => Some((rating.max(1.0), d_luck)),
            (None, Some(difficulty)) => Some((difficulty.max(1.0), 0)),
            (None, None) => None,
        };
        let (hit, blow, margin) = match theirs {
            None => (true, Blow::Solid, 1.0),
            Some((theirs, their_luck)) => {
                let contest = self
                    .dice
                    .opposed(u.rating.max(1.0), a_luck, theirs, their_luck);
                let ambush = u.opener && self.ambushers.contains(&user);
                match (ambush, contest.won) {
                    (true, false) => (true, Blow::Solid, contest.margin),
                    (_, won) => (won, Blow::of(contest.margin), contest.margin),
                }
            }
        };

        // For `Hurt` reactions: how hurt the target was before.
        let health_before = self.fighters.get(target).map_or(0.0, |f| f.vitals.health);

        // Done to themselves, they're "yourself" to the user and "themselves" to the room.
        let own = user == target;
        let (to_me, to_room) = if own {
            ("yourself".to_string(), "themselves".to_string())
        } else {
            (d_name.clone(), d_name.clone())
        };
        if hit {
            out.hit = true;
            // Every source of damage, each through armour and resistances: the blow, and any
            // damage it does besides. Conditions on the user move it, as does the win.
            let pierce = effects::pierce(u.effects);
            let ignores_armour = u.effects.contains(&Effect::IgnoreArmour);
            let times = blow.multiplier() * a_times;
            let inputs = self.inputs(user, target, u.power, margin);
            let mut forces = Vec::new();
            if let Some(b) = &u.blow {
                let rolled = self.roll(a_luck, b.damage);
                let force = f64::from(rolled) + b.bonus;
                forces.push((force, b.damage_type.clone(), b.pierce + f64::from(pierce)));
            }
            for effect in u.effects {
                if let Effect::Damage {
                    amount,
                    damage_type,
                } = effect
                {
                    let force = amount.amount(&inputs, self.dice, a_luck);
                    forces.push((f64::from(force), damage_type.clone(), f64::from(pierce)));
                }
            }
            let mut dealt: Option<i32> = None;
            for (force, kind, pierce) in forces {
                let force = force * times;
                let hurt = self.mitigate(target, force, &kind, pierce, ignores_armour);
                dealt = Some(dealt.unwrap_or(0) + hurt);
            }
            // A ward takes what it can, told after what it stopped.
            let mut after = Telling::default();
            let mut warded = 0;
            if let Some(dealt) = dealt.as_mut() {
                warded = self.ward_off(target, user, *dealt, &d_name, &mut after);
                *dealt -= warded;
            }
            let mut healed: Option<i32> = None;
            for effect in u.effects {
                if let Effect::Heal { amount } = effect {
                    let rolled = amount.amount(&inputs, self.dice, a_luck).max(0);
                    let Ok(mut f) = self.fighters.get_mut(target) else {
                        continue;
                    };
                    let most = Maxima::of(&f.attributes, &f.skills, self.defs, f.base).health;
                    let before = f.vitals.health;
                    f.vitals.health = (before + rolled as f32).min(most as f32);
                    let gained = (f.vitals.health - before).round() as i32;
                    healed = Some(healed.unwrap_or(0) + gained);
                }
            }

            // Coins lifted from their purse, up to what's in it.
            let mut taken: Option<u64> = None;
            for effect in u.effects {
                if let Effect::Take(Taken::Coins(coins)) = effect {
                    let wanted = coins.amount(&inputs, self.dice, a_luck).max(0) as u64;
                    let lifted = self.fighters.get_mut(target).ok().map_or(0, |mut f| {
                        f.purse.as_mut().map_or(0, |purse| {
                            let lifted = wanted.min(purse.0);
                            purse.0 -= lifted;
                            lifted
                        })
                    });
                    if let Ok(mut f) = self.fighters.get_mut(user) {
                        let character = f.player.map(|p| p.character_id);
                        if let Some(purse) = f.purse.as_mut() {
                            purse.0 += lifted;
                            purse.save(character, self.db);
                        }
                    }
                    taken = Some(taken.unwrap_or(0) + lifted);
                }
            }
            let haul = taken.map_or_else(String::new, money::coins);

            // Told in its own words, with how much it hurt: "Your slash decimates the rat." A
            // crushing blow ends with a bang.
            let with = [
                ("actor", a_name.as_str()),
                ("harm", dealt.map_or("", |n| harm(n, d_most))),
                ("coins", haul.as_str()),
            ];
            let say = |template: &str, target: &str| {
                let mut with = with.to_vec();
                with.push(("target", target));
                let line = fill(template, &with);
                match (dealt.is_some(), blow, line.strip_suffix('.')) {
                    (true, Blow::Crushing, Some(line)) => format!("{line}!"),
                    _ => line,
                }
            };
            let count = |shown: bool| match (shown, dealt, healed) {
                (true, Some(n), _) => format!(" ({n})"),
                (true, _, Some(n)) => format!(" (+{n})"),
                _ => String::new(),
            };
            let lines = &u.text.hit;
            tell.lines.push((
                user,
                format!("{}{}", say(&lines.actor, &to_me), count(a_numbers)),
            ));
            if !own && let Some(theirs) = &lines.target {
                let theirs = format!("{}{}", say(theirs, &d_name), count(d_numbers));
                tell.lines.push((
                    target,
                    if u.harmful {
                        format!("\x1b[31m{theirs}\x1b[0m")
                    } else {
                        theirs
                    },
                ));
            }
            tell.room.push(say(&lines.room, &to_room));
            tell.lines.append(&mut after.lines);
            tell.room.append(&mut after.room);
            if u.blow.is_some() && dealt == Some(0) && warded == 0 {
                let line = format!("The blow glances off {d_name}'s armour.");
                tell.lines.push((user, line.clone()));
                tell.lines
                    .push((target, "The blow glances off your armour.".into()));
                tell.room.push(line);
            }
            if dealt.is_some() && d_gear.reduction > 0 {
                self.events.write(GameEvent::HitInArmour { who: target });
            }
            if let Some(hurt) = dealt.filter(|n| *n > 0)
                && self.wound(target, hurt, &mut tell, user, &d_name, room, d_mob)
            {
                out.died.push(target);
            }
            // What it does to the target, if they're still standing, in the conditions' words.
            if !out.died.contains(&target) {
                out.calmed = self.affect(
                    user,
                    target,
                    u.effects,
                    Who::Target,
                    u.power,
                    margin,
                    u.reacting,
                    &mut tell,
                );
            }
            if let Some(hurt) = dealt.filter(|n| *n > 0)
                && u.effects.contains(&Effect::Cleave)
            {
                self.cleave(user, target, hurt, room, &a_name, &mut tell, &mut out.died);
            }
        } else if u.resist == Some(Resist::Evasion) {
            // The miss is the shield's in proportion to its share of the defence, and otherwise
            // a dodge. Either is told in the words of the defender's skill for it, if they have
            // one: a shield untrained says so plainly, and a miss nobody dodged is just a miss.
            let share = parts.block / parts.total().max(1.0);
            let blocked = parts.block > 0.0 && self.dice.chance(0, share);
            let words = |trigger: Trigger| {
                d_skills.0.keys().find_map(|key| {
                    let skill = self.defs.skills.get(key)?;
                    match &skill.def.kind {
                        SkillKind::Passive { triggers, .. } if triggers.contains(&trigger) => {
                            skill.def.text.as_ref().map(|t| t.hit.clone())
                        }
                        _ => None,
                    }
                })
            };
            let avoided = if blocked {
                Some(words(Trigger::Blocked).unwrap_or_else(shield_block))
            } else {
                words(Trigger::Dodged)
            };
            if let Some(avoided) = avoided {
                // The defender is the one using the skill.
                let (theirs, mine, seen) = told(&avoided, &d_name, &a_name);
                tell.lines.push((user, mine));
                tell.lines.push((target, theirs));
                tell.room.push(seen);
            } else {
                let miss = u
                    .text
                    .miss
                    .clone()
                    .or_else(|| super::strike::untrained().miss)
                    .unwrap_or_else(|| u.text.hit.clone());
                let (mine, theirs, seen) = told(&miss, &a_name, &d_name);
                tell.lines.push((user, mine));
                tell.lines.push((target, theirs));
                tell.room.push(seen);
            }
            self.events.write(if blocked {
                GameEvent::Blocked { who: target }
            } else {
                GameEvent::Dodged { who: target }
            });
            out.dodged = !blocked;
        } else {
            let miss = u.text.miss.as_ref().unwrap_or(&u.text.hit);
            let (mine, theirs, seen) = told(miss, &a_name, &d_name);
            tell.lines.push((user, mine));
            tell.lines.push((target, theirs));
            tell.room.push(seen);
            if u.resist == Some(Resist::Dodge) {
                self.events.write(GameEvent::Dodged { who: target });
            }
        }
        self.tell(tell, room, user, target);

        // What the bearers of reactions do about it, told after it: thorns tearing at whoever
        // struck. Reactions set off no more.
        if !u.reacting {
            let mut reactions = Vec::new();
            if out.hit && u.harmful && user != target {
                reactions.push((target, user, When::Struck));
                reactions.push((user, target, When::Hits));
                let health = self.fighters.get(target).map_or(0.0, |f| f.vitals.health);
                for share in self.hurt_shares(target) {
                    let line = share * d_most as f32;
                    if health_before >= line && health < line && health > 0.0 {
                        reactions.push((target, user, When::Hurt(f64::from(share))));
                    }
                }
            }
            if out.died.contains(&target) {
                if user != target {
                    reactions.push((user, target, When::Kills));
                }
                reactions.push((target, user, When::Dies));
            }
            if out.dodged {
                reactions.push((target, user, When::Dodges));
            }
            for (bearer, other, when) in reactions {
                for died in self.react(bearer, other, when) {
                    if !out.died.contains(&died) {
                        out.died.push(died);
                    }
                }
            }
        }
        out
    }

    /// Whether `who` counts `other` an ally: for a mob, its own kind and those its `allies` name;
    /// for a player, other players.
    pub(super) fn allied(&self, who: Entity, other: Entity) -> bool {
        // A player, or a mob's template key.
        let kind = |e: Entity| {
            self.fighters.get(e).ok().map(|f| match f.player {
                Some(_) => None,
                None => f.mob.map(|m| m.key.clone()),
            })
        };
        let (Some(a), Some(b)) = (kind(who), kind(other)) else {
            return false;
        };
        fn def<'k>(
            mobs: &'k MobTemplates,
            key: &'k Option<MobKey>,
        ) -> Option<(&'k MobKey, &'k MobDef)> {
            key.as_ref().and_then(|k| Some((k, mobs.0.get(k)?)))
        }
        match (&a, def(self.mobs, &a)) {
            (None, _) => b.is_none(),
            (Some(_), Some((key, own))) => own.allied(key, def(self.mobs, &b)),
            (Some(_), None) => false,
        }
    }

    /// The `Hurt` shares `who`'s reactions watch for.
    fn hurt_shares(&self, who: Entity) -> Vec<f32> {
        self.reactions_of(who)
            .into_iter()
            .filter_map(|(r, ..)| match r.on {
                When::Hurt(share) => Some(share as f32),
                _ => None,
            })
            .collect()
    }

    /// Every reaction `who` has; see [`reactions_of`].
    fn reactions_of(&self, who: Entity) -> Vec<Found> {
        self.fighters.get(who).map_or_else(
            |_| Vec::new(),
            |f| reactions_of(f.conditions, f.skills, self.conditions, self.defs),
        )
    }

    /// Set off a room's conditions' reactions to `when` on `who`, there: brambles tearing at
    /// whoever comes in. The place has no hands, so it's resolved as `who`'s own, told in the
    /// reaction's `actor` line to them and its `room` line to everyone else. Returns who died.
    pub(super) fn react_to_room(
        &mut self,
        who: Entity,
        when: When,
        room: &[(String, i32)],
    ) -> Vec<Entity> {
        let found = room
            .iter()
            .filter_map(|(key, strength)| self.conditions.get(key).map(|def| (def, *strength)))
            .flat_map(|(def, strength)| {
                def.reactions
                    .iter()
                    .map(move |r| (r.clone(), strength, 0, None))
            })
            .collect();
        self.set_off(who, who, when, found)
    }

    /// Set off `bearer`'s reactions to `when`, at `other`. Returns who they killed.
    pub(super) fn react(&mut self, bearer: Entity, other: Entity, when: When) -> Vec<Entity> {
        let found = self.reactions_of(bearer);
        self.set_off(bearer, other, when, found)
    }

    /// Set off one condition's own reactions to `when` (taking hold, running out) on `bearer`, at
    /// `other`, whether or not they still have it. Returns who they killed.
    pub(super) fn react_as(
        &mut self,
        bearer: Entity,
        other: Entity,
        (condition, strength): (&str, i32),
        when: When,
    ) -> Vec<Entity> {
        let found = self.conditions.get(condition).map_or_else(Vec::new, |def| {
            def.reactions
                .iter()
                .map(|r| (r.clone(), strength, 0, None))
                .collect()
        });
        self.set_off(bearer, other, when, found)
    }

    fn set_off(
        &mut self,
        bearer: Entity,
        other: Entity,
        when: When,
        found: Vec<Found>,
    ) -> Vec<Entity> {
        let mut died = Vec::new();
        for (reaction, strength, power, skill) in found {
            if reaction.on != when {
                continue;
            }
            // Not from beyond the grave but for a dying, and not at the dead but for a killing or
            // a dying.
            let standing = |e: Entity| {
                self.fighters
                    .get(e)
                    .is_ok_and(|f| !f.dead && f.vitals.health > 0.0)
            };
            let dying = when == When::Dies;
            if (!dying && !standing(bearer)) || (!dying && when != When::Kills && !standing(other))
            {
                break;
            }
            if reaction.only == Some(Only::Foes) && self.allied(bearer, other) {
                continue;
            }
            if let Some(chance) = &reaction.chance {
                let inputs = self.inputs(bearer, other, power, 1.0);
                let p = chance
                    .with_strength(f64::from(strength))
                    .eval(&inputs, self.dice, 0);
                if !self.dice.chance(0, p) {
                    continue;
                }
            }
            let effects: Vec<Effect> = reaction
                .effects
                .iter()
                .map(|e| e.with_strength(strength))
                .collect();
            let harmful = effects.iter().any(|e| matches!(e, Effect::Damage { .. }));
            let text = SkillText {
                hit: reaction.text.clone(),
                miss: None,
            };
            let outcome = self.resolve(Use {
                user: bearer,
                target: other,
                rating: 0.0,
                resist: None,
                against: None,
                tags: Vec::new(),
                blow: None,
                effects: &effects,
                power,
                text: &text,
                opener: false,
                harmful,
                reacting: true,
            });
            died.extend(outcome.died);
            if let Some(skill) = skill {
                self.practise(bearer, &skill);
            }
        }
        died
    }

    /// A roll between `low` and `high`, either way about.
    fn roll(&mut self, luck: i32, (low, high): (u32, u32)) -> i32 {
        let (low, high) = (low.min(high), low.max(high));
        self.dice.range(luck, i64::from(low)..=i64::from(high)) as i32
    }

    /// Cleave: what landed carries on into another foe fighting the user, even when it felled
    /// the first.
    #[allow(clippy::too_many_arguments)]
    fn cleave(
        &mut self,
        user: Entity,
        first: Entity,
        dealt: i32,
        room: Entity,
        a_name: &str,
        tell: &mut Telling,
        died: &mut Vec<Entity>,
    ) {
        let second = self.targets.iter().find_map(|(who, whom)| {
            (*whom == user && *who != first && !died.contains(who))
                .then_some(*who)
                .filter(|who| {
                    self.fighters
                        .get(*who)
                        .is_ok_and(|f| f.in_room.0 == room && !f.dead)
                })
        });
        let Some(second) = second else {
            return;
        };
        let second_name = self.name(second);
        let second_mob = self
            .fighters
            .get(second)
            .ok()
            .and_then(|f| f.mob.map(|m| m.key.clone()));
        tell.lines.push((
            user,
            format!("Your blow carries on into {second_name}! ({dealt})"),
        ));
        tell.lines.push((
            second,
            format!(
                "\x1b[31m{}'s blow carries on into you! ({dealt})\x1b[0m",
                capitalized(a_name)
            ),
        ));
        tell.room.push(format!(
            "{}'s blow carries on into {second_name}.",
            capitalized(a_name)
        ));
        if self.wound(second, dealt, tell, user, &second_name, room, second_mob) {
            died.push(second);
        }
    }
}
