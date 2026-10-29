//! Crafting: making things from other things.
//!
//! Recipes are data, one book per craft in `assets/recipes/` (`cooking.recipes.ron`), hot
//! reloaded like spell books. A book names the skill that uses it (Cooking's recipes are
//! `survival:cooking`'s), and each recipe what goes in (items or tags, so many of each), what comes
//! out, and where and with what it's made: a `station` in the room (a fire, an anvil: any item
//! tagged so), and a `tool` on the crafter, as skills' tools are. A recipe's key is
//! `<book>:<id>`.
//!
//! `craft <recipe>` is an action: the crafter's skill (level plus attribute) against the
//! recipe's `against` difficulty. Won, what it makes is better made the surer the work; lost,
//! the ingredients are ruined. Either way they're used up and the skill is practised. How well
//! it's made starts from how surely the work went, moves with the ingredients' quality (fine
//! venison, a finer roast), and stays within the recipe's `quality` range: a plain recipe may never
//! make better than fine, a rare one never worse than standard.
//!
//! Most recipes come with their skill: whoever knows it at the recipe's `level` knows them.
//! Others are `Taught`: learned from a trainer who `teaches` them (`learn`, for their `price`),
//! and saved with the character (`character_recipes`), so a rare recipe for a better blade or a
//! finer dish is something to seek out. `recipes` lists what you know.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadedFolder};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use serde::Deserialize;
use thiserror::Error;

use crate::combat::Blow;
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Database;
use crate::formula::{Formula, Side};
use crate::items::{ItemLocation, ItemQuery, ItemTemplates, Quality, make_for};
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::requirements::ItemMatch;
use crate::skills::{Learner, Practice, Rarity, fill, has_tool};
use crate::world::{ItemKey, RoomKey, ron_options};

/// Asset folder holding one `.recipes.ron` file per craft.
pub const RECIPES_PATH: &str = "recipes";
/// The extension recipe book files use.
const RECIPE_EXTENSION: &str = "recipes.ron";

pub struct CraftingPlugin;

impl Plugin for CraftingPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<RecipeBookDef>()
            .register_asset_loader(RecipeBookLoader)
            .init_resource::<Recipes>()
            .add_systems(Startup, load_recipe_books)
            .add_systems(Update, sync_recipes)
            .add_command(
                CommandSpec::new("craft", Category::Interaction)
                    .usage("craft <recipe>")
                    .summary("make something from a recipe you know, with its makings to hand")
                    .action(),
                craft,
            )
            .add_command(
                CommandSpec::new("recipes", Category::Character)
                    .usage("recipes")
                    .summary("list what your crafts can make, and what each needs"),
                recipes,
            );
    }
}

/// A craft's recipe book file: the skill that uses it, and its recipes by id.
#[derive(Asset, TypePath, Deserialize, Debug, Clone)]
pub struct RecipeBookDef {
    /// Taken from the file name, e.g. `cooking` for `recipes/cooking.recipes.ron`.
    #[serde(skip)]
    pub id: String,
    /// The skill (`tree:skill`) that makes everything in it: Cooking, Smithing.
    pub skill: String,
    pub recipes: BTreeMap<String, RecipeDef>,
}

/// One recipe: what goes in, what comes out, and where and with what it's made.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RecipeDef {
    /// What it's called, to `craft` it by: "roast venison".
    pub name: String,
    /// The skill level it's made at; 0, the default, as soon as the skill is known.
    #[serde(default)]
    pub level: u32,
    /// How it's learned: with the skill (the default), or `Taught` by a trainer.
    #[serde(default)]
    pub learned: Learned,
    /// What a trainer asks to teach it, in copper, if it's taught.
    #[serde(default)]
    pub price: u32,
    /// How rare it is: a rare recipe makes something better than most can.
    #[serde(default)]
    pub rarity: Rarity,
    /// The worst and best it can make, however the work goes: crude to masterwork by default.
    #[serde(default)]
    pub quality: QualityRange,
    /// What goes in, and how many of each: a template (`zone:item`) or anything with a tag.
    pub ingredients: Vec<(ItemMatch, u32)>,
    /// What comes out (`zone:item`), and how many.
    pub produces: (String, u32),
    /// What must be in the room to make it, by tag: `"fire"`, `"anvil"`.
    #[serde(default)]
    pub station: Option<String>,
    /// What the crafter must hold or carry, by tag, not used up: `"hammer"`.
    #[serde(default)]
    pub tool: Option<String>,
    /// The difficulty, a formula rolled without Luck; without one, it always works.
    #[serde(default)]
    pub against: Option<Formula>,
}

