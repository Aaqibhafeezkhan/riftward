//! Active skills made of effects: First Aid binding a wound. `use <skill> [<someone>]` finds one
//! (`skills.rs`) and hands it to `ability`, which resolves it like any spell or blow
//! (`resolve.rs`): the user's skill (level plus attribute) against what the target resists it
//! with, or against a fixed difficulty, then its effects. Active skills that are like nothing
//! else keep their own commands (`pick`, `track`).
//!
//! In a fight, `use` is an action, so it takes the user's turn; out of one it happens at once,
//! and one aimed at a foe starts a fight.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use super::casting::MobMove;
use super::resolve::{Outcome, Use};
use super::states::HIDDEN;
use super::states::{ConditionDefs, Imbued, ItemConditions};
use super::strike::told;
use super::strike::{Fighters, Round};
use super::{Fighting, calm, fell};
use crate::attributes::Attribute;
use crate::command::{CommandRegistry, Invocation};
use crate::db::Database;
use crate::db::Role;
use crate::dice::Dice;
use crate::effects::{self, Effect, Who};
use crate::events::GameEvent;
use crate::healing::used_up;
use crate::items::{ItemQuery, ItemTemplates, Snapshot, capitalized};
use crate::mobs::MobTemplates;
use crate::output::{Outbox, Outgoing};
use crate::skills::{Cost, SkillDefs, SkillKind};
use crate::spells::{Aim, Aimed, SpellDefs};
use crate::stealth::can_see;
use crate::targeting::Keywords;
use crate::vitals::{Maxima, Resting};

/// What came of using an active skill, and the item it used up.
pub(super) struct Used {
    pub outcome: Outcome,
    pub item: Option<Entity>,
}

impl Round<'_, '_, '_, '_, '_, '_> {
    /// `user` uses the active skill `key` on `target`: the skill ready and paid for, the item it
    /// uses up found, then resolved and practised. A refusal says why, and nothing is spent.
    pub(super) fn ability(
        &mut self,
        user: Entity,
        target: Entity,
        key: &str,
    ) -> Result<Used, String> {
        let defs = self.defs;
        let skill = defs
            .skills
            .get(key)
            .ok_or("That skill isn't part of this world any more.")?;
        let SkillKind::Active {
            cost,
            cooldown,
            aim,
            uses,
            tool,
            resist,
            against,
            tags,
            effects,
            ..
        } = &skill.def.kind
        else {
            return Err(format!("{} isn't something you use.", skill.def.name));
        };
        // Checked at load: an active skill made of effects has its text.
        let text = skill.def.text.as_ref().ok_or_else(String::new)?;
        // Players don't rob each other, for now.
        let robs = effects.iter().any(|e| matches!(e, Effect::Take(_)));
        if robs && target != user && self.fighters.get(target).is_ok_and(|f| f.player.is_some()) {
            return Err("You can't steal from other players.".into());
        }
        self.needed(user, target, effects)?;
        // Hiding is plain to see in a fight.
        let hiding = effects.iter().find_map(|e| match e {
            Effect::Apply {
                condition,
                strength,
                lasts,
                on: Who::User,
            } if condition == HIDDEN => Some((strength, *lasts)),
            _ => None,
        });
        if hiding.is_some() && self.fighters.get(user).is_ok_and(|f| f.fighting.is_some()) {
            return Err("You can't hide in the middle of a fight!".into());
        }
        if let Some(tool) = tool
            && self.carried(user, tool).is_none()
        {
            return Err(format!("You need a {tool} for that."));
        }
        let item = match uses {
            Some(tag) => Some(
                self.carried(user, tag)
                    .ok_or(format!("You have no {tag}s."))?,
            ),
            None => None,
        };

        // Ready, and paid for.
        let now = self.now;
        let Ok(mut f) = self.fighters.get_mut(user) else {
            return Err(String::new());
        };
        let state = f
            .skills
            .0
            .get_mut(key)
            .ok_or_else(|| format!("You don't know how to do that ({}).", skill.def.name))?;
        if state.ready_at > now {
            return Err(format!(
                "You need a moment before you can try that again ({:.0}s).",
                (state.ready_at - now).ceil()
            ));
        }
        let power = state.level as i32 + f.attributes.get(skill.attribute);
        let luck = f.attributes.get(Attribute::Luck);
        if let Cost::Stamina(n) = *cost
            && f.vitals.stamina < n as f32
        {
            return Err("You're too tired. Rest a little first.".into());
        }
        if let Some(state) = f.skills.0.get_mut(key) {
            state.ready_at = now + cooldown;
        }
        if let Cost::Stamina(n) = *cost {
            f.vitals.stamina -= n as f32;
            self.events.write(GameEvent::StaminaSpent {
                who: user,
                amount: n as f32,
            });
        }

        // Hiding: everyone here rolls to notice, their Intuition against how well the user hid,
        // before anything is told, so only those who noticed see it. Seen by them all, it fails.
        let mut effects = effects.clone();
        if let Some((strength, lasts)) = hiding {
            let inputs = self.inputs(user, user, power, 1.0);
            let stealth = strength.amount(&inputs, self.dice, luck).max(1);
            let (seen_by, everyone) = self.notice(user, stealth, luck);
            if everyone {
                let name = capitalized(&self.name(user));
                let miss = text.miss.as_ref().unwrap_or(&text.hit);
                // Done to themselves, as `resolve` says it.
                let (mine, ..) = told(miss, &name, "yourself");
                let (.., seen) = told(miss, &name, "themselves");
                let room = self.fighters.get(user).map(|f| f.in_room.0);
                self.say(user, mine);
                if let Ok(room) = room {
                    self.tell_room(room, user, user, &seen);
                }
                self.practise(user, key);
                return Ok(Used {
                    outcome: Outcome::default(),
                    item,
                });
            }
            let lasts = lasts.lasts(now);
            if let Ok(mut f) = self.fighters.get_mut(user)
                && f.conditions.apply(self.conditions, HIDDEN, stealth, lasts)
                && let Some(hidden) = f.conditions.hidden_mut()
            {
                hidden.seen_by = seen_by;
            }
            effects
                .retain(|e| !matches!(e, Effect::Apply { condition, .. } if condition == HIDDEN));
        }

        let difficulty = against.as_ref().map(|against| {
            let inputs = self.inputs(user, target, power, 1.0);
            against.eval(&inputs, self.dice, 0)
        });
        let outcome = self.resolve(Use {
            user,
            target,
            rating: f64::from(power),
            resist: *resist,
            against: difficulty,
            tags: effects::tags(tags, &effects, self.conditions),
            blow: None,
            effects: &effects,
            power,
            text,
            opener: false,
            harmful: *aim == Aim::Foe,
            reacting: false,
        });
        self.practise(user, key);
        Ok(Used { outcome, item })
    }

