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
//!
//! Trainers teach only each tree's entry skills. The rest of a tree can be discovered, very
//! rarely, by using its skills: every use has a small chance to reveal another skill in the same
//! tree whose requirements the character meets.

use std::collections::{BTreeMap, HashMap};

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::attributes::{Attribute, Attributes};
use crate::combat::{Fighting, Outmatched};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::conditions::{Condition, Facts};
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
/// The chance, per use of a skill, of discovering another in its tree, before Luck.
const DISCOVERY_CHANCE: f64 = 0.001;
/// The most a tough fight multiplies the chances of a gain or a discovery.
const TOUGH_FIGHT_MAX_BOOST: f64 = 2.0;
/// Skill levels per character level.
pub const LEVELS_PER_CHARACTER_LEVEL: u32 = 5;
/// Skills every character starts with, whatever their race and background: a basic attack, and
/// the passives everyone practises by living.
pub const UNIVERSAL_SKILLS: &[&str] =
    &["brawling:basics", "athletics:endurance", "athletics:dodge"];

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
                    .summary("use an active skill, like 'use lockpicking north'")
                    .action(),
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
    /// Practised whenever one of its triggers happens, and raising numbers as it levels.
    Passive {
        #[serde(default)]
        triggers: Vec<Trigger>,
        #[serde(default)]
        bonuses: Vec<PassiveBonus>,
        /// After dodging an attack while fighting with this attack skill, strike back at once:
        /// Riposte. Practised each time it fires.
        #[serde(default)]
        counter: Option<String>,
    },
    /// A special move made with an attack skill, in place of the auto-attack: `use cleave`.
    Technique(Technique),
    /// An attack, made every round of a fight. A weapon skill (Swordplay) has no damage of its
    /// own and needs a weapon that uses it in hand; an unarmed or natural attack (Brawling, a
    /// rat's Bite) has its own.
    Attack {
        /// What it does, as the attacker says it: "punch" ("Grok punches the rat").
        verb: String,
        /// Damage before the Strength bonus, low to high, for attacks made without a weapon.
        #[serde(default)]
        damage: Option<(u32, u32)>,
        /// What each use takes out of the attacker. Basic attacks are free; heavy blows aren't.
        #[serde(default)]
        cost: Cost,
    },
    /// While its holder is alive, mobs made from the `affects` template in the same zone fight
    /// better: the Rat King's command over his rats.
    Aura {
        /// A mob template, `zone:mob`.
        affects: String,
        bonus: Bonus,
    },
}

/// A number a passive skill raises as it levels: one point for every `per` levels, plus `base`.
/// With `with`, it only counts for attacks made with that skill: Bladework's accuracy is for
/// swords.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PassiveBonus {
    pub stat: Stat,
    #[serde(default = "default_per")]
    pub per: u32,
    #[serde(default)]
    pub base: i32,
    #[serde(default)]
    pub with: Option<String>,
}

fn default_per() -> u32 {
    5
}

/// What a passive bonus raises.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    MaxHealth,
    MaxStamina,
    /// Added to attack ratings.
    Attack,
    /// Added to defence against attacks.
    Defence,
    /// Added to damage dealt.
    Damage,
    /// Taken off damage received (every hit still does at least 1).
    Armour,
    Initiative,
}

/// Everything a character's passive skills add up to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Boosts {
    pub max_health: i32,
    pub max_stamina: i32,
    pub attack: i32,
    pub defence: i32,
    pub damage: i32,
    pub armour: i32,
    pub initiative: i32,
    /// Attack and damage that only count with one skill, by `tree:skill`.
    pub with: HashMap<String, (i32, i32)>,
}

impl Boosts {
    /// Attack bonus for an attack made with `skill`.
    pub fn attack_with(&self, skill: &str) -> i32 {
        self.attack + self.with.get(skill).map_or(0, |(attack, _)| *attack)
    }

