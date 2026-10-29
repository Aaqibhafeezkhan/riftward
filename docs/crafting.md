# Crafting

[Design summary](README.md) · Status: built (2026-10-29): recipe books, `craft` and `recipes`, recipes taught by trainers, and Cooking, the first craft. Brewing, tanning and smithing are the same machinery with their own books.

Crafting makes things from other things: a haunch of venison over a fire becomes a meal, two hides and a needle a jerkin, ore at an anvil a blade. Every craft works the same way, so a new one is a skill and a recipe book, not new code.

## Recipe books

Recipes are data, one book per craft in `assets/recipes/` (`cooking.recipes.ron`), hot-reloaded. **A book names the skill that uses it**: these are Cooking's recipes, those Smithing's. A recipe's key is `<book>:<id>` (`cooking:roast_venison`).

```ron
(
    skill: "survival:cooking",
    recipes: {
        "roast_venison": (
            name: "roast venison",
            ingredients: [(Item("forest:venison"), 1)],
            produces: ("forest:roast_venison", 1),
            station: "fire",
            against: "3",
        ),
        "hunters_stew": (
            name: "hunter's stew",
            level: 5,
            learned: Taught,
            price: 25,
            rarity: Uncommon,
            ingredients: [(Item("forest:venison"), 1), (Item("forest:mushrooms"), 1), (Tag("herb"), 2)],
            produces: ("forest:hunters_stew", 1),
            station: "fire",
            against: "5",
        ),
    },
)
```

| Field | Means |
| --- | --- |
| `name` | What it's called, to `craft` it by |
| `ingredients` | What goes in, and how many of each: a template, `Item("zone:item")`, or anything with a tag, `Tag("herb")`. Each carried item counts once |
| `produces` | What comes out (`zone:item`), and how many |
| `station` | A tag something in the room must have: `"fire"` (a campfire, a hearth), `"anvil"`, `"workbench"`. Stations are ordinary items, fixed in place by builders |
| `tool` | A tag the crafter must hold or carry, not used up: `"hammer"`, `"needle"`, as skills' [tools](effects.md#active-skills-as-built) are |
| `against` | The difficulty, a [formula](effects.md#formulas-as-built) rolled without Luck; without one, it always works |
| `level` | The skill level it's made at (default 0) |
| `learned` | `WithSkill` (the default): known to whoever knows the skill at its level. `Taught`: learned from a trainer, and saved |
| `price` | What a trainer asks to teach it, in copper |
| `rarity` | How rare it is: a rare recipe makes something better than most can, and is worth seeking out |
| `quality` | The worst and best it can make, `(worst: Standard, best: Fine)`: crude to masterwork by default. Simple fare tops out at fine; the hunter's stew is never worse than standard |

The recipe test checks every book's skill exists, every item it takes and makes exists, and every taught recipe is taught by someone.

## Making things

`craft <recipe>` (or `use <craft> <recipe>`: a crafting skill's `command` is `craft`) is an action:

1. **The recipe must be known**, its station in the room ("You need a fire here to make that."), its tool on you, and its ingredients carried ("You don't have the makings of hunter's stew: you still need a cluster of mushrooms and 2 herbs."). Only carried items count, not what lies on the ground. A refusal costs nothing.
2. **The roll:** the crafter's skill (level plus its attribute) against `against`, by opposed roll ([the ratio rule](attributes.md#the-ratio-rule)), with Luck. **Fine and rare makings are harder to work** (decided 2026-10-29): the difficulty is multiplied by 1.5 for each step their average quality is above standard, and by 2 for each step the rarest is above common (`crafting` in `assets/rules.ron`). Poor makings are no easier. A legendary cut of meat is sixteen times as hard: a master cook makes a feast of it, and anyone else most likely ruins it.
3. **The ingredients are used up** either way (decided 2026-10-29): won, it's made; lost, they're ruined ("Your cooking goes wrong, and the makings of roast venison are ruined.").
4. **How well it's made** ([quality](items.md)) has three levers: how surely the work went (as a find's: poor for a near thing, standard, fine, masterwork for a crushing win), moved by the ingredients' average quality, rounded (fine venison makes a finer roast, poor makings a worse one), and kept within the recipe's `quality` range.
5. **The skill is practised**, as any use is ([pacing](progression.md#pacing) applies).

The words are the craft skill's own `text`, with `{item}` what was made (or would have been).

## Learning recipes

- **With the skill:** most recipes come with their craft, at their `level`, so a cook learns to make more as they improve.
- **Taught:** a trainer whose template `teaches` a recipe's key (`"cooking:hunters_stew"`) teaches it with `learn`, for its `price`, to someone who knows its craft at its level. Taught recipes are saved per character (`character_recipes`).
- **Later:** recipes found as loot (a recipe scroll to read), given by quests, or discovered by experimenting.

`recipes` lists what you can make, by craft, with what each needs and where.

## Cooking

Survival's **Cooking** (Intuition), taught by the hunter, turns what's hunted and gathered into meals that keep you `nourished` longer and more strongly than raw food: roast rabbit, fried frog legs and roast venison from the start, boar stew at 3, and the hunter's own stew (taught, uncommon), which heals a little too. Raw meat isn't food. The hunter's camp has a campfire; a hearth in town comes with the town's kitchen.

## Open questions

- [x] **Should ingredients' quality carry into what's made?** Yes: their average moves the result's (2026-10-29).
- [ ] **Recipe scrolls and quest rewards:** the item and action that teach a recipe.
- [ ] **Crafting trees:** one Crafting tree, or one per craft (see [Skill trees](trees.md#open-questions))? Cooking sits in Survival for now.
- [ ] **Stations you make or carry:** a campfire built with Survival, a portable kit.