impl RecipeDef {
    /// What a recipe can't do without.
    pub fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("has no name".into());
        }
        if self.ingredients.is_empty() {
            return Err("has no ingredients".into());
        }
        if self.ingredients.iter().any(|(_, n)| *n == 0) || self.produces.1 == 0 {
            return Err("asks for or makes none of something".into());
        }
        if self.quality.worst > self.quality.best {
            return Err("can make nothing: its worst quality is better than its best".into());
        }
        if !self.produces.0.contains(':') {
            return Err("must name what it makes as zone:item".into());
        }
        Ok(())
    }
}

/// The worst and best quality a recipe can make.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(default)]
pub struct QualityRange {
    pub worst: Quality,
    pub best: Quality,
}

impl Default for QualityRange {
    fn default() -> Self {
        Self {
            worst: Quality::Crude,
            best: Quality::Masterwork,
        }
    }
}

/// How well something is made: from how surely the work went (`blow`), moved by the ingredients'
/// average quality (rounded: two fine and one standard make it a step better), within `range`.
pub fn made_quality(blow: Blow, ingredients: &[Quality], range: QualityRange) -> Quality {
    let shift = if ingredients.is_empty() {
        0
    } else {
        let total: i32 = ingredients.iter().map(|q| q.steps()).sum();
        (f64::from(total) / ingredients.len() as f64).round() as i32
    };
    Quality::of_find(blow)
        .shifted(shift)
        .clamp(range.worst, range.best)
}

/// How much harder fine or rare makings are to work (`crafting` in `rules.ron`): the
/// difficulty is multiplied by `fine_makings` for each step their average quality is above
/// standard, and by `rare_makings` for each step the rarest is above common. A cut of legendary
/// meat wants a master cook; anyone else will most likely ruin it.
pub fn makings_difficulty(qualities: &[Quality], rarities: &[Rarity]) -> f64 {
    let rules = &crate::rules::rules().crafting;
    let finer = if qualities.is_empty() {
        0.0
    } else {
        let total: i32 = qualities.iter().map(|q| q.steps()).sum();
        (f64::from(total) / qualities.len() as f64).max(0.0)
    };
    let rarer = rarities.iter().map(|r| r.steps()).max().unwrap_or(0);
    rules.fine_makings.powf(finer) * rules.rare_makings.powi(rarer)
}

/// How a recipe is learned.
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Learned {
    /// Known to whoever knows its skill at its level.
    #[default]
    WithSkill,
    /// Learned from a trainer who teaches it, and saved.
    Taught,
}

/// The recipes a character has been taught, by key. Those that come with a skill aren't here.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct KnownRecipes(pub BTreeSet<String>);

/// A recipe, with its book's skill.
#[derive(Debug, Clone, PartialEq)]
pub struct Recipe {
    /// `<book>:<id>`.
    pub key: String,
    /// The skill that makes it.
    pub skill: String,
    pub def: RecipeDef,
}

impl Recipe {
    /// Whether someone with these skill levels and taught recipes can make it.
    pub fn knows(&self, levels: &HashMap<String, u32>, taught: &KnownRecipes) -> bool {
        levels
            .get(&self.skill)
            .is_some_and(|l| *l >= self.def.level)
            && (self.def.learned == Learned::WithSkill || taught.0.contains(&self.key))
    }
}

/// Every recipe from every loaded book, by key.
#[derive(Resource, Default, Debug)]
pub struct Recipes(pub BTreeMap<String, Recipe>);

/// What crafting reads and changes.
#[derive(SystemParam)]
struct Workshop<'w, 's> {
    commands: Commands<'w, 's>,
    players: Characters<'w, 's>,
    outbox: ResMut<'w, Outbox>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    rooms: Query<'w, 's, &'static RoomKey>,
    practice: Practice<'w, 's>,
    recipes: Res<'w, Recipes>,
    taught: Query<'w, 's, &'static KnownRecipes>,
    db: Res<'w, Database>,
}

