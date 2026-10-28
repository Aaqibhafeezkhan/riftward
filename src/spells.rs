//! Spells: what a caster knows, cast through a school.
//!
//! Spells aren't skills. A spell is learned once (from a tradition's teacher, for now) and never
//! levels; its power is its school's: the school skill's level, plus the school's attribute, plus
//! Magic. Casting practises the school, so every spell in it grows stronger together.
//!
//! Spell books are data, one file per tradition in `assets/spells/` (`divine.spells.ron`), hot
//! reloaded like skill trees. A spell's key is `<tradition>:<id>`.
//!
//! `cast <spell> [<target>]` is an action. In a fight, the combat round casts it on the caster's
//! turn (see `combat/casting.rs`), where its damage, conditions and deaths are handled like any
//! blow. Out of one, a spell aimed at a foe starts the fight and is cast on the first turn, the
//! way an opener is; any other spell is cast at once.
//!
//! Casting costs Drain: stamina, resisted by Willpower plus the school's attribute. Resisting it
//! well halves the cost. Mobs know spells from their templates and cast them by their tactics,
//! like techniques.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::attributes::{Attribute, Attributes};
use crate::combat::{
    ConditionDefs, Conditions, Fighters, Fighting, Imbued, ItemConditions, Round, the,
};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Database;
use crate::dice::Dice;
use crate::effects::{self, Effect};
use crate::events::GameEvent;
use crate::items::{ItemQuery, ItemTemplates};
use crate::mobs::MobTemplates;
use crate::output::{Outbox, Outgoing, deliver};
use crate::skills::{SkillDefs, SkillText, Skills};
use crate::stealth::can_see;
use crate::targeting::{Keywords, Target};
use crate::vitals::Resting;
use crate::world::ron_options;

/// Asset folder holding one `.spells.ron` file per tradition.
pub const SPELLS_PATH: &str = "spells";
/// The extension spell book files use.
const SPELL_EXTENSION: &str = "spells.ron";
/// Drain is resisted against its stamina cost times this, as a fixed difficulty.
const DRAIN_DIFFICULTY: f64 = 2.0;

pub struct SpellsPlugin;

impl Plugin for SpellsPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<SpellBookDef>()
            .register_asset_loader(SpellBookLoader)
            .init_resource::<SpellDefs>()
            .add_systems(Startup, load_spell_books)
            .add_systems(Update, sync_spell_defs)
            .add_command(
                CommandSpec::new("spells", Category::Character)
                    .usage("spells [<spell>]")
                    .summary("list the spells you know, or read about one"),
                spells_cmd,
            )
            .add_command(
                CommandSpec::new("cast", Category::Interaction)
                    .shortcuts(&["c"])
                    .usage("cast <spell> [<target>]")
                    .summary("cast a spell you know: at a foe, on someone here, or on yourself")
                    .action(),
                cast,
            );
    }
}

/// A tradition's spell book file.
#[derive(Asset, TypePath, Serialize, Deserialize, Debug, Clone)]
pub struct SpellBookDef {
    /// Taken from the file name, e.g. `divine` for `spells/divine.spells.ron`.
    #[serde(skip)]
    pub id: String,
    pub spells: BTreeMap<String, SpellDef>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SpellDef {
    pub name: String,
    /// What players read about it: what it does.
    pub description: String,
    /// The school skill it's cast through (`tree:skill`), whose level it needs and gives it power.
    pub school: String,
    /// The school level it needs.
    #[serde(default)]
    pub min_level: u32,
    /// What a teacher asks to teach it, in copper. A tradition's starter spell is free.
    #[serde(default)]
    pub price: u32,
    /// Stamina it costs; resisting the Drain well halves it.
    pub drain: u32,
    pub aim: Aim,
    /// What the target resists it with; without, it can't be resisted.
    #[serde(default)]
    pub resist: Option<Resist>,
    /// What it is, beyond what its effects say (`effects::tags`), for skills that resist it:
    /// `["charm"]`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// What it does, in the one effect vocabulary (`effects.rs`). Amounts rolled get a third of
    /// the spell's power on top.
    pub effects: Vec<Effect>,
    /// What everyone sees. `{target}` is "yourself" (or "themselves", for the room) when the
    /// caster is their own target.
    pub text: SkillText,
}

/// Whom a spell is cast at.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// A foe: it starts a fight, or carries one on. Your foe, unless you name another.
    Foe,
    /// Yourself or someone here, friend or not. Yourself, unless you name another.
    Ally,
    /// Someone else here, without starting a fight. Your foe, unless you name another.
    Other,
    /// A door or a container that closes, by direction or name: Lockpicking. Active skills
    /// only, for now.
    Lock,
    /// The room the user is in: Foraging. Active skills only, for now.
    Room,
    /// An item the user can reach, carried or here: Appraisal. Active skills only, for now.
    Item,
    /// The user and no one else: Stealth's hiding.
    Me,
}