    /// Damage bonus for an attack made with `skill`.
    pub fn damage_with(&self, skill: &str) -> i32 {
        self.damage + self.with.get(skill).map_or(0, |(_, damage)| *damage)
    }
}

/// Add up what a character's passive skills give them, at their current levels.
pub fn boosts(skills: &Skills, defs: &SkillDefs) -> Boosts {
    let mut total = Boosts::default();
    for (key, state) in &skills.0 {
        let Some(SkillKind::Passive { bonuses, .. }) = defs.skills.get(key).map(|s| &s.def.kind)
        else {
            continue;
        };
        for bonus in bonuses {
            let value = bonus.base + (state.level / bonus.per.max(1)) as i32;
            match (&bonus.with, bonus.stat) {
                (Some(skill), Stat::Attack) => {
                    total.with.entry(skill.clone()).or_default().0 += value
                }
                (Some(skill), Stat::Damage) => {
                    total.with.entry(skill.clone()).or_default().1 += value
                }
                (_, Stat::MaxHealth) => total.max_health += value,
                (_, Stat::MaxStamina) => total.max_stamina += value,
                (_, Stat::Attack) => total.attack += value,
                (_, Stat::Defence) => total.defence += value,
                (_, Stat::Damage) => total.damage += value,
                (_, Stat::Armour) => total.armour += value,
                (_, Stat::Initiative) => total.initiative += value,
            }
        }
    }
    total
}

/// A technique: a special move made with an attack skill, used through the action queue in a
/// fight (or, for an opener, to start one).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Technique {
    /// The attack skill it's made with, `tree:skill`; its weapon (or fists) must be ready.
    pub with: String,
    /// What it does: "kick" ("Grok kicks the rat").
    pub verb: String,
    #[serde(default)]
    pub cost: Cost,
    /// Added to the attack rating.
    #[serde(default)]
    pub accuracy: i32,
    /// Added to the damage.
    #[serde(default)]
    pub damage: i32,
    /// Whether it strikes at all, and whether a hit does damage.
    #[serde(default)]
    pub strike: StrikeKind,
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// Only works on someone not yet fighting back, or who is down or resting: Backstab. It can
    /// start a fight.
    #[serde(default)]
    pub opener: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StrikeKind {
    /// Rolls to hit and does damage.
    #[default]
    Hit,
    /// Rolls to hit, but a hit only applies its effects: a grab.
    Harmless,
    /// Doesn't strike: its effects are on the user, like raising a guard.
    None,
}

/// What a technique does besides damage.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// The target loses their next turn.
    Stagger,
    /// The target loses their next turn and defends at half until then.
    KnockDown,
    /// The target loses `damage` health on each of their next `rounds` turns.
    Bleed { damage: u32, rounds: u32 },
    /// The target can't flee and defends at half for `rounds` turns.
    Hold { rounds: u32 },
    /// The blow carries on into a second foe fighting you.
    Cleave,
    /// The target's armour doesn't help.
    IgnoreArmour,
    /// The user defends at half until their next turn.
    Expose,
    /// The user defends better for `rounds` turns.
    Guard { defence: i32, rounds: u32 },
}

/// What an aura adds to the mobs it affects.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Bonus {
    /// Added to their attack rating.
    #[serde(default)]
    pub attack: i32,
    /// Added to their defence rating.
    #[serde(default)]
    pub defence: i32,
    /// Taken off each hit they take (every hit still does at least 1).
    #[serde(default)]
    pub armour: i32,
}

