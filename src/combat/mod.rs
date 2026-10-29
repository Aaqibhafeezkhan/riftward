//! Combat: fights between players and mobs, made entirely of skills.
//!
//! Fights run in rounds, every `round_seconds` (`rules.ron`). Each round, everyone fighting acts once, in order of
//! initiative (Reaction + Intuition, rolled): a player's next queued action (see `ActionQueue`), or
//! otherwise their auto-attack, an attack skill they choose with `autoattack` (the basic attack,
//! Brawling, until they do). Mobs fight with attack skills from their templates, by the same rules.
//!
//! An attack pits the attack skill (level + its attribute) against the defender's Reaction plus
//! Dodge, as a ratio, each side with its Luck. Someone caught resting defends at half, and spends
//! their first turn getting up. Mobs fight better under an aura (see `SkillKind::Aura`): while the
//! Rat King lives, every sewer rat in the sewers is tougher. A hit does the skill's damage plus Strength / 2.
//! At 0 health a mob dies and leaves a corpse; a player wakes at the Wayside Shrine, hurt, and
//! everything they carried stays on their corpse where they fell.
//!
//! Players can't fight each other yet.

mod abilities;
mod casting;
mod effects;
mod fleeing;
mod help;
mod resolve;
mod rounds;
mod spawning;
mod states;
mod strike;
mod triggers;

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;

use crate::attributes::{Attribute, Attributes};
use crate::command::{
    self, ActionQueue, AddCommand, Category, CommandRegistry, CommandSpec, Invocation,
};
use crate::commands::movement::{View, travel};
use crate::db::{Database, ItemPlace};
use crate::dice::Dice;
use crate::effects::When;
use crate::events::GameEvent;
use crate::groups::{self, Groups, InGroup, NoAssist};
use crate::items::{
    self, ItemLocation, ItemMutQuery, ItemQuery, ItemTemplates, Quality, capitalized,
};
use crate::mobs::{self, Mob, MobQuery, MobTemplates};
use crate::money::{self, Purse};
use crate::net::Connection;
use crate::output::{Actor, Characters, Outbox, Outgoing, PuppetedBy, RoomQuery, deliver};
use crate::skills::{Bonus, Practice, SkillDefs, SkillKind, Skills, boosts};
use crate::spawns::{SpawnedBy, Spawns};
use crate::spells::{Aim, Casting, SpellDefs};
use crate::stealth::{reveal, sees};
use crate::targeting::{Keywords, Target};
use crate::time::heartbeat;
use crate::vitals::{Maxima, Resting, Vitals};
use crate::world::{InRoom, ItemKey, MobKey, Player, RoomIndex, RoomKey, without_article};

pub use casting::MobMove;
pub use rounds::Rounds;
#[cfg(test)]
pub use rounds::takes_in;
#[cfg(test)]
pub use states::defs_on_disk;
pub use states::{
    ConditionDefs, Conditions, FORMLESS, HIDDEN, INANIMATE, Imbued, ItemCondition, ItemConditions,
    Lasts, Modifier,
};
pub use strike::{Blow, Fighters, Move, Round, name_of};

/// Everyone's most basic attack, used until they choose another.
pub const BASIC_ATTACK: &str = "brawling:basics";

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(states::ConditionsPlugin)
            .init_resource::<Encounters>()
            .init_resource::<WantedLoot>()
            .add_message::<triggers::Trigger>()
            .add_systems(Update, expire_conditions.run_if(heartbeat()))
            .add_systems(Update, expire_readied.run_if(heartbeat()))
            .add_systems(
                Update,
                (
                    // Aggressive mobs pick their fights just before each round, and groups
                    // join their members'.
                    (aggression, assist, combat_round).chain().run_if(on_timer(
                        Duration::from_secs_f64(crate::rules::rules().round_seconds),
                    )),
                    // Then what conditions and skills do about what happened, out of the round.
                    (triggers::triggers, triggers::react).chain(),
                    // Those who heard a shout for help come running.
                    help::answer_calls,
                    // Those an effect brought into being arrive.
                    spawning::spawn_from_effects,
                    // And those an effect told to run, run.
                    fleeing::flee_from_effects,
                    abandon_fights,
                    handle_deaths,
                )
                    .chain()
                    .after(command::dispatch),
            )
            .add_command(
                CommandSpec::new("kill", Category::Interaction)
                    .shortcuts(&["attack", "k"])
                    .usage("kill <target>")
                    .summary("attack something; you fight each round until one of you falls")
                    .quiet()
                    .action(),
                kill,
            )
            .add_command(
                CommandSpec::new("flee", Category::Interaction)
                    .summary("try to escape a fight through a random exit (costs stamina)")
                    .action(),
                flee,
            )
            .add_command(
                CommandSpec::new("autoattack", Category::Character)
                    .usage("autoattack [<skill>]")
                    .summary("see or choose the attack you make each round of a fight"),
                autoattack,
            )
            .add_command(
                CommandSpec::new("numbers", Category::Character)
                    .usage("numbers [on | off]")
                    .summary("show or hide damage numbers in fights"),
                numbers,
            )
            .add_command(
                CommandSpec::new("affects", Category::Character)
                    .shortcuts(&["aff"])
                    .summary(
                        "see what's affecting you, like a blessing or a ward, and for how long",
                    ),
                affects,
            )
            .add_command(
                CommandSpec::new("stop", Category::Character)
                    .summary("forget the actions you've queued up"),
                stop,
            )
            .add_command(
                CommandSpec::new("steal", Category::Interaction)
                    .usage("steal <someone>")
                    .summary("lift coins from someone's purse, if you know Pickpocketing")
                    .action(),
                abilities::steal,
            )
            // Run by `use` for an active skill made of effects; players don't type it.
            .add_command(
                CommandSpec::new("ability", Category::Interaction)
                    .usage("use <skill> [<someone>]")
                    .summary("use an active skill on yourself or someone here")
                    .quiet()
                    .unlisted(),
                abilities::ability,
            )
            // Run by `use` for a technique; players don't type it themselves.
            .add_command(
                CommandSpec::new("technique", Category::Interaction)
                    .usage("use <technique> [<target>]")
                    .summary("make a technique: in a fight, or an opener to start one")
                    .quiet()
                    .unlisted(),
                technique,
            );
    }
}

/// A thing, not a creature (`inanimate` on its template): it never acts in a fight, is worth no
/// experience, and leaves no corpse.
#[derive(Component, Debug)]
pub struct Inanimate;

/// Who someone is fighting.
#[derive(Component, Debug, Clone, Copy)]
pub struct Fighting(pub Entity);

/// Just set upon, and fighting back, but not yet having had a turn: in the first round they act
/// after whoever attacked them (`first_strike`).
#[derive(Component, Debug)]
pub struct DrawnIn;

/// The attack someone makes each round: `None` means the basic attack.
#[derive(Component, Debug, Clone, Default)]
pub struct AutoAttack(pub Option<String>);

/// A player who'd rather fights didn't show damage numbers: the words say enough.
#[derive(Component, Debug)]
pub struct HideNumbers;

/// A mob that attacks those who come near: anywhere, or only in its home room (`Home`).
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Aggressive {
    pub home: Option<Entity>,
}

impl Aggressive {
    /// Aggressive wherever it is.
    pub const ANYWHERE: Aggressive = Aggressive { home: None };
}

/// How far a player is outmatched by the foe they're fighting (see [`strike::outmatched`]),
/// updated each round. A tough fight makes their skills quicker to improve and reveal.
#[derive(Component, Debug, Clone, Copy)]
pub struct Outmatched(pub f64);

/// The chance, in percent, that a mob makes one of its techniques on its turn instead of its
/// auto-attack.
#[derive(Component, Debug, Clone, Copy)]
pub struct Tactics(pub u32);

/// Killed this round, waiting for `handle_deaths`.
#[derive(Component, Debug)]
pub struct Dead;

/// Who struck the killing blow, whose Luck the loot is rolled with.
#[derive(Component, Debug, Clone, Copy)]
pub struct Slayer(pub Entity);

/// "the sewer rat" for "a sewer rat"; names stay as they are.
pub fn the(name: &str) -> String {
    let bare = without_article(name);
    if bare.len() == name.trim().len() {
        name.to_string()
    } else {
        format!("the {bare}")
    }
}

/// A technique made ready to use on the fighter's next turn: an opener that starts a fight.
#[derive(Component, Debug, Clone)]
pub struct Readied {
    pub skill: String,
    pub ready_at: f64,
}

/// Those out of a fight with a move or spell still prepared.
type Prepared<'w, 's> = Query<
    'w,
    's,
    (Entity, Option<&'static Readied>, Option<&'static Casting>),
    (Or<(With<Readied>, With<Casting>)>, Without<Fighting>),
>;

/// Drop prepared moves that never get a first turn, rather than carrying them into a later fight.
fn expire_readied(
    mut commands: Commands,
    ready: Prepared,
    time: Res<Time>,
    skills: Res<SkillDefs>,
    spells: Res<SpellDefs>,
    mut log: ResMut<Outbox>,
) {
    let now = time.elapsed_secs_f64();
    for (entity, readied, casting) in &ready {
        if let Some(readied) = readied
            && now >= readied.ready_at + crate::rules::rules().ready_timeout_seconds
        {
            let name = skills
                .skills
                .get(&readied.skill)
                .map_or(readied.skill.as_str(), |skill| skill.def.name.as_str());
            commands.entity(entity).remove::<Readied>();
            log.0
                .push(Outgoing::line(entity, format!("You let your {name} fade.")));
        }
        if let Some(casting) = casting
            && now >= casting.ready_at + crate::rules::rules().ready_timeout_seconds
        {
            let name = spells
                .0
                .get(&casting.spell)
                .map_or(casting.spell.as_str(), |spell| spell.def.name.as_str());
            commands.entity(entity).remove::<Casting>();
            log.0
                .push(Outgoing::line(entity, format!("You let your {name} fade.")));
        }
    }
}

/// The aura each living mob fights under: every living mob holding an aura skill lends its bonus
/// to the mobs it affects in its zone.
fn auras(
    fighters: &Fighters,
    rooms: &RoomQuery,
    defs: &SkillDefs,
    mobs: &MobTemplates,
) -> HashMap<Entity, Bonus> {
    let zone_of = |room: Entity| rooms.get(room).ok().map(|(_, key, _)| key.zone.clone());
    // Each aura in the world: its holder, its zone, what it affects and how.
    let mut held: Vec<(Entity, String, &str, Bonus)> = Vec::new();
    for fighter in fighters {
        let (false, Some(_), Some(zone)) = (fighter.dead, fighter.mob, zone_of(fighter.in_room.0))
        else {
            continue;
        };
        for key in fighter.skills.0.keys() {
            if let Some(SkillKind::Aura { affects, bonus }) =
                defs.skills.get(key).map(|s| &s.def.kind)
            {
                held.push((fighter.entity, zone.clone(), affects, *bonus));
            }
        }
    }
    if held.is_empty() {
        return HashMap::new();
    }
    // Every other mob in the zone of the template it names, or with the tag.
    fighters
        .iter()
        .filter_map(|fighter| {
            let zone = zone_of(fighter.in_room.0)?;
            let key = &fighter.mob?.key;
            let tags = mobs
                .0
                .get(key)
                .map(|d| d.tags.as_slice())
                .unwrap_or_default();
            let mut total = Bonus::default();
            let mut any = false;
            for (holder, at, affects, bonus) in &held {
                let named = MobKey::parse(affects, at) == *key || tags.iter().any(|t| t == affects);
                if *holder != fighter.entity && *at == zone && named {
                    total += *bonus;
                    any = true;
                }
            }
            any.then_some((fighter.entity, total))
        })
        .collect()
}

/// The turn order, with everyone just drawn into a fight moved after whoever drew them in: a dog
/// doesn't know it's being attacked until the first blow or spell lands, however quick it is.
/// `drawn` is everyone just set upon.
fn first_strike(
    order: Vec<(i64, Entity)>,
    drawn: &HashSet<Entity>,
    targets: &HashMap<Entity, Entity>,
) -> Vec<(i64, Entity)> {
    let drawn_in = |who: Entity| drawn.contains(&who).then(|| targets.get(&who)).flatten();
    let mut waiting: Vec<(i64, Entity)> = Vec::new();
    let mut out = Vec::with_capacity(order.len());
    for turn in order {
        if drawn_in(turn.1).is_some() {
            waiting.push(turn);
            continue;
        }
        out.push(turn);
        // Those this one drew in go next, in their own order.
        let (now, later): (Vec<_>, Vec<_>) = waiting
            .into_iter()
            .partition(|(_, who)| drawn_in(*who) == Some(&turn.1));
        out.extend(now);
        waiting = later;
    }
    // Anyone whose attacker takes no turn still gets theirs.
    out.extend(waiting);
    out
}

/// The technique a queued `use` names, if it is one the player knows.
fn queued_technique(queue: &ActionQueue, skills: &Skills, defs: &SkillDefs) -> Option<String> {
    let (_, invocation) = queue.peek()?;
    if invocation.command != "use" {
        return None;
    }
    let (skill, _) = defs.find_known_in(skills, &invocation.args)?;
    matches!(skill.def.kind, SkillKind::Technique(_)).then(|| skill.key.clone())
}

/// One side's fight in one room, rated once over its whole length: everyone who has fought on the
/// side and everyone who has fought against it (or waits their turn to), with their attack and
/// defence when they joined. Both only grow, and the fallen still count: the fight doesn't get
/// easier as foes fall, as a whole fight's worth was spent on it. It ends when none of the side
/// is fighting there. See `docs/progression.md#tough-fights`.
#[derive(Default, Debug)]
pub struct Encounter {
    members: HashMap<Entity, (f64, f64)>,
    /// Mobs not on the side who have fought its foes beside it: they count for its strength,
    /// but earn nothing.
    allies: HashMap<Entity, (f64, f64)>,
    foes: HashMap<Entity, (f64, f64)>,
}

impl Encounter {
    /// How outmatched the side, and those fighting beside it, are by the whole fight (see
    /// `strike::side_odds`).
    fn odds(&self) -> Option<f64> {
        let side: Vec<_> = self
            .members
            .values()
            .chain(self.allies.values())
            .copied()
            .collect();
        let foes: Vec<_> = self.foes.values().copied().collect();
        strike::side_odds(&side, &foes)
    }

    /// The experience felling `foe` is worth: the whole fight's, in the share of its foes'
    /// strength that `foe` was.
    fn worth(&self, foe: Entity) -> Option<u64> {
        let strength = |(attack, defence): (f64, f64)| (attack * defence).sqrt();
        let total: f64 = self.foes.values().copied().map(strength).sum();
        let share = strength(*self.foes.get(&foe)?) / total;
        Some((victory_experience(self.odds()?) as f64 * share).round() as u64)
    }
}

/// The encounters under way, by side (a group's leader, or someone alone) and room.
#[derive(Resource, Default)]
pub struct Encounters(HashMap<(Entity, Entity), Encounter>);

