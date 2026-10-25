//! Skills: what a character can do, learned and improved only by use.
//!
//! Skill trees are data, one file per tree in `assets/skills/` (`thievery.skill.ron`; the double
//! extension keeps Bevy from mistaking them for zone files), hot-reloaded like zones. A skill is
//! active (used with a command, costing stamina, with a cooldown) or passive (practised whenever
//! a game event it listens for happens). Every use builds practice and may roll for a level; see
//! `docs/progression.md` for the formulas. Each five skill levels a character has in total is a
//! character level, and each character level earns a point of Karma.
//!
//! An active skill names the command that performs it, like `pick` for Lockpicking: that command
//! does the work and calls [`Practice`] to check, charge for and count the use, and `use <skill>`
//! just runs it.

use std::collections::{BTreeMap, HashMap};

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::attributes::{Attribute, Attributes};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::conditions::Condition;
use crate::db::{Database, Permission};
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::output::{Actor, Outgoing, PlayerQuery, Reply, deliver, reply_line, targets_online};
use crate::targeting::{Keywords, Target};
use crate::vitals::Vitals;
use crate::world::ron_options;

/// Asset folder holding one `.skill.ron` file per skill tree.
pub const SKILLS_PATH: &str = "skills";
/// The extension skill tree files use.
const SKILL_EXTENSION: &str = "skill.ron";

/// How long practice takes to halve while a skill goes unused, in seconds.
const PRACTICE_HALF_LIFE: f64 = 30.0 * 60.0;
/// The gain chance of an untouched skill at level 0, before practice and difficulty.
const BASE_GAIN_CHANCE: f64 = 0.25;
/// The cooldown after a gain at level 0, in seconds; each level multiplies it by `COOLDOWN_GROWTH`.
const BASE_COOLDOWN: f64 = 5.0 * 60.0;
const COOLDOWN_GROWTH: f64 = 1.1;
/// Skill levels per character level.
pub const LEVELS_PER_CHARACTER_LEVEL: u32 = 5;
/// Skills every character starts with, whatever their race and background.
pub const UNIVERSAL_SKILLS: &[&str] = &["athletics:endurance"];

pub struct SkillsPlugin;

impl Plugin for SkillsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<SkillTreeDef>()
            .register_asset_loader(SkillTreeLoader)
            .init_resource::<SkillDefs>()
            .add_systems(Startup, load_skill_trees)
            .add_systems(Update, (sync_skill_defs, practise_passives))
            .add_command(
                CommandSpec::new("skills", Category::Character)
                    .summary("list your skills, their levels, and how warmed up they are"),
                skills_cmd,
            )
            .add_command(
                CommandSpec::new("use", Category::Interaction)
                    .usage("use <skill> [<target>]")
                    .summary("use an active skill, like 'use lockpicking north'"),
                use_cmd,
            )
            .add_command(
                CommandSpec::new("grantskill", Category::Admin)
                    .usage("grantskill <player> <tree:skill> [<level>]")
                    .summary("teach an online player a skill, or set its level, for testing")
                    .permission(Permission::ManageRoles),
                grantskill_cmd,
            );
    }
}