impl std::ops::AddAssign for Bonus {
    fn add_assign(&mut self, other: Self) {
        self.attack += other.attack;
        self.defence += other.defence;
        self.armour += other.armour;
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub enum Cost {
    #[default]
    Free,
    Stamina(u32),
}

/// A game event that practises a passive skill.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum Trigger {
    /// One use for every `per` stamina spent.
    StaminaSpent { per: u32 },
    /// One use for every attack dodged.
    Dodged,
    /// One use for every `per` damage taken.
    Damaged { per: u32 },
    /// One use for every use of another skill in its tree: a discipline that grows with the
    /// powers built on it, like the Adept's Inner Discipline.
    UsedTree,
    /// One use for every use of this skill (`tree:skill`): Inner Discipline grows with Brawling.
    UsedSkill(String),
}

impl Trigger {
    /// How much of this trigger makes one use, if `event` is one of it, and how much happened.
    fn counts(&self, event: &GameEvent, who: Entity) -> Option<(f64, f64)> {
        match (self, event) {
            (Trigger::StaminaSpent { per }, GameEvent::StaminaSpent { who: w, amount })
                if *w == who =>
            {
                Some((f64::from((*per).max(1)), f64::from(*amount)))
            }
            (Trigger::Dodged, GameEvent::Dodged { who: w }) if *w == who => Some((1.0, 1.0)),
            (Trigger::Damaged { per }, GameEvent::Damaged { who: w, amount }) if *w == who => {
                Some((f64::from((*per).max(1)), f64::from(*amount)))
            }
            _ => None,
        }
    }
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
    /// How it was learned: `Starting`, `Taught`, `Discovered`, `Loot`, `Epiphany` or `Admin`.
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
            self.events.write(GameEvent::StaminaSpent {
                who,
                amount: n as f32,
            });
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

    /// Count one use of `key` by `who`: practice, maybe a level, maybe a discovery. Returns what
    /// to tell them.
    pub fn exercise(&mut self, who: Entity, learner: Learner, key: &str) -> Vec<String> {
        let Ok((mut skills, _, mut attributes)) = self.sheets.get_mut(who) else {
            return Vec::new();
        };
        count_use(
            &self.defs,
            &mut skills,
            &mut attributes,
            &mut self.dice,
            &self.db,
            learner,
            key,
        )
    }
}

/// The character practising a skill: who to save it for, their race for requirements, and how
/// much a tough fight multiplies their chances of a gain or a discovery (1 for none).
#[derive(Clone, Copy)]
pub struct Learner<'a> {
    pub character_id: i64,
    pub race: &'a str,
    pub boost: f64,
}

/// What a fight against odds of `outmatched` (see `combat::Outmatched`) does for learning: the
/// more the foe outclasses them, the likelier a gain or discovery, up to double. An even or easy
/// fight, or none, gives nothing extra.
pub fn tough_fight_boost(outmatched: Option<f64>) -> f64 {
    outmatched.map_or(1.0, |odds| odds.clamp(1.0, TOUGH_FIGHT_MAX_BOOST))
}

/// One use of a skill a character knows: practice, maybe a level, and maybe a character level
/// and its Karma, or a discovery. Saves the skill, and returns what to tell them.
pub fn count_use(
    defs: &SkillDefs,
    skills: &mut Skills,
    attributes: &mut Attributes,
    dice: &mut Dice,
    db: &Database,
    learner: Learner,
    key: &str,
) -> Vec<String> {
    let character_id = learner.character_id;
    if !skills.0.contains_key(key) {
        return Vec::new();
    }
    let level_before = skills.character_level();
    let luck = attributes.get(Attribute::Luck);
    let mut lines = Vec::new();
    // Skills that grow with this one or its tree, like the Adept's Inner Discipline, are practised
    // by this use too.
    let mut practised = vec![key.to_string()];
    practised.extend(grows_with(defs, skills, key));
    for key in &practised {
        let (Some(skill), Some(state)) = (defs.skills.get(key), skills.0.get_mut(key)) else {
            continue;
        };
        let gained = roll_skill_gain(
            state,
            skill.def.max_level,
            skill.def.difficulty,
            unix_now(),
            |chance| dice.chance(luck, chance * learner.boost),
        );
        if let Err(err) = db.save_skill(character_id, key, state) {
            error!("failed to save {key} for character {character_id}: {err}");
        }
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
        }
    }
    let level_after = skills.character_level();
    if level_after > level_before {
        attributes.set_earned(level_after as i32);
        lines.push(format!(
            "\x1b[1;33mYou feel more experienced. You have earned a point of Karma \
             ({} to spend).\x1b[0m",
            attributes.karma_unspent()
        ));
    }
    let facts = Facts {
        attributes,
        race: learner.race,
        skills: Some(skills),
    };
    let chance = DISCOVERY_CHANCE * learner.boost;
    if let Some(found) = discover(defs, &practised, &facts, dice, chance) {
        let (found, name) = (found.key.clone(), found.def.name.clone());
        learn(skills, &found, "Discovered", character_id, db);
        lines.push(format!(
            "\x1b[1;35mSomething clicks into place. You have discovered {name}.\x1b[0m"
        ));
    }
    lines
}