impl Workshop<'_, '_> {
    /// The recipes `who` knows: those of the skills they know, at their level, that come with
    /// the skill or that they've been taught.
    fn known(&mut self, who: Entity) -> Vec<Recipe> {
        let taught = self.taught.get(who).cloned().unwrap_or_default();
        let Some((skills, ..)) = self.practice.sheet(who) else {
            return Vec::new();
        };
        let levels: HashMap<String, u32> = skills
            .0
            .iter()
            .map(|(key, state)| (key.clone(), state.level))
            .collect();
        self.recipes
            .0
            .values()
            .filter(|r| r.knows(&levels, &taught))
            .cloned()
            .collect()
    }

    /// The name of the item template `id`, or the id itself.
    fn item_name(&self, id: &str) -> String {
        self.templates
            .0
            .get(&ItemKey::parse(id, ""))
            .map_or_else(|| id.to_string(), |def| def.name.clone())
    }

    /// How an ingredient reads: "a haunch of venison", "2 herbs".
    fn ingredient(&self, which: &ItemMatch, n: u32) -> String {
        match (which, n) {
            (ItemMatch::Item(id), 1) => self.item_name(id),
            (ItemMatch::Item(id), n) => format!("{n} × {}", self.item_name(id)),
            (ItemMatch::Tag(tag), 1) => format!("a {tag}"),
            (ItemMatch::Tag(tag), n) => format!("{n} {tag}s"),
        }
    }
}

/// The recipe `wanted` names among `known`: by name, by the start of its name, or by id.
fn find<'a>(known: &'a [Recipe], wanted: &str) -> Option<&'a Recipe> {
    let id = |r: &Recipe| r.key.rsplit(':').next().unwrap_or_default().to_string();
    known
        .iter()
        .find(|r| r.def.name.to_lowercase() == wanted || id(r) == wanted)
        .or_else(|| {
            known
                .iter()
                .find(|r| r.def.name.to_lowercase().starts_with(wanted))
        })
}

/// Which of `carried` (each item's entity, template and tags) a recipe's ingredients take, each
/// item once; or, short of any, what's missing.
fn makings(
    ingredients: &[(ItemMatch, u32)],
    carried: &[(Entity, ItemKey, Vec<String>)],
) -> Result<Vec<Entity>, Vec<(ItemMatch, u32)>> {
    let mut taken = Vec::new();
    let mut missing = Vec::new();
    for (which, n) in ingredients {
        let fits = |(_, key, tags): &&(Entity, ItemKey, Vec<String>)| match which {
            ItemMatch::Item(id) => ItemKey::parse(id, "") == *key,
            ItemMatch::Tag(tag) => tags.contains(tag),
        };
        let found: Vec<Entity> = carried
            .iter()
            .filter(fits)
            .map(|(entity, ..)| *entity)
            .filter(|entity| !taken.contains(entity))
            .take(*n as usize)
            .collect();
        if found.len() < *n as usize {
            missing.push((which.clone(), *n - found.len() as u32));
        }
        taken.extend(found);
    }
    if missing.is_empty() {
        Ok(taken)
    } else {
        Err(missing)
    }
}