    /// The active skills `mob` could usefully use now, fighting `foe`: made of effects and aimed
    /// at someone, ready, affordable, with any item they use up to hand, and doing some good, as
    /// a mob's spells must. Nothing that only makes sense out of a fight: hiding, stealing,
    /// finding or reading things.
    pub(super) fn mob_abilities(&self, mob: Entity, foe: Entity) -> Vec<MobMove> {
        let Ok(f) = self.fighters.get(mob) else {
            return Vec::new();
        };
        let most = Maxima::of(f.attributes, f.skills, self.defs, f.base).health;
        let hurt = f.vitals.health < most as f32 / 2.0;
        let mut usable = Vec::new();
        for (key, state) in &f.skills.0 {
            let Some(SkillKind::Active {
                command: None,
                aim,
                cost,
                uses,
                tool,
                effects,
                ..
            }) = self.defs.skills.get(key).map(|s| &s.def.kind)
            else {
                continue;
            };
            let target = match aim {
                Aim::Foe | Aim::Other => foe,
                Aim::Ally | Aim::Me => mob,
                Aim::Lock | Aim::Room | Aim::Item => continue,
            };
            let out_of_place = effects.iter().any(|e| match e {
                Effect::Take(_) | Effect::Find { .. } | Effect::Reveal(_) => true,
                Effect::Apply { condition, .. } => condition == HIDDEN,
                _ => false,
            });
            let needs = match cost {
                Cost::Free => 0.0,
                Cost::Stamina(n) => *n as f32,
            };
            let ready = state.ready_at <= self.now && f.vitals.stamina >= needs;
            let to_hand = [uses, tool]
                .into_iter()
                .flatten()
                .all(|tag| self.carried(mob, tag).is_some());
            if out_of_place || !ready || !to_hand || self.needed(mob, target, effects).is_err() {
                continue;
            }
            let useful = effects.iter().all(|effect| match effect {
                Effect::Heal { .. } => hurt,
                // Not twice: not on itself, nor on a foe that already has it.
                Effect::Apply { condition, on, .. } => {
                    let bearer = if *on == Who::User { mob } else { target };
                    self.fighters
                        .get(bearer)
                        .is_ok_and(|b| !b.conditions.has(condition))
                }
                _ => true,
            });
            if useful {
                usable.push(MobMove::Ability(key.clone(), target));
            }
        }
        usable
    }