/// The skills a character knows that are practised whenever `used` is: passives triggered by
/// using it, or by using anything in their own tree, like a tradition's discipline growing with
/// its powers.
fn grows_with(defs: &SkillDefs, skills: &Skills, used: &str) -> Vec<String> {
    let Some(tree) = defs.skills.get(used).map(|s| &s.tree) else {
        return Vec::new();
    };
    skills
        .0
        .keys()
        .filter(|key| key.as_str() != used)
        .filter(|key| {
            let Some(skill) = defs.skills.get(*key) else {
                return false;
            };
            let SkillKind::Passive { triggers, .. } = &skill.def.kind else {
                return false;
            };
            triggers.iter().any(|trigger| match trigger {
                Trigger::UsedTree => &skill.tree == tree,
                Trigger::UsedSkill(skill) => skill == used,
                _ => false,
            })
        })
        .cloned()
        .collect()
}

/// The skills a character could discover by practising `used`: the others in their trees that
/// they don't know and whose requirements they meet. Entry skills, which require nothing, are
/// taught, not discovered.
fn discoverable<'a>(defs: &'a SkillDefs, used: &[String], facts: &Facts) -> Vec<&'a Skill> {
    let trees: Vec<&String> = used
        .iter()
        .filter_map(|key| defs.skills.get(key).map(|s| &s.tree))
        .collect();
    let mut found: Vec<&Skill> = defs
        .skills
        .values()
        .filter(|skill| trees.contains(&&skill.tree))
        .filter(|skill| {
            facts
                .skills
                .is_none_or(|known| !known.0.contains_key(&skill.key))
        })
        .filter(|skill| skill.def.requires.as_ref().is_some_and(|c| c.met(facts)))
        .collect();
    found.sort_by(|a, b| a.key.cmp(&b.key));
    found
}