/// `craft <recipe>`: make something from a recipe you know.
fn craft(In(inv): In<Invocation>, mut w: Workshop) {
    let Some(actor) = Actor::get(&w.players, inv.entity) else {
        return;
    };
    let wanted = inv.args.trim().to_lowercase();
    let mut out = Vec::new();
    let mut gains = Vec::new();
    let reply = (|| {
        if wanted.is_empty() {
            return Err("Craft what? Type 'recipes' to see what you can make.".to_string());
        }
        let known = w.known(actor.entity);
        let recipe = match find(&known, &wanted) {
            Some(recipe) => recipe.clone(),
            None => {
                let all: Vec<Recipe> = w.recipes.0.values().cloned().collect();
                return Err(match find(&all, &wanted) {
                    Some(_) => "You don't know how to make that.".into(),
                    None => format!("You know no recipe for '{wanted}'."),
                });
            }
        };
        let def = &recipe.def;
        if let Some(station) = &def.station {
            let here = w.items.iter().any(|(_, item, location)| {
                *location == ItemLocation::Room(actor.room)
                    && w.templates
                        .0
                        .get(&item.key)
                        .is_some_and(|d| d.tags.contains(station))
            });
            if !here {
                return Err(format!("You need a {station} here to make that."));
            }
        }
        if let Some(tool) = &def.tool
            && !has_tool(actor.entity, tool, &w.items, &w.templates)
        {
            return Err(format!("You need a {tool} for that."));
        }
        let carried = crate::items::carried_items(actor.entity, &w.items, &w.templates);
        let used = makings(&def.ingredients, &carried).map_err(|missing| {
            let lacking: Vec<String> = missing
                .iter()
                .map(|(which, n)| w.ingredient(which, *n))
                .collect();
            format!(
                "You don't have the makings of {}: you still need {}.",
                def.name,
                crate::items::list(&lacking)
            )
        })?;
        let ready = w.practice.begin(actor.entity, &recipe.skill, |tool| {
            has_tool(actor.entity, tool, &w.items, &w.templates)
        })?;
        // What the makings are: how well made, and the rarest of them.
        let (qualities, rarities): (Vec<Quality>, Vec<Rarity>) = used
            .iter()
            .filter_map(|e| w.items.get(*e).ok())
            .map(|(_, item, _)| {
                let rarity = w.templates.0.get(&item.key).and_then(|d| d.rarity);
                (item.quality, rarity.unwrap_or_default())
            })
            .unzip();
        let (made, margin) = match &def.against {
            None => (true, 1.0),
            Some(against) => {
                let inputs = w
                    .practice
                    .inputs(actor.entity, ready.effectiveness, Side::default());
                let difficulty = w.practice.eval(against, &inputs).max(1.0)
                    * makings_difficulty(&qualities, &rarities);
                let skill = f64::from(ready.effectiveness.max(1));
                let contest = w.practice.opposed(skill, ready.luck, difficulty, 0);
                (contest.won, contest.margin)
            }
        };
        // Made or ruined, the makings are used up.
        for entity in used {
            if let Ok((_, item, _)) = w.items.get(entity) {
                crate::healing::used_up(&mut w.commands, &w.db, entity, item);
            }
        }
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
            fighting: false,
        };
        gains = w.practice.exercise(actor.entity, learner, &recipe.skill);

        let (id, count) = &def.produces;
        let mut name = w.item_name(id);
        let mut note = "";
        if made {
            let here = w.rooms.get(actor.room).map_err(|_| String::new())?.clone();
            let quality = made_quality(Blow::of(margin), &qualities, def.quality);
            for _ in 0..*count {
                let (made_name, where_to) = make_for(
                    &mut w.commands,
                    &w.templates,
                    &w.db,
                    &w.items,
                    &actor,
                    &here,
                    ItemKey::parse(id, ""),
                    quality,
                )?;
                name = made_name;
                if where_to == "here" {
                    note = " Your hands are full, so it's on the ground.";
                }
            }
            if *count > 1 {
                name = format!("{name} (×{count})");
            }
        }
        let skill = w
            .practice
            .defs
            .skills
            .get(&recipe.skill)
            .cloned()
            .ok_or_else(String::new)?;
        let text = skill.def.text.as_ref().ok_or_else(String::new)?;
        let lines = match (made, &text.miss) {
            (false, Some(miss)) => miss,
            _ => &text.hit,
        };
        let with = [("actor", actor.name.as_str()), ("item", name.as_str())];
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: fill(&lines.room, &with) + "\r\n",
        });
        Ok(fill(&lines.actor, &with) + note)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    out.extend(
        gains
            .into_iter()
            .map(|line| Outgoing::line(actor.entity, line)),
    );
    deliver(out, &mut w.outbox);
}

