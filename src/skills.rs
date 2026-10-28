//! Skills: what a character can do, learned and improved only by use.
//!
//! Skill trees are data, one file per tree in `assets/skills/` (`thievery.skill.ron`; the double
//! extension tells Bevy which loader reads them), hot-reloaded like zones. A skill is
//! active (used with a command, costing stamina, with a cooldown) or passive (practised whenever
//! a game event it listens for happens). Every use builds practice and may roll for a level; see
//! `docs/progression.md` for the formulas. Levels have no cap, but each is harder to reach than
//! the last, and a skill well above its soft cap (set by the character's level, which comes from
//! experience, not skills) grows slowly.
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
use crate::combat::{Conditions, Fighting, Outmatched};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::conditions::{Condition, Facts};
use crate::db::{Database, Permission};
use crate::dice::{Contest, Dice};
use crate::effects::{self, Effect, Reaction, Resist};
use crate::events::GameEvent;
use crate::formula::{Formula, Inputs};
use crate::output::{
    Actor, Characters, Outbox, Outgoing, Reply, deliver, reply_line, targets_online,
};
use crate::spells::{Aim, SpellDefs, Spells};
use crate::stealth::reveal;
use crate::targeting::{Keywords, Target};
use crate::vitals::Vitals;
use crate::world::ron_options;

/// Asset folder holding one `.skill.ron` file per skill tree.
pub const SKILLS_PATH: &str = "skills";
/// The extension skill tree files use.
const SKILL_EXTENSION: &str = "skill.ron";