/// A skill tree file.
#[derive(Asset, TypePath, Serialize, Deserialize, Debug, Clone)]
pub struct SkillTreeDef {
    /// Taken from the file name, e.g. `thievery` for `skills/thievery.skill.ron`.
    #[serde(skip)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub rarity: Rarity,
    /// The attribute its skills add to their level, unless a skill names another.
    pub attribute: Attribute,
    pub skills: BTreeMap<String, SkillDef>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rarity {
    #[default]
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SkillDef {
    pub name: String,
    pub kind: SkillKind,
    /// The level at which it's mastered.
    #[serde(default = "default_max_level")]
    pub max_level: u32,
    /// Overrides the tree's attribute.
    #[serde(default)]
    pub attribute: Option<Attribute>,
    #[serde(default)]
    pub rarity: Option<Rarity>,
    /// Multiplies the gain chance: below 1 is slower to learn.
    #[serde(default = "default_difficulty")]
    pub difficulty: f64,
    /// Who may learn it. Checked by teaching later; starting skills are granted regardless.
    #[serde(default)]
    pub requires: Option<Condition>,
}

fn default_max_level() -> u32 {
    50
}

fn default_difficulty() -> f64 {
    1.0
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SkillKind {
    /// Used on purpose, through `command`.
    Active {
        /// The command that performs it: `use lockpicking north` runs `pick north`.
        command: String,
        #[serde(default)]
        cost: Cost,
        /// Seconds before it can be used again.
        #[serde(default)]
        cooldown: f64,
    },
    /// Practised whenever one of its triggers happens.
    Passive { triggers: Vec<Trigger> },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub enum Cost {
    #[default]
    Free,
    Stamina(u32),
}

/// A game event that practises a passive skill.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum Trigger {
    /// One use for every `per` stamina spent.
    StaminaSpent { per: u32 },
}

/// A skill with what it inherits from its tree.
#[derive(Clone, Debug)]
pub struct Skill {
    /// `tree:skill`.
    pub key: String,
    pub tree: String,
    pub def: SkillDef,
    pub attribute: Attribute,
}

/// Every skill from every loaded tree, by `tree:skill`, and the trees' names.
#[derive(Resource, Default)]
pub struct SkillDefs {
    pub skills: HashMap<String, Skill>,
    pub trees: HashMap<String, String>,
}

impl SkillDefs {
    /// The skill `name` refers to among `known`: by `tree:skill`, by its id within its tree, or by
    /// the start of a word of its name. `lock` finds Lockpicking.
    pub fn find_known<'a>(&'a self, known: &Skills, name: &str) -> Option<&'a Skill> {
        let name = name.to_lowercase();
        let mut candidates = known.0.keys().filter_map(|key| self.skills.get(key));
        let exact = |skill: &&Skill| {
            skill.key == name || skill.key.split_once(':').is_some_and(|(_, id)| id == name)
        };
        let all: Vec<&Skill> = candidates.by_ref().collect();
        all.iter().copied().find(exact).or_else(|| {
            all.into_iter().find(|skill| {
                skill
                    .def
                    .name
                    .to_lowercase()
                    .split_whitespace()
                    .any(|word| word.starts_with(&name))
                    || skill
                        .key
                        .split_once(':')
                        .is_some_and(|(_, id)| id.starts_with(&name))
            })
        })
    }
}

/// A character's progress in one skill.
#[derive(Clone, Debug, PartialEq)]
pub struct SkillState {
    pub level: u32,
    /// How warmed up it is, 0 to 1. Fades while unused.
    pub practice: f64,
    /// Unix seconds.
    pub last_used: Option<i64>,
    /// When it can next gain a level, in unix seconds.
    pub next_gain_at: i64,
    /// How it was learned: `Starting`, `Taught`, `Loot`, `Epiphany` or `Admin`.
    pub learned_via: String,
    /// When an active skill can next be used, in seconds since the server started. Not saved.
    pub ready_at: f64,
    /// Trigger amounts towards a passive skill's next use, like stamina spent. Not saved.
    pub pending: f64,
}

impl SkillState {
    pub fn new(learned_via: &str) -> Self {
        Self {
            level: 0,
            practice: 0.0,
            last_used: None,
            next_gain_at: 0,
            learned_via: learned_via.into(),
            ready_at: 0.0,
            pending: 0.0,
        }
    }
}

/// A character's skills, by `tree:skill`. Lives on the player entity.
#[derive(Component, Clone, Debug, Default)]
pub struct Skills(pub BTreeMap<String, SkillState>);

impl Skills {
    /// Total skill levels / 5: each character level earns a point of Karma.
    pub fn character_level(&self) -> u32 {
        self.0.values().map(|s| s.level).sum::<u32>() / LEVELS_PER_CHARACTER_LEVEL
    }
}

/// Practice after `elapsed` seconds unused.
pub fn faded(practice: f64, elapsed: i64) -> f64 {
    practice * 0.5f64.powf(elapsed.max(0) as f64 / PRACTICE_HALF_LIFE)
}

/// The chance one use gains a level.
pub fn gain_chance(level: u32, max_level: u32, difficulty: f64, practice: f64) -> f64 {
    if level >= max_level {
        return 0.0;
    }
    let room = 1.0 - f64::from(level) / f64::from(max_level.max(1));
    difficulty * BASE_GAIN_CHANCE * room * (0.2 + 0.8 * practice)
}

/// Seconds before a skill can gain again, after gaining a level from `level`.
pub fn gain_cooldown(level: u32) -> i64 {
    (BASE_COOLDOWN * COOLDOWN_GROWTH.powi(level as i32)) as i64
}

/// One use of a skill at unix time `now`: practice fades and builds, and if the skill is off its
/// gain cooldown, `roll` is asked whether the chance comes up. Returns whether it gained a level.
pub fn roll_skill_gain(
    state: &mut SkillState,
    max_level: u32,
    difficulty: f64,
    now: i64,
    roll: impl FnOnce(f64) -> bool,
) -> bool {
    let practice = faded(state.practice, state.last_used.map_or(0, |at| now - at));
    let mut gained = false;
    if state.level < max_level && now >= state.next_gain_at {
        let chance = gain_chance(state.level, max_level, difficulty, practice);
        if roll(chance) {
            state.next_gain_at = now + gain_cooldown(state.level);
            state.level += 1;
            gained = true;
        }
    }
    state.practice = practice + 0.1 * (1.0 - practice);
    state.last_used = Some(now);
    gained
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// What commands and systems use to check, charge for and count uses of skills.
#[derive(SystemParam)]
pub struct Practice<'w, 's> {
    pub defs: Res<'w, SkillDefs>,
    sheets: Query<
        'w,
        's,
        (
            &'static mut Skills,
            &'static mut Vitals,
            &'static mut Attributes,
        ),
    >,
    dice: ResMut<'w, Dice>,
    db: Res<'w, Database>,
    time: Res<'w, Time>,
    events: MessageWriter<'w, GameEvent>,
}

/// A character ready to use an active skill.
pub struct Ready {
    /// Level plus the skill's attribute: what the skill pits against a difficulty.
    pub effectiveness: i32,
    pub luck: i32,
}

impl Practice<'_, '_> {
    /// Check that `who` can use the active skill `key` now (they know it, it's off cooldown, they
    /// can pay), and pay for it.
    pub fn begin(&mut self, who: Entity, key: &str) -> Result<Ready, String> {
        let skill = self
            .defs
            .skills
            .get(key)
            .ok_or_else(|| "That skill isn't part of this world any more.".to_string())?;
        let SkillKind::Active { cost, cooldown, .. } = skill.def.kind else {
            return Err(format!(
                "{} isn't something you use; it just works.",
                skill.def.name
            ));
        };
        let (mut skills, mut vitals, attributes) =
            self.sheets.get_mut(who).map_err(|_| String::new())?;
        let elapsed = self.time.elapsed_secs_f64();
        let state = skills
            .0
            .get_mut(key)
            .ok_or_else(|| format!("You don't know how to do that ({}).", skill.def.name))?;
        if state.ready_at > elapsed {
            return Err(format!(
                "You need a moment before you can try that again ({:.0}s).",
                (state.ready_at - elapsed).ceil()
            ));
        }
        if let Cost::Stamina(n) = cost {
            if vitals.stamina < n as f32 {
                return Err("You're too tired. Rest a little first.".into());
            }
            vitals.stamina -= n as f32;
            self.events
                .write(GameEvent::StaminaSpent { who, amount: n });
        }
        state.ready_at = elapsed + cooldown;
        Ok(Ready {
            effectiveness: state.level as i32 + attributes.get(skill.attribute),
            luck: attributes.get(Attribute::Luck),
        })
    }

    /// Roll a yes-or-no with `who`'s Luck.
    pub fn chance(&mut self, luck: i32, p: f64) -> bool {
        self.dice.chance(luck, p)
    }

    /// Count one use of `key` by `who` (character `character_id`): practice, and maybe a level.
    /// Returns what to tell them.
    pub fn exercise(&mut self, who: Entity, character_id: i64, key: &str) -> Vec<String> {
        let Ok((mut skills, _, mut attributes)) = self.sheets.get_mut(who) else {
            return Vec::new();
        };
        count_use(
            &self.defs,
            &mut skills,
            &mut attributes,
            &mut self.dice,
            &self.db,
            character_id,
            key,
        )
    }
}

/// One use of a skill a character knows: practice, maybe a level, and maybe a character level
/// and its Karma. Saves the skill, and returns what to tell them.
fn count_use(
    defs: &SkillDefs,
    skills: &mut Skills,
    attributes: &mut Attributes,
    dice: &mut Dice,
    db: &Database,
    character_id: i64,
    key: &str,
) -> Vec<String> {
    let Some(skill) = defs.skills.get(key) else {
        return Vec::new();
    };
    let level_before = skills.character_level();
    let luck = attributes.get(Attribute::Luck);
    let Some(state) = skills.0.get_mut(key) else {
        return Vec::new();
    };
    let gained = roll_skill_gain(
        state,
        skill.def.max_level,
        skill.def.difficulty,
        unix_now(),
        |chance| dice.chance(luck, chance),
    );
    if let Err(err) = db.save_skill(character_id, key, state) {
        error!("failed to save {key} for character {character_id}: {err}");
    }
    let mut lines = Vec::new();
    if gained {
        let level = state.level;
        let mastered = if level >= skill.def.max_level {
            " You have mastered it."
        } else {
            ""
        };
        lines.push(format!(
            "\x1b[1;32mYour {} improves to {level}.{mastered}\x1b[0m",
            skill.def.name
        ));
        let level_after = skills.character_level();
        if level_after > level_before {
            attributes.set_earned(level_after as i32);
            lines.push(format!(
                "\x1b[1;33mYou feel more experienced. You have earned a point of Karma \
                 ({} to spend).\x1b[0m",
                attributes.karma_unspent()
            ));
        }
    }
    lines
}

/// Learn a skill at level 0, if it isn't known already. Returns whether it was new.
pub fn learn(skills: &mut Skills, key: &str, via: &str, character_id: i64, db: &Database) -> bool {
    if skills.0.contains_key(key) {
        return false;
    }
    let state = SkillState::new(via);
    if let Err(err) = db.save_skill(character_id, key, &state) {
        error!("failed to save new skill {key} for character {character_id}: {err}");
    }
    skills.0.insert(key.to_string(), state);
    true
}

/// Practise passive skills when their triggers happen.
#[allow(clippy::too_many_arguments)]
fn practise_passives(
    mut events: MessageReader<GameEvent>,
    defs: Res<SkillDefs>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mut players: PlayerQuery,
) {
    let spent: Vec<(Entity, u32)> = events
        .read()
        .filter_map(|event| match event {
            GameEvent::StaminaSpent { who, amount } => Some((*who, *amount)),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for (who, amount) in spent {
        let (Some(actor), Ok((mut skills, mut attributes))) =
            (Actor::get(&players, who), sheets.get_mut(who))
        else {
            continue;
        };
        let passives: Vec<(String, u32)> = skills
            .0
            .keys()
            .filter_map(|key| match &defs.skills.get(key)?.def.kind {
                // Stamina spent is the only trigger so far.
                SkillKind::Passive { triggers } => triggers
                    .iter()
                    .map(|Trigger::StaminaSpent { per }| (key.clone(), (*per).max(1)))
                    .next(),
                SkillKind::Active { .. } => None,
            })
            .collect();
        for (key, per) in passives {
            let Some(state) = skills.0.get_mut(&key) else {
                continue;
            };
            state.pending += f64::from(amount);
            let uses = (state.pending / f64::from(per)).floor();
            state.pending -= uses * f64::from(per);
            for _ in 0..uses as u32 {
                let lines = count_use(
                    &defs,
                    &mut skills,
                    &mut attributes,
                    &mut dice,
                    &db,
                    actor.character_id,
                    &key,
                );
                out.extend(lines.into_iter().map(|line| Outgoing::line(who, line)));
            }
        }
    }
    deliver(out, &mut players);
}

/// `skills`: what you know, by tree.
fn skills_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    sheets: Query<(&Skills, &Attributes)>,
    defs: Res<SkillDefs>,
    time: Res<Time>,
) {
    let Ok((skills, attributes)) = sheets.get(inv.entity) else {
        return;
    };
    let text = skills_text(
        skills,
        attributes,
        &defs,
        unix_now(),
        time.elapsed_secs_f64(),
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// How warmed up a skill feels.
fn warmth(practice: f64) -> &'static str {
    match practice {
        p if p >= 0.75 => "hot",
        p if p >= 0.4 => "warm",
        p if p >= 0.1 => "warming",
        _ => "cold",
    }
}

/// "4m", "2h 5m".
fn duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

fn skills_text(
    skills: &Skills,
    attributes: &Attributes,
    defs: &SkillDefs,
    now: i64,
    _elapsed: f64,
) -> String {
    if skills.0.is_empty() {
        return "You don't know any skills yet.".into();
    }
    let mut by_tree: BTreeMap<String, Vec<(&Skill, &SkillState)>> = BTreeMap::new();
    let mut unknown = Vec::new();
    for (key, state) in &skills.0 {
        match defs.skills.get(key) {
            Some(skill) => {
                let tree = defs.trees.get(&skill.tree).cloned().unwrap_or_default();
                by_tree.entry(tree).or_default().push((skill, state));
            }
            None => unknown.push(key.as_str()),
        }
    }
    let total: u32 = skills.0.values().map(|s| s.level).sum();
    let mut text = format!(
        "Your skills (character level {}, {total} skill levels):",
        skills.character_level()
    );
    for (tree, list) in by_tree {
        text += &format!("\r\n  \x1b[1m{tree}\x1b[0m");
        for (skill, state) in list {
            let practice = faded(state.practice, state.last_used.map_or(0, |at| now - at));
            let resting = if state.next_gain_at > now && state.level < skill.def.max_level {
                format!(
                    ", can improve again in {}",
                    duration(state.next_gain_at - now)
                )
            } else {
                String::new()
            };
            let kind = match skill.def.kind {
                SkillKind::Active { .. } => "",
                SkillKind::Passive { .. } => " (passive)",
            };
            text += &format!(
                "\r\n    {:<22}{:>3}/{:<3} {} +{} {}{resting}{kind}",
                skill.def.name,
                state.level,
                skill.def.max_level,
                warmth(practice),
                attributes.get(skill.attribute),
                skill.attribute.name(),
            );
        }
    }
    if !unknown.is_empty() {
        text += &format!(
            "\r\n  \x1b[2m(forgotten by this world for now: {})\x1b[0m",
            unknown.join(", ")
        );
    }
    text
}

/// `use <skill> [<target>]`: run an active skill's command.
fn use_cmd(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    sheets: Query<&Skills>,
    defs: Res<SkillDefs>,
    registry: Res<CommandRegistry>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let (name, target) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let reply: Reply = match sheets.get(inv.entity) {
        _ if name.is_empty() => Err(inv.usage_error()),
        Ok(skills) => match defs.find_known(skills, name) {
            None => Err(format!("You don't know a skill called '{name}'.")),
            Some(skill) => match &skill.def.kind {
                SkillKind::Passive { .. } => Err(format!(
                    "{} isn't something you use; it improves as you go.",
                    skill.def.name
                )),
                SkillKind::Active { command, .. } => match registry.find(command, actor.role) {
                    Some((spec, id)) => {
                        commands.run_system_with(
                            *id,
                            Invocation {
                                entity: inv.entity,
                                command: spec.name,
                                usage: spec.usage,
                                args: target.trim().into(),
                            },
                        );
                        return;
                    }
                    None => Err(format!("You can't put {} to use yet.", skill.def.name)),
                },
            },
        },
        Err(_) => return,
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

/// `grantskill <player> <tree:skill> [<level>]`: an admin teaches a skill or sets its level.
fn grantskill_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    keywords: Query<&Keywords>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    defs: Res<SkillDefs>,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    let reply = grant_skill(&inv, &players, &keywords, &mut sheets, &defs, &db, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut players);
}

fn grant_skill(
    inv: &Invocation,
    players: &PlayerQuery,
    keywords: &Query<&Keywords>,
    sheets: &mut Query<(&mut Skills, &mut Attributes)>,
    defs: &SkillDefs,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let mut parts = inv.args.split_whitespace();
    let (Some(target), Some(key), level, None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(inv.usage_error());
    };
    let key = key.to_lowercase();
    let skill = defs
        .skills
        .get(&key)
        .ok_or_else(|| format!("There's no skill [{key}]. Skills are named tree:skill."))?;
    let level = match level {
        None => None,
        Some(n) => Some(
            n.parse::<u32>()
                .ok()
                .filter(|n| *n <= skill.def.max_level)
                .ok_or_else(|| format!("Levels go from 0 to {}.", skill.def.max_level))?,
        ),
    };
    let target = Target::parse(target).ok_or_else(|| inv.usage_error())?;
    let actor = target
        .find_one(inv.entity, targets_online(players, keywords))
        .and_then(|e| Actor::get(players, e))
        .ok_or_else(|| format!("No one matching {} is online.", target.describe()))?;
    let (mut skills, mut attributes) = sheets
        .get_mut(actor.entity)
        .map_err(|_| format!("{} has no skills loaded.", actor.name))?;
    let new = learn(&mut skills, &key, "Admin", actor.character_id, db);
    if let Some(level) = level
        && let Some(state) = skills.0.get_mut(&key)
    {
        state.level = level;
        if let Err(err) = db.save_skill(actor.character_id, &key, state) {
            error!("failed to save {key} for {}: {err}", actor.name);
        }
    }
    attributes.set_earned(skills.character_level() as i32);
    let level = skills.0.get(&key).map_or(0, |s| s.level);
    if actor.entity != inv.entity && new {
        out.push(Outgoing::line(
            actor.entity,
            format!("\x1b[1;32mYou have learned {}.\x1b[0m", skill.def.name),
        ));
    }
    Ok(format!(
        "{} {} {} at level {level}.",
        actor.name,
        if new { "now knows" } else { "has" },
        skill.def.name
    ))
}

// Loading skill trees, like zones.

#[derive(Resource)]
struct SkillTreesHandle(#[allow(dead_code)] Handle<LoadedFolder>);

fn load_skill_trees(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(SkillTreesHandle(assets.load_folder(SKILLS_PATH)));
}

/// Rebuild the skill table whenever a tree file loads, changes or goes away.
fn sync_skill_defs(
    mut events: MessageReader<AssetEvent<SkillTreeDef>>,
    trees: Res<Assets<SkillTreeDef>>,
    mut defs: ResMut<SkillDefs>,
) {
    if events.read().count() == 0 {
        return;
    }
    defs.skills.clear();
    defs.trees.clear();
    for (_, tree) in trees.iter() {
        defs.trees.insert(tree.id.clone(), tree.name.clone());
        for (id, def) in &tree.skills {
            let key = format!("{}:{id}", tree.id);
            defs.skills.insert(
                key.clone(),
                Skill {
                    key,
                    tree: tree.id.clone(),
                    attribute: def.attribute.unwrap_or(tree.attribute),
                    def: def.clone(),
                },
            );
        }
    }
    info!(
        "loaded {} skills in {} trees",
        defs.skills.len(),
        defs.trees.len()
    );
}

#[derive(TypePath)]
struct SkillTreeLoader;

#[derive(Debug, Error)]
enum SkillTreeLoaderError {
    #[error("could not read skill tree: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse skill tree: {0}")]
    Ron(#[from] ron::error::SpannedError),
}

impl AssetLoader for SkillTreeLoader {
    type Asset = SkillTreeDef;
    type Settings = ();
    type Error = SkillTreeLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<SkillTreeDef, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut tree: SkillTreeDef = ron_options().from_bytes(&bytes)?;
        tree.id = tree_id(load_context.path().path());
        Ok(tree)
    }

    fn extensions(&self) -> &[&str] {
        &[SKILL_EXTENSION]
    }
}

/// A tree's id from its file: `skills/thievery.skill.ron` is `thievery`.
fn tree_id(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(&format!(".{SKILL_EXTENSION}"))
        .unwrap_or(&name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.005
    }

    #[test]
    fn gain_chances_match_the_design_table() {
        // A skill at level 10 of 50: 20% base, 4% cold, 18% after twenty uses in a row.
        assert!(close(gain_chance(10, 50, 1.0, 1.0), 0.20));
        assert!(close(gain_chance(10, 50, 1.0, 0.0), 0.04));
        let mut practice = 0.0;
        for _ in 0..20 {
            practice += 0.1 * (1.0 - practice);
        }
        assert!(close(practice, 0.88), "{practice}");
        assert!(close(gain_chance(10, 50, 1.0, practice), 0.18));
        // Mastered skills don't gain.
        assert_eq!(gain_chance(50, 50, 1.0, 1.0), 0.0);
    }

    #[test]
    fn practice_halves_every_half_hour() {
        assert!(close(faded(0.88, 1800), 0.44));
        assert!(close(faded(0.88, 7200), 0.055));
        assert!(close(faded(0.5, 0), 0.5));
    }

    #[test]
    fn cooldowns_grow_with_level() {
        assert_eq!(gain_cooldown(0), 300);
        let hours = gain_cooldown(49) as f64 / 3600.0;
        assert!((8.5..9.5).contains(&hours), "{hours}");
    }

    #[test]
    fn a_gain_starts_the_cooldown_and_uses_still_build_practice() {
        let mut state = SkillState::new("Starting");
        assert!(roll_skill_gain(&mut state, 50, 1.0, 1000, |_| true));
        assert_eq!(state.level, 1);
        assert_eq!(state.next_gain_at, 1300);
        assert!(close(state.practice, 0.1));
        // On cooldown: no roll, but practice still builds.
        let rolled = std::cell::Cell::new(false);
        assert!(!roll_skill_gain(&mut state, 50, 1.0, 1010, |_| {
            rolled.set(true);
            true
        }));
        assert!(!rolled.get());
        assert_eq!(state.level, 1);
        assert!(state.practice > 0.18);
        // After the cooldown it can gain again.
        assert!(roll_skill_gain(&mut state, 50, 1.0, 1300, |_| true));
        assert_eq!(state.level, 2);
    }

    #[test]
    fn character_level_is_total_levels_over_five() {
        let mut skills = Skills::default();
        for (key, level) in [("a:x", 3), ("a:y", 4), ("b:z", 2)] {
            let mut state = SkillState::new("Admin");
            state.level = level;
            skills.0.insert(key.into(), state);
        }
        assert_eq!(skills.character_level(), 1);
    }

    /// Every skill tree in `assets/skills` parses, and its active skills name a command.
    #[test]
    fn skill_tree_files_parse() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/skills");
        let mut count = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.to_string_lossy().ends_with(SKILL_EXTENSION) {
                let text = std::fs::read_to_string(&path).unwrap();
                let tree: SkillTreeDef = ron_options()
                    .from_str(&text)
                    .unwrap_or_else(|err| panic!("{} doesn't parse: {err}", path.display()));
                for (id, skill) in &tree.skills {
                    if let SkillKind::Active { command, .. } = &skill.kind {
                        assert!(!command.is_empty(), "{id} has no command");
                    }
                }
                assert!(!tree_id(&path).contains('.'), "{}", path.display());
                count += 1;
            }
        }
        assert!(count > 0);
    }
}