pub use crate::effects::Resist;

impl SpellDef {
    /// Whether it's complete, so nothing is forgotten. A spell that fails this keeps its whole
    /// book from loading.
    pub fn check(&self) -> Result<(), String> {
        if self.description.trim().is_empty() {
            return Err("needs a description".into());
        }
        if self.effects.is_empty() {
            return Err("needs effects: what it does".into());
        }
        crate::effects::check(&self.effects)?;
        if self.harms() && self.aim != Aim::Foe {
            return Err("does damage, so it must be aimed at a Foe".into());
        }
        if matches!(self.aim, Aim::Lock | Aim::Room | Aim::Item) {
            return Err(
                "can't be aimed at a lock, a room or an item: only active skills can, for now"
                    .into(),
            );
        }
        if effects::in_a_room(&self.effects) {
            return Err("can't Find or Reveal: only active skills can, for now".into());
        }
        if self.aim == Aim::Foe && self.resist.is_none() {
            return Err("is aimed at a Foe, so needs something to resist it".into());
        }
        let text = &self.text;
        if text.hit.target.is_none() {
            return Err("needs text for its target (hit.target)".into());
        }
        let harms = self.harms();
        let lines = [
            Some(&text.hit.actor),
            text.hit.target.as_ref(),
            Some(&text.hit.room),
        ];
        if harms
            && lines
                .into_iter()
                .flatten()
                .any(|line| !line.contains("{harm}"))
        {
            return Err("needs {harm} in every hit line, for how much it hurt".into());
        }
        match &text.miss {
            None if self.resist.is_some() => {
                Err("can be resisted, so needs text for when it is (miss)".into())
            }
            Some(lines) if lines.target.is_none() => {
                Err("needs text for its target when it's resisted (miss.target)".into())
            }
            _ => Ok(()),
        }
    }

    pub fn harms(&self) -> bool {
        self.effects
            .iter()
            .any(|e| matches!(e, Effect::Damage { .. }))
    }
}

/// A spell with its key, `tradition:spell`.
#[derive(Clone, Debug)]
pub struct Spell {
    pub key: String,
    pub def: SpellDef,
}

/// Every spell from every loaded book, by `tradition:spell`.
#[derive(Resource, Default)]
pub struct SpellDefs(pub HashMap<String, Spell>);

impl SpellDefs {
    /// The known spell named at the start of `args`, and the rest: the longest run of words that
    /// names one, so `spirit grasp rat` is Spirit Grasp on the rat.
    pub fn find_known_in<'a, 'b>(
        &'a self,
        known: &Spells,
        args: &'b str,
    ) -> Option<(&'a Spell, &'b str)> {
        let args = args.trim();
        let mut ends: Vec<usize> = args.match_indices(' ').map(|(i, _)| i).collect();
        ends.push(args.len());
        ends.into_iter().rev().find_map(|end| {
            let (name, rest) = args.split_at(end);
            let name = name.trim().to_lowercase();
            if name.is_empty() {
                return None;
            }
            let known: Vec<&Spell> = known.0.iter().filter_map(|k| self.0.get(k)).collect();
            known
                .iter()
                .find(|s| s.key == name || s.key.split_once(':').is_some_and(|(_, id)| id == name))
                .or_else(|| known.iter().find(|s| matches_name(&s.def.name, &name)))
                .map(|s| (*s, rest.trim()))
        })
    }
}

/// Whether `wanted` names a spell called `name`: its start, or the start of a word of it.
pub fn matches_name(name: &str, wanted: &str) -> bool {
    let name = name.to_lowercase();
    name.starts_with(wanted) || name.split_whitespace().any(|w| w.starts_with(wanted))
}

/// The spells someone knows, by `tradition:spell`. Players' are saved; mobs' come from their
/// templates.
#[derive(Component, Clone, Debug, Default)]
pub struct Spells(pub BTreeSet<String>);

/// A spell made ready to cast on the caster's next turn in a fight, at `target`.
#[derive(Component, Debug, Clone)]
pub struct Casting {
    pub spell: String,
    pub target: Entity,
    pub ready_at: f64,
}