/// One round of every fight.
#[allow(clippy::too_many_arguments)]
fn combat_round(
    mut commands: Commands,
    mut fighters: Fighters,
    items: ItemQuery,
    (templates, mobs): (Res<ItemTemplates>, Res<MobTemplates>),
    (defs, spells, condition_defs): (Res<SkillDefs>, Res<SpellDefs>, Res<ConditionDefs>),
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    time: Res<Time>,
    mut events: MessageWriter<GameEvent>,
    mut log: ResMut<Outbox>,
    resting_query: Query<Entity, With<Resting>>,
    (readied, casting, drawn): (
        Query<&Readied>,
        Query<&Casting>,
        Query<Entity, With<DrawnIn>>,
    ),
    rooms: RoomQuery,
    mut item_conditions: Query<(Entity, &mut ItemConditions)>,
    (groups, mut encounters, story): (Groups, ResMut<Encounters>, Res<crate::facts::Facts>),
) {
    let now = time.elapsed_secs_f64();
    let auras = auras(&fighters, &rooms, &defs, &mobs);
    let resting: HashSet<Entity> = resting_query.iter().collect();
    let mut targets: HashMap<Entity, Entity> = HashMap::new();
    let mut alive_players: Vec<(Entity, Entity)> = Vec::new();
    for fighter in &fighters {
        if fighter.dead {
            continue;
        }
        // A thing takes blows but never fights: whatever set it on someone, it isn't.
        if fighter.inanimate {
            if fighter.fighting.is_some() {
                commands.entity(fighter.entity).remove::<Fighting>();
            }
            continue;
        }
        if let Some(Fighting(target)) = fighter.fighting {
            targets.insert(fighter.entity, *target);
        }
        if fighter.player.is_some() {
            alive_players.push((fighter.entity, fighter.in_room.0));
        }
    }

    // Fights end when the target is gone, dead or elsewhere.
    let place = |entity: Entity| {
        fighters
            .get(entity)
            .ok()
            .filter(|f| !f.dead)
            .map(|f| f.in_room.0)
    };
    let pairs: Vec<(Entity, Entity)> = targets.iter().map(|(a, t)| (*a, *t)).collect();
    for (attacker, target) in pairs {
        if place(target).is_none() || place(target) != place(attacker) {
            targets.remove(&attacker);
            commands.entity(attacker).remove::<Fighting>();
        }
    }
    // Who was fighting whom before anyone fought back, for openers.
    let engaged = targets.clone();
    // Defenders fight back, but things don't.
    let pairs: Vec<(Entity, Entity)> = targets.iter().map(|(a, t)| (*a, *t)).collect();
    for (attacker, target) in pairs {
        if fighters.get(target).is_ok_and(|f| f.inanimate) {
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) = targets.entry(target) {
            entry.insert(attacker);
            commands.entity(target).insert(Fighting(attacker));
        }
    }

    // Initiative: Reaction + Intuition, plus a roll; in hundredths, so a passive's fraction counts.
    let mut order: Vec<(i64, Entity)> = targets
        .keys()
        .filter_map(|entity| {
            let fighter = fighters.get(*entity).ok()?;
            let luck = fighter.attributes.get(Attribute::Luck);
            let roll = dice.range(luck, 1..=6);
            let quick = boosts(fighter.skills, &defs).initiative;
            let rating = f64::from(fighter.attributes.initiative()) + quick + roll as f64;
            Some(((rating * 100.0).round() as i64, *entity))
        })
        .collect();
    order.sort_by(|a, b| b.cmp(a));
    // Those just set upon: fighting back since the attack, or only now.
    let mut drawn_in: HashSet<Entity> = drawn.iter().collect();
    drawn_in.extend(targets.keys().filter(|who| !engaged.contains_key(*who)));
    for who in &drawn_in {
        commands.entity(*who).remove::<DrawnIn>();
    }
    let order = first_strike(order, &drawn_in, &targets);

    // The encounters under way, each side's whole fight rated once: who has fought on it, and
    // against it, since it began (see `Encounter`).
    let rate = |e: Entity| strike::ratings(e, &fighters, &auras, &items, &templates, &defs);
    let tracked = |who: Entity| {
        fighters.get(who).is_ok_and(|f| f.player.is_some()) || groups.get(who).is_ok()
    };
    let side = |who: Entity| groups::side(who, &groups);
    // A fight is over for a side once none of it is fighting there.
    let fighting: HashSet<(Entity, Entity)> = targets
        .keys()
        .filter(|who| tracked(**who))
        .filter_map(|who| Some((side(*who), place(*who)?)))
        .collect();
    encounters.0.retain(|key, _| fighting.contains(key));
    for (a, b) in &targets {
        for (us, them) in [(*a, *b), (*b, *a)] {
            let (Some(room), true) = (place(us), tracked(us)) else {
                continue;
            };
            if side(us) == side(them) || fighters.get(them).is_ok_and(|f| f.inanimate) {
                continue;
            }
            let encounter = encounters.0.entry((side(us), room)).or_default();
            if !encounter.members.contains_key(&us)
                && let Some(rating) = rate(us)
            {
                encounter.members.insert(us, rating);
            }
            if !encounter.foes.contains_key(&them)
                && let Some(rating) = rate(them)
            {
                encounter.foes.insert(them, rating);
            }
        }
    }
    // Those waiting their turn are in the fight too: aggressive mobs in the room who'd attack a
    // player and aren't fighting anyone else, as a pack's limit holds them back. And mobs
    // fighting beside the side (a guard taking on the same rats) make it easier, though they
    // aren't on it and earn nothing.
    for ((us, room), encounter) in &mut encounters.0 {
        for f in &fighters {
            if let Some(foe) = targets.get(&f.entity)
                && f.player.is_none()
                && groups.get(f.entity).is_err()
                && f.in_room.0 == *room
                && !f.dead
                && encounter.foes.contains_key(foe)
                && !encounter.foes.contains_key(&f.entity)
                && !encounter.allies.contains_key(&f.entity)
                && let Some(rating) = rate(f.entity)
            {
                encounter.allies.insert(f.entity, rating);
            }
            let waiting = f.aggressive
                && !targets.contains_key(&f.entity)
                && !f.dead
                && !f.inanimate
                && !f.puppeted
                && f.in_room.0 == *room
                && f.mob
                    .and_then(|m| Some((m, mobs.0.get(&m.key)?)))
                    .is_some_and(|(m, def)| !def.spares(&m.key, None));
            if waiting
                && !encounter.foes.contains_key(&f.entity)
                && !encounter.allies.contains_key(&f.entity)
                && groups::side(f.entity, &groups) != *us
                && let Some(rating) = rate(f.entity)
            {
                encounter.foes.insert(f.entity, rating);
            }
        }
    }
    // How outmatched each player is: their side's whole fight.
    let outmatched: HashMap<Entity, f64> = alive_players
        .iter()
        .filter_map(|(who, room)| {
            let odds = encounters.0.get(&(side(*who), *room))?.odds()?;
            targets.contains_key(who).then_some((*who, odds))
        })
        .collect();
    for (who, _) in &alive_players {
        match outmatched.get(who) {
            Some(odds) => commands.entity(*who).insert(Outmatched(*odds)),
            None => commands.entity(*who).remove::<Outmatched>(),
        };
    }

    // Attackers their targets haven't noticed: an opener from hiding can't be avoided.
    let ambushers: HashSet<Entity> = targets
        .iter()
        .filter(|(a, t)| {
            fighters
                .get(**a)
                .is_ok_and(|f| f.conditions.hidden().is_some() && !f.conditions.seen_by(**t))
        })
        .map(|(a, _)| *a)
        .collect();

    let mut fallen: HashSet<Entity> = HashSet::new();
    // Who fell, and to whom.
    let mut felled: Vec<(Entity, Entity)> = Vec::new();
    let mut calmed: HashSet<Entity> = HashSet::new();
    let mut used_up_items: Vec<Entity> = Vec::new();
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
        watchers: &alive_players,
        engaged: &engaged,
        targets: &targets,
        auras: &auras,
        resting: &resting,
        outmatched: &outmatched,
        ambushers: &ambushers,
        now,
        imbued: &mut imbued,
    };
    for (_, attacker) in order {
        let Some(&target) = targets.get(&attacker) else {
            continue;
        };
        if fallen.contains(&attacker) || fallen.contains(&target) {
            continue;
        }
        // A calmed animal fights no more, and no one fights it.
        if calmed.contains(&attacker) || calmed.contains(&target) {
            continue;
        }
        // Someone caught resting spends their first turn getting up.
        if resting.contains(&attacker) {
            commands.entity(attacker).remove::<Resting>();
            round.log.0.push(Outgoing::line(
                attacker,
                "\x1b[1mYou scramble to your feet!\x1b[0m",
            ));
            continue;
        }
        // Bleeding, staggers and falls come first.
        let (acts, bled_out, killer) = round.start_turn(attacker);
        if bled_out {
            fallen.insert(attacker);
            // Bled to death by someone's blow: their kill, as if they'd struck it.
            match killer {
                Some(slayer) => {
                    fell(&mut commands, attacker, slayer, &targets);
                    felled.push((attacker, slayer));
                }
                None => {
                    commands.entity(attacker).insert(Dead).remove::<Fighting>();
                }
            }
            continue;
        }
        // What their conditions and skills do as their turn begins.
        for died in round.react(attacker, target, When::TurnStarts) {
            let slayer = if died == attacker { target } else { attacker };
            fell(&mut commands, died, slayer, &targets);
            felled.push((died, slayer));
            fallen.insert(died);
        }
        if !acts || fallen.contains(&attacker) || fallen.contains(&target) {
            continue;
        }
        // A spell or opener readied for this turn, or a technique or spell next in the queue,
        // takes the turn.
        let mut spell = casting
            .get(attacker)
            .ok()
            .map(|c| (c.spell.clone(), c.target));
        let mut technique = None;
        let mut queued_cast = None;
        if spell.is_some() {
            commands.entity(attacker).remove::<Casting>();
        } else if let Ok(r) = readied.get(attacker) {
            technique = Some(r.skill.clone());
            commands.entity(attacker).remove::<Readied>();
        } else if let Ok(mut fighter) = round.fighters.get_mut(attacker)
            && let Some(queue) = fighter.queue.as_mut()
        {
            // A player's, or a puppeted mob's, queued actions.
            technique = queued_technique(queue, &fighter.skills, &defs);
            if technique.is_some() {
                queue.pop();
                queue.next_at = now + crate::rules::rules().round_seconds;
            } else if let Some((id, invocation)) = queue.pop() {
                queue.next_at = now + crate::rules::rules().round_seconds;
                if invocation.command == "cast" {
                    queued_cast = Some(invocation.args);
                } else {
                    // Any other action takes the turn too.
                    commands.run_system_with(id, invocation);
                    continue;
                }
            }
        }
        if let Some(args) = queued_cast {
            match round.queued_spell(attacker, target, &args) {
                Ok(chosen) => spell = Some(chosen),
                Err(refusal) => round.say(attacker, refusal),
            }
        }
        // Mobs choose their own, now and then, unless someone's playing them.
        let puppeted = round.fighters.get(attacker).is_ok_and(|f| f.puppeted);
        let mut ability = None;
        if technique.is_none() && spell.is_none() && !puppeted {
            match round.mob_move(attacker) {
                Some(MobMove::Technique(key)) => technique = Some(key),
                Some(MobMove::Spell(key, whom)) => spell = Some((key, whom)),
                Some(MobMove::Ability(key, whom)) => ability = Some((key, whom)),
                None => {}
            }
        }

        // An active skill a mob chose, used as a player's would be: what it uses up goes once
        // the round is done.
        let used = ability.and_then(|(key, whom)| {
            round
                .ability(attacker, whom, &key)
                .ok()
                .map(|used| (used, whom))
        });
        let died = if let Some((used, whom)) = used {
            used_up_items.extend(used.item);
            if used.outcome.calmed {
                calm(&mut commands, whom, &targets);
                calmed.insert(whom);
            }
            used.outcome.died
        } else if let Some((key, whom)) = spell {
            let cast = round.cast(attacker, whom, &key);
            // A foe a spell is cast at fights back, if it wasn't already fighting.
            let hostile = spells.0.get(&key).is_some_and(|s| s.def.aim == Aim::Foe);
            if hostile && !targets.contains_key(&whom) && !cast.died.contains(&whom) {
                commands.entity(whom).insert(Fighting(attacker));
            }
            if let Some(beast) = cast.calmed {
                calm(&mut commands, beast, &targets);
                calmed.insert(beast);
            }
            cast.died
        } else {
            let moves = technique
                .as_ref()
                .and_then(|key| match defs.skills.get(key) {
                    Some(skill) => match &skill.def.kind {
                        SkillKind::Technique(t) => Some(Move {
                            key: key.as_str(),
                            technique: t,
                        }),
                        _ => None,
                    },
                    None => None,
                });
            round.strike(attacker, target, moves, false)
        };
        for died in died {
            fallen.insert(died);
            // A riposte can fell the attacker; then the target struck the blow.
            let slayer = if died == attacker { target } else { attacker };
            fell(&mut commands, died, slayer, &targets);
            felled.push((died, slayer));
        }
    }
    imbued.write_back(&mut item_conditions);
    for entity in used_up_items {
        if let Ok((_, item, _)) = items.get(entity) {
            crate::healing::used_up(&mut commands, &db, entity, item);
        }
    }

    reveal_fighters(&mut commands, &mut events, &fighters, &targets);

    // Experience for each foe that falls, to the side that felled it: its share of what the
    // whole fight was worth, for everyone on that side in the room. A thing was no risk.
    for &(died, slayer) in &felled {
        let Some(name) = fighters
            .get(died)
            .ok()
            .filter(|f| !f.inanimate)
            .and_then(|f| f.mob.map(|m| the(&m.name)))
        else {
            continue;
        };
        let us = groups::side(slayer, &groups);
        let Some(room) = fighters.get(slayer).ok().map(|f| f.in_room.0) else {
            continue;
        };
        let Some(points) = encounters
            .0
            .get(&(us, room))
            .and_then(|encounter| encounter.worth(died))
        else {
            continue;
        };
        for &(who, in_room) in &alive_players {
            if in_room != room || fallen.contains(&who) || groups::side(who, &groups) != us {
                continue;
            }
            let Ok(mut fighter) = fighters.get_mut(who) else {
                continue;
            };
            let Some(character_id) = fighter.player.map(|p| p.character_id) else {
                continue;
            };
            let deed = format!("for defeating {name}");
            for line in fighter.attributes.earn(points, &deed, character_id, &db) {
                log.0.push(Outgoing::line(who, line));
            }
        }
    }
}

/// `died` has fallen to `slayer`: they're dead, and whoever was fighting them stops.
fn fell(commands: &mut Commands, died: Entity, slayer: Entity, targets: &HashMap<Entity, Entity>) {
    commands
        .entity(died)
        .insert((Dead, Slayer(slayer)))
        .remove::<Fighting>();
    for (who, whom) in targets {
        if *whom == died {
            commands.entity(*who).remove::<Fighting>();
        }
    }
}