    /// Whether there's anything for `effects` to do to `target`: one that only heals and cures
    /// isn't used on someone unhurt with nothing to cure.
    fn needed(&self, user: Entity, target: Entity, effects: &[Effect]) -> Result<(), String> {
        let mends = |e: &Effect| {
            matches!(
                e,
                Effect::Heal { .. } | Effect::Remove { .. } | Effect::Cure { .. }
            )
        };
        if !effects.iter().all(mends) {
            return Ok(());
        }
        let Ok(f) = self.fighters.get(target) else {
            return Ok(());
        };
        let most = Maxima::of(f.attributes, f.skills, self.defs, f.base).health;
        let hurt = f.vitals.health < most as f32;
        let curable = effects
            .iter()
            .filter(|e| e.on() == Who::Target)
            .any(|e| match e {
                Effect::Remove { condition, .. } => f.conditions.has(condition),
                Effect::Cure { tag, .. } => f.conditions.0.iter().any(|active| {
                    self.conditions
                        .get(&active.key)
                        .is_some_and(|def| def.tags.contains(tag))
                }),
                _ => false,
            });
        if hurt || curable {
            return Ok(());
        }
        Err(if target == user {
            "You aren't hurt.".into()
        } else {
            format!("{} isn't hurt.", capitalized(&self.name(target)))
        })
    }

    /// Everyone else here rolls to notice `who` hiding as well as `stealth`, with their `luck`:
    /// their Intuition against it, by opposed roll. Returns those who did, and whether that's all
    /// of them (and there was anyone to see).
    fn notice(&mut self, who: Entity, stealth: i32, luck: i32) -> (HashSet<Entity>, bool) {
        let Ok(room) = self.fighters.get(who).map(|f| f.in_room.0) else {
            return (HashSet::new(), false);
        };
        let watchers: Vec<(Entity, f64, i32)> = self
            .fighters
            .iter()
            .filter(|f| f.entity != who && f.in_room.0 == room && !f.dead)
            .map(|f| {
                (
                    f.entity,
                    f64::from(f.attributes.get(Attribute::Intuition).max(1)),
                    f.attributes.get(Attribute::Luck),
                )
            })
            .collect();
        let mut seen_by = HashSet::new();
        for &(watcher, intuition, their_luck) in &watchers {
            if self
                .dice
                .opposed(intuition, their_luck, f64::from(stealth), luck)
                .won
            {
                seen_by.insert(watcher);
            }
        }
        let everyone = !watchers.is_empty() && seen_by.len() == watchers.len();
        (seen_by, everyone)
    }

    /// An item `who` carries (in hand or in something they carry) with this tag.
    fn carried(&self, who: Entity, tag: &str) -> Option<Entity> {
        let snapshot = Snapshot::new(
            self.items.iter().map(|(e, i, l)| (e, i, *l)),
            self.templates,
        );
        self.items.iter().find_map(|(entity, item, _)| {
            let theirs = snapshot.root(entity).and_then(|l| l.holder()) == Some(who);
            let tagged = self
                .templates
                .0
                .get(&item.key)
                .is_some_and(|def| def.tags.iter().any(|t| t == tag));
            (theirs && tagged).then_some(entity)
        })
    }
}

/// The skill `steal` uses.
const PICKPOCKETING: &str = "thievery:pickpocketing";

/// `steal <someone>`: Pickpocketing, by the name players know. It's an active skill made of
/// effects (`thievery.skill.ron`), so this is `use pickpocketing`.
pub(super) fn steal(
    In(inv): In<Invocation>,
    mut commands: Commands,
    registry: Res<CommandRegistry>,
) {
    let Some((spec, id)) = registry.find("ability", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: format!("{PICKPOCKETING} {}", inv.args.trim()),
        },
    );
}