/// A spell's power for a caster: their school's level, plus its attribute, plus Magic.
pub fn power(spell: &SpellDef, skills: &Skills, attributes: &Attributes, defs: &SkillDefs) -> i32 {
    let level = skills.0.get(&spell.school).map_or(0, |s| s.level as i32);
    let attribute = defs
        .skills
        .get(&spell.school)
        .map_or(0, |s| attributes.get(s.attribute));
    level + attribute + attributes.get(Attribute::Magic)
}

/// What resists a caster's Drain: Willpower plus their school's attribute.
pub fn drain_resistance(spell: &SpellDef, attributes: &Attributes, defs: &SkillDefs) -> i32 {
    let attribute = defs
        .skills
        .get(&spell.school)
        .map_or(0, |s| attributes.get(s.attribute));
    attributes.get(Attribute::Willpower) + attribute
}

/// The stamina a cast takes: half, rounded up, when the Drain is resisted.
pub fn drain_paid(dice: &mut Dice, spell: &SpellDef, resistance: i32, luck: i32) -> u32 {
    let difficulty = f64::from(spell.drain) * DRAIN_DIFFICULTY;
    let held = spell.drain == 0
        || dice
            .opposed(f64::from(resistance.max(1)), luck, difficulty, 0)
            .won;
    if held {
        spell.drain.div_ceil(2)
    } else {
        spell.drain
    }
}

/// Whether a caster with these skills, this much stamina and these conditions can cast `spell`
/// now; if not, why.
pub fn can_cast(
    spell: &SpellDef,
    skills: &Skills,
    stamina: f32,
    defs: &SkillDefs,
    (conditions, condition_defs): (&Conditions, &ConditionDefs),
) -> Result<(), String> {
    if !conditions.can_cast(condition_defs) {
        return Err(format!(
            "You can't cast {}: something stops the words.",
            spell.name
        ));
    }
    let school = defs
        .skills
        .get(&spell.school)
        .map_or(spell.school.as_str(), |s| s.def.name.as_str());
    match skills.0.get(&spell.school) {
        None => Err(format!(
            "You can't cast {}: it's cast through {school}, which you don't know.",
            spell.name
        )),
        Some(state) if state.level < spell.min_level => Err(format!(
            "Your {school} isn't strong enough to cast {} yet (it needs {}).",
            spell.name, spell.min_level
        )),
        _ if stamina < spell.drain as f32 => {
            Err(format!("You're too drained to cast {}.", spell.name))
        }
        _ => Ok(()),
    }
}

/// Whether `spell` can take hold of `target` at all: only animals can be calmed.
pub fn takes_hold(spell: &SpellDef, target_skills: &Skills) -> bool {
    !spell.effects.contains(&Effect::Calm) || is_beast(target_skills)
}

/// An animal: something that fights with the Beast tree's natural attacks.
pub fn is_beast(skills: &Skills) -> bool {
    skills.0.keys().any(|key| key.starts_with("beast:"))
}

/// Whom a spell is cast at: the one `words` name among `candidates` (everyone here the caster
/// can see), or by default the caster's `foe` or the caster themselves, as its aim says.
/// `player` says who's a player: no spell is aimed at one as a foe.
pub fn choose_target(
    spell: &SpellDef,
    caster: Entity,
    foe: Option<Entity>,
    words: &str,
    candidates: Vec<(Entity, &Keywords)>,
    player: impl Fn(Entity) -> bool,
) -> Result<Entity, String> {
    let aimed = Aimed {
        aim: spell.aim,
        name: &spell.name,
        verb: "cast",
        at: "at",
    };
    aimed.choose(caster, foe, words, candidates, player)
}

/// Something aimed at someone, a spell or an active skill: how, and the words for asking whom.
pub struct Aimed<'a> {
    pub aim: Aim,
    pub name: &'a str,
    /// "cast", "use".
    pub verb: &'a str,
    /// "at", "on": "Cast Spark at whom?", "Use First Aid on whom?"
    pub at: &'a str,
}