/// With `chance` (and the character's Luck), one of the skills practising `used` could reveal.
fn discover<'a>(
    defs: &'a SkillDefs,
    used: &[String],
    facts: &Facts,
    dice: &mut Dice,
    chance: f64,
) -> Option<&'a Skill> {
    let found = discoverable(defs, used, facts);
    if found.is_empty() || !dice.chance(facts.attributes.get(Attribute::Luck), chance) {
        return None;
    }
    let pick = dice.range(0, 0..=found.len() as i64 - 1) as usize;
    found.get(pick).copied()
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
fn practise_passives(
    mut events: MessageReader<GameEvent>,
    defs: Res<SkillDefs>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    fights: Query<&Outmatched, With<Fighting>>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mut players: PlayerQuery,
) {
    let mut out = Vec::new();
    for event in events.read() {
        let who = match event {
            GameEvent::StaminaSpent { who, .. }
            | GameEvent::Dodged { who }
            | GameEvent::Damaged { who, .. } => *who,
            _ => continue,
        };
        // Only players practise; mobs fight at the levels their templates give them.
        let (Some(actor), Ok((mut skills, mut attributes))) =
            (Actor::get(&players, who), sheets.get_mut(who))
        else {
            continue;
        };
        let passives: Vec<(String, f64, f64)> = skills
            .0
            .keys()
            .filter_map(|key| match &defs.skills.get(key)?.def.kind {
                SkillKind::Passive { triggers, .. } => triggers
                    .iter()
                    .find_map(|trigger| trigger.counts(event, who))
                    .map(|(per, amount)| (key.clone(), per, amount)),
                _ => None,
            })
            .collect();
        for (key, per, amount) in passives {
            let Some(state) = skills.0.get_mut(&key) else {
                continue;
            };
            state.pending += amount;
            let uses = (state.pending / per).floor();
            state.pending -= uses * per;
            for _ in 0..uses as u32 {
                let learner = Learner {
                    character_id: actor.character_id,
                    race: &actor.race,
                    boost: tough_fight_boost(fights.get(who).ok().map(|o| o.0)),
                };
                let lines = count_use(
                    &defs,
                    &mut skills,
                    &mut attributes,
                    &mut dice,
                    &db,
                    learner,
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
                SkillKind::Attack { .. } => " (attack)",
                SkillKind::Technique(_) => " (technique)",
                SkillKind::Aura { .. } => " (aura)",
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
                SkillKind::Attack { .. } => Err(format!(
                    "{} is an attack: make it your auto-attack with 'autoattack {}', then fight.",
                    skill.def.name,
                    skill
                        .key
                        .split_once(':')
                        .map_or(skill.key.as_str(), |(_, id)| id)
                )),
                SkillKind::Passive { .. } | SkillKind::Aura { .. } => Err(format!(
                    "{} isn't something you use; it improves as you go.",
                    skill.def.name
                )),
                // Techniques belong to combat: in a fight, the round makes them on the user's
                // turn; out of one, the `technique` command starts a fight with an opener.
                SkillKind::Technique(_) => match registry.find("technique", actor.role) {
                    Some((spec, id)) => {
                        commands.run_system_with(
                            *id,
                            Invocation {
                                entity: inv.entity,
                                command: spec.name,
                                usage: spec.usage,
                                args: format!("{} {}", skill.key, target.trim()),
                            },
                        );
                        return;
                    }
                    None => Err(format!("You can't put {} to use yet.", skill.def.name)),
                },
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
    fn passive_bonuses_grow_with_level() {
        let tough = Skill {
            key: "athletics:toughness".into(),
            tree: "athletics".into(),
            attribute: Attribute::Body,
            def: SkillDef {
                name: "Toughness".into(),
                kind: SkillKind::Passive {
                    triggers: Vec::new(),
                    counter: None,
                    bonuses: vec![
                        PassiveBonus {
                            stat: Stat::MaxHealth,
                            per: 5,
                            base: 0,
                            with: None,
                        },
                        PassiveBonus {
                            stat: Stat::Attack,
                            per: 10,
                            base: 1,
                            with: Some("swordplay:basics".into()),
                        },
                    ],
                },
                max_level: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires: None,
            },
        };
        let defs = SkillDefs {
            skills: HashMap::from([(tough.key.clone(), tough)]),
            trees: HashMap::new(),
        };
        let mut skills = Skills::default();
        let mut state = SkillState::new("Admin");
        state.level = 23;
        skills.0.insert("athletics:toughness".into(), state);
        let boosts = boosts(&skills, &defs);
        assert_eq!(boosts.max_health, 4);
        assert_eq!(boosts.attack_with("swordplay:basics"), 3);
        assert_eq!(boosts.attack_with("axes:basics"), 0);
    }

    /// A skill in `tree` requiring `requires`.
    fn tree_skill(key: &str, requires: Option<Condition>) -> (String, Skill) {
        let skill = Skill {
            key: key.into(),
            tree: key.split_once(':').unwrap().0.into(),
            attribute: Attribute::Strength,
            def: SkillDef {
                name: key.into(),
                kind: SkillKind::Passive {
                    triggers: Vec::new(),
                    counter: None,
                    bonuses: Vec::new(),
                },
                max_level: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires,
            },
        };
        (key.into(), skill)
    }

    #[test]
    fn using_a_skill_can_reveal_others_in_its_tree_whose_requirements_are_met() {
        let at = |level| Some(Condition::Skill("axes:basics".into(), level));
        let defs = SkillDefs {
            skills: HashMap::from([
                tree_skill("axes:basics", None),
                tree_skill("axes:cleave", at(15)),
                tree_skill("axes:hew", at(15)),
                tree_skill("axes:whirlwind", at(45)),
                tree_skill("axes:dwarf_axe", Some(Condition::Race("dwarf".into()))),
                tree_skill("swordplay:lunge", None),
            ]),
            trees: HashMap::new(),
        };
        let attributes = Attributes::default();
        let mut skills = Skills::default();
        let mut basics = SkillState::new("Taught");
        basics.level = 15;
        skills.0.insert("axes:basics".into(), basics);
        skills
            .0
            .insert("axes:hew".into(), SkillState::new("Taught"));
        let facts = Facts {
            attributes: &attributes,
            race: "human",
            skills: Some(&skills),
        };
        // Not what they know, nor the entry, nor what they don't qualify for, nor other trees.
        let used = |key: &str| vec![key.to_string()];
        let found: Vec<&str> = discoverable(&defs, &used("axes:basics"), &facts)
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(found, ["axes:cleave"]);
        // Using any skill in the tree can reveal it, given luck enough.
        let mut dice = Dice::seeded(1);
        let found = discover(&defs, &used("axes:hew"), &facts, &mut dice, 1.0);
        assert_eq!(found.map(|s| s.key.as_str()), Some("axes:cleave"));
        assert!(discover(&defs, &used("axes:hew"), &facts, &mut dice, 0.0).is_none());
        assert!(discover(&defs, &used("swordplay:lunge"), &facts, &mut dice, 1.0).is_none());
    }

    #[test]
    fn a_discipline_is_practised_by_every_power_in_its_tree() {
        let (key, mut discipline) = tree_skill("adept:foundation", None);
        discipline.def.kind = SkillKind::Passive {
            triggers: vec![
                Trigger::UsedTree,
                Trigger::UsedSkill("brawling:basics".into()),
            ],
            counter: None,
            bonuses: Vec::new(),
        };
        let defs = SkillDefs {
            skills: HashMap::from([
                (key, discipline),
                tree_skill("adept:killing_hands", None),
                tree_skill("brawling:basics", None),
                tree_skill("axes:basics", None),
            ]),
            trees: HashMap::new(),
        };
        let mut skills = Skills::default();
        for key in [
            "adept:foundation",
            "adept:killing_hands",
            "brawling:basics",
            "axes:basics",
        ] {
            skills.0.insert(key.into(), SkillState::new("Taught"));
        }
        let (mut attributes, mut dice) = (Attributes::default(), Dice::seeded(3));
        let db = Database::open(":memory:").unwrap();
        let learner = Learner {
            character_id: 1,
            race: "human",
            boost: 1.0,
        };
        let practice = |skills: &Skills, key: &str| skills.0[key].practice;
        let mut use_skill = |skills: &mut Skills, key: &str| {
            count_use(&defs, skills, &mut attributes, &mut dice, &db, learner, key);
        };
        // A power practises the discipline too.
        use_skill(&mut skills, "adept:killing_hands");
        assert!(practice(&skills, "adept:killing_hands") > 0.0);
        let after_power = practice(&skills, "adept:foundation");
        assert!(after_power > 0.0);
        // So does the skill it names, but not other trees' skills.
        use_skill(&mut skills, "brawling:basics");
        let after_brawl = practice(&skills, "adept:foundation");
        assert!(after_brawl > after_power);
        use_skill(&mut skills, "axes:basics");
        assert_eq!(practice(&skills, "adept:foundation"), after_brawl);
        // Nor does the discipline count itself twice.
        let mut alone = Skills::default();
        alone
            .0
            .insert("adept:foundation".into(), SkillState::new("Taught"));
        use_skill(&mut alone, "adept:foundation");
        assert!((practice(&alone, "adept:foundation") - 0.1).abs() < 1e-9);
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