/// `affects`: what's affecting you, and for how long; what an item grants, while you have it.
fn affects(
    In(inv): In<Invocation>,
    mut outbox: ResMut<Outbox>,
    conditions: Query<&Conditions>,
    defs: Res<ConditionDefs>,
    items: ItemQuery,
    time: Res<Time>,
) {
    let name = |item: Entity| {
        items
            .get(item)
            .map_or("something".into(), |(_, i, _)| i.name.clone())
    };
    let lines = conditions
        .get(inv.entity)
        .map(|c| c.listed(time.elapsed_secs_f64(), &defs, name))
        .unwrap_or_default();
    let text = if lines.is_empty() {
        "Nothing is affecting you.".to_string()
    } else {
        format!("You are:\r\n  {}", lines.join("\r\n  "))
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// End conditions whose time is up, in a fight or out of one, and tell their bearers, and those
/// around them, in each condition's words.
fn expire_conditions(
    mut conditions: Query<(Entity, &mut Conditions)>,
    characters: Characters,
    defs: Res<ConditionDefs>,
    time: Res<Time>,
    mut log: ResMut<Outbox>,
    mut events: MessageWriter<GameEvent>,
) {
    let now = time.elapsed_secs_f64();
    for (who, mut conditions) in &mut conditions {
        for over in conditions.expire(now) {
            events.write(GameEvent::ConditionFaded {
                who,
                condition: over.key.clone(),
                strength: over.strength,
            });
            let Some(fades) = defs.get(&over.key).and_then(|d| d.text.fades.as_ref()) else {
                continue;
            };
            let Some(bearer) = Actor::get(&characters, who) else {
                continue;
            };
            let (mine, theirs) = fades.fill(&bearer.name, over.strength);
            log.0.push(Outgoing::line(who, mine));
            if let Some(theirs) = theirs {
                log.0.push(Outgoing::Room {
                    room: bearer.room,
                    except: who,
                    text: theirs + "\r\n",
                });
            }
        }
    }
}

/// Everyone living, where they are, and whether a mob (and which) or a player.
type Present<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static InRoom, Option<&'static Mob>, Has<Player>),
    (Or<(With<Player>, With<Mob>)>, Without<Dead>),
>;

/// Aggressive mobs pick a fight with someone in their room: players, or, for a mob with `allies`,
/// anyone who isn't one (guards go for the rats, rats for the guards). Only those they've noticed
/// (an unseen one slips past), and no more of a spawn's mobs ganging up on one than its `pack`
/// allows; the rest wait their turn. A puppeted mob leaves it to whoever is playing it.
#[allow(clippy::too_many_arguments)]
fn aggression(
    mut commands: Commands,
    aggressors: Query<(Entity, &InRoom, &Mob, Option<&SpawnedBy>, &Aggressive), Aggressors>,
    present: Present,
    characters: Characters,
    fights: Query<(Entity, &Fighting)>,
    spawned: Query<&SpawnedBy>,
    (spawns, templates): (Res<Spawns>, Res<MobTemplates>),
    hidden: Query<&Conditions>,
    mut outbox: ResMut<Outbox>,
) {
    let spawn_of = |entity: Entity| spawned.get(entity).ok().map(|s| s.0.clone());
    // Who's set on whom: those already fighting, and those picking a fight now.
    let mut on: Vec<(Entity, Entity)> = fights.iter().map(|(a, f)| (a, f.0)).collect();
    let mut present: Vec<_> = present.iter().collect();
    present.sort_by_key(|(entity, ..)| *entity);
    let mut out = Vec::new();
    for (mob, in_room, me, spawned_by, aggressive) in &aggressors {
        let Some(def) = templates.0.get(&me.key) else {
            continue;
        };
        // Away from home, a territorial one lets you pass.
        if aggressive.home.is_some_and(|home| home != in_room.0) {
            continue;
        }
        let pack = spawned_by
            .and_then(|s| spawns.0.get(&s.0))
            .and_then(|def| def.pack);
        let mine = spawned_by.map(|s| s.0.clone());
        let room_for = |victim: Entity, on: &[(Entity, Entity)]| {
            pack.is_none_or(|limit| {
                let ganging = on
                    .iter()
                    .filter(|(a, t)| *t == victim && spawn_of(*a) == mine)
                    .count();
                (ganging as u32) < limit
            })
        };
        let hostile = |other: Option<&Mob>| {
            let other = other.and_then(|m| Some((&m.key, templates.0.get(&m.key)?)));
            !def.spares(&me.key, other)
        };
        let victim = present.iter().find(|(victim, room, other, player)| {
            *victim != mob
                && room.0 == in_room.0
                && hostile(other.filter(|_| !player))
                && room_for(*victim, &on)
                && sees(&hidden, mob, *victim)
        });
        let (Some((victim, ..)), Some(attacker)) = (victim, Actor::get(&characters, mob)) else {
            continue;
        };
        let Some(foe) = Actor::get(&characters, *victim) else {
            continue;
        };
        let foe_fighting = on.iter().any(|(a, _)| *a == foe.entity);
        on.push((mob, foe.entity));
        attack(
            &mut commands,
            &characters,
            &attacker,
            &foe,
            foe_fighting,
            &mut out,
        );
    }
    deliver(out, &mut outbox);
}

/// Group members free to join a fight: alive, not in one, not resting, not being played, and
/// not with `autoassist` off.
type Assisting = (
    With<InGroup>,
    Without<Fighting>,
    Without<Dead>,
    Without<Resting>,
    Without<NoAssist>,
    Without<PuppetedBy>,
);

/// Group members join a fight one of their group in the room is in, against whoever that member
/// is fighting: only one they can see, and never one of their own group.
#[allow(clippy::too_many_arguments)]
fn assist(
    mut commands: Commands,
    idle: Query<Entity, Assisting>,
    fights: Query<&Fighting>,
    alive: Query<(), Without<Dead>>,
    groups: Groups,
    characters: Characters,
    hidden: Query<&Conditions>,
    mut outbox: ResMut<Outbox>,
) {
    let mut out = Vec::new();
    let mut joined: HashSet<Entity> = HashSet::new();
    for who in &idle {
        let Some(me) = Actor::get(&characters, who) else {
            continue;
        };
        let mine = groups::side(who, &groups);
        let foe = groups::members(who, &groups)
            .into_iter()
            .find_map(|member| {
                let Fighting(foe) = fights.get(member).ok()?;
                let foe = Actor::get(&characters, *foe)?;
                let near =
                    |e: Entity| Actor::get(&characters, e).is_some_and(|a| a.room == me.room);
                (member != who
                    && near(member)
                    && foe.room == me.room
                    && alive.get(foe.entity).is_ok()
                    && groups::side(foe.entity, &groups) != mine
                    && sees(&hidden, who, foe.entity))
                .then_some(foe)
            });
        let Some(foe) = foe else {
            continue;
        };
        let foe_fighting = fights.get(foe.entity).is_ok() || joined.contains(&foe.entity);
        joined.insert(foe.entity);
        attack(
            &mut commands,
            &characters,
            &me,
            &foe,
            foe_fighting,
            &mut out,
        );
    }
    deliver(out, &mut outbox);
}

/// Aggressive mobs free to pick a fight: alive, not in one, and not being played.
type Aggressors = (
    With<Aggressive>,
    Without<Fighting>,
    Without<Dead>,
    Without<PuppetedBy>,
);

/// Let an animal out of its fights: it stops fighting, no one fights it, and it no longer
/// attacks those who come near.
pub fn calm(commands: &mut Commands, beast: Entity, targets: &HashMap<Entity, Entity>) {
    commands.entity(beast).remove::<(Fighting, Aggressive)>();
    for (who, whom) in targets {
        if *whom == beast {
            commands.entity(*who).remove::<Fighting>();
        }
    }
}

/// Once blows are struck, no one in a fight stays hidden.
fn reveal_fighters(
    commands: &mut Commands,
    events: &mut MessageWriter<GameEvent>,
    fighters: &Fighters,
    targets: &HashMap<Entity, Entity>,
) {
    let fighting: HashSet<Entity> = targets.keys().chain(targets.values()).copied().collect();
    for who in fighting {
        if let Ok(f) = fighters.get(who)
            && f.conditions.hidden().is_some()
        {
            let name = capitalized(&name_of(f.player, f.mob));
            reveal(commands, events, who, &name, f.in_room.0);
        }
    }
}

/// Experience for a victory: 100 times the chance of having lost, from how outmatched the victor
/// was (see `strike::outmatched`). An even fight gives 50. A fight they'd have lost less than one
/// time in ten gives nothing: a foe under about half their strength teaches them nothing, so a
/// character outgrows easy prey and has to seek harder fights to level.
pub fn victory_experience(outmatched: f64) -> u64 {
    let odds = outmatched.max(0.0).powf(crate::dice::STEEPNESS);
    let risk = odds / (1.0 + odds);
    let numbers = crate::attributes::progression();
    if risk < numbers.worthwhile_risk {
        return 0;
    }
    (numbers.victory_experience * risk).round() as u64
}

/// `technique <skill> [<target>]`, run by `use` for a technique outside a fight: an opener starts
/// one, readied for the user's first turn; anything else needs a fight already going.
#[allow(clippy::too_many_arguments)]
fn technique(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    fighting: Query<&Fighting>,
    dead: Query<(), With<Dead>>,
    hidden: Query<&Conditions>,
    defs: Res<SkillDefs>,
    time: Res<Time>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let (key, target) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let Some(skill) = defs.skills.get(key) else {
        return;
    };
    let SkillKind::Technique(t) = &skill.def.kind else {
        return;
    };
    let text = if fighting.get(inv.entity).is_ok() {
        // Readied for their next turn.
        commands.entity(inv.entity).insert(Readied {
            skill: key.to_string(),
            ready_at: time.elapsed_secs_f64(),
        });
        format!("You ready yourself to {}.", skill.def.name)
    } else if !t.opener {
        format!("You need to be in a fight to {}.", skill.def.name)
    } else {
        let candidates: Vec<_> = mobs::targets_in(actor.room, &mobs, &keywords)
            .into_iter()
            .filter(|(entity, _)| dead.get(*entity).is_err() && sees(&hidden, inv.entity, *entity))
            .collect();
        match Target::parse(target)
            .and_then(|t| t.find_one(inv.entity, candidates))
            .and_then(|e| mobs.get(e).ok())
        {
            None if target.trim().is_empty() => format!("{} whom?", skill.def.name),
            None => format!("You don't see {} here.", target.trim()),
            Some((mob_entity, mob, _)) => {
                commands
                    .entity(inv.entity)
                    .insert((
                        Fighting(mob_entity),
                        Readied {
                            skill: key.to_string(),
                            ready_at: time.elapsed_secs_f64(),
                        },
                    ))
                    .remove::<Resting>();
                format!(
                    "You slip toward {}, ready to {}.",
                    the(&mob.name),
                    skill.def.name
                )
            }
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// Fighters who haven't fallen yet.
type StillFighting = (With<Fighting>, Without<Dead>);

/// Fights going on, and what can't fight back, for `in_danger`.
pub type Fights<'w, 's> = (
    Query<'w, 's, (Entity, &'static Fighting), Without<Dead>>,
    Query<'w, 's, (), With<Inanimate>>,
);

/// Whether `who` is in a fight that can hurt them: fighting something that isn't inanimate, or
/// set upon by anyone. Hacking at a training dummy is no danger.
pub fn in_danger(
    who: Entity,
    fights: &Query<(Entity, &Fighting), Without<Dead>>,
    inanimate: &Query<(), With<Inanimate>>,
) -> bool {
    fights.iter().any(|(fighter, Fighting(foe))| {
        (fighter == who && inanimate.get(*foe).is_err()) || *foe == who
    })
}

/// Anyone who disconnects in the middle of a fight is cut down as they go: no one escapes a
/// fight by leaving the game (`quit` refuses while they're in one). They die like anyone else,
/// so their gear stays on their corpse and they wake at the shrine next time.
pub fn abandon_fights(
    mut commands: Commands,
    leaving: Query<(Entity, &Connection, &Player, &InRoom), StillFighting>,
    fights: Query<(Entity, &Fighting), Without<Dead>>,
    inanimate: Query<(), With<Inanimate>>,
    mut log: ResMut<Outbox>,
) {
    for (entity, conn, player, in_room) in &leaving {
        if !conn.closing {
            continue;
        }
        if !in_danger(entity, &fights, &inanimate) {
            continue;
        }
        commands.entity(entity).insert(Dead).remove::<Fighting>();
        log.0.push(Outgoing::line(
            entity,
            "\x1b[1;31mYou turn to flee the fight, and are cut down as you go.\x1b[0m",
        ));
        log.0.push(Outgoing::Room {
            room: in_room.0,
            except: entity,
            text: format!(
                "{} tries to slip away from the fight, and is cut down!\r\n",
                player.name
            ),
        });
        info!("{} left in the middle of a fight, and died", player.name);
    }
}

/// The items each player still needs for a quest they're on (filled by `quests` just before
/// deaths are handled): a boss whose loot can hold one always leaves one for its slayer's group,
/// whatever the roll. An ordinary mob's drops are left to chance.
#[derive(Resource, Default, Debug)]
pub struct WantedLoot(pub HashMap<Entity, Vec<ItemKey>>);

/// Mobs killed this round, with who killed them.
type DeadMobs<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Mob,
        &'static InRoom,
        Option<&'static Slayer>,
        Has<Inanimate>,
    ),
    (With<Dead>, mobs::OnlyMobs),
>;

/// Lay out the dead: mobs leave a corpse, and players leave one with everything they carried,
/// then wake at the shrine.
#[allow(clippy::too_many_arguments)]
pub fn handle_deaths(
    mut commands: Commands,
    dead_mobs: DeadMobs,
    dead_players: Query<Entity, (With<Dead>, With<Player>)>,
    (mob_templates, mut dice): (Res<MobTemplates>, ResMut<Dice>),
    players: Characters,
    mut log: ResMut<Outbox>,
    mut sheets: Query<(
        &mut Vitals,
        &Attributes,
        &Skills,
        Option<&mut ActionQueue>,
        &mut Conditions,
    )>,
    defs: Res<SkillDefs>,
    mut items: ItemMutQuery,
    templates: Res<ItemTemplates>,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    registry: Res<CommandRegistry>,
    db: Res<Database>,
    mut purses: Query<&mut Purse>,
    (groups, wanted): (Groups, Res<WantedLoot>),
) {
    let now = unix_now();
    for (entity, mob, in_room, slayer, inanimate) in &dead_mobs {
        // A thing leaves no corpse: what it held falls where it stood.
        if inanimate {
            for (_, _, mut location) in &mut items {
                if location.holder() == Some(entity) {
                    *location = ItemLocation::Room(in_room.0);
                }
            }
            commands.entity(entity).despawn();
            continue;
        }
        // Something formless, a slime, leaves no body either: what it held and leaves falls
        // where it died.
        let formless = sheets
            .get(entity)
            .is_ok_and(|(.., conditions)| conditions.has(FORMLESS));
        let corpse = if formless {
            None
        } else {
            items::spawn_corpse(
                &mut commands,
                &templates,
                &mob.name,
                None,
                in_room.0,
                now + items::mob_corpse_secs(),
            )
        };
        // What can be cut from it is harvested from the corpse, while it lasts.
        if let Some(corpse) = corpse {
            commands.entity(corpse).insert(items::Carcass {
                mob: mob.key.clone(),
                taken: Vec::new(),
            });
        }
        let place = match corpse {
            Some(corpse) => Some(ItemLocation::Inside(corpse)),
            None if formless => Some(ItemLocation::Room(in_room.0)),
            None => None,
        };
        // Its gear falls with it, for whoever wants it, and its loot, one find from each table
        // with its slayer's Luck.
        if let Some(place) = place {
            for (_, _, mut location) in &mut items {
                if location.holder() == Some(entity) {
                    *location = place;
                }
            }
            // Its slayer, and their group in the room: alive, and players.
            let party: Vec<Actor> = slayer
                .map(|s| groups::members(s.0, &groups))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|e| Actor::get(&players, e))
                .filter(|a| {
                    a.room == in_room.0
                        && a.character_id.is_some()
                        && dead_players.get(a.entity).is_err()
                })
                .collect();
            // Loot is found with the Luck of the slayer, or their group's, on average.
            let lucks: Vec<i32> = match party.as_slice() {
                [] => slayer.into_iter().map(|s| s.0).collect::<Vec<Entity>>(),
                _ => party.iter().map(|a| a.entity).collect(),
            }
            .into_iter()
            .filter_map(|e| sheets.get(e).ok())
            .map(|(_, attributes, ..)| attributes.get(Attribute::Luck))
            .collect();
            let luck = match lucks.len() {
                0 => 0,
                n => (f64::from(lucks.iter().sum::<i32>()) / n as f64).round() as i32,
            };
            // The coins it carried (what's left after any pickpocket) go to whoever killed it,
            // if that was a player, shared evenly with their group in the room: the odd coins
            // to the slayer.
            let carried = purses.get(entity).map_or(0, |purse| purse.0);
            let killer = slayer.and_then(|s| party.iter().find(|a| a.entity == s.0));
            if let Some(killer) = killer
                && carried > 0
            {
                let share = carried / party.len() as u64;
                let odd = carried - share * party.len() as u64;
                for member in &party {
                    let got = share
                        + if member.entity == killer.entity {
                            odd
                        } else {
                            0
                        };
                    let Ok(mut purse) = purses.get_mut(member.entity) else {
                        continue;
                    };
                    let line = if party.len() == 1 {
                        format!("You take {} from {}.", money::coins(got), the(&mob.name))
                    } else if member.entity == killer.entity {
                        format!(
                            "You take {} from {} and share it out: {} for you.",
                            money::coins(carried),
                            the(&mob.name),
                            money::coins(got)
                        )
                    } else if got > 0 {
                        format!(
                            "{} shares out what {} carried: {} for you.",
                            killer.name,
                            the(&mob.name),
                            money::coins(got)
                        )
                    } else {
                        continue;
                    };
                    if got > 0 {
                        purse.0 += got;
                        purse.save(member.character_id, &db);
                    }
                    log.0.push(Outgoing::line(member.entity, line));
                }
            }
            let def = mob_templates.0.get(&mob.key);
            let tables = def.map(|def| &def.loot);
            let boss = def.is_some_and(|def| def.boss);
            let mut found = Vec::new();
            for table in tables.into_iter().flatten() {
                if let Some(Some(id)) = dice.pick(luck, table) {
                    found.push(ItemKey::parse(id, &mob.key.zone));
                }
            }
            // A boss drops what the slayer's group needs for a quest, if it can and didn't: there's
            // only one of it.
            let could: Vec<ItemKey> = tables
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|(id, _)| id.as_ref())
                .map(|id| ItemKey::parse(id, &mob.key.zone))
                .collect();
            let needed: Vec<ItemKey> = party
                .iter()
                .map(|a| a.entity)
                .chain(slayer.map(|s| s.0))
                .filter_map(|who| wanted.0.get(&who))
                .flatten()
                .filter(|key| boss && could.contains(key) && !found.contains(key))
                .cloned()
                .collect();
            for key in needed {
                if !found.contains(&key) {
                    found.push(key);
                }
            }
            for key in found {
                // Found with the slayer's (or their group's) Luck, which helps its quality too.
                let quality = templates
                    .0
                    .get(&key)
                    .map_or(Quality::Standard, |def| Quality::roll(&mut dice, luck, def));
                items::spawn_item(&mut commands, &templates, key, None, place, quality);
            }
        }
        commands.entity(entity).despawn();
    }

    for entity in &dead_players {
        let Some(actor) = Actor::get(&players, entity) else {
            continue;
        };
        let room_key = rooms
            .get(actor.room)
            .map(|(_, key, _)| key.to_string())
            .unwrap_or_default();

        // Their corpse is real, so it and what's in it survive a restart.
        let corpse_id = match db.create_item(
            &items::corpse_key().to_string(),
            &ItemPlace::Room(room_key.clone()),
            Quality::Standard,
        ) {
            Ok(id) => Some(id),
            Err(err) => {
                error!("failed to save {}'s corpse: {err}", actor.name);
                None
            }
        };
        let name = format!("the corpse of {}", actor.name);
        if let Some(id) = corpse_id
            && let Err(err) = db.set_item_name(id, &name)
        {
            error!("failed to name {}'s corpse: {err}", actor.name);
        }
        if let Some(corpse) = items::spawn_corpse(
            &mut commands,
            &templates,
            &actor.name,
            corpse_id,
            actor.room,
            now + items::player_corpse_secs(),
        ) {
            for (_, mut item, mut location) in &mut items {
                if location.holder() != Some(entity) {
                    continue;
                }
                *location = ItemLocation::Inside(corpse);
                let Some(corpse_id) = corpse_id else {
                    continue;
                };
                let place = ItemPlace::Inside(corpse_id);
                let saved = match item.id {
                    Some(id) => db.move_item(id, &place),
                    None => db
                        .create_item(&item.key.to_string(), &place, item.quality)
                        .map(|id| item.id = Some(id)),
                };
                if let Err(err) = saved {
                    error!("failed to move an item to {}'s corpse: {err}", actor.name);
                }
            }
        }

        // They wake at the shrine, hurt and tired.
        let shrine = index
            .0
            .get(&RoomKey::parse(&crate::world::world_def().respawn_room, ""))
            .copied()
            .or_else(|| index.start())
            .unwrap_or(actor.room);
        commands.entity(entity).insert(InRoom(shrine));
        if let (Some(id), Ok((_, key, _))) = (actor.character_id, rooms.get(shrine))
            && let Err(err) = db.save_location(id, &key.to_string())
        {
            error!("failed to save {}'s location: {err}", actor.name);
        }
        if let Ok((mut vitals, attributes, skills, queue, mut conditions)) = sheets.get_mut(entity)
        {
            let max = Maxima::of(attributes, skills, &defs, None);
            vitals.health = (max.health as f32 / 4.0).max(1.0);
            vitals.stamina = max.stamina as f32 / 4.0;
            if let Some(mut queue) = queue {
                queue.clear();
            }
            conditions.clear();
        }
        commands.entity(entity).remove::<(Dead, Fighting)>();

        let death = &crate::world::world_def().death;
        let mut out = vec![Outgoing::line(entity, death.you.clone())];
        out.push(Outgoing::Room {
            room: shrine,
            except: entity,
            text: format!("{}\r\n", death.room.replace("{name}", &actor.name)),
        });
        deliver(out, &mut log);
        if let Some((spec, id)) = registry.find("look", actor.role) {
            commands.run_system_with(
                *id,
                Invocation {
                    entity,
                    command: spec.name,
                    usage: spec.usage,
                    args: String::new(),
                },
            );
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// `kill <target>`: start a fight with a mob here.
#[allow(clippy::too_many_arguments)]
fn kill(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    fighting: Query<&Fighting>,
    dead: Query<(), With<Dead>>,
    hidden: Query<&Conditions>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    // Players fight mobs; a mob, puppeted, can go for anyone here.
    let mut candidates = mobs::targets_in(actor.room, &mobs, &keywords);
    let players_here = crate::output::targets_in_room(actor.room, &players, &keywords);
    if actor.character_id.is_none() {
        candidates.extend(players_here.iter().copied());
    }
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(entity, _)| {
            *entity != inv.entity
                && dead.get(*entity).is_err()
                && sees(&hidden, inv.entity, *entity)
        })
        .collect();
    let target = Target::parse(&inv.args).and_then(|t| t.find_one(inv.entity, candidates));
    match target.and_then(|e| Actor::get(&players, e)) {
        _ if inv.args.trim().is_empty() => out.push(Outgoing::line(inv.entity, "Kill what?")),
        None if players_here.iter().any(|(_, k)| k.matches(inv.args.trim())) => {
            out.push(Outgoing::line(
                inv.entity,
                "You can't attack other players.",
            ));
        }
        None => out.push(Outgoing::line(
            inv.entity,
            format!("You don't see {} here.", inv.args.trim()),
        )),
        Some(foe) => {
            if fighting.get(inv.entity).is_ok_and(|f| f.0 == foe.entity) {
                out.push(Outgoing::line(
                    inv.entity,
                    format!("You're already fighting {}!", foe.name),
                ));
            } else {
                let foe_fighting = fighting.get(foe.entity).is_ok();
                attack(
                    &mut commands,
                    &players,
                    &actor,
                    &foe,
                    foe_fighting,
                    &mut out,
                );
            }
        }
    }
    deliver(out, &mut outbox);
}

/// `attacker` picks a fight with `foe`, as `kill` does: they fight, `foe` fights back unless
/// already busy with someone else, and the room sees it.
fn attack(
    commands: &mut Commands,
    players: &Characters,
    attacker: &Actor,
    foe: &Actor,
    foe_fighting: bool,
    out: &mut Vec<Outgoing>,
) {
    commands
        .entity(attacker.entity)
        .insert(Fighting(foe.entity))
        .remove::<Resting>();
    if !foe_fighting {
        commands
            .entity(foe.entity)
            .insert((Fighting(attacker.entity), DrawnIn));
    }
    out.push(Outgoing::line(
        attacker.entity,
        format!("\x1b[1mYou attack {}!\x1b[0m", foe.name),
    ));
    // The one attacked is told so; everyone else sees it.
    for (other, _, _, in_room) in players {
        let line = if other == foe.entity {
            format!("\x1b[1;31m{} attacks you!\x1b[0m", attacker.name)
        } else {
            format!("{} attacks {}!", attacker.name, foe.name)
        };
        if in_room.0 == attacker.room && other != attacker.entity {
            out.push(Outgoing::line(other, line));
        }
    }
}

/// `flee`: try to run from a fight through a random open exit.
#[allow(clippy::too_many_arguments)]
fn flee(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    view: View,
    fighting: Query<&Fighting>,
    mut practice: Practice,
    db: Res<Database>,
    (conditions, condition_defs): (Query<&Conditions>, Res<ConditionDefs>),
    mob_templates: Res<MobTemplates>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let reply = |text: &str| vec![Outgoing::line(inv.entity, text)];
    // A mob keeps out of rooms holding what it avoids, while it has anywhere else to run.
    let me = view
        .mobs
        .get(inv.entity)
        .ok()
        .map(|(_, m, _)| m.key.clone());
    let shunned = |room: Entity| {
        fleeing::shuns(
            me.as_ref(),
            room,
            view.rooms.get(room).is_ok_and(|(r, _, _)| r.no_wander),
            view.mobs.iter().map(|(_, m, at)| (m, at.0)),
            &mob_templates,
        )
    };
    // Where they run, once they've paid for it and broken away.
    let mut escape = None;
    let mut out = if fighting.get(inv.entity).is_err() {
        reply("You aren't fighting anyone.")
    } else if conditions
        .get(inv.entity)
        .is_ok_and(|c| !c.can_flee(&condition_defs))
    {
        reply("You're held fast and can't break away!")
    } else if let Some((_, mut vitals, attributes)) = practice.sheet(inv.entity) {
        let luck = attributes.get(Attribute::Luck);
        let flee_cost = crate::rules::rules().flee_stamina;
        if vitals.stamina < flee_cost {
            reply("You're too exhausted to run!")
        } else {
            vitals.stamina -= flee_cost;
            practice.events.write(GameEvent::StaminaSpent {
                who: inv.entity,
                amount: flee_cost,
            });
            let mut ways: Vec<(String, Entity)> = view
                .rooms
                .get(actor.room)
                .map(|(_, _, exits)| {
                    exits
                        .0
                        .iter()
                        // No one flees up a cliff or into deep water.
                        .filter(|(_, exit)| {
                            exit.needs.is_none()
                                && exit
                                    .door
                                    .and_then(|d| view.doors.get(d).ok())
                                    .is_none_or(|d| d.is_open())
                        })
                        .map(|(dir, exit)| (dir.clone(), exit.to))
                        .collect()
                })
                .unwrap_or_default();
            if ways.iter().any(|(_, to)| !shunned(*to)) {
                ways.retain(|(_, to)| !shunned(*to));
            }
            let exits: Vec<String> = ways.into_iter().map(|(dir, _)| dir).collect();
            if exits.is_empty() {
                reply("There's nowhere to run!")
            } else if !practice.chance(luck, crate::rules::rules().flee_chance) {
                let mut out = reply("You try to break away, but can't get clear!");
                out.push(Outgoing::Room {
                    room: actor.room,
                    except: inv.entity,
                    text: format!("{} tries to flee, but can't get away.\r\n", actor.name),
                });
                out
            } else {
                let pick = practice.roll(0, 0..=(exits.len() as i64 - 1)) as usize;
                commands.entity(inv.entity).remove::<Fighting>();
                escape = Some(exits[pick].clone());
                reply("You flee in a panic!")
            }
        }
    } else {
        Vec::new()
    };
    // Fleeing has already paid its way.
    if let Some(dir) = escape {
        out.extend(travel(
            inv.entity,
            &dir,
            &players,
            &view,
            &db,
            &mut practice,
            &mut commands,
            false,
        ));
    }
    deliver(out, &mut outbox);
}

/// `numbers [on | off]`: show or hide damage numbers in fights, saved with the character.
fn numbers(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    hidden: Query<(), With<HideNumbers>>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let shown = hidden.get(inv.entity).is_err();
    let reply = match inv.args.trim().to_lowercase().as_str() {
        "" if shown => "Fights show damage numbers. ('numbers off' to hide them.)".to_string(),
        "" => "Fights don't show damage numbers. ('numbers on' to show them.)".to_string(),
        word @ ("on" | "off") => {
            let hide = word == "off";
            if hide {
                commands.entity(inv.entity).insert(HideNumbers);
            } else {
                commands.entity(inv.entity).remove::<HideNumbers>();
            }
            if let Some(id) = actor.character_id
                && let Err(err) = db.set_hide_numbers(id, hide)
            {
                error!("failed to save {}'s numbers setting: {err}", actor.name);
            }
            if hide {
                "Damage numbers hidden: the words will have to say it.".into()
            } else {
                "Damage numbers shown.".into()
            }
        }
        _ => inv.usage_error(),
    };
    deliver(vec![Outgoing::line(inv.entity, reply)], &mut outbox);
}

/// `autoattack [<skill>]`: see or choose the attack made each round.
fn autoattack(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut sheets: Query<(&Skills, &mut AutoAttack)>,
    defs: Res<SkillDefs>,
    db: Res<Database>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let Ok((skills, mut auto)) = sheets.get_mut(inv.entity) else {
        return;
    };
    let attacks: Vec<&str> = skills
        .0
        .keys()
        .filter_map(|key| defs.skills.get(key))
        .filter(|skill| matches!(skill.def.kind, SkillKind::Attack { .. }))
        .map(|skill| skill.def.name.as_str())
        .collect();
    let name_of = |key: &str| {
        defs.skills
            .get(key)
            .map_or(key.to_string(), |s| s.def.name.clone())
    };
    let current = match auto.0.as_deref() {
        Some(key) => name_of(key),
        None => format!(
            "the weapon in your hand, or your fists ({}) without one",
            name_of(BASIC_ATTACK)
        ),
    };
    let text = if inv.args.trim().is_empty() {
        format!(
            "Each round of a fight you attack with {current}. Your attacks: {}. \
             ('autoattack default' goes back to fighting with what you hold.)",
            attacks.join(", ")
        )
    } else if inv.args.trim().eq_ignore_ascii_case("default") {
        auto.0 = None;
        if let Some(id) = actor.character_id
            && let Err(err) = db.set_autoattack(id, None)
        {
            error!("failed to save {}'s auto-attack: {err}", actor.name);
        }
        "You'll fight with the weapon in your hand, or your fists.".into()
    } else {
        match defs.find_known(skills, inv.args.trim()) {
            Some(skill) if matches!(skill.def.kind, SkillKind::Attack { .. }) => {
                let key = skill.key.clone();
                if let Some(id) = actor.character_id
                    && let Err(err) = db.set_autoattack(id, Some(&key))
                {
                    error!("failed to save {}'s auto-attack: {err}", actor.name);
                }
                let weapon = match &skill.def.kind {
                    SkillKind::Attack { damage: None, .. } => {
                        " It needs a weapon for it in your hand; without one, you'll fight with \
                         what you hold, or brawl."
                            .to_string()
                    }
                    _ => String::new(),
                };
                auto.0 = Some(key);
                format!("You'll attack with {} each round.{weapon}", skill.def.name)
            }
            Some(skill) => format!("{} isn't an attack.", skill.def.name),
            None => format!("You don't know an attack called '{}'.", inv.args.trim()),
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// `stop`: forget queued actions.
fn stop(In(inv): In<Invocation>, mut outbox: ResMut<Outbox>, mut queues: Query<&mut ActionQueue>) {
    let text = match queues.get_mut(inv.entity) {
        Ok(mut queue) if queue.len() > 0 => {
            let n = queue.len();
            queue.clear();
            format!("You put aside your plans ({n} forgotten).")
        }
        _ => "You have nothing planned.".into(),
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Effect, Lasting, Reaction, When, Who};
    use crate::formula::Formula;
    use crate::items::Item;
    use crate::skills::{
        Cost, Lines, PassiveBonus, Skill, SkillDef, SkillKind, SkillState, SkillText, Stat,
        StrikeKind, Technique,
    };
    use crate::spells::{Resist, Spell, SpellDef, Spells};
    use crate::vitals::BaseHealth;
    use crate::world::{ArmourDef, DoorState, Exits, ItemDef, ItemKey, Room, Slot, Terrain};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;

    /// A skill whose text is made from its id: "You cleave {target}.", "{actor} cleaves you."
    fn skill(key: &str, kind: SkillKind) -> (String, Skill) {
        let (tree, id) = key.split_once(':').unwrap();
        let lines = |verb: &str| Lines {
            actor: format!("You {verb} {{target}}."),
            target: Some(format!("{{actor}} {verb}s you.")),
            room: format!("{{actor}} {verb}s {{target}}."),
            beyond: None,
        };
        let def = SkillDef {
            name: key.into(),
            description: key.into(),
            kind,
            text: Some(SkillText {
                hit: lines(id),
                miss: Some(lines(&format!("miss with a {id} at"))),
            }),
            mastery: 50,
            attribute: None,
            rarity: None,
            difficulty: 1.0,
            requires: None,
            karma: None,
        };
        let skill = Skill {
            key: key.into(),
            tree: tree.into(),
            attribute: Attribute::Strength,
            def,
        };
        (key.into(), skill)
    }

    /// A world with one room and the skills these tests use: fists, a riposte with them, and a
    /// cleave made with them.
    fn world() -> (World, Entity) {
        let mut world = World::new();
        let defs = SkillDefs {
            skills: HashMap::from([
                skill(
                    BASIC_ATTACK,
                    SkillKind::Attack {
                        damage: Some((2, 2)),
                        cost: Cost::Free,
                        damage_type: "physical".into(),
                    },
                ),
                skill(
                    "brawling:riposte",
                    SkillKind::Passive {
                        triggers: Vec::new(),
                        bonuses: Vec::new(),
                        counter: Some(BASIC_ATTACK.into()),
                        reactions: Vec::new(),
                    },
                ),
                skill(
                    "brawling:cleave",
                    SkillKind::Technique(Technique {
                        resist: Some(Resist::Evasion),
                        tags: Vec::new(),
                        with: BASIC_ATTACK.into(),
                        cost: Cost::Free,
                        accuracy: 0,
                        damage: 0,
                        strike: StrikeKind::Hit,
                        effects: vec![Effect::Cleave],
                        opener: false,
                    }),
                ),
                // Leaves the target down and the user open, without hurting.
                skill(
                    "brawling:trip",
                    SkillKind::Technique(Technique {
                        resist: Some(Resist::Evasion),
                        tags: Vec::new(),
                        with: BASIC_ATTACK.into(),
                        cost: Cost::Free,
                        accuracy: 0,
                        damage: 0,
                        strike: StrikeKind::Harmless,
                        effects: vec![
                            Effect::Apply {
                                condition: "down".into(),
                                strength: Formula::number(1.0),
                                lasts: Lasting::Turns(2),
                                on: Who::Target,
                            },
                            Effect::Apply {
                                condition: "exposed".into(),
                                strength: Formula::number(1.0),
                                lasts: Lasting::Turns(2),
                                on: Who::User,
                            },
                        ],
                        opener: false,
                    }),
                ),
            ]),
            trees: HashMap::new(),
        };
        world.insert_resource(defs);
        world.insert_resource(SpellDefs::default());
        world.insert_resource(states::defs_on_disk());
        world.insert_resource(ItemTemplates::default());
        world.insert_resource(Dice::seeded(7));
        world.insert_resource(Database::open(":memory:").unwrap());
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Outbox::default());
        world.insert_resource(Spawns::default());
        world.init_resource::<Encounters>();
        world.init_resource::<WantedLoot>();
        world.init_resource::<crate::facts::Facts>();
        world.init_resource::<MobTemplates>();
        world.init_resource::<Messages<GameEvent>>();
        let room = world
            .spawn((
                Room {
                    name: "Arena".into(),
                    description: String::new(),
                    no_wander: false,
                    indoors: true,
                    terrain: Terrain::Paved,
                },
                RoomKey::parse("test:arena", ""),
                Exits::default(),
            ))
            .id();
        (world, room)
    }

    /// A mob fighter with this Strength (its punch) and Reaction (its dodge), knowing these
    /// skills. Every other attribute is 1.
    fn fighter(
        world: &mut World,
        room: Entity,
        strength: i32,
        reaction: i32,
        skills: &[&str],
    ) -> Entity {
        let attributes = Attributes::starting(
            |attribute| match attribute {
                Attribute::Strength => strength,
                Attribute::Reaction => reaction,
                _ => 1,
            },
            0,
        );
        let vitals = Vitals {
            health: 100.0,
            stamina: 100.0,
        };
        let skills = Skills(
            skills
                .iter()
                .map(|key| (key.to_string(), SkillState::new("Template")))
                .collect(),
        );
        let mob = Mob {
            key: MobKey::parse("test:mob", ""),
            name: "a brawler".into(),
            here: String::new(),
            description: String::new(),
            race: None,
        };
        world
            .spawn((
                mob,
                InRoom(room),
                attributes,
                vitals,
                skills,
                BaseHealth(100),
            ))
            .id()
    }

    fn said(world: &World, who: Entity, text: &str) -> bool {
        world.resource::<Outbox>().0.iter().any(|out| match out {
            Outgoing::To(entity, line) => *entity == who && line.contains(text),
            _ => false,
        })
    }

    #[test]
    fn a_dodge_can_be_riposted() {
        let (mut world, room) = world();
        // The attacker can barely hit; the defender dodges almost everything.
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let defender = fighter(
            &mut world,
            room,
            1,
            1000,
            &[BASIC_ATTACK, "brawling:riposte"],
        );
        world.entity_mut(attacker).insert(Fighting(defender));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, defender, "You riposte"));
    }

    /// Put an armour piece on `who`, at `location`.
    fn armour(
        world: &mut World,
        id: &str,
        wear: Option<Slot>,
        armour: ArmourDef,
        at: ItemLocation,
    ) {
        let key = ItemKey::parse(id, "test");
        let def = ItemDef {
            wear,
            armour: Some(armour),
            ..ItemDef::named(id)
        };
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(key.clone(), def);
        world.spawn((
            Item {
                key,
                name: id.into(),
                id: None,
                state: DoorState::Open,
                quality: Quality::Standard,
            },
            at,
        ));
    }

    /// Put a club in `who`'s hand, swung with Brawling, and return it.
    fn club(world: &mut World, who: Entity) -> Entity {
        let key = ItemKey::parse("club", "test");
        let def = ItemDef {
            weapon: Some(crate::world::WeaponDef {
                skill: BASIC_ATTACK.into(),
                damage: (1, 1),
                damage_type: "physical".into(),
            }),
            ..ItemDef::named("club")
        };
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(key.clone(), def);
        world
            .spawn((
                Item {
                    key,
                    name: "club".into(),
                    id: None,
                    state: DoorState::Open,
                    quality: Quality::Standard,
                },
                ItemLocation::Held(who),
            ))
            .id()
    }

    /// Someone whose fists never miss.
    fn sure_hands(world: &mut World, who: Entity) {
        let mut skills = world.get_mut::<Skills>(who).unwrap();
        skills.0.get_mut(BASIC_ATTACK).unwrap().level = 1000;
    }

    /// Make `who` a player, as far as combat cares.
    fn as_player(world: &mut World, who: Entity) {
        world.entity_mut(who).insert(Player {
            name: "Grok".into(),
            character_id: 1,
            account: "grok".into(),
            role: crate::db::Role::Player,
            race: "human".into(),
            background: String::new(),
            description: String::new(),
        });
    }

    /// Hide `who` from everyone.
    fn hide(world: &mut World, who: Entity) {
        world.get_mut::<Conditions>(who).unwrap().apply(
            &defs_on_disk(),
            HIDDEN,
            10,
            Lasts::Indefinitely,
        );
    }

    /// A spell for these tests, cast through `school`, with text that says what happened: "You
    /// spark {target}." and so on, and `{harm}` in every hit line.
    fn spell(key: &str, aim: Aim, resist: Option<Resist>, effects: Vec<Effect>) -> Spell {
        let (_, id) = key.split_once(':').unwrap();
        let lines = |verb: &str| Lines {
            actor: format!("You {verb} {{target}} ({{harm}})."),
            target: Some(format!("{{actor}} {verb}s you ({{harm}}).")),
            room: format!("{{actor}} {verb}s {{target}} ({{harm}})."),
            beyond: None,
        };
        let def = SpellDef {
            name: id.into(),
            description: id.into(),
            school: SCHOOL.into(),
            min_level: 0,
            price: 0,
            drain: 2,
            aim,
            resist,
            tags: Vec::new(),
            effects,
            text: SkillText {
                hit: lines(id),
                miss: Some(lines(&format!("miss with a {id} at"))),
            },
        };
        def.check().unwrap();
        Spell {
            key: key.into(),
            def,
        }
    }

    /// The school the test spells are cast through.
    const SCHOOL: &str = "hermetic:foundation";

    /// Teach `who` these spells and their school, with Magic enough that they never fail.
    fn caster(world: &mut World, who: Entity, spells: Vec<Spell>) {
        world.resource_mut::<SkillDefs>().skills.extend([skill(
            SCHOOL,
            SkillKind::Passive {
                triggers: Vec::new(),
                bonuses: Vec::new(),
                counter: None,
                reactions: Vec::new(),
            },
        )]);
        world
            .get_mut::<Skills>(who)
            .unwrap()
            .0
            .insert(SCHOOL.into(), SkillState::new("Template"));
        world
            .get_mut::<Attributes>(who)
            .unwrap()
            .set(Attribute::Magic, 1000);
        let keys = spells.iter().map(|s| s.key.clone()).collect();
        world.entity_mut(who).insert(Spells(keys));
        let mut defs = world.resource_mut::<SpellDefs>();
        for spell in spells {
            defs.0.insert(spell.key.clone(), spell);
        }
    }

    #[test]
    fn a_spell_cast_in_a_fight_hurts_drains_and_practises_its_school() {
        let (mut world, room) = world();
        let mage = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, mage);
        let spark = spell(
            "hermetic:spark",
            Aim::Foe,
            Some(Resist::Dodge),
            vec![Effect::Damage {
                amount: Formula::parse("roll(2, 2)").unwrap(),
                damage_type: "force".into(),
            }],
        );
        caster(&mut world, mage, vec![spark]);
        world.entity_mut(mage).insert((
            Fighting(rat),
            Casting {
                spell: "hermetic:spark".into(),
                target: rat,
                ready_at: 0.0,
            },
        ));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, mage, "You spark the brawler"));
        assert!(world.get::<Vitals>(rat).unwrap().health < 100.0);
        assert!(
            world.get::<Vitals>(mage).unwrap().stamina < 100.0,
            "drained"
        );
        assert!(world.get::<Skills>(mage).unwrap().0[SCHOOL].practice > 0.0);
        assert!(world.get::<Casting>(mage).is_none(), "cast, and done");
    }

    #[test]
    fn armour_stops_physical_harm_but_not_force() {
        let hurt = |kind: &str| {
            let (mut world, room) = world();
            let mage = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
            let knight = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
            armour(
                &mut world,
                "plate",
                Some(Slot::Body),
                // Thick enough for a bolt from the test caster's great power.
                ArmourDef {
                    reduction: 10_000,
                    ..ArmourDef::default()
                },
                ItemLocation::Worn(knight, Slot::Body),
            );
            let bolt = spell(
                "hermetic:bolt",
                Aim::Foe,
                Some(Resist::Dodge),
                vec![Effect::Damage {
                    amount: Formula::parse("roll(5, 5)").unwrap(),
                    damage_type: kind.into(),
                }],
            );
            caster(&mut world, mage, vec![bolt]);
            world.entity_mut(mage).insert((
                Fighting(knight),
                Casting {
                    spell: "hermetic:bolt".into(),
                    target: knight,
                    ready_at: 0.0,
                },
            ));
            world.run_system_once(combat_round).unwrap();
            100.0 - world.get::<Vitals>(knight).unwrap().health
        };
        assert_eq!(hurt("physical"), 0.0, "plate stops it all");
        assert!(hurt("force") > 0.0, "force goes through plate");
    }

    #[test]
    fn a_ward_takes_blows_in_its_bearers_place() {
        let (mut world, room) = world();
        let brute = fighter(&mut world, room, 5, 1, &[BASIC_ATTACK]);
        let warded = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, brute);
        let defs = states::defs_on_disk();
        world.get_mut::<Conditions>(warded).unwrap().apply(
            &defs,
            "warded",
            1000,
            Lasts::Until(1e9),
        );
        world.entity_mut(brute).insert(Fighting(warded));
        world.run_system_once(combat_round).unwrap();
        assert_eq!(world.get::<Vitals>(warded).unwrap().health, 100.0);
        assert!(said(&world, warded, "Your ward takes the blow"));
    }

    #[test]
    fn a_hurt_mob_heals_itself_with_a_spell() {
        let (mut world, room) = world();
        // A foe who can't land a blow, against a badly hurt healer who always casts.
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let healer = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        let mend = spell(
            "divine:mend",
            Aim::Ally,
            None,
            vec![Effect::Heal {
                amount: Formula::number(5.0),
            }],
        );
        caster(&mut world, healer, vec![mend]);
        world.entity_mut(healer).insert(Tactics(100));
        world.get_mut::<Vitals>(healer).unwrap().health = 20.0;
        world.entity_mut(foe).insert(Fighting(healer));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Vitals>(healer).unwrap().health > 20.0);
    }

    #[test]
    fn a_spell_cures_removes_and_empowers_conditions() {
        let (mut world, room) = world();
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let priest = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        let purify = spell(
            "divine:purify",
            Aim::Ally,
            None,
            vec![
                Effect::Cure {
                    tag: "poison".into(),
                    on: Who::Target,
                },
                Effect::Remove {
                    condition: "held".into(),
                    on: Who::Target,
                },
                Effect::Apply {
                    condition: "blessed".into(),
                    strength: Formula::parse("1 + power / 3").unwrap(),
                    lasts: Lasting::Minutes(10),
                    on: Who::Target,
                },
            ],
        );
        caster(&mut world, priest, vec![purify]);
        let defs = defs_on_disk();
        let mut conditions = world.get_mut::<Conditions>(priest).unwrap();
        conditions.apply(&defs, "poisoned", 1, Lasts::Turns(5));
        conditions.apply(&defs, "held", 1, Lasts::Turns(5));
        conditions.apply(&defs, "guarding", 2, Lasts::Turns(5));
        world.entity_mut(priest).insert(Tactics(100));
        world.entity_mut(foe).insert(Fighting(priest));
        world.run_system_once(combat_round).unwrap();
        let conditions = world.get::<Conditions>(priest).unwrap();
        assert!(!conditions.has("poisoned"), "cured");
        assert!(!conditions.has("held"), "removed");
        assert!(conditions.has("guarding"), "untouched");
        assert!(
            conditions.attack(&defs) > 1,
            "a strong caster's blessing is stronger"
        );
    }

    #[test]
    fn skills_made_for_it_help_resist_a_spell() {
        // How often a weak sleep spell takes hold in twenty casts, against a target with or
        // without a skill that resists sleep.
        let slept = |resists: bool| {
            (0..20)
                .filter(|seed| {
                    let (mut world, room) = world();
                    world.insert_resource(Dice::seeded(*seed));
                    let mage = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
                    let elf = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
                    let sleep = spell(
                        "hermetic:sleep",
                        Aim::Foe,
                        Some(Resist::Attribute(Attribute::Willpower)),
                        vec![Effect::Apply {
                            condition: "asleep".into(),
                            strength: Formula::number(1.0),
                            lasts: Lasting::Turns(5),
                            on: Who::Target,
                        }],
                    );
                    caster(&mut world, mage, vec![sleep]);
                    world
                        .get_mut::<Attributes>(mage)
                        .unwrap()
                        .set(Attribute::Magic, 3);
                    if resists {
                        let (key, fey) = skill(
                            "test:steady_mind",
                            SkillKind::Passive {
                                triggers: Vec::new(),
                                bonuses: vec![PassiveBonus {
                                    stat: Stat::Resist("sleep".into()),
                                    per: 10,
                                    base: 50,
                                    with: None,
                                }],
                                counter: None,
                                reactions: Vec::new(),
                            },
                        );
                        world
                            .resource_mut::<SkillDefs>()
                            .skills
                            .insert(key.clone(), fey);
                        let mut skills = world.get_mut::<Skills>(elf).unwrap();
                        skills.0.insert(key, SkillState::new("Starting"));
                    }
                    world.entity_mut(mage).insert((Fighting(elf), Tactics(100)));
                    world.run_system_once(combat_round).unwrap();
                    world.get::<Conditions>(elf).unwrap().has("asleep")
                })
                .count()
        };
        let (without, with) = (slept(false), slept(true));
        assert!(without >= 10, "a weak spell mostly works ({without})");
        assert!(with <= 2, "but an elf mostly shrugs it off ({with})");
    }

    #[test]
    fn traits_stay_through_cures_and_death_and_help_resist() {
        let defs = states::defs_on_disk();
        let mut elf = Conditions::default().with_traits(&["fey_mind".to_string()]);
        let sleep = ["sleep".to_string()];
        assert_eq!(elf.withstanding(&defs, &sleep), 4.0);
        assert!(
            !elf.apply(&defs, "fey_mind", 1, Lasts::Turns(3)),
            "not put on as a condition"
        );
        assert!(!elf.remove("fey_mind"));
        elf.apply(&defs, "blessed", 1, Lasts::Turns(3));
        elf.cure(&defs, "magic");
        elf.clear();
        assert!(elf.has("fey_mind"), "cures and death leave it");
        assert!(!elf.has("blessed"));
        assert!(
            elf.listed(0.0, &defs, |_| String::new()).is_empty(),
            "not in affects"
        );
        assert_eq!(elf.names(&defs, true), vec!["Fey Mind".to_string()]);
    }

    #[test]
    fn a_weapons_conditions_ride_on_its_blows_until_spent() {
        let (mut world, room) = world();
        let hitter = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, hitter);
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let club = club(&mut world, hitter);
        world
            .get_mut::<ItemConditions>(club)
            .unwrap()
            .imbue(ItemCondition {
                key: "envenomed".into(),
                strength: 1,
                ends_at: None,
                charges: Some(1),
            });
        world.entity_mut(hitter).insert(Fighting(foe));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(foe).unwrap().has("poisoned"));
        assert!(
            world.get::<ItemConditions>(club).unwrap().0.is_empty(),
            "its one charge spent"
        );
        assert!(said(&world, hitter, "venom is gone"));
    }

    #[test]
    fn a_spell_imbues_the_weapon_in_hand() {
        let (mut world, room) = world();
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let mage = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        let club = club(&mut world, mage);
        let kindle = spell(
            "hermetic:kindle",
            Aim::Ally,
            None,
            vec![Effect::Imbue {
                condition: "flaming".into(),
                strength: Formula::parse("1 + power / 3").unwrap(),
                lasts: Some(Lasting::Minutes(10)),
                charges: Some(5),
                on: Who::Target,
            }],
        );
        caster(&mut world, mage, vec![kindle]);
        world.entity_mut(mage).insert(Tactics(100));
        world.entity_mut(foe).insert(Fighting(mage));
        world.run_system_once(combat_round).unwrap();
        let on_club = &world.get::<ItemConditions>(club).unwrap().0;
        assert_eq!(on_club.len(), 1);
        assert_eq!(on_club[0].key, "flaming");
        assert_eq!(on_club[0].charges, Some(5));
        assert!(
            on_club[0].strength > 1,
            "a strong caster's flames burn hotter"
        );
        assert!(on_club[0].ends_at.is_some());
        assert!(said(&world, mage, "Flames run along your weapon!"));
    }

    /// Teach `who` a passive skill whose one reaction is `on`, doing `effects`.
    fn reacting(world: &mut World, who: Entity, key: &str, on: When, effects: Vec<Effect>) {
        let lines = Lines {
            actor: "You react to {target}.".into(),
            target: Some("{actor} reacts to you.".into()),
            room: "{actor} reacts to {target}.".into(),
            beyond: None,
        };
        let (key, skill) = skill(
            key,
            SkillKind::Passive {
                triggers: Vec::new(),
                bonuses: Vec::new(),
                counter: None,
                reactions: vec![Reaction {
                    on,
                    chance: None,
                    only: None,
                    effects,
                    text: lines,
                }],
            },
        );
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), skill);
        world
            .get_mut::<Skills>(who)
            .unwrap()
            .0
            .insert(key, SkillState::new("Template"));
    }

    fn on_self(condition: &str) -> Effect {
        Effect::Apply {
            condition: condition.into(),
            strength: Formula::number(1.0),
            lasts: Lasting::Turns(3),
            on: Who::User,
        }
    }

    #[test]
    fn thorns_tear_at_whoever_strikes_but_set_off_nothing_more() {
        let (mut world, room) = world();
        let defs = defs_on_disk();
        let striker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, striker);
        let thorny = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        for who in [striker, thorny] {
            world
                .get_mut::<Conditions>(who)
                .unwrap()
                .apply(&defs, "thorns", 3, Lasts::Turns(10));
        }
        // The thorny one can't strike back, so only their thorns answer.
        world
            .get_mut::<Conditions>(thorny)
            .unwrap()
            .apply(&defs, "stunned", 1, Lasts::Turns(2));
        world.entity_mut(striker).insert(Fighting(thorny));
        world.run_system_once(combat_round).unwrap();
        assert_eq!(
            world.get::<Vitals>(striker).unwrap().health,
            97.0,
            "3 thorns"
        );
        assert!(said(&world, thorny, "Your thorny hide"));
        assert!(
            !said(&world, striker, "Your thorny hide"),
            "the striker's own thorns don't answer the thorns"
        );
    }

    #[test]
    fn a_reaction_at_no_chance_never_happens() {
        let (mut world, room) = world();
        let defs = defs_on_disk();
        let striker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, striker);
        let thorny = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .resource_mut::<ConditionDefs>()
            .0
            .get_mut("thorns")
            .unwrap()
            .reactions[0]
            .chance = Some(Formula::number(0.0));
        let mut conditions = world.get_mut::<Conditions>(thorny).unwrap();
        conditions.apply(&defs, "thorns", 3, Lasts::Turns(10));
        conditions.apply(&defs, "stunned", 1, Lasts::Turns(2));
        world.entity_mut(striker).insert(Fighting(thorny));
        world.run_system_once(combat_round).unwrap();
        assert_eq!(world.get::<Vitals>(striker).unwrap().health, 100.0);
    }

    #[test]
    fn a_killing_sets_off_a_reaction() {
        // The victor is strengthened.
        let (mut world, room) = world();
        let victor = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK]);
        let victim = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        reacting(
            &mut world,
            victor,
            "test:bloodlust",
            When::Kills,
            vec![on_self("strengthened")],
        );
        world.entity_mut(victor).insert(Fighting(victim));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(victor).unwrap().has("strengthened"));
        assert_eq!(
            deaths(&world),
            vec![(victim, Some(victor))],
            "named the killer"
        );
    }

    #[test]
    fn whoever_made_them_bleed_killed_them() {
        let (mut world, room) = world();
        let cutter = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let victim = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Vitals>(victim).unwrap().health = 1.0;
        world.resource_scope(|world, defs: Mut<ConditionDefs>| {
            let mut conditions = world.get_mut::<Conditions>(victim).unwrap();
            conditions.apply_by(&defs, "bleeding", 5, Lasts::Turns(1), Some(cutter));
        });
        // Only the victim takes a turn, and bleeds out before striking.
        world.entity_mut(victim).insert(Fighting(cutter));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Dead>(victim).is_some());
        assert_eq!(world.get::<Slayer>(victim).unwrap().0, cutter, "their kill");
        assert_eq!(deaths(&world), vec![(victim, Some(cutter))]);
    }

    /// Who died this round, and who killed them.
    fn deaths(world: &World) -> Vec<(Entity, Option<Entity>)> {
        let events = world.resource::<Messages<GameEvent>>();
        events
            .get_cursor()
            .read(events)
            .filter_map(|e| match e {
                GameEvent::Died { who, killer, .. } => Some((*who, *killer)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_dying_sets_off_a_reaction_at_the_killer() {
        // A dying curse: the killer is weakened.
        let (mut world, room) = world();
        let victor = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK]);
        let victim = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let curse = Effect::Apply {
            condition: "weakened".into(),
            strength: Formula::number(1.0),
            lasts: Lasting::Turns(3),
            on: Who::Target,
        };
        reacting(&mut world, victim, "test:curse", When::Dies, vec![curse]);
        world.entity_mut(victor).insert(Fighting(victim));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Dead>(victim).is_some());
        assert!(world.get::<Conditions>(victor).unwrap().has("weakened"));
    }

    #[test]
    fn a_reaction_for_foes_only_ignores_allies() {
        // Struck by its own kind, nothing; by a rat, it bristles.
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let deer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let herdmate = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        reacting(
            &mut world,
            deer,
            "test:bristle",
            When::Struck,
            vec![on_self("strengthened")],
        );
        if let Some(SkillKind::Passive { reactions, .. }) = world
            .resource_mut::<SkillDefs>()
            .skills
            .get_mut("test:bristle")
            .map(|s| &mut s.def.kind)
        {
            reactions[0].only = Some(crate::effects::Only::Foes);
        }
        sure_hands(&mut world, herdmate);
        world.entity_mut(herdmate).insert(Fighting(deer));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get::<Vitals>(deer).unwrap().health < 100.0, "struck");
        assert!(!world.get::<Conditions>(deer).unwrap().has("strengthened"));
        let rat = other_mob(&mut world, room, "rat", &["vermin"]);
        sure_hands(&mut world, rat);
        world.entity_mut(herdmate).remove::<Fighting>();
        world.entity_mut(deer).remove::<Fighting>();
        world.entity_mut(rat).insert(Fighting(deer));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(deer).unwrap().has("strengthened"));
    }

    #[test]
    fn a_dying_ooze_splits_and_the_halves_fight_its_killer() {
        let (mut world, room) = world();
        let half = MobKey::parse("test:half", "");
        world.insert_resource(MobTemplates(HashMap::from([(
            half.clone(),
            crate::world::MobDef::named("half an ooze"),
        )])));
        world.insert_resource(crate::characters::Races::default());
        let victor = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK]);
        let ooze = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        // A bare id is in the ooze's own zone.
        let split = Effect::Spawn {
            mob: "half".into(),
            count: Formula::number(2.0),
        };
        reacting(&mut world, ooze, "test:split", When::Dies, vec![split]);
        world.entity_mut(victor).insert(Fighting(ooze));
        world.run_system_once(combat_round).unwrap();
        world.run_system_once(spawning::spawn_from_effects).unwrap();
        world.flush();
        let halves: Vec<(Entity, Option<Entity>)> = world
            .query::<(Entity, &Mob, &InRoom, Option<&Fighting>)>()
            .iter(&world)
            .filter(|(_, mob, at, _)| mob.key == half && at.0 == room)
            .map(|(e, _, _, fighting)| (e, fighting.map(|f| f.0)))
            .collect();
        assert_eq!(halves.len(), 2);
        assert!(halves.iter().all(|(_, foe)| *foe == Some(victor)));
    }

    #[test]
    fn a_bad_wound_sets_off_a_reaction() {
        // Below half: the wounded raise their guard.
        let (mut world, room) = world();
        let striker = fighter(&mut world, room, 40, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, striker);
        let wounded = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Vitals>(wounded).unwrap().health = 60.0;
        reacting(
            &mut world,
            wounded,
            "test:last_stand",
            When::Hurt(0.5),
            vec![on_self("guarding")],
        );
        world.entity_mut(striker).insert(Fighting(wounded));
        world.run_system_once(combat_round).unwrap();
        let health = world.get::<Vitals>(wounded).unwrap().health;
        assert!(health > 0.0 && health < 50.0, "{health}");
        assert!(world.get::<Conditions>(wounded).unwrap().has("guarding"));
    }

    #[test]
    fn a_calmed_animal_stops_fighting() {
        let (mut world, room) = world();
        let shaman = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        let dog = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "beast:bite"]);
        as_player(&mut world, shaman);
        world.entity_mut(dog).insert(Aggressive::ANYWHERE);
        let calm = spell(
            "shamanic:beast_speech",
            Aim::Other,
            Some(Resist::Attribute(Attribute::Willpower)),
            vec![Effect::Calm],
        );
        caster(&mut world, shaman, vec![calm]);
        world.entity_mut(shaman).insert((
            Fighting(dog),
            Casting {
                spell: "shamanic:beast_speech".into(),
                target: dog,
                ready_at: 0.0,
            },
        ));
        world.entity_mut(dog).insert(Fighting(shaman));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get::<Fighting>(dog).is_none());
        assert!(world.get::<Fighting>(shaman).is_none());
        assert!(world.get::<Aggressive>(dog).is_none());
    }

    #[test]
    fn a_blessing_wears_off_in_time_fighting_or_not() {
        let (mut world, room) = world();
        let resting = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Conditions>(resting).unwrap().apply(
            &states::defs_on_disk(),
            "blessed",
            2,
            Lasts::Until(600.0),
        );
        world.run_system_once(expire_conditions).unwrap();
        assert_eq!(
            world
                .get::<Conditions>(resting)
                .unwrap()
                .attack(&states::defs_on_disk()),
            2
        );
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(600));
        world.run_system_once(expire_conditions).unwrap();
        assert_eq!(
            world
                .get::<Conditions>(resting)
                .unwrap()
                .attack(&states::defs_on_disk()),
            0
        );
        assert!(said(&world, resting, "blessing fades"));
    }

    #[test]
    fn a_backstab_from_hiding_cant_be_avoided() {
        let (mut world, room) = world();
        world.resource_mut::<SkillDefs>().skills.extend([skill(
            "brawling:backstab",
            SkillKind::Technique(Technique {
                resist: Some(Resist::Evasion),
                tags: Vec::new(),
                with: BASIC_ATTACK.into(),
                cost: Cost::Free,
                accuracy: 0,
                damage: 0,
                strike: StrikeKind::Hit,
                effects: Vec::new(),
                opener: true,
            }),
        )]);
        // A defender who dodges everything, against a weak attacker hiding in the shadows.
        let assassin = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "brawling:backstab"]);
        let mark = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        hide(&mut world, assassin);
        world.entity_mut(assassin).insert((
            Fighting(mark),
            Readied {
                skill: "brawling:backstab".into(),
                ready_at: 0.0,
            },
        ));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Vitals>(mark).unwrap().health < 100.0);
        // Blows have been struck: the assassin is seen now.
        assert!(
            world
                .get::<Conditions>(assassin)
                .unwrap()
                .hidden()
                .is_none()
        );
    }

    #[test]
    fn aggressive_mobs_ignore_players_they_havent_noticed() {
        let (mut world, room) = world();
        let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(rat).insert(Aggressive::ANYWHERE);
        let player = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, player);
        world.entity_mut(player).insert(Keywords::from_name("Grok"));
        hide(&mut world, player);
        aggressive_template(&mut world, None);
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert!(world.get::<Fighting>(rat).is_none(), "slipped past");
        // Noticed, they're fair game.
        let mut conditions = world.get_mut::<Conditions>(player).unwrap();
        conditions.hidden_mut().unwrap().seen_by.insert(rat);
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert_eq!(world.get::<Fighting>(rat).map(|f| f.0), Some(player));
        assert!(said(&world, player, "attacks you!"));
    }

    #[test]
    fn a_territorial_mob_attacks_only_at_home() {
        let (mut world, room) = world();
        let cave = world.spawn_empty().id();
        let bear = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .entity_mut(bear)
            .insert(Aggressive { home: Some(cave) });
        let player = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, player);
        world.entity_mut(player).insert(Keywords::from_name("Grok"));
        aggressive_template(&mut world, None);
        // Met on the path, it lets them pass.
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert!(world.get::<Fighting>(bear).is_none(), "away from home");
        // In its cave, it doesn't.
        world.get_mut::<InRoom>(bear).unwrap().0 = cave;
        world.get_mut::<InRoom>(player).unwrap().0 = cave;
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert_eq!(world.get::<Fighting>(bear).map(|f| f.0), Some(player));
    }

    #[test]
    fn fleeing_a_fight_keeps_away_from_what_a_mob_avoids() {
        let (mut world, room) = world();
        world.init_resource::<crate::light::LitRooms>();
        let side = |world: &mut World, id: &str| {
            world
                .spawn((
                    Room {
                        name: id.into(),
                        description: String::new(),
                        no_wander: false,
                        indoors: true,
                        terrain: Terrain::Paved,
                    },
                    RoomKey::parse(id, ""),
                    Exits::default(),
                ))
                .id()
        };
        let (den, glade) = (side(&mut world, "test:den"), side(&mut world, "test:glade"));
        let exit = |to| crate::world::Exit {
            to,
            door: None,
            needs: None,
        };
        world.entity_mut(room).insert(Exits(
            [
                ("east".to_string(), exit(den)),
                ("west".to_string(), exit(glade)),
            ]
            .into(),
        ));
        // The fighters' template avoids wolves, and a wolf waits in the den.
        let mut shy = crate::world::MobDef::named("a brawler");
        shy.avoids = vec!["wolf".into()];
        let wolf_key = MobKey::parse("test:wolf", "");
        world.insert_resource(MobTemplates(HashMap::from([
            (MobKey::parse("test:mob", ""), shy),
            (wolf_key.clone(), crate::world::MobDef::named("a wolf")),
        ])));
        let wolf = fighter(&mut world, den, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Mob>(wolf).unwrap().key = wolf_key;
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let runner = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let mut escaped = 0;
        for _ in 0..12 {
            world.get_mut::<InRoom>(runner).unwrap().0 = room;
            world.entity_mut(runner).insert(Fighting(foe));
            world.get_mut::<Vitals>(runner).unwrap().stamina = 100.0;
            let inv = Invocation {
                entity: runner,
                command: "flee",
                usage: "",
                args: String::new(),
            };
            world.run_system_once_with(flee, inv).unwrap();
            world.flush();
            let at = world.get::<InRoom>(runner).unwrap().0;
            assert_ne!(at, den, "into the den");
            escaped += usize::from(at == glade);
        }
        assert!(escaped > 0, "it gets away, the other way");
    }

    #[test]
    fn armour_counts_where_it_belongs() {
        let (mut world, room) = world();
        let who = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let plate = ArmourDef {
            reduction: 4,
            penalty: 3,
            block: 0,
            against: Vec::new(),
        };
        let shield = ArmourDef {
            reduction: 0,
            penalty: 1,
            block: 4,
            against: Vec::new(),
        };
        armour(
            &mut world,
            "plate",
            Some(Slot::Body),
            plate,
            ItemLocation::Worn(who, Slot::Body),
        );
        armour(&mut world, "shield", None, shield, ItemLocation::Held(who));
        // Carried, not worn: a spare helm in hand counts for nothing.
        let helm = ArmourDef {
            reduction: 1,
            penalty: 1,
            block: 0,
            against: Vec::new(),
        };
        armour(
            &mut world,
            "helm",
            Some(Slot::Head),
            helm,
            ItemLocation::Held(who),
        );
        let total = world
            .run_system_once(move |items: ItemQuery, templates: Res<ItemTemplates>| {
                strike::gear(who, &items, &templates)
            })
            .unwrap();
        assert_eq!(
            total,
            ArmourDef {
                reduction: 4,
                penalty: 4,
                block: 4,
                against: Vec::new()
            }
        );
    }

    #[test]
    fn armour_can_turn_a_blow_to_nothing_and_a_point_can_find_the_gaps() {
        let (mut world, room) = world();
        world.resource_mut::<SkillDefs>().skills.extend([skill(
            "brawling:jab",
            SkillKind::Technique(Technique {
                resist: Some(Resist::Evasion),
                tags: Vec::new(),
                with: BASIC_ATTACK.into(),
                cost: Cost::Free,
                accuracy: 0,
                damage: 0,
                strike: StrikeKind::Hit,
                effects: vec![Effect::Pierce(3)],
                opener: false,
            }),
        )]);
        // Fists do 2, doubled by the crushing blows of an attacker who can't miss: 4. A breastplate
        // takes 4.
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "brawling:jab"]);
        let knight = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, attacker);
        let plate = ArmourDef {
            reduction: 4,
            penalty: 3,
            block: 0,
            against: Vec::new(),
        };
        armour(
            &mut world,
            "plate",
            Some(Slot::Body),
            plate,
            ItemLocation::Worn(knight, Slot::Body),
        );
        world.entity_mut(attacker).insert(Fighting(knight));
        world.entity_mut(knight).insert(Fighting(attacker));
        world.get_mut::<Skills>(knight).unwrap().0.clear();
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, attacker, "glances off the brawler's armour"));
        assert_eq!(world.get::<Vitals>(knight).unwrap().health, 100.0);
        // Piercing 3 of its 4 leaves 1 of armour: 3 gets through.
        world.entity_mut(attacker).insert(Readied {
            skill: "brawling:jab".into(),
            ready_at: 0.0,
        });
        world.run_system_once(combat_round).unwrap();
        assert_eq!(world.get::<Vitals>(knight).unwrap().health, 97.0);
    }

    #[test]
    fn a_shield_takes_the_credit_for_what_it_stops() {
        let (mut world, room) = world();
        let attacker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        // Reaction 1 and the shield's penalty of 1 leave no dodging at all: every miss is the
        // shield's.
        let defender = fighter(&mut world, room, 1, 1, &[]);
        let shield = ArmourDef {
            reduction: 0,
            penalty: 1,
            block: 50,
            against: Vec::new(),
        };
        armour(
            &mut world,
            "shield",
            None,
            shield,
            ItemLocation::Held(defender),
        );
        world.entity_mut(attacker).insert(Fighting(defender));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, defender, "You get your shield in the way"));
        assert!(said(
            &world,
            attacker,
            "gets a shield in the way of your attack"
        ));
        let events = world.resource::<Messages<GameEvent>>();
        let mut cursor = events.get_cursor();
        let blocked = cursor
            .read(events)
            .filter(|e| matches!(e, GameEvent::Blocked { who } if *who == defender))
            .count();
        assert!(blocked >= 1);
    }

    #[test]
    fn a_cleave_carries_on_into_a_second_foe() {
        let (mut world, room) = world();
        // The cleaver can't miss, and hits hard.
        let cleaver = fighter(
            &mut world,
            room,
            1000,
            1,
            &[BASIC_ATTACK, "brawling:cleave"],
        );
        let first = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let second = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(cleaver).insert((
            Fighting(first),
            Readied {
                skill: "brawling:cleave".into(),
                ready_at: 0.0,
            },
        ));
        world.entity_mut(second).insert(Fighting(cleaver));
        world.run_system_once(combat_round).unwrap();
        assert!(said(&world, cleaver, "carries on into"));
        assert!(world.get::<Vitals>(second).unwrap().health < 100.0);
    }

    #[test]
    fn a_technique_puts_conditions_on_the_target_and_the_user() {
        let (mut world, room) = world();
        let tripper = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK, "brawling:trip"]);
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(tripper).insert((
            Fighting(foe),
            Readied {
                skill: "brawling:trip".into(),
                ready_at: 0.0,
            },
        ));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(foe).unwrap().has("down"));
        assert!(world.get::<Conditions>(tripper).unwrap().has("exposed"));
        assert!(said(&world, foe, "You go down!"), "told in its words");
        assert_eq!(world.get::<Vitals>(foe).unwrap().health, 100.0, "harmless");
    }

    #[test]
    fn a_technique_can_be_resisted_by_an_attribute_and_do_any_damage() {
        // A trip made a grip against Strength, that burns besides, used on a foe this strong.
        let trip_against = |strength: i32| {
            let (mut world, room) = world();
            let grappler = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK, "brawling:trip"]);
            let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "brawling:riposte"]);
            world
                .get_mut::<Attributes>(foe)
                .unwrap()
                .set(Attribute::Strength, strength);
            let mut defs = world.resource_mut::<SkillDefs>();
            let trip = defs.skills.get_mut("brawling:trip").unwrap();
            let SkillKind::Technique(t) = &mut trip.def.kind else {
                unreachable!()
            };
            t.resist = Some(Resist::Attribute(Attribute::Strength));
            t.effects.push(Effect::Damage {
                amount: Formula::parse("roll(5, 5)").unwrap(),
                damage_type: "fire".into(),
            });
            world.entity_mut(grappler).insert((
                Fighting(foe),
                Readied {
                    skill: "brawling:trip".into(),
                    ready_at: 0.0,
                },
            ));
            world.run_system_once(combat_round).unwrap();
            (world, grappler, foe)
        };
        // A giant breaks the grip however quick the grappler, and that's no dodge to riposte.
        let (world, grappler, giant) = trip_against(100_000);
        assert!(!world.get::<Conditions>(giant).unwrap().has("down"));
        assert!(
            !said(&world, grappler, "ripost"),
            "a broken grip isn't a dodge"
        );
        // Someone weak goes down, and the fire hurts though the grab doesn't.
        let (world, _, weakling) = trip_against(1);
        assert!(world.get::<Conditions>(weakling).unwrap().has("down"));
        assert!(
            world.get::<Vitals>(weakling).unwrap().health < 100.0,
            "burned"
        );
    }

    #[test]
    fn mobs_make_their_techniques_by_their_tactics() {
        for (tactics, cleaves) in [(100, true), (0, false)] {
            let (mut world, room) = world();
            let cleaver = fighter(
                &mut world,
                room,
                1000,
                1,
                &[BASIC_ATTACK, "brawling:cleave"],
            );
            let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
            world
                .entity_mut(cleaver)
                .insert((Fighting(foe), Tactics(tactics)));
            world.run_system_once(combat_round).unwrap();
            assert_eq!(said(&world, foe, "cleaves"), cleaves, "tactics {tactics}");
        }
    }

    #[test]
    fn players_know_when_they_are_outmatched() {
        let (mut world, room) = world();
        let player = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        let giant = fighter(&mut world, room, 8, 8, &[BASIC_ATTACK]);
        let equal = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        let challenger = fighter(&mut world, room, 2, 2, &[BASIC_ATTACK]);
        for who in [player, challenger] {
            world.entity_mut(who).insert(Player {
                name: "Grok".into(),
                character_id: 1,
                account: "grok".into(),
                role: crate::db::Role::Player,
                race: "human".into(),
                background: String::new(),
                description: String::new(),
            });
        }
        world.entity_mut(player).insert(Fighting(giant));
        world.entity_mut(challenger).insert(Fighting(equal));
        world.run_system_once(combat_round).unwrap();
        // Attack and defence 2 against the giant's 8: four times outmatched. An equal foe is
        // even.
        let odds = |who| world.get::<Outmatched>(who).map(|o| o.0);
        assert!(
            odds(player).is_some_and(|o| (o - 4.0).abs() < 1e-9),
            "{:?}",
            odds(player)
        );
        assert!(odds(challenger).is_some_and(|o| (o - 1.0).abs() < 1e-9));
        // Mobs don't learn, so they aren't counted.
        assert!(odds(giant).is_none());
        assert_eq!(crate::skills::tough_fight_boost(odds(player)), 2.0);
        assert_eq!(crate::skills::tough_fight_boost(odds(challenger)), 1.0);
        assert_eq!(crate::skills::tough_fight_boost(Some(0.5)), 1.0);
    }

    #[test]
    fn a_hit_reads_by_the_share_of_health_it_took() {
        use strike::harm;
        assert_eq!(harm(0, 10), "doesn't hurt");
        assert_eq!(harm(1, 100), "scratches");
        // The same five points: a graze to a veteran, a massacre to a rat.
        assert_eq!(harm(5, 60), "grazes");
        assert_eq!(harm(5, 3), "MASSACRES");
        assert_eq!(harm(4, 10), "decimates");
    }

    #[test]
    fn decisive_wins_land_harder() {
        use strike::Blow;
        assert_eq!(Blow::of(1.0), Blow::Glancing);
        assert_eq!(Blow::of(1.3), Blow::Solid);
        assert_eq!(Blow::of(2.0), Blow::Strong);
        assert_eq!(Blow::of(10.0), Blow::Crushing);
        assert_eq!(Blow::Glancing.multiplier(), 0.5);
        assert_eq!(Blow::Crushing.multiplier(), 2.0);
    }

    #[test]
    fn victories_are_worth_the_risk_overcome() {
        assert_eq!(victory_experience(1.0), 50);
        assert_eq!(victory_experience(2.0), 89);
        assert!(victory_experience(0.75) > 0, "a near thing still counts");
        // A foe under about half your strength is worth nothing.
        assert_eq!(victory_experience(0.45), 0);
        assert!(victory_experience(0.5) > 0);
        assert!(victory_experience(100.0) <= 100);
    }

    #[test]
    fn names_read_naturally() {
        assert_eq!(the("a sewer rat"), "the sewer rat");
        assert_eq!(the("an old wolf"), "the old wolf");
        assert_eq!(the("Grok"), "Grok");
        assert_eq!(the("the drill sergeant"), "the drill sergeant");
    }
    #[test]
    fn a_turn_starting_sets_off_a_reaction() {
        // A fighter on their guard at the start of every turn.
        let (mut world, room) = world();
        let wary = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        reacting(
            &mut world,
            wary,
            "test:wary",
            When::TurnStarts,
            vec![on_self("guarding")],
        );
        world.entity_mut(wary).insert(Fighting(foe));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(wary).unwrap().has("guarding"));
        assert!(!world.get::<Conditions>(foe).unwrap().has("guarding"));
    }

    #[test]
    fn casting_sets_off_a_reaction() {
        let (mut world, room) = world();
        let mage = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let spark = spell(
            "hermetic:spark",
            Aim::Foe,
            Some(Resist::Dodge),
            vec![Effect::Damage {
                amount: Formula::parse("roll(2, 2)").unwrap(),
                damage_type: "force".into(),
            }],
        );
        caster(&mut world, mage, vec![spark]);
        reacting(
            &mut world,
            mage,
            "test:focus",
            When::Casts,
            vec![on_self("strengthened")],
        );
        world.entity_mut(mage).insert((
            Fighting(rat),
            Casting {
                spell: "hermetic:spark".into(),
                target: rat,
                ready_at: 0.0,
            },
        ));
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(mage).unwrap().has("strengthened"));
    }

    /// Lines for a reaction in a test.
    fn reaction_lines() -> Lines {
        Lines {
            actor: "Your omen stirs at {target}.".into(),
            target: Some("{actor}'s omen stirs at you.".into()),
            room: "{actor}'s omen stirs at {target}.".into(),
            beyond: None,
        }
    }

    /// A condition, "omen", made like blessed, with these reactions.
    fn omen(world: &mut World, reactions: Vec<(When, Effect)>) {
        let mut defs = world.resource_mut::<ConditionDefs>();
        let mut def = defs.get("blessed").unwrap().clone();
        def.reactions = reactions
            .into_iter()
            .map(|(on, effect)| Reaction {
                on,
                chance: None,
                only: None,
                effects: vec![effect],
                text: reaction_lines(),
            })
            .collect();
        defs.0.insert("omen".into(), def);
    }

    /// What's happened since the last, through to the reactions it sets off.
    fn react_to_events(world: &mut World) {
        world.run_system_once(triggers::triggers).unwrap();
        world.run_system_once(triggers::react).unwrap();
        world.flush();
    }

    #[test]
    fn a_condition_reacts_as_it_takes_hold_and_as_it_runs_out() {
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        let bearer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        omen(
            &mut world,
            vec![
                (When::Applied, on_self("guarding")),
                (When::Fades, on_self("strengthened")),
            ],
        );
        world.resource_scope(|world, defs: Mut<ConditionDefs>| {
            let mut conditions = world.get_mut::<Conditions>(bearer).unwrap();
            conditions.apply(&defs, "omen", 1, Lasts::Until(0.0));
        });
        world.write_message(GameEvent::ConditionApplied {
            who: bearer,
            by: bearer,
            condition: "omen".into(),
            strength: 1,
            reacting: false,
        });
        react_to_events(&mut world);
        let conditions = world.get::<Conditions>(bearer).unwrap();
        assert!(conditions.has("guarding"), "taking hold");
        assert!(!conditions.has("strengthened"));

        // Its time is up: it runs out, and leaves the bearer strengthened.
        world.run_system_once(expire_conditions).unwrap();
        react_to_events(&mut world);
        let conditions = world.get::<Conditions>(bearer).unwrap();
        assert!(!conditions.has("omen"));
        assert!(conditions.has("strengthened"), "running out");
    }

    #[test]
    fn a_condition_put_on_by_a_reaction_sets_off_nothing() {
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        let bearer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        omen(&mut world, vec![(When::Applied, on_self("guarding"))]);
        world.write_message(GameEvent::ConditionApplied {
            who: bearer,
            by: bearer,
            condition: "omen".into(),
            strength: 1,
            reacting: true,
        });
        react_to_events(&mut world);
        assert!(!world.get::<Conditions>(bearer).unwrap().has("guarding"));
    }

    #[test]
    fn arriving_sets_off_reactions_in_those_who_see_it() {
        // A watchful one leaves whoever comes in exposed, but not someone they didn't see.
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        let watcher = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let walker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let sneak = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let exposing = Effect::Apply {
            condition: "exposed".into(),
            strength: Formula::number(1.0),
            lasts: Lasting::Turns(3),
            on: Who::Target,
        };
        reacting(
            &mut world,
            watcher,
            "test:watchful",
            When::Arrives,
            vec![exposing],
        );
        reacting(
            &mut world,
            walker,
            "test:footsore",
            When::Enters,
            vec![on_self("guarding")],
        );
        for (who, witnesses) in [(walker, None), (sneak, Some(Vec::new()))] {
            world.write_message(GameEvent::Entered {
                who,
                name: "a brawler".into(),
                room,
                way: crate::events::Way::Walked(None),
                witnesses,
            });
        }
        react_to_events(&mut world);
        let has = |who, key| world.get::<Conditions>(who).unwrap().has(key);
        assert!(has(walker, "exposed"), "seen coming in");
        assert!(!has(sneak, "exposed"), "unseen");
        assert!(!has(watcher, "exposed"));
        assert!(has(walker, "guarding"), "their own coming in");
    }

    #[test]
    fn a_rooms_own_conditions_answer_whoever_comes_in() {
        // The brambles (`places.conditions.ron`) tear at whoever pushes in.
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        world
            .entity_mut(room)
            .insert(crate::world::RoomConditions(vec![("brambles".into(), 1)]));
        let walker = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.write_message(GameEvent::Entered {
            who: walker,
            name: "a brawler".into(),
            room,
            way: crate::events::Way::Walked(None),
            witnesses: None,
        });
        react_to_events(&mut world);
        let health = world.get::<Vitals>(walker).unwrap().health;
        assert!((98.0..100.0).contains(&health), "scratched: {health}");
        assert!(said(&world, walker, "as you push through."));
    }

    #[test]
    fn some_reactions_come_round_every_so_often() {
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        let bearer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        reacting(
            &mut world,
            bearer,
            "test:tides",
            When::Every(Lasting::Minutes(10)),
            vec![on_self("guarding")],
        );
        let react = world.register_system(triggers::react);
        world.run_system(react).unwrap();
        // Ten game minutes is 75 real seconds.
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(30));
        world.run_system(react).unwrap();
        assert!(!world.get::<Conditions>(bearer).unwrap().has("guarding"));
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs(50));
        world.run_system(react).unwrap();
        assert!(world.get::<Conditions>(bearer).unwrap().has("guarding"));
    }

    #[test]
    fn a_reaction_outside_a_fight_can_kill() {
        // A curse that does its bearer in as it runs out.
        let (mut world, room) = world();
        world.init_resource::<Messages<triggers::Trigger>>();
        let bearer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Vitals>(bearer).unwrap().health = 3.0;
        omen(
            &mut world,
            vec![(
                When::Fades,
                Effect::Damage {
                    amount: Formula::number(10.0),
                    damage_type: "physical".into(),
                },
            )],
        );
        let mut defs = world.resource_mut::<ConditionDefs>();
        defs.0.get_mut("omen").unwrap().reactions[0].text = Lines {
            actor: "Your doom {harm} {target}.".into(),
            target: Some("{actor}'s doom {harm} you.".into()),
            room: "{actor}'s doom {harm} {target}.".into(),
            beyond: None,
        };
        world.write_message(GameEvent::ConditionFaded {
            who: bearer,
            condition: "omen".into(),
            strength: 1,
        });
        react_to_events(&mut world);
        assert!(world.get::<Dead>(bearer).is_some());
        assert_eq!(world.get::<Slayer>(bearer).unwrap().0, bearer);
        assert_eq!(
            deaths(&world),
            vec![(bearer, None)],
            "killed by no one else"
        );
    }

    /// Teach `who` an active skill made of these effects, at a level that can't fail an easy
    /// contest, with First Aid's words.
    fn active(
        world: &mut World,
        who: Entity,
        key: &str,
        aim: Aim,
        uses: Option<&str>,
        against: Option<&str>,
        effects: Vec<Effect>,
    ) {
        let (key, skill) = skill(
            key,
            SkillKind::Active {
                command: None,
                quiet: false,
                provokes: false,
                cost: Cost::Stamina(2),
                cooldown: 5.0,
                aim,
                uses: uses.map(String::from),
                tool: None,
                resist: None,
                against: against.map(|f| Formula::parse(f).unwrap()),
                tags: Vec::new(),
                effects,
            },
        );
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), skill);
        let mut state = SkillState::new("Template");
        state.level = 1000;
        world.get_mut::<Skills>(who).unwrap().0.insert(key, state);
    }

    /// A bandage in `who`'s hand.
    fn bandage(world: &mut World, who: Entity) -> Entity {
        let key = ItemKey::parse("bandage", "test");
        let def = ItemDef {
            tags: vec!["bandage".into()],
            ..ItemDef::named("a bandage")
        };
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(key.clone(), def);
        world
            .spawn((
                Item {
                    key,
                    name: "a bandage".into(),
                    id: None,
                    state: DoorState::Open,
                    quality: Quality::Standard,
                },
                ItemLocation::Held(who),
            ))
            .id()
    }

    fn first_aid(world: &mut World, medic: Entity) {
        active(
            world,
            medic,
            "healing:first_aid",
            Aim::Ally,
            Some("bandage"),
            Some("(target.most_health - target.health) / 2"),
            vec![
                Effect::Heal {
                    amount: Formula::parse("10").unwrap(),
                },
                Effect::Remove {
                    condition: "bleeding".into(),
                    on: Who::Target,
                },
            ],
        );
    }

    fn use_ability(world: &mut World, who: Entity, args: &str) {
        let inv = Invocation {
            entity: who,
            command: "ability",
            usage: "",
            args: args.into(),
        };
        world.run_system_once_with(abilities::ability, inv).unwrap();
        world.flush();
    }

    #[test]
    fn first_aid_binds_a_wound_with_a_bandage() {
        let (mut world, room) = world();
        let medic = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let patient = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .entity_mut(patient)
            .insert(Keywords::from_name("patient"));
        first_aid(&mut world, medic);
        let roll = bandage(&mut world, medic);
        world.get_mut::<Vitals>(patient).unwrap().health = 50.0;
        let defs = defs_on_disk();
        world
            .get_mut::<Conditions>(patient)
            .unwrap()
            .apply(&defs, "bleeding", 1, Lasts::Turns(5));
        use_ability(&mut world, medic, "healing:first_aid patient");
        assert_eq!(world.get::<Vitals>(patient).unwrap().health, 60.0);
        assert!(!world.get::<Conditions>(patient).unwrap().has("bleeding"));
        assert!(world.get_entity(roll).is_err(), "the bandage is used up");
        assert_eq!(world.get::<Vitals>(medic).unwrap().stamina, 98.0);
        assert!(said(&world, medic, "You first_aid the brawler. (+10)"));
        assert!(said(&world, patient, "The brawler first_aids you. (+10)"));
        // And it needs a moment before the next.
        bandage(&mut world, medic);
        use_ability(&mut world, medic, "healing:first_aid patient");
        assert!(said(&world, medic, "You need a moment"));
    }

    #[test]
    fn first_aid_needs_a_wound_and_a_bandage() {
        let (mut world, room) = world();
        let medic = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        first_aid(&mut world, medic);
        use_ability(&mut world, medic, "healing:first_aid");
        assert!(said(&world, medic, "You aren't hurt."));
        world.get_mut::<Vitals>(medic).unwrap().health = 50.0;
        use_ability(&mut world, medic, "healing:first_aid");
        assert!(said(&world, medic, "You have no bandages."));
        assert_eq!(
            world.get::<Vitals>(medic).unwrap().stamina,
            100.0,
            "nothing spent on a refusal"
        );
        bandage(&mut world, medic);
        use_ability(&mut world, medic, "healing:first_aid");
        assert_eq!(
            world.get::<Vitals>(medic).unwrap().health,
            60.0,
            "on yourself"
        );
    }

    #[test]
    fn an_active_skill_turned_on_a_foe_starts_a_fight() {
        let (mut world, room) = world();
        let bully = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let victim = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .entity_mut(victim)
            .insert(Keywords::from_name("victim"));
        active(
            &mut world,
            bully,
            "presence:glare",
            Aim::Foe,
            None,
            None,
            vec![Effect::Apply {
                condition: "exposed".into(),
                strength: Formula::number(1.0),
                lasts: Lasting::Turns(2),
                on: Who::Target,
            }],
        );
        use_ability(&mut world, bully, "presence:glare");
        assert!(said(&world, bully, "Use presence:glare on whom?"));
        use_ability(&mut world, bully, "presence:glare victim");
        assert!(world.get::<Conditions>(victim).unwrap().has("exposed"));
        assert_eq!(world.get::<Fighting>(bully).map(|f| f.0), Some(victim));
        assert_eq!(world.get::<Fighting>(victim).map(|f| f.0), Some(bully));
    }

    /// Teach `thief` Pickpocketing at `level`: their skill against the mark's Intuition, for
    /// everything in the mark's purse, and caught, the mark turns on them.
    fn pickpocket(world: &mut World, thief: Entity, level: u32) {
        let (key, mut skill) = skill(
            "thievery:pickpocketing",
            SkillKind::Active {
                command: None,
                quiet: false,
                provokes: true,
                cost: Cost::Free,
                cooldown: 0.0,
                aim: Aim::Other,
                uses: None,
                tool: None,
                resist: Some(Resist::Attribute(Attribute::Intuition)),
                against: None,
                tags: Vec::new(),
                effects: vec![Effect::Take(crate::effects::Taken::Coins(
                    Formula::parse("target.coins").unwrap(),
                ))],
            },
        );
        if let Some(text) = skill.def.text.as_mut() {
            text.hit.actor = "You lift {coins} from {target}.".into();
        }
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), skill);
        let mut state = SkillState::new("Template");
        state.level = level;
        world.get_mut::<Skills>(thief).unwrap().0.insert(key, state);
        world.entity_mut(thief).insert(Purse(0));
    }

    #[test]
    fn a_clean_lift_takes_coins_from_a_mobs_purse() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let mark = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .entity_mut(mark)
            .insert((Keywords::from_name("mark"), Purse(250)));
        pickpocket(&mut world, thief, 1000);
        use_ability(&mut world, thief, "thievery:pickpocketing mark");
        assert_eq!(world.get::<Purse>(mark).unwrap().0, 0);
        assert_eq!(world.get::<Purse>(thief).unwrap().0, 250);
        assert!(said(
            &world,
            thief,
            "You lift 2 gold and 5 silver from the brawler."
        ));
        assert!(world.get::<Fighting>(mark).is_none(), "unnoticed");
    }

    #[test]
    fn a_caught_pickpocket_is_attacked() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let mark = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world
            .entity_mut(mark)
            .insert((Keywords::from_name("mark"), Purse(250)));
        world
            .get_mut::<Attributes>(mark)
            .unwrap()
            .set(Attribute::Intuition, 1000);
        pickpocket(&mut world, thief, 0);
        use_ability(&mut world, thief, "thievery:pickpocketing mark");
        assert_eq!(world.get::<Purse>(mark).unwrap().0, 250);
        assert_eq!(world.get::<Fighting>(mark).map(|f| f.0), Some(thief));
    }

    #[test]
    fn players_dont_rob_each_other() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let mark = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, mark);
        world
            .entity_mut(mark)
            .insert((Keywords::from_name("mark"), Purse(250)));
        pickpocket(&mut world, thief, 1000);
        use_ability(&mut world, thief, "thievery:pickpocketing mark");
        assert!(said(&world, thief, "You can't steal from other players."));
        assert_eq!(world.get::<Purse>(mark).unwrap().0, 250);
    }

    /// Teach `who` Stealth as it is in its file: hiding as well as their Stealth, at this level.
    fn stealthy(world: &mut World, who: Entity, level: u32) {
        let (key, skill) = skill(
            "stealth:basics",
            SkillKind::Active {
                command: None,
                quiet: true,
                provokes: false,
                cost: Cost::Free,
                cooldown: 0.0,
                aim: Aim::Me,
                uses: None,
                tool: None,
                resist: None,
                against: None,
                tags: Vec::new(),
                effects: vec![Effect::Apply {
                    condition: HIDDEN.into(),
                    strength: Formula::parse("power").unwrap(),
                    lasts: Lasting::Indefinitely,
                    on: Who::User,
                }],
            },
        );
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), skill);
        let mut state = SkillState::new("Template");
        state.level = level;
        world.get_mut::<Skills>(who).unwrap().0.insert(key, state);
    }

    #[test]
    fn hiding_is_seen_only_by_those_who_notice() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let guard = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, guard);
        stealthy(&mut world, thief, 1000);
        use_ability(&mut world, thief, "stealth:basics");
        let conditions = world.get::<Conditions>(thief).unwrap();
        let hidden = conditions.hidden().expect("hidden");
        assert!(hidden.strength > 1000, "as well as their Stealth");
        assert!(!conditions.seen_by(guard));
        assert!(said(&world, thief, "You basics yourself."));
        assert!(
            !said(&world, guard, "basics"),
            "the guard didn't notice, so sees nothing"
        );
    }

    #[test]
    fn hiding_in_front_of_watchful_eyes_fails() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let guard = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, guard);
        world
            .get_mut::<Attributes>(guard)
            .unwrap()
            .set(Attribute::Intuition, 1000);
        stealthy(&mut world, thief, 0);
        use_ability(&mut world, thief, "stealth:basics");
        assert!(world.get::<Conditions>(thief).unwrap().hidden().is_none());
        assert!(said(&world, thief, "You miss with a basics at yourself."));
        assert!(said(&world, guard, "miss with a basics"), "everyone saw");
    }

    #[test]
    fn no_hiding_in_a_fight_or_hiding_someone_else() {
        let (mut world, room) = world();
        let thief = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let guard = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(guard).insert(Keywords::from_name("guard"));
        stealthy(&mut world, thief, 1000);
        use_ability(&mut world, thief, "stealth:basics guard");
        assert!(said(
            &world,
            thief,
            "You can only use stealth:basics on yourself."
        ));
        world.entity_mut(thief).insert(Fighting(guard));
        use_ability(&mut world, thief, "stealth:basics");
        assert!(said(
            &world,
            thief,
            "You can't hide in the middle of a fight!"
        ));
        assert!(world.get::<Conditions>(thief).unwrap().hidden().is_none());
    }

    #[test]
    fn a_sword_in_hand_is_swung_with_swordplay_and_practises_it() {
        let (mut world, room) = world();
        let (key, swordplay) = skill(
            "swordplay:basics",
            SkillKind::Attack {
                damage: None,
                cost: Cost::Free,
                damage_type: "physical".into(),
            },
        );
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), swordplay);
        let fencer = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK, "swordplay:basics"]);
        as_player(&mut world, fencer);
        let dummy = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let sword = ItemKey::parse("sword", "test");
        world.resource_mut::<ItemTemplates>().0.insert(
            sword.clone(),
            ItemDef {
                weapon: Some(crate::world::WeaponDef {
                    skill: key.clone(),
                    damage: (3, 3),
                    damage_type: "physical".into(),
                }),
                ..ItemDef::named("a sword")
            },
        );
        world.spawn((
            Item {
                key: sword,
                name: "a sword".into(),
                id: None,
                state: DoorState::Open,
                quality: Quality::Standard,
            },
            ItemLocation::Held(fencer),
        ));
        world.entity_mut(fencer).insert(Fighting(dummy));
        world.run_system_once(combat_round).unwrap();
        let skills = world.get::<Skills>(fencer).unwrap();
        assert!(skills.0[&key].practice > 0.0, "Swordplay practised");
        assert_eq!(skills.0[BASIC_ATTACK].practice, 0.0, "not Brawling");
    }

    #[test]
    fn a_hurt_mob_binds_its_own_wounds_but_a_whole_one_doesnt() {
        let (mut world, room) = world();
        let medic = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        first_aid(&mut world, medic);
        let roll = bandage(&mut world, medic);
        world
            .entity_mut(medic)
            .insert((Tactics(100), Fighting(foe)));
        // Whole, it fights instead.
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(
            world.get_entity(roll).is_ok(),
            "no bandage spent on nothing"
        );
        // Badly hurt, it bandages itself, using it up.
        world.get_mut::<Vitals>(medic).unwrap().health = 30.0;
        world
            .get_mut::<Skills>(medic)
            .unwrap()
            .0
            .get_mut("healing:first_aid")
            .unwrap()
            .ready_at = 0.0;
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get_entity(roll).is_err(), "the bandage is used up");
        assert!(world.get::<Vitals>(medic).unwrap().health > 30.0);
    }

    #[test]
    fn a_mob_turns_its_skills_on_its_foe_but_not_twice_or_to_hide() {
        let (mut world, room) = world();
        let bully = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let foe = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        active(
            &mut world,
            bully,
            "presence:glare",
            Aim::Foe,
            None,
            None,
            vec![Effect::Apply {
                condition: "exposed".into(),
                strength: Formula::number(1.0),
                lasts: Lasting::Turns(5),
                on: Who::Target,
            }],
        );
        stealthy(&mut world, bully, 1000);
        world
            .entity_mut(bully)
            .insert((Tactics(100), Fighting(foe)));
        // Of these, only the glare does any good in a fight.
        let moves = world
            .run_system_once(
                move |mut fighters: Fighters,
                      items: ItemQuery,
                      templates: Res<ItemTemplates>,
                      defs: Res<SkillDefs>,
                      spells: Res<SpellDefs>,
                      conditions: Res<ConditionDefs>,
                      mut dice: ResMut<Dice>,
                      db: Res<Database>,
                      mut events: MessageWriter<GameEvent>,
                      mut log: ResMut<Outbox>| {
                    let none = HashMap::new();
                    let mut imbued = Imbued::default();
                    let mobs = MobTemplates::default();
                    let round = Round {
                        fighters: &mut fighters,
                        items: &items,
                        templates: &templates,
                        mobs: &mobs,
                        defs: &defs,
                        spells: &spells,
                        conditions: &conditions,
                        dice: &mut dice,
                        db: &db,
                        story: None,
                        events: &mut events,
                        log: &mut log,
                        watchers: &[],
                        engaged: &none,
                        targets: &none,
                        auras: &HashMap::new(),
                        resting: &HashSet::new(),
                        outmatched: &HashMap::new(),
                        ambushers: &HashSet::new(),
                        now: 0.0,
                        imbued: &mut imbued,
                    };
                    round
                        .mob_abilities(bully, foe)
                        .into_iter()
                        .map(|m| match m {
                            MobMove::Ability(key, whom) => (key, whom),
                            other => panic!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                },
            )
            .unwrap();
        assert_eq!(moves, vec![("presence:glare".to_string(), foe)]);
        world.run_system_once(combat_round).unwrap();
        assert!(world.get::<Conditions>(foe).unwrap().has("exposed"));
        assert!(world.get::<Conditions>(bully).unwrap().hidden().is_none());
    }

    /// Make every test mob's template (`test:mob`) aggressive, sparing `allies`.
    fn aggressive_template(world: &mut World, allies: Option<Vec<String>>) {
        let def = crate::world::MobDef {
            aggressive: crate::world::Aggression::Always,
            allies,
            ..crate::world::MobDef::named("a brawler")
        };
        world.insert_resource(MobTemplates(HashMap::from([(
            MobKey::parse("test:mob", ""),
            def,
        )])));
    }

    /// Spawn a mob of another template, `test:<id>`, tagged with `tags`.
    fn other_mob(world: &mut World, room: Entity, id: &str, tags: &[&str]) -> Entity {
        let key = MobKey::parse(&format!("test:{id}"), "");
        let def = crate::world::MobDef {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            ..crate::world::MobDef::named(id)
        };
        world
            .resource_mut::<MobTemplates>()
            .0
            .insert(key.clone(), def);
        let who = fighter(world, room, 1, 1, &[BASIC_ATTACK]);
        world.get_mut::<Mob>(who).unwrap().key = key;
        who
    }

    #[test]
    fn aggressive_mobs_with_allies_attack_mobs_that_arent() {
        let (mut world, room) = world();
        aggressive_template(&mut world, Some(vec!["townsfolk".into(), "players".into()]));
        let guard = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(guard).insert(Aggressive::ANYWHERE);
        let baker = other_mob(&mut world, room, "baker", &["townsfolk"]);
        let player = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, player);
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert!(
            world.get::<Fighting>(guard).is_none(),
            "townsfolk and players are spared"
        );
        let rat = other_mob(&mut world, room, "rat", &["vermin"]);
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert_eq!(world.get::<Fighting>(guard).map(|f| f.0), Some(rat));
        assert_eq!(
            world.get::<Fighting>(rat).map(|f| f.0),
            Some(guard),
            "fights back"
        );
        assert!(world.get::<Fighting>(baker).is_none());
        assert!(said(&world, player, "attacks"), "the room sees it");
    }

    #[test]
    fn a_pack_spreads_across_everyone_it_can_attack() {
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let key = crate::world::SpawnKey::parse("test:rats", "");
        let def = crate::world::ron_options()
            .from_str("(mob: \"mob\", room: \"r\", every: Minutes(1), refill: Keep(8), pack: 2)")
            .unwrap();
        world.insert_resource(Spawns(HashMap::from([(key.clone(), def)])));
        let rats: Vec<Entity> = (0..8)
            .map(|_| {
                let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
                world
                    .entity_mut(rat)
                    .insert((Aggressive::ANYWHERE, SpawnedBy(key.clone())));
                rat
            })
            .collect();
        let players: Vec<Entity> = ["Ada", "Bob", "Wren", "Tam"]
            .iter()
            .map(|name| {
                let player = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
                as_player(&mut world, player);
                world.entity_mut(player).insert(Keywords::from_name(name));
                player
            })
            .collect();
        world.run_system_once(aggression).unwrap();
        world.flush();
        for player in players {
            let on = rats
                .iter()
                .filter(|rat| world.get::<Fighting>(**rat).map(|f| f.0) == Some(player))
                .count();
            assert_eq!(on, 2, "two rats on each of the four");
        }
    }

    #[test]
    fn aggressive_mobs_without_allies_still_go_only_for_players() {
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(rat).insert(Aggressive::ANYWHERE);
        let dog = other_mob(&mut world, room, "dog", &[]);
        world.run_system_once(aggression).unwrap();
        world.flush();
        assert!(world.get::<Fighting>(rat).is_none(), "leaves the dog alone");
        assert!(world.get::<Fighting>(dog).is_none());
    }

    /// A row of rooms east of `room`, each joined to the last both ways. Returns them in order.
    fn rooms_east(world: &mut World, room: Entity, count: usize) -> Vec<Entity> {
        use crate::world::Exit;
        let mut row = vec![room];
        for i in 0..count {
            let next = world
                .spawn((
                    Room {
                        name: format!("Room {i}"),
                        description: String::new(),
                        no_wander: false,
                        indoors: true,
                        terrain: Terrain::Paved,
                    },
                    RoomKey::parse(&format!("test:room{i}"), ""),
                    Exits::default(),
                ))
                .id();
            let last = *row.last().unwrap();
            let exit = |to| Exit {
                to,
                door: None,
                needs: None,
            };
            world
                .get_mut::<Exits>(last)
                .unwrap()
                .0
                .insert("east".into(), exit(next));
            world
                .get_mut::<Exits>(next)
                .unwrap()
                .0
                .insert("west".into(), exit(last));
            row.push(next);
        }
        row
    }

    #[test]
    fn allies_within_earshot_come_running_to_a_call_for_help() {
        let (mut world, room) = world();
        aggressive_template(&mut world, Some(vec!["townsfolk".into(), "players".into()]));
        let row = rooms_east(&mut world, room, 3);
        let caller = fighter(&mut world, row[0], 1, 1, &[BASIC_ATTACK]);
        let rat = other_mob(&mut world, row[0], "rat", &["vermin"]);
        world.entity_mut(caller).insert(Fighting(rat));
        // Two rooms off, a guard hears; three off, one doesn't. A rat and a dog nearby aren't
        // the caller's allies.
        let near = fighter(&mut world, row[2], 1, 1, &[BASIC_ATTACK]);
        let far = fighter(&mut world, row[3], 1, 1, &[BASIC_ATTACK]);
        let other_rat = other_mob(&mut world, row[1], "rat", &["vermin"]);
        let dog = other_mob(&mut world, row[1], "dog", &[]);
        world.init_resource::<Messages<GameEvent>>();
        world.write_message(GameEvent::CalledForHelp {
            who: caller,
            foe: rat,
            rooms: 2,
        });
        world.run_system_once(help::answer_calls).unwrap();
        world.flush();
        assert_eq!(world.get::<InRoom>(near).unwrap().0, row[0], "came running");
        assert_eq!(world.get::<Fighting>(near).map(|f| f.0), Some(rat));
        assert_eq!(
            world.get::<InRoom>(far).unwrap().0,
            row[3],
            "out of earshot"
        );
        for stayed in [other_rat, dog] {
            assert_eq!(world.get::<InRoom>(stayed).unwrap().0, row[1]);
            assert!(world.get::<Fighting>(stayed).is_none());
        }
    }

    #[test]
    fn whoever_starts_a_fight_strikes_before_those_they_draw_in() {
        // A quick dog against a slow fighter who kills it with the first blow: it never gets to
        // bite, however much quicker it is.
        let (mut world, room) = world();
        let slow = fighter(&mut world, room, 1000, 1, &[BASIC_ATTACK]);
        sure_hands(&mut world, slow);
        let dog = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        sure_hands(&mut world, dog);
        world.entity_mut(slow).insert(Fighting(dog));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get::<Dead>(dog).is_some());
        assert_eq!(
            world.get::<Vitals>(slow).unwrap().health,
            100.0,
            "never bitten"
        );

        // The same when `kill` has already set the dog fighting back.
        let dog = fighter(&mut world, room, 1, 1000, &[BASIC_ATTACK]);
        sure_hands(&mut world, dog);
        world.entity_mut(slow).insert(Fighting(dog));
        world.entity_mut(dog).insert((Fighting(slow), DrawnIn));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get::<Dead>(dog).is_some());
        assert_eq!(
            world.get::<Vitals>(slow).unwrap().health,
            100.0,
            "never bitten, though it was fighting back already"
        );
        assert!(world.get::<DrawnIn>(dog).is_none());
    }

    #[test]
    fn those_drawn_into_a_fight_take_their_turns_after_who_drew_them_in() {
        let e = |n: u32| Entity::from_raw_u32(n).unwrap();
        let (wolf, ada, deer, bob) = (e(1), e(2), e(3), e(4));
        // Ada attacks the wolf, the deer is set upon by Bob; Bob and the wolf are quicker than
        // those who started it.
        let drawn = HashSet::from([wolf, deer]);
        let targets = HashMap::from([(ada, wolf), (wolf, ada), (bob, deer), (deer, bob)]);
        let order = vec![(90, wolf), (80, deer), (70, ada), (60, bob)];
        let turns: Vec<Entity> = first_strike(order, &drawn, &targets)
            .into_iter()
            .map(|(_, who)| who)
            .collect();
        assert_eq!(turns, vec![ada, wolf, bob, deer]);
    }

    #[test]
    fn a_side_adds_up_its_fighters_against_its_foes() {
        let (ogre, one) = ((4.0, 4.0), (1.0, 1.0));
        assert_eq!(strike::side_odds(&[one], &[ogre]), Some(4.0));
        assert_eq!(
            strike::side_odds(&[one, one], &[ogre]),
            Some(2.0),
            "two together are half as outmatched"
        );
        assert_eq!(
            strike::side_odds(&[one], &[one, one, one, one]),
            Some(4.0),
            "four rats are as much as an ogre"
        );
    }

    #[test]
    fn a_whole_fight_is_rated_once_and_shared_out_by_kills() {
        let e = |n: u32| Entity::from_raw_u32(n).unwrap();
        let encounter = Encounter {
            members: HashMap::from([(e(1), (1.0, 1.0))]),
            allies: HashMap::new(),
            foes: HashMap::from([(e(2), (1.0, 1.0)), (e(3), (1.0, 1.0))]),
        };
        // Two rats are twice an even fight; each is worth half of what that's worth.
        let whole = victory_experience(2.0);
        assert_eq!(
            encounter.worth(e(2)),
            Some((whole as f64 / 2.0).round() as u64)
        );
        assert_eq!(encounter.worth(e(4)), None, "not in this fight");
    }

    #[test]
    fn a_fight_counts_those_waiting_their_turn_and_ends_when_the_side_stops() {
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, ada);
        // One rat at her, another held back by its pack's limit.
        let (rat, waiting) = (
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
        );
        for who in [ada, rat, waiting] {
            world.get_mut::<Vitals>(who).unwrap().health = 10_000.0;
        }
        world.entity_mut(waiting).insert(Aggressive::ANYWHERE);
        world.entity_mut(ada).insert(Fighting(rat));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let foes = |world: &World| {
            world
                .resource::<Encounters>()
                .0
                .get(&(ada, room))
                .map(|e| e.foes.len())
        };
        assert_eq!(foes(&world), Some(2), "both rats are in it");
        // She stops fighting: the fight is over.
        world.entity_mut(ada).remove::<Fighting>();
        world.entity_mut(rat).remove::<Fighting>();
        world.entity_mut(waiting).remove::<Aggressive>();
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert_eq!(foes(&world), None);
    }

    #[test]
    fn reinforcements_make_a_fight_harder_and_the_fallen_still_count() {
        let (mut world, room) = world();
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, ada);
        let (rat, more) = (
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
        );
        for who in [ada, rat, more] {
            world.get_mut::<Vitals>(who).unwrap().health = 10_000.0;
        }
        let fight = |world: &World| {
            let encounter = &world.resource::<Encounters>().0[&(ada, room)];
            (encounter.foes.len(), encounter.odds().unwrap())
        };
        world.entity_mut(ada).insert(Fighting(rat));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let (foes, alone) = fight(&world);
        assert_eq!(foes, 1);
        // Another rat comes in and joins the fight: the same fight, harder.
        world.entity_mut(more).insert(Fighting(ada));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let (foes, joined) = fight(&world);
        assert_eq!(foes, 2);
        assert!(joined > alone, "harder with two");
        // The first rat falls: the fight goes on, and is no easier for it.
        world.entity_mut(rat).insert(Dead).remove::<Fighting>();
        world.entity_mut(ada).insert(Fighting(more));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert_eq!(fight(&world), (2, joined));
    }

    #[test]
    fn a_mob_fighting_beside_you_makes_the_fight_easier() {
        let (mut world, room) = world();
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, ada);
        let (rat, guard) = (
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
        );
        for who in [ada, rat, guard] {
            world.get_mut::<Vitals>(who).unwrap().health = 10_000.0;
        }
        let odds = |world: &World| world.resource::<Encounters>().0[&(ada, room)].odds();
        world.entity_mut(ada).insert(Fighting(rat));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let alone = odds(&world).unwrap();
        // A guard takes on the same rat.
        world.entity_mut(guard).insert(Fighting(rat));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let encounter = &world.resource::<Encounters>().0[&(ada, room)];
        assert!(encounter.allies.contains_key(&guard));
        assert!(!encounter.members.contains_key(&guard), "not on her side");
        assert!(odds(&world).unwrap() < alone, "easier with the guard");
    }

    #[test]
    fn a_mob_busy_with_someone_else_isnt_waiting_for_you() {
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        as_player(&mut world, ada);
        let rat = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        // Another rat, as aggressive, brawls with a guard.
        let (brawler, guard) = (
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
            fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]),
        );
        for who in [ada, rat, brawler, guard] {
            world.get_mut::<Vitals>(who).unwrap().health = 10_000.0;
        }
        world
            .entity_mut(brawler)
            .insert((Aggressive::ANYWHERE, Fighting(guard)));
        world.entity_mut(guard).insert(Fighting(brawler));
        world.entity_mut(ada).insert(Fighting(rat));
        world.run_system_once(combat_round).unwrap();
        world.flush();
        let encounter = &world.resource::<Encounters>().0[&(ada, room)];
        assert_eq!(
            encounter.foes.keys().copied().collect::<Vec<_>>(),
            vec![rat],
            "only the rat she fights"
        );
        assert!(encounter.allies.is_empty(), "the guard fights another rat");
    }

    /// Put `members` in a group led by the first of them.
    fn group_up(world: &mut World, members: &[Entity]) {
        for (joined, member) in members.iter().enumerate() {
            world.entity_mut(*member).insert(InGroup {
                leader: members[0],
                joined: joined as u64,
            });
        }
    }

    #[test]
    fn a_group_in_the_room_shares_a_victory() {
        let (mut world, room) = world();
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let bob = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let cal = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        for player in [ada, bob, cal] {
            as_player(&mut world, player);
        }
        // Ada fights; Bob, in her group, stands by; Cal isn't in it.
        group_up(&mut world, &[ada, bob]);
        let ogre = fighter(&mut world, room, 6, 1, &[BASIC_ATTACK]);
        world.get_mut::<Vitals>(ogre).unwrap().health = 1.0;
        world.entity_mut(ada).insert(Fighting(ogre));
        for _ in 0..30 {
            world.run_system_once(combat_round).unwrap();
            world.flush();
            if world.get::<Dead>(ogre).is_some() {
                break;
            }
        }
        assert!(world.get::<Dead>(ogre).is_some(), "the ogre falls");
        let earned = |who: Entity| world.get::<Attributes>(who).unwrap().experience();
        assert!(earned(ada) > 0, "a real risk");
        assert_eq!(earned(bob), earned(ada), "the group shares it in full");
        assert_eq!(earned(cal), 0, "a bystander earns nothing");
    }

    #[test]
    fn a_group_splits_the_coins_its_foe_carried() {
        let (mut world, room) = world();
        world.init_resource::<RoomIndex>();
        world.init_resource::<CommandRegistry>();
        world.insert_resource(MobTemplates(HashMap::from([(
            MobKey::parse("test:mob", ""),
            crate::world::MobDef::named("a bandit"),
        )])));
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let bob = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        for player in [ada, bob] {
            as_player(&mut world, player);
            world.entity_mut(player).insert(Purse(0));
        }
        group_up(&mut world, &[ada, bob]);
        let bandit = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let traits = Conditions::default().with_traits(&[FORMLESS.to_string()]);
        world
            .entity_mut(bandit)
            .insert((traits, Dead, Slayer(bob), Purse(11)));
        world.run_system_once(handle_deaths).unwrap();
        world.flush();
        assert_eq!(world.get::<Purse>(ada).unwrap().0, 5);
        assert_eq!(
            world.get::<Purse>(bob).unwrap().0,
            6,
            "the odd copper to the slayer"
        );
    }

    #[test]
    fn group_members_join_each_others_fights() {
        let (mut world, room) = world();
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let bob = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let cal = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        for player in [ada, bob, cal] {
            as_player(&mut world, player);
        }
        group_up(&mut world, &[ada, bob, cal]);
        world.entity_mut(cal).insert(NoAssist);
        let wolf = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        world.entity_mut(ada).insert(Fighting(wolf));
        world.entity_mut(wolf).insert(Fighting(ada));
        world.run_system_once(assist).unwrap();
        world.flush();
        assert_eq!(world.get::<Fighting>(bob).map(|f| f.0), Some(wolf));
        assert!(world.get::<Fighting>(cal).is_none(), "autoassist is off");
        assert_eq!(
            world.get::<Fighting>(wolf).map(|f| f.0),
            Some(ada),
            "the wolf keeps its fight"
        );
    }

    #[test]
    fn a_formless_mob_leaves_its_loot_where_it_died_and_no_corpse() {
        let (mut world, room) = world();
        world.init_resource::<RoomIndex>();
        world.init_resource::<CommandRegistry>();
        let jelly = ItemKey::parse("test:jelly", "");
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(jelly.clone(), ItemDef::named("a gob of jelly"));
        let mut def = crate::world::MobDef::named("an ooze");
        def.loot = vec![vec![(Some("jelly".into()), 100)]];
        world.insert_resource(MobTemplates(HashMap::from([(
            MobKey::parse("test:mob", ""),
            def,
        )])));
        let ooze = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let traits = Conditions::default().with_traits(&[FORMLESS.to_string()]);
        world.entity_mut(ooze).insert((traits, Dead));
        world.run_system_once(handle_deaths).unwrap();
        world.flush();
        assert!(world.get_entity(ooze).is_err(), "gone");
        let left: Vec<(ItemKey, ItemLocation)> = world
            .query::<(&Item, &ItemLocation)>()
            .iter(&world)
            .map(|(item, at)| (item.key.clone(), *at))
            .collect();
        assert_eq!(
            left,
            vec![(jelly, ItemLocation::Room(room))],
            "no corpse, jelly on the floor"
        );
    }

    #[test]
    fn what_a_group_needs_from_a_boss_drops_once_whatever_the_roll() {
        let (mut world, room) = world();
        world.init_resource::<RoomIndex>();
        world.init_resource::<CommandRegistry>();
        let tooth = ItemKey::parse("test:tooth", "");
        world
            .resource_mut::<ItemTemplates>()
            .0
            .insert(tooth.clone(), ItemDef::named("a tooth"));
        // A rat that leaves its loot on the floor, and drops a tooth `chance` in 100; the Rat
        // King, if it's a `boss`.
        let rats = |world: &mut World, chance: u32, has_tooth: bool, boss: bool| {
            let mut def = crate::world::MobDef::named("a rat");
            def.boss = boss;
            if has_tooth {
                def.loot = vec![vec![(None, 100 - chance), (Some("tooth".into()), chance)]];
            }
            world.insert_resource(MobTemplates(HashMap::from([(
                MobKey::parse("test:mob", ""),
                def,
            )])));
        };
        let teeth = |world: &mut World| {
            world
                .query::<&Item>()
                .iter(world)
                .filter(|item| item.key == tooth)
                .count()
        };
        // Ada, Bob and Cal, in a group, each on a quest for a tooth.
        let ada = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let bob = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        let cal = fighter(&mut world, room, 1, 1, &[BASIC_ATTACK]);
        for player in [ada, bob, cal] {
            as_player(&mut world, player);
        }
        group_up(&mut world, &[ada, bob, cal]);
        let kill = |world: &mut World| {
            let rat = fighter(world, room, 1, 1, &[BASIC_ATTACK]);
            let traits = Conditions::default().with_traits(&[FORMLESS.to_string()]);
            world.entity_mut(rat).insert((traits, Dead, Slayer(bob)));
            world.run_system_once(handle_deaths).unwrap();
            world.flush();
            let n = teeth(world);
            // Clear the floor for the next.
            let found: Vec<Entity> = world
                .query::<(Entity, &Item)>()
                .iter(world)
                .filter(|(_, item)| item.key == tooth)
                .map(|(e, _)| e)
                .collect();
            for e in found {
                world.despawn(e);
            }
            n
        };

        rats(&mut world, 0, true, true);
        assert_eq!(kill(&mut world), 0, "left to chance, it never drops");
        world.resource_mut::<WantedLoot>().0 = HashMap::from([
            (ada, vec![tooth.clone()]),
            (bob, vec![tooth.clone(), tooth.clone()]),
            (cal, vec![tooth.clone()]),
        ]);
        assert_eq!(
            kill(&mut world),
            1,
            "three want it from a boss: one drops, not three"
        );
        rats(&mut world, 100, true, true);
        assert_eq!(kill(&mut world), 1, "it dropped anyway: no second one");
        rats(&mut world, 0, false, true);
        assert_eq!(kill(&mut world), 0, "a boss that can't drop one doesn't");
        rats(&mut world, 0, true, false);
        assert_eq!(
            kill(&mut world),
            0,
            "an ordinary rat's drops are left to chance, wanted or not"
        );
    }

    #[test]
    fn an_aura_strengthens_mobs_of_its_tag_but_not_its_holder() {
        let (mut world, room) = world();
        aggressive_template(&mut world, None);
        let aura = Bonus {
            attack: 2,
            ..Bonus::default()
        };
        let (key, will) = skill(
            "test:will",
            SkillKind::Aura {
                affects: "slime".into(),
                bonus: aura,
            },
        );
        world
            .resource_mut::<SkillDefs>()
            .skills
            .insert(key.clone(), will);
        let lord = other_mob(&mut world, room, "lord", &["slime"]);
        world
            .get_mut::<Skills>(lord)
            .unwrap()
            .0
            .insert(key, SkillState::new("Template"));
        let ooze = other_mob(&mut world, room, "ooze", &["slime"]);
        let rat = other_mob(&mut world, room, "rat", &["vermin"]);
        let auras = world
            .run_system_once(
                |fighters: Fighters,
                 rooms: RoomQuery,
                 defs: Res<SkillDefs>,
                 mobs: Res<MobTemplates>| {
                    auras(&fighters, &rooms, &defs, &mobs)
                },
            )
            .unwrap();
        assert_eq!(auras.get(&ooze), Some(&aura));
        assert_eq!(auras.get(&lord), None, "not its holder");
        assert_eq!(auras.get(&rat), None, "not another kind");
    }

    #[test]
    fn a_training_dummy_takes_blows_and_never_fights_back() {
        let (mut world, room) = world();
        let trainee = fighter(&mut world, room, 5, 1, &[BASIC_ATTACK]);
        as_player(&mut world, trainee);
        sure_hands(&mut world, trainee);
        // Strong enough that beating it would be worth something, if it were alive.
        let dummy = fighter(&mut world, room, 200, 1, &[BASIC_ATTACK]);
        world.entity_mut(dummy).insert(Inanimate);
        world.entity_mut(trainee).insert(Fighting(dummy));
        for _ in 0..3 {
            world.run_system_once(combat_round).unwrap();
            world.flush();
        }
        assert!(
            world.get::<Vitals>(dummy).unwrap().health < 100.0,
            "it takes blows"
        );
        assert_eq!(
            world.get::<Vitals>(trainee).unwrap().health,
            100.0,
            "never one back"
        );
        assert!(world.get::<Fighting>(dummy).is_none());
        let skills = world.get::<Skills>(trainee).unwrap();
        assert!(
            skills.0[BASIC_ATTACK].practice > 0.0,
            "the trainee practises"
        );
        // Worth no experience when it's broken.
        world.get_mut::<Vitals>(dummy).unwrap().health = 1.0;
        let before = world.get::<Attributes>(trainee).unwrap().experience();
        world.run_system_once(combat_round).unwrap();
        world.flush();
        assert!(world.get::<Dead>(dummy).is_some());
        assert_eq!(
            world.get::<Attributes>(trainee).unwrap().experience(),
            before
        );
    }

    #[test]
    fn hacking_at_a_dummy_is_no_danger_but_any_real_fight_is() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        let (ada, bob) = (world.spawn_empty().id(), world.spawn_empty().id());
        let dummy = world.spawn(Inanimate).id();
        let rat = world.spawn_empty().id();
        let danger = |world: &mut World, who: Entity| {
            world
                .run_system_once(
                    move |fights: Query<(Entity, &Fighting), Without<Dead>>,
                          inanimate: Query<(), With<Inanimate>>| {
                        in_danger(who, &fights, &inanimate)
                    },
                )
                .unwrap()
        };
        assert!(!danger(&mut world, ada), "not fighting");
        world.entity_mut(ada).insert(Fighting(dummy));
        assert!(!danger(&mut world, ada), "a dummy can't hit back");
        world.entity_mut(rat).insert(Fighting(ada));
        assert!(danger(&mut world, ada), "set upon while at the dummy");
        world.entity_mut(bob).insert(Fighting(rat));
        assert!(danger(&mut world, bob), "fighting something alive");
        world.entity_mut(rat).insert(Dead);
        assert!(!danger(&mut world, ada), "a dead attacker is no danger");
    }

    #[test]
    fn prepared_moves_expire_and_tell_the_player() {
        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Outbox::default());
        world.insert_resource(SkillDefs::default());
        world.insert_resource(SpellDefs::default());

        let target = world.spawn_empty().id();
        let opener = world
            .spawn(Readied {
                skill: "brawling:backstab".into(),
                ready_at: -6.0,
            })
            .id();
        let caster = world
            .spawn(Casting {
                spell: "arcane:spark".into(),
                target,
                ready_at: -6.0,
            })
            .id();
        let engaged = world
            .spawn((
                Fighting(target),
                Readied {
                    skill: "brawling:jab".into(),
                    ready_at: -6.0,
                },
            ))
            .id();

        world.run_system_once(expire_readied).unwrap();

        assert!(world.get::<Readied>(opener).is_none());
        assert!(world.get::<Casting>(caster).is_none());
        assert!(world.get::<Readied>(engaged).is_some());
        assert!(said(&world, opener, "You let your brawling:backstab fade."));
        assert!(said(&world, caster, "You let your arcane:spark fade."));
    }

    #[test]
    fn eating_heals_and_applies_conditions_on_the_eater() {
        let (mut world, room) = world();
        let eater = fighter(&mut world, room, 1, 1, &[]);
        world.get_mut::<Vitals>(eater).unwrap().health = 50.0;
        let effects = vec![
            Effect::Heal {
                amount: Formula::number(10.0),
            },
            Effect::Apply {
                condition: "nourished".into(),
                strength: Formula::number(2.0),
                lasts: Lasting::Minutes(40),
                on: Who::User,
            },
        ];
        let healed = world
            .run_system_once(move |mut rounds: Rounds| {
                let mut healed = 0;
                rounds.now(|round| {
                    round.take_in(eater, &effects, |n| {
                        healed = n;
                        String::new()
                    })
                });
                healed
            })
            .unwrap();
        world.flush();
        assert_eq!(healed, 10);
        assert_eq!(world.get::<Vitals>(eater).unwrap().health, 60.0);
        assert!(
            world
                .get::<Conditions>(eater)
                .is_some_and(|c| c.has("nourished"))
        );
    }
}