impl Aimed<'_> {
    /// The one `words` name among `candidates`, or by default the user's `foe` or the user, as
    /// its aim says. `player` says who's a player: nothing is aimed at one as a foe.
    pub fn choose(
        &self,
        caster: Entity,
        foe: Option<Entity>,
        words: &str,
        candidates: Vec<(Entity, &Keywords)>,
        player: impl Fn(Entity) -> bool,
    ) -> Result<Entity, String> {
        let (verb, at, name) = (self.verb, self.at, self.name);
        let words = words.trim();
        let named = if words.is_empty() {
            None
        } else {
            Some(
                Target::parse(words)
                    .and_then(|t| t.find_one(caster, candidates))
                    .ok_or_else(|| format!("You don't see {words} here."))?,
            )
        };
        let target = match (self.aim, named) {
            (Aim::Me, Some(target)) if target != caster => {
                return Err(format!("You can only {verb} {name} {at} yourself."));
            }
            (Aim::Me, _) => caster,
            (_, Some(target)) => target,
            (Aim::Ally, None) => caster,
            (Aim::Foe | Aim::Other, None) => {
                foe.ok_or_else(|| format!("{} {name} {at} whom?", crate::items::capitalized(verb)))?
            }
            // Locks are found among doors and containers, not people.
            (Aim::Lock | Aim::Room | Aim::Item, None) => {
                return Err(format!("You can't {verb} {name} {at} someone."));
            }
        };
        match self.aim {
            Aim::Foe | Aim::Other if target == caster => {
                Err(format!("You can't {verb} {name} {at} yourself."))
            }
            Aim::Foe if player(target) && player(caster) => {
                Err("You can't attack other players.".into())
            }
            _ => Ok(target),
        }
    }
}