/// `recipes`: what your crafts can make, by craft, and what each needs.
fn recipes(In(inv): In<Invocation>, mut w: Workshop) {
    let Some(actor) = Actor::get(&w.players, inv.entity) else {
        return;
    };
    let known = w.known(actor.entity);
    let text = if known.is_empty() {
        "You don't know any recipes. Crafts like Cooking teach them as you learn them.".to_string()
    } else {
        let mut by_skill: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for recipe in &known {
            let def = &recipe.def;
            let needs: Vec<String> = def
                .ingredients
                .iter()
                .map(|(which, n)| w.ingredient(which, *n))
                .collect();
            let rare = match def.rarity {
                Rarity::Common => String::new(),
                rarity => format!(" ({})", rarity.name()),
            };
            let mut line = format!("  {}{rare}: {}", def.name, crate::items::list(&needs));
            if let Some(station) = &def.station {
                line += &format!(", at a {station}");
            }
            if let Some(tool) = &def.tool {
                line += &format!(", with a {tool}");
            }
            let craft = w
                .practice
                .defs
                .skills
                .get(&recipe.skill)
                .map_or_else(|| recipe.skill.clone(), |s| s.def.name.clone());
            by_skill.entry(craft).or_default().push(line);
        }
        by_skill
            .into_iter()
            .map(|(craft, lines)| format!("{craft}:\r\n{}", lines.join("\r\n")))
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    deliver(vec![Outgoing::line(actor.entity, text)], &mut w.outbox);
}

// Loading recipe books, like spell books.

#[derive(Resource)]
struct RecipeBooksHandle(#[allow(dead_code)] Handle<LoadedFolder>);

fn load_recipe_books(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(RecipeBooksHandle(assets.load_folder(RECIPES_PATH)));
}

/// Rebuild the recipe table whenever a book loads, changes or goes away.
fn sync_recipes(
    mut events: MessageReader<AssetEvent<RecipeBookDef>>,
    books: Res<Assets<RecipeBookDef>>,
    mut recipes: ResMut<Recipes>,
) {
    if events.read().count() == 0 {
        return;
    }
    recipes.0.clear();
    for (_, book) in books.iter() {
        for (id, def) in &book.recipes {
            let key = format!("{}:{id}", book.id);
            recipes.0.insert(
                key.clone(),
                Recipe {
                    key,
                    skill: book.skill.clone(),
                    def: def.clone(),
                },
            );
        }
    }
    info!("loaded {} recipes", recipes.0.len());
}

#[derive(TypePath)]
struct RecipeBookLoader;

#[derive(Debug, Error)]
enum RecipeBookLoaderError {
    #[error("could not read recipe book: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not parse recipe book: {0}")]
    Ron(#[from] ron::error::SpannedError),
    #[error("recipe {0}")]
    Incomplete(String),
}

impl AssetLoader for RecipeBookLoader {
    type Asset = RecipeBookDef;
    type Settings = ();
    type Error = RecipeBookLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<RecipeBookDef, Self::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        let mut book: RecipeBookDef = ron_options().from_bytes(&bytes)?;
        book.id = book_id(load_context.path().path());
        for (id, recipe) in &book.recipes {
            recipe.check().map_err(|err| {
                RecipeBookLoaderError::Incomplete(format!("{}:{id} {err}", book.id))
            })?;
        }
        Ok(book)
    }

    fn extensions(&self) -> &[&str] {
        &[RECIPE_EXTENSION]
    }
}

/// A book's id from its file: `recipes/cooking.recipes.ron` is `cooking`.
fn book_id(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(&format!(".{RECIPE_EXTENSION}")))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECIPES_PATH_ON_DISK: &str = "assets/recipes";

    /// Every recipe book in `assets/recipes` parses, names a skill that exists, and makes and
    /// takes items that exist; every recipe that's taught is taught by someone.
    #[test]
    fn recipe_books_are_complete() {
        let mut skills = std::collections::HashSet::new();
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
            skills.extend(tree.skills.keys().map(|skill| format!("{id}:{skill}")));
        }
        let mut items = std::collections::HashSet::new();
        let mut teaches = Vec::new();
        for entry in std::fs::read_dir("assets/zones").unwrap() {
            let path = entry.unwrap().path();
            let zone: crate::world::ZoneDef = ron_options()
                .from_bytes(&std::fs::read(&path).unwrap())
                .unwrap();
            let id = crate::world::zone_id(&path);
            items.extend(zone.items.keys().map(|item| format!("{id}:{item}")));
            teaches.extend(zone.mobs.values().flat_map(|m| m.teaches.clone()));
        }
        let mut count = 0;
        for entry in std::fs::read_dir(RECIPES_PATH_ON_DISK).unwrap() {
            let path = entry.unwrap().path();
            let book: RecipeBookDef = ron_options()
                .from_bytes(&std::fs::read(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let id = book_id(&path);
            assert!(
                skills.contains(&book.skill),
                "{id}: no skill {}",
                book.skill
            );
            for (recipe_id, recipe) in &book.recipes {
                let here = format!("{id}:{recipe_id}");
                recipe.check().unwrap_or_else(|err| panic!("{here} {err}"));
                assert!(
                    items.contains(&recipe.produces.0),
                    "{here}: makes nothing real"
                );
                for (which, _) in &recipe.ingredients {
                    if let ItemMatch::Item(item) = which {
                        assert!(items.contains(item), "{here}: no ingredient {item}");
                    }
                }
                if recipe.learned == Learned::Taught {
                    assert!(
                        teaches.contains(&here),
                        "{here} is taught, but no one teaches it"
                    );
                }
                count += 1;
            }
        }
        assert!(count > 0);
    }

    #[test]
    fn the_makings_take_each_item_once() {
        let (meat, garlic, yarrow) = (
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
            Entity::from_raw_u32(3).unwrap(),
        );
        let carried = vec![
            (
                meat,
                ItemKey::parse("forest:venison", ""),
                vec!["meat".into()],
            ),
            (
                garlic,
                ItemKey::parse("forest:wild_garlic", ""),
                vec!["food".into()],
            ),
            (
                yarrow,
                ItemKey::parse("forest:yarrow", ""),
                vec!["herb".into()],
            ),
        ];
        let stew = vec![
            (ItemMatch::Item("forest:venison".into()), 1),
            (ItemMatch::Tag("herb".into()), 1),
        ];
        assert_eq!(makings(&stew, &carried), Ok(vec![meat, yarrow]));
        // Two herbs wanted, one carried: one more is missing.
        let more = vec![(ItemMatch::Tag("herb".into()), 2)];
        assert_eq!(
            makings(&more, &carried),
            Err(vec![(ItemMatch::Tag("herb".into()), 1)])
        );
        // Meat by tag and by template can't both take the one haunch.
        let twice = vec![
            (ItemMatch::Tag("meat".into()), 1),
            (ItemMatch::Item("forest:venison".into()), 1),
        ];
        assert!(makings(&twice, &carried).is_err());
    }

    #[test]
    fn quality_follows_the_work_and_the_makings_within_the_recipe() {
        let any = QualityRange::default();
        assert_eq!(made_quality(Blow::Solid, &[], any), Quality::Standard);
        assert_eq!(made_quality(Blow::Crushing, &[], any), Quality::Masterwork);
        // Fine makings, a finer result; poor ones, worse.
        let fine = [Quality::Fine, Quality::Fine, Quality::Standard];
        assert_eq!(made_quality(Blow::Solid, &fine, any), Quality::Fine);
        let poor = [Quality::Poor, Quality::Poor];
        assert_eq!(made_quality(Blow::Strong, &poor, any), Quality::Standard);
        // A plain recipe tops out; a rare one never makes worse than it promises.
        let plain = QualityRange {
            worst: Quality::Crude,
            best: Quality::Fine,
        };
        assert_eq!(made_quality(Blow::Crushing, &[], plain), Quality::Fine);
        let rare = QualityRange {
            worst: Quality::Standard,
            best: Quality::Masterwork,
        };
        assert_eq!(made_quality(Blow::Glancing, &poor, rare), Quality::Standard);
    }

    #[test]
    fn fine_and_rare_makings_are_harder_to_work() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(close(makings_difficulty(&[], &[]), 1.0));
        assert!(close(
            makings_difficulty(&[Quality::Standard], &[Rarity::Common]),
            1.0
        ));
        // Poor makings are no easier: they only make a worse result.
        assert!(close(makings_difficulty(&[Quality::Crude], &[]), 1.0));
        // A step finer on average, half as hard again; the rarest counts, doubling a step.
        assert!(close(makings_difficulty(&[Quality::Fine], &[]), 1.5));
        assert!(close(
            makings_difficulty(&[], &[Rarity::Common, Rarity::Legendary]),
            16.0
        ));
    }

    #[test]
    fn taught_recipes_are_known_only_once_learned() {
        let recipe = |learned, level| Recipe {
            key: "cooking:stew".into(),
            skill: "survival:cooking".into(),
            def: RecipeDef {
                name: "stew".into(),
                level,
                learned,
                price: 0,
                rarity: Rarity::Common,
                quality: QualityRange::default(),
                ingredients: vec![(ItemMatch::Tag("meat".into()), 1)],
                produces: ("forest:stew".into(), 1),
                station: None,
                tool: None,
                against: None,
            },
        };
        let levels = HashMap::from([("survival:cooking".to_string(), 3)]);
        let none = KnownRecipes::default();
        assert!(recipe(Learned::WithSkill, 3).knows(&levels, &none));
        assert!(
            !recipe(Learned::WithSkill, 4).knows(&levels, &none),
            "too hard yet"
        );
        assert!(!recipe(Learned::Taught, 0).knows(&levels, &none));
        let taught = KnownRecipes(BTreeSet::from(["cooking:stew".to_string()]));
        assert!(recipe(Learned::Taught, 0).knows(&levels, &taught));
        assert!(
            !recipe(Learned::Taught, 0).knows(&HashMap::new(), &taught),
            "needs the skill"
        );
    }
}