/// The gain chance of an untouched skill at level 0, before practice and difficulty.
const BASE_GAIN_CHANCE: f64 = 0.25;
/// The level at which the gain chance is half its first: it's 25 / (25 + level) of it.
const LEVEL_EASE: f64 = 25.0;
/// The soft cap: a skill grows at full speed up to 10 + 2 × character level...
const SOFT_CAP_BASE: u32 = 10;
const SOFT_CAP_PER_LEVEL: u32 = 2;
/// ...and past it, its gain chance halves for every 5 levels over.
const SOFT_CAP_HALVING: f64 = 5.0;
/// The cooldown after a gain at level 0, in seconds; each level multiplies it by `COOLDOWN_GROWTH`.
const BASE_COOLDOWN: f64 = 5.0 * 60.0;
const COOLDOWN_GROWTH: f64 = 1.1;
/// The chance, per use of a skill, of discovering another in its tree, before Luck.
const DISCOVERY_CHANCE: f64 = 0.001;
/// The most a tough fight multiplies the chances of a gain or a discovery.
const TOUGH_FIGHT_MAX_BOOST: f64 = 2.0;
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
                    .usage("skills [<skill>]")
                    .summary("list your skills, their levels and how warmed up they are, or read about one"),
                skills_cmd,
            )
            .add_command(
                CommandSpec::new("use", Category::Interaction)
                    .usage("use <skill> [<target>]")
                    .summary("use an active skill, like 'use lockpicking north'; spells are cast, but 'use' finds them too")
                    .quiet()
                    .action(),
                use_cmd,
            )
            .add_command(
                CommandSpec::new("karma", Category::Character)
                    .usage("karma [<skill>]")
                    .summary("see the skills in your trees Karma can unlock, or spend Karma on one"),
                karma_cmd,
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

impl Rarity {
    pub fn name(self) -> &'static str {
        match self {
            Rarity::Common => "common",
            Rarity::Uncommon => "uncommon",
            Rarity::Rare => "rare",
            Rarity::Epic => "epic",
            Rarity::Legendary => "legendary",
        }
    }

    /// What it does to an item's value.
    pub fn value_factor(self) -> f64 {
        match self {
            Rarity::Common => 1.0,
            Rarity::Uncommon => 1.5,
            Rarity::Rare => 3.0,
            Rarity::Epic => 6.0,
            Rarity::Legendary => 12.0,
        }
    }

    /// The colour names of this rarity show in; common things are uncoloured.
    pub fn colour(self) -> Option<&'static str> {
        match self {
            Rarity::Common => None,
            Rarity::Uncommon => Some("\x1b[32m"),
            Rarity::Rare => Some("\x1b[1;34m"),
            Rarity::Epic => Some("\x1b[1;35m"),
            Rarity::Legendary => Some("\x1b[1;33m"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SkillDef {
    pub name: String,
    /// What players read about it: what it is and does.
    pub description: String,
    pub kind: SkillKind,
    /// What everyone sees when it's used. Required for every skill that shows itself in play
    /// (attacks, techniques, counters and active skills); see [`SkillDef::check`].
    #[serde(default)]
    pub text: Option<SkillText>,
    /// The level at which it's mastered: a milestone, not a cap. Levels go on, ever harder.
    #[serde(default = "default_mastery", alias = "max_level")]
    pub mastery: u32,
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
    /// Karma to unlock it without a teacher (`karma <skill>`), for a character who already has a
    /// skill in its tree and meets its requirements. Without one, Karma can't unlock it: entry
    /// skills, which trainers teach, and the world's hidden skills.
    #[serde(default)]
    pub karma: Option<u32>,
}

impl SkillDef {
    /// Whether it has all the text its kind shows in play, so none is forgotten. A skill that
    /// fails this keeps its whole tree from loading.
    pub fn check(&self) -> Result<(), String> {
        if self.description.trim().is_empty() {
            return Err("needs a description".into());
        }
        if self.karma.is_some() && self.requires.is_none() {
            return Err("has a Karma cost but no requirements: entry skills are taught".into());
        }
        if let SkillKind::Technique(t) = &self.kind {
            effects::check(&t.effects)?;
            if effects::in_a_room(&t.effects) {
                return Err("can't Find: only an active skill aimed at the room can".into());
            }
        }
        if let SkillKind::Active {
            command,
            effects,
            aim,
            ..
        } = &self.kind
        {
            match (command, effects.is_empty()) {
                (Some(_), false) => return Err("does its command or its effects, not both".into()),
                (None, true) => return Err("needs a command or effects".into()),
                _ => effects::check(effects)?,
            }
            // A lock can only be locked or unlocked, for now.
            let on_a_lock = |e: &Effect| {
                matches!(e, Effect::Apply { condition, .. } | Effect::Remove { condition, .. }
                    if condition == LOCKED)
            };
            if *aim == Aim::Lock && !effects.iter().all(on_a_lock) {
                return Err(format!(
                    "is aimed at a lock, so it can only Apply or Remove \"{LOCKED}\""
                ));
            }
            // A room yields what's found in it, or tells its tracks; an item its worth. Each one
            // thing, for now.
            let only = |fits: fn(&Effect) -> bool| effects.len() == 1 && fits(&effects[0]);
            let ok = match aim {
                Aim::Room => only(|e| {
                    matches!(
                        e,
                        Effect::Find { .. } | Effect::Reveal(effects::Revealed::Tracks)
                    )
                }),
                Aim::Item => only(|e| matches!(e, Effect::Reveal(effects::Revealed::Worth))),
                _ => !effects::in_a_room(effects),
            };
            if !ok {
                return Err(match aim {
                    Aim::Room => "is aimed at the room, so it does one Find or Reveal(Tracks)",
                    Aim::Item => "is aimed at an item, so it does one Reveal(Worth)",
                    _ => "Finds or Reveals, so it must be aimed at the Room or an Item",
                }
                .into());
            }
        }
        if let SkillKind::Passive { reactions, .. } = &self.kind {
            reactions.iter().try_for_each(Reaction::check)?;
            if reactions.iter().any(|r| r.on.own()) {
                return Err("only a condition reacts to taking hold or running out".into());
            }
        }
        let (target, miss) = match &self.kind {
            // Struck at someone, so their view and a miss are needed.
            SkillKind::Attack { .. } => (true, true),
            SkillKind::Technique(t) if t.strike == StrikeKind::None => (false, false),
            SkillKind::Technique(_) => (true, true),
            SkillKind::Passive {
                counter: Some(_), ..
            } => (true, false),
            // Its command says whether it worked.
            SkillKind::Active {
                command: Some(_), ..
            } => (false, true),
            // Done to someone, who sees it, unless it's only to yourself, to a lock or the room,
            // or slipped past them (`Other`, a pickpocket's); it can fail when it's a contest.
            SkillKind::Active {
                resist,
                against,
                aim,
                ..
            } => (
                matches!(aim, Aim::Ally | Aim::Foe),
                resist.is_some() || against.is_some(),
            ),
            // Nothing to show.
            SkillKind::Passive { .. } | SkillKind::Aura { .. } => return Ok(()),
        };
        let Some(text) = &self.text else {
            return Err("needs text: what it looks like when used".into());
        };
        // A blow that does damage says how much, in Diku's words: "Your slash {harm} the rat."
        let hurts = match &self.kind {
            SkillKind::Attack { .. } => true,
            SkillKind::Technique(t) => t.strike == StrikeKind::Hit,
            SkillKind::Active { effects, .. } => {
                effects.iter().any(|e| matches!(e, Effect::Damage { .. }))
            }
            _ => false,
        };
        let lines = [
            Some(&text.hit.actor),
            text.hit.target.as_ref(),
            Some(&text.hit.room),
        ];
        if hurts
            && lines
                .into_iter()
                .flatten()
                .any(|line| !line.contains("{harm}"))
        {
            return Err("needs {harm} in every hit line, for how much it hurt".into());
        }
        if target && text.hit.target.is_none() {
            return Err("needs text for its target (hit.target)".into());
        }
        match &text.miss {
            None if miss => Err("needs text for when it misses or fails (miss)".into()),
            Some(lines) if target && lines.target.is_none() => {
                Err("needs text for its target when it misses (miss.target)".into())
            }
            _ => Ok(()),
        }
    }
}

/// What everyone sees when a skill is used, as templates filled in at the time: `{actor}` is the
/// user and `{target}` what it's used on, and a command can offer more (`pick` gives `{door}`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SkillText {
    /// When it works: a blow that lands, a lock that gives.
    pub hit: Lines,
    /// When it doesn't: a miss, a lock that won't give.
    #[serde(default)]
    pub miss: Option<Lines>,
}

/// One moment, as each onlooker sees it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Lines {
    /// What the user sees: "You kick {target}."
    pub actor: String,
    /// What the one it's used on sees, when that's someone: "{actor} kicks you."
    #[serde(default)]
    pub target: Option<String>,
    /// What everyone else there sees: "{actor} kicks {target}."
    pub room: String,
    /// What's heard through a door, for skills used on one.
    #[serde(default)]
    pub beyond: Option<String>,
}

/// Fill in a text template: each `{name}` from `with`. A line that starts with a name, like
/// "{actor} kicks you.", gets its capital letter: "The town guard kicks you."
pub fn fill(template: &str, with: &[(&str, &str)]) -> String {
    let mut text = template.to_string();
    for (name, value) in with {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

fn default_mastery() -> u32 {
    50
}

fn default_difficulty() -> f64 {
    1.0
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum SkillKind {
    /// Used on purpose, with `use`: through its own `command`, for what's like nothing else
    /// (picking a lock, tracking), or by doing its `effects` to someone, resolved like a spell.
    Active {
        /// The command that performs it: `use lockpicking north` runs `pick north`.
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        cost: Cost,
        /// Seconds before it can be used again.
        #[serde(default)]
        cooldown: f64,
        /// Whom its effects are for: yourself or someone here (the default), or a foe.
        #[serde(default = "ally")]
        aim: Aim,
        /// An item it uses up each try, by tag: First Aid's bandage.
        #[serde(default)]
        uses: Option<String>,
        /// What the target resists it with, by opposed roll against the user's skill (its level
        /// plus its attribute). Without, it always works, unless it's `against` a difficulty.
        #[serde(default)]
        resist: Option<Resist>,
        /// A fixed difficulty to beat instead, a formula: how bad a wound is to bind,
        /// `"(target.most_health - target.health) / 2"`.
        #[serde(default)]
        against: Option<Formula>,
        /// What it is, for the skills that resist it, beside the tags of the conditions it puts on.
        #[serde(default)]
        tags: Vec<String>,
        /// What it does. Formulas see the user's skill (level plus attribute) as `power`.
        #[serde(default)]
        effects: Vec<Effect>,
        /// When it fails, a mob it was tried on turns on the user: a pickpocket caught.
        #[serde(default)]
        provokes: bool,
        /// Using it doesn't show a hidden user: reading tracks, weighing an item's worth.
        #[serde(default)]
        quiet: bool,
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
        /// What it does by itself when its bearer hits, is struck, dodges, kills or is hurt.
        /// Effects' formulas see the skill's level as `power`. Practised each time one fires.
        #[serde(default)]
        reactions: Vec<Reaction>,
    },
    /// A special move made with an attack skill, in place of the auto-attack: `use cleave`.
    Technique(Technique),
    /// An attack, made every round of a fight. A weapon skill (Swordplay) has no damage of its
    /// own and needs a weapon that uses it in hand; an unarmed or natural attack (Brawling, a
    /// rat's Bite) has its own.
    Attack {
        /// Damage before the Strength bonus, low to high, for attacks made without a weapon.
        #[serde(default)]
        damage: Option<(u32, u32)>,
        /// What each use takes out of the attacker. Basic attacks are free; heavy blows aren't.
        #[serde(default)]
        cost: Cost,
        /// What kind of harm it does without a weapon: physical, unless it says otherwise (a
        /// fire beetle's bite). A weapon in hand does its own type.
        #[serde(rename = "type", default = "crate::world::physical")]
        damage_type: String,
    },
    /// While its holder is alive, mobs made from the `affects` template in the same zone fight
    /// better: the Rat King's command over his rats.
    Aura {
        /// A mob template (`mob` in the holder's zone, or `zone:mob`), or a tag its mobs have:
        /// every other one in the holder's zone fights better. Never the holder itself.
        affects: String,
        bonus: Bonus,
    },
}

/// The condition a lock is in when it's locked: on a door or a container, it's their state.
pub const LOCKED: &str = "locked";

/// Whether `effects` find something in a room, rather than reading it.
fn finds(effects: &[Effect]) -> bool {
    effects.iter().any(|e| matches!(e, Effect::Find { .. }))
}

fn ally() -> Aim {
    Aim::Ally
}

/// A number a passive skill raises as it levels: a point for every `per` levels, growing a
/// fraction with each one, plus `base`, so level 1 and level 2 differ. With `with`, it only counts
/// for attacks made with that skill: Bladework's accuracy is for swords.
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
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Stat {
    MaxHealth,
    MaxStamina,
    /// Added to attack ratings.
    Attack,
    /// Added to defence against attacks.
    Defence,
    /// Added to damage dealt.
    Damage,
    /// Taken off damage received, even to nothing.
    Armour,
    Initiative,
    /// Armour ignored by attacks: with `with`, only that skill's.
    Pierce,
    /// Taken off the penalty armour puts on defence, never below none.
    ArmourEase,
    /// Added to a held shield's block.
    Block,
    /// Added to the rating they resist a spell or technique with, when it carries this tag: an
    /// elf's mind shrugging off sleep, `Resist("sleep")`. Tags come from the conditions it puts on,
    /// the damage it does and its own `tags` (`effects::tags`).
    Resist(String),
}

/// Everything a character's passive skills add up to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Boosts {
    pub max_health: f64,
    pub max_stamina: f64,
    pub attack: f64,
    pub defence: f64,
    pub damage: f64,
    pub armour: f64,
    pub initiative: f64,
    pub pierce: f64,
    pub armour_ease: f64,
    pub block: f64,
    /// What only counts with one skill, by `tree:skill`.
    pub with: HashMap<String, WithSkill>,
    /// Added to resisting what carries a tag, by tag.
    pub resist: HashMap<String, f64>,
}

/// Bonuses for attacks made with one skill.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WithSkill {
    pub attack: f64,
    pub damage: f64,
    pub pierce: f64,
}

impl Boosts {
    /// Attack bonus for an attack made with `skill`.
    pub fn attack_with(&self, skill: &str) -> f64 {
        self.attack + self.with.get(skill).map_or(0.0, |w| w.attack)
    }

    /// Damage bonus for an attack made with `skill`.
    pub fn damage_with(&self, skill: &str) -> f64 {
        self.damage + self.with.get(skill).map_or(0.0, |w| w.damage)
    }

    /// Added to resisting something carrying `tags`: the best of their bonuses that match, so a
    /// sleep spell that's also a mind spell doesn't count both.
    pub fn resisting(&self, tags: &[String]) -> f64 {
        tags.iter()
            .filter_map(|tag| self.resist.get(tag))
            .copied()
            .fold(0.0, f64::max)
    }

    /// Armour ignored by an attack made with `skill`.
    pub fn pierce_with(&self, skill: &str) -> f64 {
        self.pierce + self.with.get(skill).map_or(0.0, |w| w.pierce)
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
            // Every level counts: a point for every `per` levels, a fraction of one for each.
            let value =
                f64::from(bonus.base) + f64::from(state.level) / f64::from(bonus.per.max(1));
            match (&bonus.with, &bonus.stat) {
                (Some(skill), Stat::Attack) => {
                    total.with.entry(skill.clone()).or_default().attack += value
                }
                (Some(skill), Stat::Damage) => {
                    total.with.entry(skill.clone()).or_default().damage += value
                }
                (Some(skill), Stat::Pierce) => {
                    total.with.entry(skill.clone()).or_default().pierce += value
                }
                (_, Stat::MaxHealth) => total.max_health += value,
                (_, Stat::MaxStamina) => total.max_stamina += value,
                (_, Stat::Attack) => total.attack += value,
                (_, Stat::Defence) => total.defence += value,
                (_, Stat::Damage) => total.damage += value,
                (_, Stat::Armour) => total.armour += value,
                (_, Stat::Initiative) => total.initiative += value,
                (_, Stat::Pierce) => total.pierce += value,
                (_, Stat::ArmourEase) => total.armour_ease += value,
                (_, Stat::Block) => total.block += value,
                (_, Stat::Resist(tag)) => *total.resist.entry(tag.clone()).or_default() += value,
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
    /// What the target resists it with: `Some(Evasion)`, as a blow, unless it names another
    /// (`Some(Attribute(Strength))` for a grip), or `None` if nothing can.
    #[serde(default = "effects::evasion")]
    pub resist: Option<Resist>,
    /// What it is, beyond what its effects say (`effects::tags`), for skills that resist it.
    #[serde(default)]
    pub tags: Vec<String>,
    /// What it does besides its blow, in the one effect vocabulary (`effects.rs`): conditions
    /// on the target when it lands, or on the user whether or not it does.
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
    /// One use for every attack turned aside with a shield.
    Blocked,
    /// One use for every hit taken in armour.
    HitInArmour,
    /// One use for every `per` damage taken.
    Damaged { per: u32 },
    /// One use for every use of another skill in its tree: a discipline that grows with the
    /// powers built on it, like the Adept's Inner Discipline.
    UsedTree,
    /// One use for every use of this skill (`tree:skill`).
    UsedSkill(String),
    /// One use for every attack made with an attack skill, armed or not: a sword's swing, a
    /// punch. Inner Discipline grows with it.
    UsedAttack,
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
            (Trigger::Dodged, GameEvent::Dodged { who: w })
            | (Trigger::Blocked, GameEvent::Blocked { who: w })
            | (Trigger::HitInArmour, GameEvent::HitInArmour { who: w })
                if *w == who =>
            {
                Some((1.0, 1.0))
            }
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
    /// The known skill named at the start of `args`, and the rest: the longest run of words that
    /// names one, so `first aid grok` is First Aid on Grok and `kick rat` is Kick on the rat.
    pub fn find_known_in<'a, 'b>(
        &'a self,
        known: &Skills,
        args: &'b str,
    ) -> Option<(&'a Skill, &'b str)> {
        let args = args.trim();
        let mut ends: Vec<usize> = args.match_indices(' ').map(|(i, _)| i).collect();
        ends.push(args.len());
        ends.into_iter().rev().find_map(|end| {
            let (name, rest) = args.split_at(end);
            let name = name.trim();
            (!name.is_empty())
                .then(|| self.find_known(known, name))
                .flatten()
                .map(|skill| (skill, rest.trim()))
        })
    }

    pub fn find_known<'a>(&'a self, known: &Skills, name: &str) -> Option<&'a Skill> {
        let name = name.to_lowercase();
        let mut candidates = known.0.keys().filter_map(|key| self.skills.get(key));
        let exact = |skill: &&Skill| {
            skill.key == name || skill.key.split_once(':').is_some_and(|(_, id)| id == name)
        };
        let all: Vec<&Skill> = candidates.by_ref().collect();
        all.iter().copied().find(exact).or_else(|| {
            all.into_iter().find(|skill| {
                let full = skill.def.name.to_lowercase();
                full.starts_with(&name)
                    || full.split_whitespace().any(|word| word.starts_with(&name))
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
    /// Karma paid to unlock it, if it was unlocked.
    pub karma_paid: u32,
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
            karma_paid: 0,
        }
    }
}

/// A character's skills, by `tree:skill`. Lives on the player entity.
#[derive(Component, Clone, Debug, Default)]
pub struct Skills(pub BTreeMap<String, SkillState>);

/// Practice after `elapsed` seconds unused.
pub fn faded(practice: f64, elapsed: i64) -> f64 {
    // How long practice takes to halve while a skill goes unused (`progression.ron`).
    let half_life = crate::attributes::progression().practice_half_life_minutes * 60.0;
    practice * 0.5f64.powf(elapsed.max(0) as f64 / half_life.max(1.0))
}

/// The chance one use gains a level.
pub fn gain_chance(level: u32, difficulty: f64, practice: f64, soft_cap: u32) -> f64 {
    let level_ease = LEVEL_EASE / (LEVEL_EASE + f64::from(level));
    let over = f64::from(level.saturating_sub(soft_cap));
    difficulty
        * BASE_GAIN_CHANCE
        * level_ease
        * (0.2 + 0.8 * practice)
        * 0.5f64.powf(over / SOFT_CAP_HALVING)
}

/// How far a skill grows at full speed for a character of `level`: past it, gains come slowly.
pub fn soft_cap(level: u32) -> u32 {
    SOFT_CAP_BASE + SOFT_CAP_PER_LEVEL * level
}

/// Seconds before a skill can gain again, after gaining a level from `level`.
pub fn gain_cooldown(level: u32) -> i64 {
    (BASE_COOLDOWN * COOLDOWN_GROWTH.powi(level as i32)) as i64
}

/// One use of a skill at unix time `now`: practice fades and builds, and if the skill is off its
/// gain cooldown, `roll` is asked whether the chance comes up. Returns whether it gained a level.
pub fn roll_skill_gain(
    state: &mut SkillState,
    difficulty: f64,
    soft_cap: u32,
    now: i64,
    roll: impl FnOnce(f64) -> bool,
) -> bool {
    let practice = faded(state.practice, state.last_used.map_or(0, |at| now - at));
    let mut gained = false;
    if now >= state.next_gain_at {
        let chance = gain_chance(state.level, difficulty, practice, soft_cap);
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
    pub events: MessageWriter<'w, GameEvent>,
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

    /// What formulas see of `who` and their skill, using it on `target` (a lock's numbers, say).
    pub fn inputs(&self, who: Entity, power: i32, target: crate::formula::Side) -> Inputs {
        let attributes = self.sheets.get(who).map_or_else(
            |_| Vec::new(),
            |(_, _, a)| {
                Attribute::ALL
                    .iter()
                    .map(|x| (*x, f64::from(a.get(*x))))
                    .collect()
            },
        );
        Inputs {
            power: f64::from(power),
            user: crate::formula::Side {
                attributes,
                ..Default::default()
            },
            target,
            ..Default::default()
        }
    }

    /// Work out a fixed difficulty: a formula, rolled without Luck.
    pub fn eval(&mut self, formula: &Formula, inputs: &Inputs) -> f64 {
        formula.eval(inputs, &mut self.dice, 0)
    }

    /// An opposed roll: `rating` with `luck` against `against` with `their_luck` (0 for a fixed
    /// difficulty). See the ratio rule in `docs/attributes.md`.
    pub fn opposed(&mut self, rating: f64, luck: i32, against: f64, their_luck: i32) -> Contest {
        self.dice.opposed(rating, luck, against, their_luck)
    }

    /// Roll a yes-or-no with `who`'s Luck.
    pub fn chance(&mut self, luck: i32, p: f64) -> bool {
        self.dice.chance(luck, p)
    }

    /// `who`'s skills, vitals and attributes, for systems that need them alongside practice.
    pub fn sheet(
        &mut self,
        who: Entity,
    ) -> Option<(Mut<'_, Skills>, Mut<'_, Vitals>, Mut<'_, Attributes>)> {
        self.sheets.get_mut(who).ok()
    }

    /// Pick from a weighted table ordered worst to best, with `who`'s Luck.
    pub fn pick<'t, T>(&mut self, luck: i32, table: &'t [(T, u32)]) -> Option<&'t T> {
        self.dice.pick(luck, table)
    }

    /// Roll a value in `range`, higher being better, with `who`'s Luck.
    pub fn roll(&mut self, luck: i32, range: std::ops::RangeInclusive<i64>) -> i64 {
        self.dice.range(luck, range)
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
    /// Their saved character; a mob has none, and doesn\'t learn.
    pub character_id: Option<i64>,
    pub race: &'a str,
    pub boost: f64,
}

/// What a fight against odds of `outmatched` (see `combat::Outmatched`) does for learning: the
/// more the foe outclasses them, the likelier a gain or discovery, up to double. An even or easy
/// fight, or none, gives nothing extra.
pub fn tough_fight_boost(outmatched: Option<f64>) -> f64 {
    outmatched.map_or(1.0, |odds| odds.clamp(1.0, TOUGH_FIGHT_MAX_BOOST))
}

/// One use of a skill a character knows: practice, maybe a level, and maybe a discovery and the
/// experience it brings. Saves the skill, and returns what to tell them.
pub fn count_use(
    defs: &SkillDefs,
    skills: &mut Skills,
    attributes: &mut Attributes,
    dice: &mut Dice,
    db: &Database,
    learner: Learner,
    key: &str,
) -> Vec<String> {
    // Mobs don't improve: only a saved character learns.
    let Some(character_id) = learner.character_id else {
        return Vec::new();
    };
    if !skills.0.contains_key(key) {
        return Vec::new();
    }
    let luck = attributes.get(Attribute::Luck);
    let cap = soft_cap(attributes.level());
    let mut lines = Vec::new();
    // Skills that grow with this one or its tree, like the Adept's Inner Discipline, are practised
    // by this use too.
    let mut practised = vec![key.to_string()];
    practised.extend(grows_with(defs, skills, key));
    for key in &practised {
        let (Some(skill), Some(state)) = (defs.skills.get(key), skills.0.get_mut(key)) else {
            continue;
        };
        let gained = roll_skill_gain(state, skill.def.difficulty, cap, unix_now(), |chance| {
            dice.chance(luck, chance * learner.boost)
        });
        if let Err(err) = db.save_skill(character_id, key, state) {
            error!("failed to save {key} for character {character_id}: {err}");
        }
        if gained {
            let level = state.level;
            let mastered = if level == skill.def.mastery {
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
    // Discovery doesn't look at guild rank: a skill that needs one is taught, not stumbled on.
    let facts = Facts {
        attributes,
        race: learner.race,
        skills: Some(skills),
        guild_rank: None,
    };
    let chance = DISCOVERY_CHANCE * learner.boost;
    if let Some(found) = discover(defs, &practised, &facts, dice, chance) {
        let (found, name) = (found.key.clone(), found.def.name.clone());
        learn(skills, &found, "Discovered", Some(character_id), db);
        lines.push(format!(
            "\x1b[1;35mSomething clicks into place. You have discovered {name}.\x1b[0m"
        ));
        let deed = format!("for discovering {name}");
        lines.extend(attributes.earn(
            crate::attributes::progression().discovery_experience,
            &deed,
            character_id,
            db,
        ));
    }
    lines
}

/// The skills a character knows that are practised whenever `used` is: passives triggered by
/// using it, by using anything in their own tree, like a tradition's discipline growing with its
/// powers, or by any attack, when `used` is an attack skill.
fn grows_with(defs: &SkillDefs, skills: &Skills, used: &str) -> Vec<String> {
    let Some(used_skill) = defs.skills.get(used) else {
        return Vec::new();
    };
    let tree = &used_skill.tree;
    let attack = matches!(used_skill.def.kind, SkillKind::Attack { .. });
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
                Trigger::UsedAttack => attack,
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

/// Learn a skill at level 0, if it isn't known already, saving it for `character_id` if that's a
/// player. Returns whether it was new.
pub fn learn(
    skills: &mut Skills,
    key: &str,
    via: &str,
    character_id: Option<i64>,
    db: &Database,
) -> bool {
    if skills.0.contains_key(key) {
        return false;
    }
    let state = SkillState::new(via);
    if let Some(id) = character_id
        && let Err(err) = db.save_skill(id, key, &state)
    {
        error!("failed to save new skill {key} for character {id}: {err}");
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
    fights: Query<&Outmatched, With<Fighting>>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
) {
    let mut out = Vec::new();
    for event in events.read() {
        let who = match event {
            GameEvent::StaminaSpent { who, .. }
            | GameEvent::Dodged { who }
            | GameEvent::Blocked { who }
            | GameEvent::HitInArmour { who }
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
    deliver(out, &mut outbox);
}

/// `skills`: what you know, by tree.
/// The skills Karma could unlock for a character who knows `known`: the others in the trees they
/// have, not yet known, that have a Karma cost. Requirements aren't checked here, so `karma` can
/// show what's still out of reach. Cheapest first, within each tree.
pub fn unlockable<'a>(defs: &'a SkillDefs, known: &Skills) -> Vec<&'a Skill> {
    let trees: Vec<&String> = known
        .0
        .keys()
        .filter_map(|key| defs.skills.get(key).map(|s| &s.tree))
        .collect();
    let mut found: Vec<&Skill> = defs
        .skills
        .values()
        .filter(|s| s.def.karma.is_some())
        .filter(|s| trees.contains(&&s.tree) && !known.0.contains_key(&s.key))
        .collect();
    found.sort_by_key(|s| (s.tree.clone(), s.def.karma, s.def.name.clone()));
    found
}

/// A skill's requirements as players read them: "Swordplay 15".
fn requirement_text(defs: &SkillDefs, condition: &Condition) -> String {
    match condition {
        Condition::Skill(key, level) => {
            let name = defs
                .skills
                .get(key)
                .map_or(key.as_str(), |s| s.def.name.as_str());
            format!("{name} {level}")
        }
        other => other.describe(&|id| id.to_string()),
    }
}

/// `karma [<skill>]`: list what Karma could unlock in your trees, or unlock one. It's learned at
/// level 0, as if discovered, and the Karma is gone for good.
fn karma_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    defs: Res<SkillDefs>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    db: Res<Database>,
    shown: Query<&crate::guild::ShownRank>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let Ok((mut skills, mut attributes)) = sheets.get_mut(inv.entity) else {
        return;
    };
    let facts = Facts {
        attributes: &attributes,
        race: &actor.race,
        skills: Some(&skills),
        guild_rank: crate::guild::shown_rank(inv.entity, &shown),
    };
    let unmet = |skill: &Skill| {
        skill
            .def
            .requires
            .as_ref()
            .filter(|c| !c.met(&facts))
            .map(|c| requirement_text(&defs, c))
    };
    let karma = attributes.karma_unspent();
    let options = unlockable(&defs, &skills);
    let wanted = inv.args.trim().to_lowercase();
    let text = if wanted.is_empty() {
        if options.is_empty() {
            "There's nothing in your trees that Karma can unlock.".to_string()
        } else {
            let mut text = format!("Karma can unlock, in your trees (you have {karma} Karma):");
            let mut tree = None;
            for skill in &options {
                if tree != Some(&skill.tree) {
                    let name = defs.trees.get(&skill.tree).unwrap_or(&skill.tree);
                    text += &format!("\r\n  {name}:");
                    tree = Some(&skill.tree);
                }
                let cost = skill.def.karma.unwrap_or_default();
                let needs = unmet(skill)
                    .map(|req| format!(", requires {req}"))
                    .unwrap_or_default();
                text += &format!("\r\n    {} ({cost} Karma{needs})", skill.def.name);
            }
            text + "\r\nType 'karma <skill>' to spend the Karma."
        }
    } else {
        let found = options.iter().find(|s| {
            let name = s.def.name.to_lowercase();
            s.key == wanted
                || name.starts_with(&wanted)
                || name.split_whitespace().any(|w| w.starts_with(&wanted))
        });
        match found {
            None => format!(
                "Karma can't unlock '{}' for you. Type 'karma' to see what it can.",
                inv.args.trim()
            ),
            Some(skill) => {
                let cost = skill.def.karma.unwrap_or_default();
                if let Some(req) = unmet(skill) {
                    format!("{} requires {req} first.", skill.def.name)
                } else if !attributes.spend_on_skill(cost as i32) {
                    format!(
                        "Unlocking {} costs {cost} Karma, and you have {karma}.",
                        skill.def.name
                    )
                } else {
                    learn(&mut skills, &skill.key, "Karma", actor.character_id, &db);
                    if let Some(state) = skills.0.get_mut(&skill.key) {
                        state.karma_paid = cost;
                        if let Some(id) = actor.character_id
                            && let Err(err) = db.save_skill(id, &skill.key, state)
                        {
                            error!(
                                "failed to save {}'s Karma for {}: {err}",
                                actor.name, skill.key
                            );
                        }
                    }
                    format!(
                        "You spend {cost} Karma, and {} falls into place, as if you'd always known \
                         it.\r\n\x1b[1;32mYou have learned {}.\x1b[0m ({} Karma left.)",
                        skill.def.name,
                        skill.def.name,
                        attributes.karma_unspent()
                    )
                }
            }
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

fn skills_cmd(
    In(inv): In<Invocation>,
    mut outbox: ResMut<Outbox>,
    sheets: Query<(&Skills, &Attributes)>,
    defs: Res<SkillDefs>,
    time: Res<Time>,
) {
    let Ok((skills, attributes)) = sheets.get(inv.entity) else {
        return;
    };
    let wanted = inv.args.trim();
    if !wanted.is_empty() {
        let text = match defs.find_known(skills, wanted) {
            Some(skill) => {
                let tree = defs.trees.get(&skill.tree).cloned().unwrap_or_default();
                let level = skills.0.get(&skill.key).map_or(0, |s| s.level);
                let mastered = if level >= skill.def.mastery {
                    ", mastered".to_string()
                } else {
                    format!(", mastered at {}", skill.def.mastery)
                };
                format!(
                    "\x1b[1m{}\x1b[0m ({tree}), level {level}{mastered}\r\n{}",
                    skill.def.name, skill.def.description
                )
            }
            None => format!("You don't know a skill called '{wanted}'."),
        };
        deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
        return;
    }
    let text = skills_text(
        skills,
        attributes,
        &defs,
        unix_now(),
        time.elapsed_secs_f64(),
    );
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
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
    let level = attributes.level();
    let mut text = format!(
        "Your skills (character level {level}, {} experience; skills grow freely to {}):",
        attributes.experience(),
        soft_cap(level)
    );
    for (tree, list) in by_tree {
        text += &format!("\r\n  \x1b[1m{tree}\x1b[0m");
        for (skill, state) in list {
            let practice = faded(state.practice, state.last_used.map_or(0, |at| now - at));
            let resting = if state.next_gain_at > now {
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
            let mastered = if state.level >= skill.def.mastery {
                " (mastered)"
            } else {
                ""
            };
            text += &format!(
                "\r\n    {:<22}{:>4} {} +{} {}{resting}{kind}{mastered}",
                skill.def.name,
                state.level,
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
#[allow(clippy::too_many_arguments)]
fn use_cmd(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    sheets: Query<&Skills>,
    defs: Res<SkillDefs>,
    registry: Res<CommandRegistry>,
    (known_spells, spell_defs): (Query<&Spells>, Res<SpellDefs>),
    (conditions, mut events): (Query<&Conditions>, MessageWriter<GameEvent>),
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let name = inv.args.split_whitespace().next().unwrap_or("");
    // A spell is cast, but 'use' finds it too: it goes to 'cast', showing a hidden caster as
    // casting does.
    let spell = known_spells
        .get(inv.entity)
        .ok()
        .and_then(|known| spell_defs.find_known_in(known, &inv.args));
    let reply: Reply = match sheets.get(inv.entity) {
        _ if name.is_empty() => Err(inv.usage_error()),
        Ok(skills) => match defs.find_known_in(skills, &inv.args) {
            None if spell.is_some() => match registry.find("cast", actor.role) {
                Some((spec, id)) => {
                    if conditions
                        .get(inv.entity)
                        .is_ok_and(|c| c.hidden().is_some())
                    {
                        reveal(
                            &mut commands,
                            &mut events,
                            actor.entity,
                            &actor.name,
                            actor.room,
                        );
                    }
                    commands.run_system_with(
                        *id,
                        Invocation {
                            entity: inv.entity,
                            command: spec.name,
                            usage: spec.usage,
                            args: inv.args.clone(),
                        },
                    );
                    return;
                }
                None => Err(format!("You don't know a skill called '{name}'.")),
            },
            None => Err(format!("You don't know a skill called '{name}'.")),
            Some((skill, target)) => match &skill.def.kind {
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
                // One like nothing else has its own command; the rest are their effects, done by
                // `ability`, which shows a hidden user as casting does.
                SkillKind::Active {
                    command,
                    aim,
                    effects,
                    quiet,
                    ..
                } => {
                    let (verb, args) = match (command, aim) {
                        (Some(command), _) => (command.as_str(), target.trim().to_string()),
                        (None, Aim::Lock) => {
                            ("lockwork", format!("{} {}", skill.key, target.trim()))
                        }
                        // The room's tracks are `trackwork`'s; what's found there `roomwork`'s.
                        (None, Aim::Room) if effects::in_a_room(effects) && !finds(effects) => {
                            ("trackwork", format!("{} {}", skill.key, target.trim()))
                        }
                        (None, Aim::Room) => ("roomwork", skill.key.clone()),
                        (None, Aim::Item) => {
                            ("itemwork", format!("{} {}", skill.key, target.trim()))
                        }
                        (None, _) => ("ability", format!("{} {}", skill.key, target.trim())),
                    };
                    match registry.find(verb, actor.role) {
                        Some((spec, id)) => {
                            if command.is_none()
                                && !quiet
                                && conditions
                                    .get(inv.entity)
                                    .is_ok_and(|c| c.hidden().is_some())
                            {
                                reveal(
                                    &mut commands,
                                    &mut events,
                                    actor.entity,
                                    &actor.name,
                                    actor.room,
                                );
                            }
                            commands.run_system_with(
                                *id,
                                Invocation {
                                    entity: inv.entity,
                                    command: spec.name,
                                    usage: spec.usage,
                                    args,
                                },
                            );
                            return;
                        }
                        None => Err(format!("You can't put {} to use yet.", skill.def.name)),
                    }
                }
            },
        },
        Err(_) => return,
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut outbox);
}

/// `grantskill <player> <tree:skill> [<level>]`: an admin teaches a skill or sets its level.
fn grantskill_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    keywords: Query<&Keywords>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    defs: Res<SkillDefs>,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    let reply = grant_skill(&inv, &players, &keywords, &mut sheets, &defs, &db, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut outbox);
}

fn grant_skill(
    inv: &Invocation,
    players: &Characters,
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
                .ok_or("A level is a whole number, 0 or more.")?,
        ),
    };
    let target = Target::parse(target).ok_or_else(|| inv.usage_error())?;
    let actor = target
        .find_one(inv.entity, targets_online(players, keywords))
        .and_then(|e| Actor::get(players, e))
        .ok_or_else(|| format!("No one matching {} is online.", target.describe()))?;
    let (mut skills, _) = sheets
        .get_mut(actor.entity)
        .map_err(|_| format!("{} has no skills loaded.", actor.name))?;
    let new = learn(&mut skills, &key, "Admin", actor.character_id, db);
    if let Some(level) = level
        && let Some(state) = skills.0.get_mut(&key)
    {
        state.level = level;
        if let Some(id) = actor.character_id
            && let Err(err) = db.save_skill(id, &key, state)
        {
            error!("failed to save {key} for {}: {err}", actor.name);
        }
    }
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
    #[error("skill {0}")]
    Incomplete(String),
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
        for (id, skill) in &tree.skills {
            skill.check().map_err(|err| {
                SkillTreeLoaderError::Incomplete(format!("{}:{id} {err}", tree.id))
            })?;
        }
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
        // A skill at level 10, within its soft cap: 18% base, 4% cold, 16% after twenty uses.
        assert!(close(gain_chance(10, 1.0, 1.0, 10), 0.18));
        assert!(close(gain_chance(10, 1.0, 0.0, 10), 0.036));
        let mut practice = 0.0;
        for _ in 0..20 {
            practice += 0.1 * (1.0 - practice);
        }
        assert!(close(practice, 0.88), "{practice}");
        assert!(close(gain_chance(10, 1.0, practice, 10), 0.16));
        // No cap: a skill past mastery still improves, if slowly.
        assert!(gain_chance(100, 1.0, 1.0, 1000) > 0.04);
    }

    #[test]
    fn skills_slow_past_the_soft_cap() {
        // 10 + 2 × character level.
        assert_eq!(soft_cap(0), 10);
        assert_eq!(soft_cap(20), 50);
        // Ten levels over a cap of 10: a quarter of the chance it would have under a higher cap.
        let free = gain_chance(20, 1.0, 1.0, 50);
        assert!(close(gain_chance(20, 1.0, 1.0, 10), free / 4.0));
        assert_eq!(gain_chance(10, 1.0, 1.0, 10), gain_chance(10, 1.0, 1.0, 50));
    }

    #[test]
    fn practice_halves_every_half_life() {
        // However long `progression.ron` says that is.
        let half_life = crate::attributes::progression().practice_half_life_minutes * 60.0;
        assert!(close(faded(0.88, half_life as i64), 0.44));
        assert!(close(faded(0.88, (half_life * 4.0) as i64), 0.055));
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
        assert!(roll_skill_gain(&mut state, 1.0, 50, 1000, |_| true));
        assert_eq!(state.level, 1);
        assert_eq!(state.next_gain_at, 1300);
        assert!(close(state.practice, 0.1));
        // On cooldown: no roll, but practice still builds.
        let rolled = std::cell::Cell::new(false);
        assert!(!roll_skill_gain(&mut state, 1.0, 50, 1010, |_| {
            rolled.set(true);
            true
        }));
        assert!(!rolled.get());
        assert_eq!(state.level, 1);
        assert!(state.practice > 0.18);
        // After the cooldown it can gain again.
        assert!(roll_skill_gain(&mut state, 1.0, 50, 1300, |_| true));
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
                description: String::new(),
                text: None,
                kind: SkillKind::Passive {
                    triggers: Vec::new(),
                    counter: None,
                    reactions: Vec::new(),
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
                mastery: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires: None,
                karma: None,
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
        // Every level counts: 23 levels at a point per 5 is 4.6, and at one per 10 on top of a
        // point to start, 3.3.
        let boosts = boosts(&skills, &defs);
        assert!(close(boosts.max_health, 4.6));
        assert!(close(boosts.attack_with("swordplay:basics"), 3.3));
        assert_eq!(boosts.attack_with("axes:basics"), 0.0);
    }

    #[test]
    fn the_best_matching_resistance_counts() {
        let boosts = Boosts {
            resist: HashMap::from([("sleep".into(), 5.0), ("mind".into(), 2.0)]),
            ..Boosts::default()
        };
        let tags = |list: &[&str]| list.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        assert_eq!(boosts.resisting(&tags(&["mind", "sleep"])), 5.0, "not both");
        assert_eq!(boosts.resisting(&tags(&["mind"])), 2.0);
        assert_eq!(boosts.resisting(&tags(&["fire"])), 0.0);
    }

    /// A skill in `tree` requiring `requires`.
    fn tree_skill(key: &str, requires: Option<Condition>) -> (String, Skill) {
        let skill = Skill {
            key: key.into(),
            tree: key.split_once(':').unwrap().0.into(),
            attribute: Attribute::Strength,
            def: SkillDef {
                name: key.into(),
                description: String::new(),
                text: None,
                kind: SkillKind::Passive {
                    triggers: Vec::new(),
                    counter: None,
                    reactions: Vec::new(),
                    bonuses: Vec::new(),
                },
                mastery: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires,
                karma: None,
            },
        };
        (key.into(), skill)
    }

    #[test]
    fn karma_unlocks_priced_skills_in_trees_you_have() {
        let at = |level| Some(Condition::Skill("axes:basics".into(), level));
        let priced = |key: &str, karma: u32| {
            let (key, mut skill) = tree_skill(key, at(15));
            skill.def.karma = Some(karma);
            (key, skill)
        };
        let defs = SkillDefs {
            skills: HashMap::from([
                tree_skill("axes:basics", None),
                priced("axes:cleave", 12),
                priced("axes:hew", 8),
                // A secret: no price, so Karma can't reach it.
                tree_skill("axes:secret", at(15)),
                tree_skill("swordplay:basics", None),
                priced("swordplay:lunge", 8),
            ]),
            trees: HashMap::new(),
        };
        let mut known = Skills::default();
        assert!(
            unlockable(&defs, &known).is_empty(),
            "no trees, nothing to unlock"
        );
        known
            .0
            .insert("axes:basics".into(), SkillState::new("Taught"));
        let keys = |known: &Skills| -> Vec<String> {
            unlockable(&defs, known)
                .iter()
                .map(|s| s.key.clone())
                .collect()
        };
        // Only the axe tree's priced skills, cheapest first; not Swordplay's, which they don't have.
        assert_eq!(keys(&known), ["axes:hew", "axes:cleave"]);
        known.0.insert("axes:hew".into(), SkillState::new("Karma"));
        assert_eq!(keys(&known), ["axes:cleave"]);
        let facts = Facts {
            attributes: &Attributes::default(),
            race: "human",
            skills: Some(&known),
            guild_rank: None,
        };
        let cleave = &defs.skills["axes:cleave"];
        assert!(
            !cleave.def.requires.as_ref().unwrap().met(&facts),
            "still needs Axes 15"
        );
    }

    #[test]
    fn a_karma_price_is_only_for_skills_with_requirements() {
        let (_, mut entry) = tree_skill("axes:basics", None);
        entry.def.description = "Axes.".into();
        assert_eq!(entry.def.check(), Ok(()));
        entry.def.karma = Some(5);
        assert!(entry.def.check().unwrap_err().contains("entry skills"));
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
            guild_rank: None,
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
            triggers: vec![Trigger::UsedTree, Trigger::UsedAttack],
            counter: None,
            reactions: Vec::new(),
            bonuses: Vec::new(),
        };
        // Attacks, armed and not.
        let attack = |key: &str| {
            let (key, mut skill) = tree_skill(key, None);
            skill.def.kind = SkillKind::Attack {
                damage: Some((1, 2)),
                cost: Cost::Free,
                damage_type: "physical".into(),
            };
            (key, skill)
        };
        let defs = SkillDefs {
            skills: HashMap::from([
                (key, discipline),
                tree_skill("adept:killing_hands", None),
                attack("brawling:basics"),
                attack("swordplay:basics"),
                tree_skill("athletics:endurance", None),
            ]),
            trees: HashMap::new(),
        };
        let mut skills = Skills::default();
        for key in [
            "adept:foundation",
            "adept:killing_hands",
            "brawling:basics",
            "swordplay:basics",
            "athletics:endurance",
        ] {
            skills.0.insert(key.into(), SkillState::new("Taught"));
        }
        let (mut attributes, mut dice) = (Attributes::default(), Dice::seeded(3));
        let db = Database::open(":memory:").unwrap();
        let learner = Learner {
            character_id: Some(1),
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
        // So does every attack, a punch or a sword's swing, but not other trees' other skills.
        use_skill(&mut skills, "brawling:basics");
        let after_brawl = practice(&skills, "adept:foundation");
        assert!(after_brawl > after_power);
        use_skill(&mut skills, "swordplay:basics");
        let after_sword = practice(&skills, "adept:foundation");
        assert!(after_sword > after_brawl, "a sword trains it too");
        use_skill(&mut skills, "athletics:endurance");
        assert_eq!(practice(&skills, "adept:foundation"), after_sword);
        // Nor does the discipline count itself twice.
        let mut alone = Skills::default();
        alone
            .0
            .insert("adept:foundation".into(), SkillState::new("Taught"));
        use_skill(&mut alone, "adept:foundation");
        assert!((practice(&alone, "adept:foundation") - 0.1).abs() < 1e-9);
    }

    #[test]
    fn skills_that_show_in_play_must_say_how() {
        let (_, mut skill) = tree_skill("brawling:basics", None);
        skill.def.description = "Fists.".into();
        // A passive with nothing to show needs only its description.
        assert!(skill.def.check().is_ok());
        skill.def.kind = SkillKind::Attack {
            damage: Some((1, 3)),
            cost: Cost::Free,
            damage_type: "physical".into(),
        };
        assert!(skill.def.check().unwrap_err().contains("needs text"));
        let lines = |target: Option<&str>| Lines {
            actor: "Your punch {harm} {target}.".into(),
            target: target.map(String::from),
            room: "{actor}'s punch {harm} {target}.".into(),
            beyond: None,
        };
        // A hit that hurts must say how much.
        skill.def.text = Some(SkillText {
            hit: lines(Some("{actor} punches you.")),
            miss: None,
        });
        assert!(skill.def.check().unwrap_err().contains("{harm}"));
        let hurts = Some("{actor}'s punch {harm} you.");
        skill.def.text = Some(SkillText {
            hit: lines(hurts),
            miss: None,
        });
        assert!(skill.def.check().unwrap_err().contains("miss"));
        skill.def.text = Some(SkillText {
            hit: lines(hurts),
            miss: Some(lines(None)),
        });
        assert!(skill.def.check().unwrap_err().contains("miss.target"));
        skill.def.text = Some(SkillText {
            hit: lines(hurts),
            miss: Some(lines(Some("{actor} swings at you, but misses."))),
        });
        assert!(skill.def.check().is_ok());
        skill.def.description.clear();
        assert!(skill.def.check().unwrap_err().contains("description"));
        // Filled in, with a capital where a name starts the line.
        assert_eq!(
            fill(
                "{actor} punches {target}.",
                &[("actor", "the guard"), ("target", "Grok")]
            ),
            "The guard punches Grok."
        );
    }

    #[test]
    fn skills_are_found_by_their_whole_name_and_the_rest_is_the_target() {
        let mut first_aid = tree_skill("healing:first_aid", None);
        first_aid.1.def.name = "First Aid".into();
        let mut kick = tree_skill("brawling:kick", None);
        kick.1.def.name = "Kick".into();
        let defs = SkillDefs {
            skills: HashMap::from([first_aid, kick]),
            trees: HashMap::new(),
        };
        let mut known = Skills::default();
        for key in ["healing:first_aid", "brawling:kick"] {
            known.0.insert(key.into(), SkillState::new("Taught"));
        }
        let found = |args: &str| {
            defs.find_known_in(&known, args)
                .map(|(skill, rest)| (skill.key.clone(), rest.to_string()))
        };
        assert_eq!(
            found("first aid grok"),
            Some(("healing:first_aid".into(), "grok".into()))
        );
        assert_eq!(
            found("first aid"),
            Some(("healing:first_aid".into(), String::new()))
        );
        assert_eq!(
            found("first grok"),
            Some(("healing:first_aid".into(), "grok".into()))
        );
        assert_eq!(
            found("kick 2.rat"),
            Some(("brawling:kick".into(), "2.rat".into()))
        );
        assert_eq!(found("dance"), None);
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
                let conditions = crate::combat::defs_on_disk();
                for (id, skill) in &tree.skills {
                    if let SkillKind::Active {
                        command: Some(command),
                        ..
                    } = &skill.kind
                    {
                        assert!(!command.is_empty(), "{id} has no command");
                    }
                    if let SkillKind::Attack { damage_type, .. } = &skill.kind {
                        assert!(
                            crate::world::damage_types().contains_key(damage_type),
                            "{} {id}: no damage type '{damage_type}'",
                            path.display()
                        );
                    }
                    // Every condition a technique or active skill leaves is a real one.
                    let effects = match &skill.kind {
                        SkillKind::Technique(t) => Some(&t.effects),
                        SkillKind::Active { effects, .. } => Some(effects),
                        _ => None,
                    };
                    if let Some(effects) = effects {
                        for key in effects.iter().filter_map(Effect::condition) {
                            assert!(
                                conditions.condition(key).is_some(),
                                "{} {id}: no condition '{key}'",
                                path.display()
                            );
                        }
                    }
                    // Its text is all there, and every placeholder is one the game fills in.
                    if let Err(err) = skill.check() {
                        panic!("{} {id} {err}", path.display());
                    }
                    let all = skill
                        .text
                        .iter()
                        .flat_map(|t| std::iter::once(&t.hit).chain(&t.miss));
                    for lines in all {
                        let templates =
                            [Some(&lines.actor), lines.target.as_ref(), Some(&lines.room)];
                        for template in templates
                            .into_iter()
                            .chain([lines.beyond.as_ref()])
                            .flatten()
                        {
                            let filled = fill(
                                template,
                                &[
                                    ("actor", "a"),
                                    ("target", "b"),
                                    ("door", "c"),
                                    ("item", "d"),
                                    ("direction", "e"),
                                    ("harm", "f"),
                                    ("deal", "g"),
                                    ("worth", "h"),
                                    ("coins", "i"),
                                ],
                            );
                            assert!(
                                !filled.contains('{'),
                                "{} {id}: unknown placeholder in {template:?}",
                                path.display()
                            );
                        }
                    }
                }
                assert!(!tree_id(&path).contains('.'), "{}", path.display());
                count += 1;
            }
        }
        assert!(count > 0);
    }
}