/// `spells [<spell>]`: the spells you know, by school, or about one of them.
fn spells_cmd(
    In(inv): In<Invocation>,
    mut outbox: ResMut<Outbox>,
    sheets: Query<&Spells>,
    spells: Res<SpellDefs>,
    defs: Res<SkillDefs>,
) {
    let known = sheets.get(inv.entity).cloned().unwrap_or_default();
    let school_name = |key: &str| {
        defs.skills
            .get(key)
            .map_or(key.to_string(), |s| s.def.name.clone())
    };
    let describe = |spell: &Spell| {
        let level = match spell.def.min_level {
            0 => String::new(),
            n => format!(" {n}"),
        };
        format!(
            "{} ({}{level}, drain {})",
            spell.def.name,
            school_name(&spell.def.school),
            spell.def.drain
        )
    };
    let text = if inv.args.trim().is_empty() {
        let mut by_school: BTreeMap<String, Vec<&Spell>> = BTreeMap::new();
        for spell in known.0.iter().filter_map(|key| spells.0.get(key)) {
            by_school
                .entry(school_name(&spell.def.school))
                .or_default()
                .push(spell);
        }
        if by_school.is_empty() {
            "You don't know any spells.".to_string()
        } else {
            let mut text = String::from("You know these spells:");
            for (school, list) in by_school {
                text += &format!("\r\n  {school}:");
                for spell in list {
                    text += &format!("\r\n    {}", describe(spell));
                }
            }
            text + "\r\nType 'spells <spell>' to read about one, and 'cast <spell>' to cast it."
        }
    } else {
        match spells.find_known_in(&known, &inv.args) {
            Some((spell, "")) => format!("{}\r\n  {}", describe(spell), spell.def.description),
            _ => format!("You don't know a spell called '{}'.", inv.args.trim()),
        }
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// `cast <spell> [<target>]`: cast a spell. Aimed at a foe out of a fight, it starts one, cast on
/// the first turn; any other spell out of a fight is cast now. In a fight, the combat round casts
/// it on the caster's turn; if this runs then anyway, it's readied for that turn.
#[allow(clippy::too_many_arguments)]
fn cast(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut fighters: Fighters,
    items: ItemQuery,
    (templates, mobs): (Res<ItemTemplates>, Res<MobTemplates>),
    defs: Res<SkillDefs>,
    spells: Res<SpellDefs>,
    mut dice: ResMut<Dice>,
    db: Res<Database>,
    mut events: MessageWriter<GameEvent>,
    mut log: ResMut<Outbox>,
    resting: Query<Entity, With<Resting>>,
    (time, condition_defs, story): (Res<Time>, Res<ConditionDefs>, Res<crate::facts::Facts>),
    mut item_conditions: Query<(Entity, &mut ItemConditions)>,
) {
    let Ok(caster) = fighters.get(inv.entity) else {
        return;
    };
    let room = caster.in_room.0;
    let fighting = caster.fighting.map(|f| f.0);
    let known = caster.spells.cloned().unwrap_or_default();
    let said = |log: &mut Outbox, text: String| log.0.push(Outgoing::line(inv.entity, text));
    if inv.args.trim().is_empty() {
        return said(
            &mut log,
            "Cast what? Type 'spells' to see what you know.".into(),
        );
    }
    let Some((spell, words)) = spells.find_known_in(&known, &inv.args) else {
        return said(
            &mut log,
            format!("You don't know a spell called '{}'.", inv.args.trim()),
        );
    };
    if let Err(refusal) = can_cast(
        &spell.def,
        caster.skills,
        caster.vitals.stamina,
        &defs,
        (caster.conditions, &condition_defs),
    ) {
        return said(&mut log, refusal);
    }
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
    let target = match choose_target(&spell.def, inv.entity, fighting, words, candidates, |e| {
        players.contains(&e)
    }) {
        Ok(target) => target,
        Err(refusal) => return said(&mut log, refusal),
    };
    let target_name = fighters
        .get(target)
        .map(|f| crate::combat::name_of(f.player, f.mob))
        .unwrap_or_default();
    if let Ok(f) = fighters.get(target)
        && !takes_hold(&spell.def, f.skills)
    {
        return said(
            &mut log,
            format!(
                "{} isn't an animal.",
                crate::items::capitalized(&the(&target_name))
            ),
        );
    }
    let casting = Casting {
        spell: spell.key.clone(),
        target,
        ready_at: time.elapsed_secs_f64(),
    };
    if fighting.is_some() {
        commands.entity(inv.entity).insert(casting);
        return said(
            &mut log,
            format!("You gather yourself to cast {}.", spell.def.name),
        );
    }
    if spell.def.aim == Aim::Foe {
        commands
            .entity(inv.entity)
            .insert((Fighting(target), casting))
            .remove::<Resting>();
        return said(
            &mut log,
            format!(
                "You turn on {}, gathering yourself to cast {}.",
                the(&target_name),
                spell.def.name
            ),
        );
    }

    // Anything else is cast now.
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
    let outcome = round.cast(inv.entity, target, &spell.key);
    imbued.write_back(&mut item_conditions);
    if let Some(calmed) = outcome.calmed {
        crate::combat::calm(&mut commands, calmed, &targets);
    }
}

// Loading spell books, like skill trees.

#[derive(Resource)]
struct SpellBooksHandle(#[allow(dead_code)] Handle<LoadedFolder>);

fn load_spell_books(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(SpellBooksHandle(assets.load_folder(SPELLS_PATH)));
}

/// Rebuild the spell table whenever a book loads, changes or goes away.
fn sync_spell_defs(
    mut events: MessageReader<AssetEvent<SpellBookDef>>,
    books: Res<Assets<SpellBookDef>>,
    mut defs: ResMut<SpellDefs>,
) {
    if events.read().count() == 0 {
        return;
    }
    defs.0.clear();
    for (_, book) in books.iter() {
        for (id, def) in &book.spells {
            let key = format!("{}:{id}", book.id);
            defs.0.insert(
                key.clone(),
                Spell {
                    key,
                    def: def.clone(),
                },
            );
        }
    }
    info!("loaded {} spells", defs.0.len());
}

#[derive(TypePath)]
struct SpellBookLoader;

#[derive(Debug, Error)]
enum SpellBookLoaderError {
    #[error("could not read spell book: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse spell book: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("spell {0}")]
    Incomplete(String),
}

impl AssetLoader for SpellBookLoader {
    type Asset = SpellBookDef;
    type Settings = ();
    type Error = SpellBookLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<SpellBookDef, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut book: SpellBookDef = ron_options().from_bytes(&bytes)?;
        book.id = book_id(load_context.path().path());
        for (id, spell) in &book.spells {
            spell.check().map_err(|err| {
                SpellBookLoaderError::Incomplete(format!("{}:{id} {err}", book.id))
            })?;
        }
        Ok(book)
    }

    fn extensions(&self) -> &[&str] {
        &[SPELL_EXTENSION]
    }
}

/// A book's id from its file: `spells/divine.spells.ron` is `divine`.
fn book_id(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(&format!(".{SPELL_EXTENSION}")))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::Formula;

    /// Every spell book in `assets/spells` parses, and every spell in it is complete and cast
    /// through a school that exists.
    #[test]
    fn spell_books_are_complete() {
        let mut schools = HashSet::new();
        for entry in std::fs::read_dir("assets/skills").unwrap() {
            let path = entry.unwrap().path();
            let tree: crate::skills::SkillTreeDef = ron_options()
                .from_bytes(&std::fs::read(&path).unwrap())
                .unwrap();
            let id = path
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .trim_end_matches(".skill.ron")
                .to_string();
            schools.extend(tree.skills.keys().map(|skill| format!("{id}:{skill}")));
        }
        let mut count = 0;
        for entry in std::fs::read_dir(SPELLS_PATH_ON_DISK).unwrap() {
            let path = entry.unwrap().path();
            let book: SpellBookDef = ron_options()
                .from_bytes(&std::fs::read(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            for (id, spell) in &book.spells {
                for effect in &spell.effects {
                    if let Some(damage_type) = effect.damage_type() {
                        assert!(
                            crate::world::damage_types().contains_key(damage_type),
                            "{id}: no damage type '{damage_type}'"
                        );
                    }
                }
                // Every condition it puts on someone is a real one.
                let conditions = crate::combat::defs_on_disk();
                for key in spell.effects.iter().filter_map(Effect::condition) {
                    assert!(
                        conditions.condition(key).is_some(),
                        "{id}: no condition '{key}'"
                    );
                }
                spell
                    .check()
                    .unwrap_or_else(|err| panic!("{}: {id} {err}", path.display()));
                assert!(
                    schools.contains(&spell.school),
                    "{id}'s school {} isn't a skill",
                    spell.school
                );
                count += 1;
            }
        }
        assert!(count >= 6, "the first two spells of each tradition");
    }

    const SPELLS_PATH_ON_DISK: &str = "assets/spells";

    fn lines(text: &str) -> crate::skills::Lines {
        crate::skills::Lines {
            actor: text.into(),
            target: Some(text.into()),
            room: text.into(),
            beyond: None,
        }
    }

    fn bolt() -> SpellDef {
        SpellDef {
            name: "Bolt".into(),
            description: "A bolt.".into(),
            school: "hermetic:foundation".into(),
            min_level: 0,
            price: 0,
            drain: 2,
            aim: Aim::Foe,
            resist: Some(Resist::Dodge),
            tags: Vec::new(),
            effects: vec![Effect::Damage {
                amount: Formula::parse("roll(1, 2)").unwrap(),
                damage_type: "force".into(),
            }],
            text: SkillText {
                hit: lines("Your bolt {harm} {target}."),
                miss: Some(lines("Your bolt misses {target}.")),
            },
        }
    }

    #[test]
    fn a_spell_must_say_what_it_does_and_be_aimed_sensibly() {
        assert_eq!(bolt().check(), Ok(()));
        let mut quiet = bolt();
        quiet.text.hit = lines("Your bolt strikes {target}.");
        assert!(quiet.check().unwrap_err().contains("{harm}"));
        let mut friendly = bolt();
        friendly.aim = Aim::Ally;
        assert!(friendly.check().unwrap_err().contains("Foe"));
        let mut sure = bolt();
        sure.resist = None;
        assert!(sure.check().is_err(), "a foe must have some way to resist");
        let mut unanswered = bolt();
        unanswered.text.miss = None;
        assert!(unanswered.check().unwrap_err().contains("miss"));
    }

    #[test]
    fn a_spell_goes_where_its_aim_says_unless_told_otherwise() {
        let mut world = World::new();
        let (me, rat, pal) = (
            world.spawn_empty().id(),
            world.spawn_empty().id(),
            world.spawn_empty().id(),
        );
        let (rat_words, pal_words) = (Keywords(vec!["rat".into()]), Keywords(vec!["pal".into()]));
        let here = || vec![(rat, &rat_words), (pal, &pal_words)];
        let players = |e: Entity| e == me || e == pal;
        let mut mend = bolt();
        mend.aim = Aim::Ally;
        assert_eq!(
            choose_target(&mend, me, Some(rat), "", here(), players),
            Ok(me)
        );
        assert_eq!(
            choose_target(&mend, me, None, "pal", here(), players),
            Ok(pal)
        );
        let spark = bolt();
        assert_eq!(
            choose_target(&spark, me, Some(rat), "", here(), players),
            Ok(rat)
        );
        assert!(choose_target(&spark, me, None, "", here(), players).is_err());
        assert!(
            choose_target(&spark, me, None, "pal", here(), players).is_err(),
            "no PvP"
        );
        assert!(choose_target(&spark, me, None, "dragon", here(), players).is_err());
    }

    #[test]
    fn book_ids_come_from_file_names() {
        assert_eq!(
            book_id(std::path::Path::new("spells/divine.spells.ron")),
            "divine"
        );
    }
}