/// `ability <tree:skill> [<someone>]`: what `use` runs for an active skill made of effects.
#[allow(clippy::too_many_arguments)]
pub(super) fn ability(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut fighters: Fighters,
    items: ItemQuery,
    (templates, mobs): (Res<ItemTemplates>, Res<MobTemplates>),
    (defs, spells, condition_defs): (Res<SkillDefs>, Res<SpellDefs>, Res<ConditionDefs>),
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mut events: MessageWriter<GameEvent>,
    mut log: ResMut<Outbox>,
    (resting, time, story): (
        Query<Entity, With<Resting>>,
        Res<Time>,
        Res<crate::facts::Facts>,
    ),
    mut item_conditions: Query<(Entity, &mut ItemConditions)>,
) {
    let (key, words) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let Some(skill) = defs.skills.get(key) else {
        return;
    };
    // A lock is `lockwork`'s (`commands/doors.rs`), the room `roomwork`'s (`gathering.rs`) or
    // `trackwork`'s (`tracking.rs`), an item `itemwork`'s (`trade.rs`).
    let SkillKind::Active { aim, provokes, .. } = &skill.def.kind else {
        return;
    };
    if matches!(aim, Aim::Lock | Aim::Room | Aim::Item) {
        return;
    }
    let Ok(user) = fighters.get(inv.entity) else {
        return;
    };
    let room = user.in_room.0;
    let foe = user.fighting.map(|f| f.0);
    let candidates: Vec<(Entity, &Keywords)> = fighters
        .iter()
        .filter(|f| f.in_room.0 == room && !f.dead && can_see(inv.entity, f.entity, f.conditions))
        .filter_map(|f| Some((f.entity, f.keywords?)))
        .collect();
    let players: HashSet<Entity> = fighters
        .iter()
        .filter(|f| f.player.is_some())
        .map(|f| f.entity)
        .collect();
    let aimed = Aimed {
        aim: *aim,
        name: &skill.def.name,
        verb: "use",
        at: "on",
    };
    let target = match aimed.choose(inv.entity, foe, words, candidates, |e| players.contains(&e)) {
        Ok(target) => target,
        Err(refusal) => {
            log.0.push(Outgoing::line(inv.entity, refusal));
            return;
        }
    };

    let watchers: Vec<(Entity, Entity)> = fighters
        .iter()
        .filter(|f| f.player.is_some())
        .map(|f| (f.entity, f.in_room.0))
        .collect();
    let targets: HashMap<Entity, Entity> = fighters
        .iter()
        .filter_map(|f| Some((f.entity, f.fighting?.0)))
        .collect();
    let resting: HashSet<Entity> = resting.iter().collect();
    let none = HashMap::new();
    let mut imbued = Imbued::gather(item_conditions.iter());
    let mut round = Round {
        fighters: &mut fighters,
        items: &items,
        templates: &templates,
        mobs: &mobs,
        defs: &defs,
        spells: &spells,
        conditions: &condition_defs,
        dice: &mut dice,
        db: &db,
        story: Some(&story),
        events: &mut events,
        log: &mut log,
        watchers: &watchers,
        engaged: &targets,
        targets: &targets,
        auras: &HashMap::new(),
        resting: &resting,
        outmatched: &none,
        ambushers: &HashSet::new(),
        now: time.elapsed_secs_f64(),
        imbued: &mut imbued,
    };
    let used = round.ability(inv.entity, target, key);
    imbued.write_back(&mut item_conditions);
    let used = match used {
        Ok(used) => used,
        Err(refusal) => {
            if !refusal.is_empty() {
                log.0.push(Outgoing::line(inv.entity, refusal));
            }
            return;
        }
    };
    // Used up, whether or not it worked.
    if let Some(entity) = used.item
        && let Ok((_, item, _)) = items.get(entity)
    {
        used_up(&mut commands, &db, entity, item);
    }
    let died = &used.outcome.died;
    for &fallen in died {
        let slayer = if fallen == inv.entity {
            target
        } else {
            inv.entity
        };
        fell(&mut commands, fallen, slayer, &targets);
    }
    // Caught at it, a mob turns on them; players don't fight each other.
    let mob = fighters
        .get(target)
        .is_ok_and(|f| f.player.is_none() && !f.dead);
    if *provokes && !used.outcome.hit && mob && target != inv.entity {
        if !targets.contains_key(&target) {
            commands.entity(target).insert(Fighting(inv.entity));
        }
        commands.entity(inv.entity).remove::<Resting>();
    }
    // Turned on a foe, it's a fight.
    if *aim == Aim::Foe && !died.contains(&target) && !died.contains(&inv.entity) {
        if !targets.contains_key(&target) {
            commands.entity(target).insert(Fighting(inv.entity));
        }
        if !targets.contains_key(&inv.entity) {
            commands
                .entity(inv.entity)
                .insert(Fighting(target))
                .remove::<Resting>();
        }
    }
    if used.outcome.calmed {
        calm(&mut commands, target, &targets);
    }
}
