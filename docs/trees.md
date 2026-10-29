# Skill trees

[Design summary](README.md) · Status: a plan, with step 1 built: every weapon tree to its journeyman techniques, Riposte, Toughness, the Endurance bonus, and the Adept's Killing Hands and Quickened Reflexes, on passive bonuses, conditions and techniques. The rest is a first pass to discuss.

This is the plan for the game's skill trees: their shape, which trees there are, a first pass at what each holds, and what the engine needs before each can work. How skills work in general (kinds, rarity, acquiring, progression) is in [Skills](skills.md) and [Progression](progression.md).

## The shape of a tree

Every tree follows the same shape, so players can read any tree at a glance and builders have a template. Levels are the entry skill's, the tree's backbone.

| Tier | Requires | Rarity | Learned from | What it is |
| --- | --- | --- | --- | --- |
| **Entry** | Nothing (or the tree's own gate, like Magic 1) | Common | Town trainers | The tree's core: an attack, a craft, a sense. Everything else hangs off it |
| **Journeyman** | Entry 15 | Common or Uncommon | Discovery through use, player teachers, loot, remote trainers as quest rewards | The first techniques: things to *do* with the core |
| **Expert** | Entry 30 | Uncommon or Rare | Discovery, player teachers, loot, remote trainers as quest rewards | Stronger techniques and passive masteries |
| **Master** | Entry 45, sometimes a second tree | Rare or Epic | Player masters, loot | One capstone per tree; hybrids live here |

- **Spells aren't in trees.** A casting tradition's tree holds its schools; spells are a separate category (see [Magic](#magic)). Everything else, weapon techniques included, is a skill.
- **Every skill can cost stamina,** by how hard it is on the body (see [Stamina costs](#stamina-costs)). Moves meant to be used every round are free; heavy ones have to be paced.
- **Every skill does something.** No skill exists only to unlock another. A passive must change a number the player can feel; an active must be worth a round.
- **Few skills per tree, at first.** The entry, two or three journeyman skills, one expert skill and a capstone. Trees grow later through hybrids and loot, not long chains.
- **Hybrids join trees.** A skill can require two trees (Flame Blade: Swordplay 30 and Fire 20), which is how builds emerge without classes (see [Skills](skills.md#skill-trees-and-skills)).
- **Attributes give trees their feel.** Each tree has one; strong characters lean to Axes and Hammers, quick ones to Knives and Swordplay, but nothing forbids a weak axe-fighter.

## Stamina costs

Stamina is the budget every character spends: on techniques, on fleeing, and as Drain for spells and Adept powers, so fighters and casters pace themselves the same way. Every skill can carry a cost, set by its weight:

| Weight | Stamina | Examples |
| --- | --- | --- |
| **Free** | 0 | Basic attacks (Brawling, a weapon's ordinary blow), always-on passives (Dodge, Toughness, Bladework) |
| **Light** | 1 | Quick techniques: Lunge, Riposte when it fires, Kick, picking a lock |
| **Medium** | 2–3 | Strong techniques: Cleave, Sweep, Backstab, Grapple, fleeing |
| **Heavy** | 4–6 | Crushing blows and capstones: Stagger, Haymaker, Hew, Berserk, Whirlwind, Earthshaker |

- **Running out leaves the free moves.** An attack a fighter can't afford falls back to their weapon's ordinary blow, then to Brawling, so a tired fighter keeps fighting, just plainly. *Built for attack skills (`cost: Stamina(n)`).*
- **Spending stamina practises Endurance,** whatever spent it.
- **Nothing recovers in a fight,** so stamina is a per-fight budget. What makes long fights possible is **Endurance**: with passive bonuses (step 1 below), it raises maximum stamina by 1 for every 5 levels, so practice, not mid-fight rest, is what deepens a fighter's reserves. Willpower raised with Karma adds to it too.
- **Walking costs stamina too,** by [terrain](world.md#terrain): mud and water are exhausting, streets are free. A character too spent to move can't press on into rough ground, and can't flee without the 2 stamina it takes.
- **Spells and Adept powers** cost Drain, which is stamina (and for great workings, health), resisted by Willpower plus the tradition's attribute.

## The trees

Grouped by what they're for. ✓ marks what exists as data.

### Fighting

| Tree | Attribute | Entry | Journeyman | Expert | Master |
| --- | --- | --- | --- | --- | --- |
| **Brawling** ✓ | Strength | Brawling ✓: fists, 1–3 | **Grapple:** hold a foe, so neither of you can flee and they defend at half. **Kick:** a slower, harder unarmed attack | **Iron Fists** (passive): +1 unarmed damage per 10 levels | **Haymaker:** a blow that staggers |
| **Swordplay** ✓ | Agility | Swordplay ✓ | **Riposte** (passive): after a dodge, a free counter-attack. **Lunge:** a surer hit that leaves you open | **Bladework** (passive): +accuracy | **Whirlwind:** strike every foe fighting you |
| **Knife Fighting** ✓ | Agility | Knife Fighting ✓ | **Backstab:** a heavy opening blow on someone who isn't fighting you, or is resting. **Throw:** a knife at range | **Open Vein:** leaves the target bleeding | **Assassinate** (with Stealth): a killing opener from hiding |
| **Axe Fighting** ✓ | Strength | Axe Fighting ✓ | **Cleave:** a hit spills onto a second foe. **Hew:** extra damage, slower | **Executioner** (passive): more damage to the badly wounded | **Berserk:** a stance trading defence for damage |
| **Hammer Fighting** ✓ | Strength | Hammer Fighting ✓ | **Stagger:** the target loses its next turn. **Crush:** ignores armour | **Weight of Iron** (passive): staggers on heavy hits | **Earthshaker:** staggers every foe in the room |
| **Staff Fighting** ✓ | Agility | Staff Fighting ✓ | **Sweep:** knock a foe down: they lose a turn and defend at half. **Guard:** a stance raising defence | **Flowing Staff** (passive): Dodge counts more | **Whirling Staff:** strike every foe, guarding as you go |
| **Archery** | Agility | Archery: attack from range | **Aimed Shot**, **Volley** | **Steady Hand** (passive) | **Rain of Arrows** |
| **Shields** | Strength | Block (passive): stop some damage | **Shield Bash:** stagger | **Bulwark:** protect a companion | **Shield Wall** |

### The body

| Tree | Attribute | Skills |
| --- | --- | --- |
| **Athletics** ✓ | Body (Reaction for Dodge, Willpower for Endurance) | **Dodge** ✓ (passive: +1 defence a level, a `Defence` bonus in its file), **Endurance** ✓ (passive: practised by spending stamina; with passive bonuses, +1 max stamina per 5 levels), **Toughness** (passive: +max health), **Climbing** and **Swimming** (rooms that need them), **Running** (flee better, travel tires less) |

### Getting by

| Tree | Attribute | Skills |
| --- | --- | --- |
| **Thievery** ✓ | Agility | **Lockpicking** ✓, **Pickpocketing** ✓ (coins from mobs with `steal`; players later), **Disarm Traps**, **Fence** (with Trade: sell stolen goods) |
| **Stealth** | Agility | **Sneak** (move without being announced), **Hide** (unseen while still), **Shadowing** (follow unseen), capstone **Vanish** (hide mid-fight) |
| **Survival** | Intuition | **Foraging** (find food and herbs, by terrain), **Tracking** (see who passed and where they went), **Skinning** ✓ and **Butchering** ✓ (working a beast's corpse for its pelt, meat and trophies: see [Harvest what's cut, loot what's left](darkwood.md#harvest-whats-cut-loot-whats-left)), **Camping** (rest well outdoors), **Navigation** |
| **Nature** | Intuition | **Herbalism** (know and gather herbs), **Brewing** (with Crafting: draughts and poultices) |
| **Healing** | Intuition | **First Aid** (bind wounds out of a fight, with bandages), **Treat Poison**, **Surgery** (expert) |
| **Animals** | Charisma | **Animal Handling** (calm an angry beast, so it won't attack you), **Riding**, **Taming** (a companion) |

### Trade, craft and learning

| Tree | Attribute | Skills |
| --- | --- | --- |
| **Trade** ✓ | Charisma | **Haggling** ✓ (better prices), **Appraisal** ✓ (know an item's worth, and later its quality) |
| **Crafting** | Logic (Strength for Smithing) | **Smithing**, **Leatherwork**, **Carpentry**, **Scribing** (copy scrolls), each a tree of its own later |
| **Lore** | Logic | **Lore** (study a mob or item: what it is, how tough), **Bestiary**, **Rift Lore**, **Languages** |
| **Performance** | Charisma | **Performance** (play for coin; inspire companions, as an aura) |
| **Command** ✓ | Charisma | **Call for Help** ✓ (passive: below half health, allies within two rooms come running), **Rally**, **Orders**. Not taught yet: the town's guards have it |
| **Sailing** | Intuition | **Sailing**: boats and rivers, when the world has them |

### Magic

Each tradition is a full tree in the same shape as the others, entered through its foundation (✓, all four), which requires Magic 1 and is taught by the tradition's teacher.

**Spells are a separate category, not skills** (see [Spells](magic.md#spells)): known, never levelled, and powered by a school. What a casting tradition's tree holds is the **craft of casting**, skills worth improving in their own right:

- **Schools,** whose level (plus Magic) is the power of every spell in them. Casting a spell practises its school.
- **Drain resistance,** so a caster can cast more before it hurts.
- **Shaping:** range, area, splitting a spell between targets.
- **Countering** another caster's spell, and **sustaining** a spell over time.
- **Rituals,** slow and powerful workings, and a capstone unique to the tradition.

| Tree | Attribute | Entry | Journeyman | Expert | Master |
| --- | --- | --- | --- | --- | --- |
| **Divine** | Intuition | **Faith** ✓: the first school (blessings, mending) | **Wrath** (school: smiting). **Warding** (school: protection). **Devotion** (passive: resist Drain) | **Litany:** sustain a blessing over time. **Sanctity** (passive): healing spells mend more | **Intercession:** call on your god once a day for a miracle beyond your spells |
| **Hermetic** | Logic | **Hermetic Principles** ✓: the first school (force and light) | **Evocation** (school: fire and lightning). **Abjuration** (school: wards and dispelling). **Arcane Discipline** (passive: resist Drain) | **Spell Shaping:** widen a spell to an area, or split it between targets. **Counterspell:** unravel a spell as it's cast. **Divination** (school: seeing and knowing) | **Grand Working:** a long ritual for effects no spell can reach |
| **Shamanic** | Charisma | **Spirit Sight** ✓: the first school (speaking with spirits) | **Beasts** (school: animal spirits). **Elements** (school: wind, earth, water). **Spirit Pact** (passive: spirits shoulder some Drain) | **Summoning:** call a spirit to fight or serve for a time. **Ancestors** (school: guidance and curses) | **Spirit Walk:** leave the body and travel the spirit world |
| **Adept** | Body | **Inner Discipline** ✓ | **Killing Hands:** unarmed strikes hit like weapons (the Adept's weapon). **Quickened Reflexes** (passive: initiative and Dodge). **Iron Breath** (passive: Endurance and stamina) | **Iron Body** (passive: natural armour). **Flowing Step:** flee and move with uncanny ease. **Spirit Strike:** Killing Hands that wound spirits and the Awakened | **Perfect Strike:** a single blow that cannot be dodged |

- **Adepts have no spells.** Their magic is turned inward, as **powers** that are ordinary skills, so the Adept tree is all skills, like a weapon tree. Their powers cost Drain like spells, resisted the same way.
- **First spells,** two per casting tradition, *built* ([The first spells](magic.md#the-first-spells-built)): Divine **Mend** (heal by touch) and **Bless** (an ally fights better); Hermetic **Spark** (a bolt of force) and **Ward** (absorbs damage); Shamanic **Beast Speech** (calm animals) and **Spirit Grasp** (a spirit holds a foe). Questioning animals, and Spark reaching into the next room, wait for later.
- **Drain resistance skills** add to the Willpower + tradition attribute that resists Drain, so a practised caster casts more and pays less.

### Racial

A `racial` tree holding each race's abilities, each requiring its race (the tree exists, empty since the elves' **Fey Mind** became a [trait](effects.md#traits) on 2026-10-27; what a race simply *is*, rather than does and improves at, is a trait): Heat Sight, Low-Light Vision, Versatility, Stone Skin, Gahad, Tail Combat, Swimming (lizardfolk), Flight, Astral Sight, Fox Form and Foxfire (kitsune), Small and Quick (halflings). Most need light and darkness first.

## What the engine needs

The trees above mostly can't be built as data alone yet. Grouped by the system each needs:

| System | Unlocks | Size |
| --- | --- | --- |
| **Combat techniques:** active skills used in a fight through the action queue, with an effect when they land | Grapple, Lunge, Backstab, Cleave, Stagger, Sweep, Shield Bash, every capstone | Medium |
| **Conditions:** short-lived states with a duration (staggered, knocked down, bleeding, grappled, guarding, blessed) | Most techniques; Riposte; Bless; Spirit Grasp | Medium |
| **Passive bonuses:** a skill that raises a number (max health, accuracy, damage, armour) by its level | Toughness, Iron Fists, Bladework, Executioner, Quickened Reflexes. Auras already do this for mobs | Small |
| **Healing and consumables:** items used up for an effect | First Aid (bandages), Herbalism, Brewing, Mend | Small to medium |
| **Room terrain and loot tables:** what a room is like, and what can be found or dropped there | Foraging, Tracking, Herbalism, Climbing, Swimming, loot scrolls | Medium |
| **Seeing and being seen:** hiding, sneaking, noticing | Stealth, Backstab from hiding, Assassinate, Heat Sight, Low-Light Vision | Medium |
| **Ranged attacks** | Archery, Throw, Spark | Small, once techniques exist |
| **Money and shops** | Trade, Fence, Performance, lesson costs | Medium |
| **Spells:** known spells, `cast`, Drain, learning from teachers and scrolls | Every spell | Medium, after techniques and conditions |
| **Companions and grouping** | Bulwark, Bless, inspiring, Taming | Large; later |

## Proposed order

1. **Combat techniques, conditions and passive bonuses together.** *Built:* see [Step 1, as built](#step-1-as-built).
2. **Armour:** damage reduction, at a cost to dodging. *Built:* see [Armour](#armour).
3. **Healing and consumables:** First Aid and bandages, so fighters can patch up without walking to town; herbs come with step 4. *Built:* see [Healing](#healing).
4. **Room terrain and loot tables:** Foraging, Herbalism, Tracking, Climbing and Swimming, and mobs that drop things. *Built:* loot tables, Foraging and Herbalism (see [Gathering](#gathering)), and Tracking, Climbing and Swimming (see [Climbing and swimming](world.md#climbing-and-swimming) and [Tracking](world.md#tracking)). Climbing and Swimming are Athletics skills, taught by the drill sergeant; Tracking is Survival's, taught by the hedge-shaman.
5. **Seeing and being seen:** Stealth, and Backstab from hiding. *Built:* see [Stealth](#stealth).
6. **Spells:** the spell category, `cast` and Drain, and the first two spells per tradition, reusing techniques' effects and conditions. *Built:* see [Spells](magic.md#spells): Mend and Bless, Spark and Ward, Beast Speech and Spirit Grasp, taught by each tradition's teacher.
7. **Money and shops,** then Trade and the crafts. *Built:* money and shops, see [Money and shops](economy.md); Trade's Haggling and Appraisal are built too ([Trade](economy.md#trade-haggling-and-appraisal)); the crafts are next.

Each step adds its trees' entry skills to the trainers in Riftwatch: the drill sergeant teaches each weapon's basics, and new trainers appear for the rest (the herbalist for Herbalism and First Aid, someone in the Back Alley for Stealth and Thievery). Trainers teach only entry skills; the rest of each tree is [discovered](skills.md#discovery) or learned another way.

## Step 1, as built

- **Passive bonuses.** A passive skill can raise a stat by a point per so many levels, growing a fraction with every level (decided 2026-10-27: `per: 5` is +0.2 a level, so level 1 and level 2 differ, with no steps). Ratings, damage, armour and the rest keep the fraction, since contests are rolled as ratios; only maximum health and stamina, which are whole points, count it once it adds up to one, optionally only for one attack skill: `bonuses: [(stat: MaxHealth, per: 5)]`, or `(stat: Attack, per: 10, with: "swordplay:basics")` for a Bladework. Stats: max health, max stamina, attack, defence, damage, armour, initiative, and since armour, pierce, armour ease and block, and `Resist(tag)`, which helps resist spells and techniques carrying that tag (see [Resisting](effects.md#resisting-as-built)). Maximum health and stamina everywhere (the prompt, `score`, recovery, respawning) now include them.
- **Conditions** on any fighter, counted in their turns: **staggered** (loses a turn), **down** (loses a turn, defends at half until then), **bleeding** (hurts each turn), **held** (can't flee, defends at half), **guarding** (defends better), **exposed** (defends at half until their next turn). Code in `src/combat/states.rs`.
- **Techniques,** a skill kind: a special move made with an attack skill, in place of the auto-attack, through the action queue (`use cleave`). Each has a stamina cost, and can add accuracy and damage, be harmless (a grab), not strike at all (a guard), carry effects, or be an **opener** that can start a fight (`use backstab rat`) and lands easily on someone unaware. A technique without its weapon in hand, or its stamina, falls back to the auto-attack with a word why.

  ```ron
  "cleave": (name: "Cleave", requires: Skill("axes:basics", 15),
      description: "A great swing that carries on into a second foe fighting you, even when it fells the first.",
      kind: Technique((with: "axes:basics", cost: Stamina(3), effects: [Cleave])),
      text: (hit: (...), miss: (...))),
  ```

- **Counters:** a passive can answer a dodge with an immediate strike for a stamina, if the defender is fighting with the attack it counters with: Riposte.
- **Built skills,** all [discovered](skills.md#discovery) by using the tree's skills once its basics reach 15 (for the Adept, while practising Inner Discipline, the Adept's school: see [Magic](magic.md)):

  | Tree | Built |
  | --- | --- |
  | Brawling | **Kick** (+2 damage, 1 stamina), **Grapple** (a harmless grab that holds for 2 turns, resisted by Strength, 2) |
  | Swordplay | **Lunge** (+4 accuracy, then exposed, 1), **Riposte** (counter a dodge, 1) |
  | Knife Fighting | **Backstab** (opener, +4 damage, 2), **Open Vein** (bleeds 1 for 3 turns, 2). Open Vein comes before Throw, which waits on ranged attacks |
  | Axe Fighting | **Cleave** (carries into a second foe, even when it kills the first, 3), **Hew** (+3 damage, −2 accuracy, 2) |
  | Hammer Fighting | **Stagger** (target loses a turn, 4), **Crush** (ignores armour, +1, 2) |
  | Staff Fighting | **Sweep** (knocks down, 3), **Guard** (+4 defence for 2 turns, no strike, 1) |
  | Athletics | **Toughness** (+1 max health per 5 levels, a fifth with each, practised by taking damage); **Endurance** now gives +1 max stamina per 5 levels |
  | Adept | **Killing Hands** (unarmed, 2–4: the Adept's weapon), **Quickened Reflexes** (+initiative, and +defence, practised by dodging) |

- **Mobs** use whatever skills their templates give them: technique skills now and then, by their `tactics` chance (see [The living world](world.md)), as well as auras and passives.

## Armour

Built. Armour is **damage reduction**: every hit loses its wearer's armour, **even to nothing**, so in heavy armour some blows are easy to ignore ("The blow glances off the town guard's armour."). It's the same `Armour` stat auras and passives feed. In exchange, armour **hampers dodging**: each piece has a penalty taken off the wearer's defence, so a knight in plate shrugs off blows they can't avoid, and a scout in leather avoids blows they couldn't shrug off. Mobs wear it by the same rules, from their template's `equipment`, and drop it.

- **Armour pieces are worn items** with `armour: (reduction: n, penalty: n)`, in their wear slot: head, body, hands, legs and feet (hands and legs are new). Pieces add up, and carried armour counts for nothing until it's worn. Bulk counts as for anything carried.
- **Shields block** instead: held in a hand, `armour: (block: n, penalty: n)` adds to defence. When an attack fails against a shield-bearer, the miss is the shield's in proportion to its share of their defence ("The town guard turns your attack aside with a shield!"), and the rest are dodges. A block isn't a dodge, so it doesn't trigger Riposte or practise Dodge.
- **The starting pieces:**

  | Piece | Slot | Reduction | Penalty | Bulk |
  | --- | --- | --- | --- | --- |
  | padded jack, leather jerkin | body | 1 | 0 | 2 |
  | studded leather coat | body | 2 | 1 | 2 |
  | mail shirt | body | 3 | 2 | 3 |
  | steel breastplate | body | 4 | 3 | 4 |
  | leather cap / iron helm | head | 1 | 0 / 1 | L / 1 |
  | leather gloves / iron gauntlets | hands | 0 / 1 | 0 / 1 | L / 1 |
  | leather leggings / iron greaves | legs | 1 | 0 / 1 | 1 / 2 |
  | buckler / round shield | held | block 2 / 4 | 0 / 1 | 1 / 2 |

  The training yard keeps a padded jack, a leather cap and a buckler; the locked armory keeps mail, a helm, a round shield, gauntlets and greaves. Town guards wear studded leather and an iron helm, and carry a round shield.
- **Skills work both sides of it.** In armour: **Heavy Armour** (the Armour tree's entry, practised by taking hits in armour; eases the penalty by 1 per 10 levels, a tenth with each) and **Block** (the Shields tree's entry, practised by blocking; adds 1 to a shield's block per 10 levels). Out of it: the Adept's Quickened Reflexes, and an unarmoured Evasion later. Both entries are taught by the drill sergeant; their higher skills are to come.
- **Piercing answers armour.** `Pierce(n)`, a technique effect, ignores `n` points of armour: **Lunge** has 1 (the point finds a gap), **Backstab** 2 (between the plates). A passive can add piercing too, `(stat: Pierce, per: 10, with: "knives:basics")`. **Crush** ignores armour entirely, so it's the answer to plate.

## Stealth

Built.

- **Stealth** (`stealth:basics`, Agility) is its tree's entry: `hide` (or `use stealth`, a skill made of effects) to slip out of sight, never in a fight, `sneak` to stay hidden on the move. The hooded figure in Riftwatch's Back Alley teaches it (and Lockpicking and Pickpocketing), and the Poacher and Street Urchin backgrounds start with it. **Shadowing** (follow someone unseen) is its journeyman skill and **Vanish** (hide mid-fight) its capstone, both to come.
- **Noticing** is an opposed roll: an observer's Intuition against the hider's Stealth plus Agility, rolled when they hide, when anyone walks into the room, and when someone `search`es, with double their Intuition. Those who notice still see them; to everyone else they're gone: not listed, not targetable, and their comings and goings unannounced. Mobs see by the same rules, so aggressive mobs leave unnoticed players alone.
- **Acting openly shows you** ("Grok steps out of the shadows."): any interaction or communication, unless it's quiet (whispering, searching, hiding, tracking), and walking away. Looking, checking your inventory and other character commands don't. **Sneaking** keeps you hidden as you move: those who noticed you see you go, and in each new room everyone rolls again.
- **Quiet things done from hiding are seen only by those who've noticed you:** reading tracks or weighing an item's worth in the shadows doesn't tell the rest of the room "Grok studies the ground." (2026-10-27; the rule is in `output.rs`, for every room message about a hidden character.)
- **From hiding, an opener can't be avoided:** a Backstab on someone who hasn't noticed you can't be dodged or blocked. Once blows are struck, everyone in the fight is seen.
- **Later:** skills that add to Backstab, such as more damage from hiding or an easier crushing blow; mobs that hide and ambush; hidden doors found with `search`.

## Healing

Built.

- **First Aid** is the Healing tree's entry skill (Intuition), taught by the herbalist and granted by the Soldier and Herbalist's Apprentice backgrounds, who also start with two bandages. `bandage [<someone>]` (or `use first aid [<someone>]`) uses up a bandage you carry. Whether it helps is your First Aid (level plus Intuition) against half the health they've lost, by opposed roll, so a bad wound is harder to treat. Done well, it heals half your First Aid plus 1 to 3, and stops bleeding; fumbled, the bandage is spoiled. It's made of effects in `healing.skill.ron`, the first active skill to be ([Active skills, as built](effects.md#active-skills-as-built)). It's an action, so in a fight it takes your turn, on yourself or anyone else there, player or mob.
- **Consumables** are items with a `consumable` section, used up by `eat` or `drink` (also actions). What they do is their `effects` ([items.md](items.md#consumables)). **Food and drink don't heal**; they make you `nourished`, which speeds recovery of health and stamina for a while, on top of resting and never in a fight: a loaf of bread at strength 2 (half again as fast) for 80 game minutes (10 real), a flask of water at 1 (a quarter faster) for 40. Another meal keeps the stronger and the later end, so eating more makes it last, not faster. **Healing draughts** `Heal` at once, 4 to 8 health rolled with Luck, so they're what to drink in a fight.
- **Where to find them:** the herbalist sells bandages and healing draughts, and the provisioner bread and water ([Shops](economy.md#riftwatchs-shops)); the guild cellar's chest has bread and water.
- **Later:** Treat Poison and Surgery (expert), Herbalism and Brewing to make draughts and poultices, and waterskins that refill (they're carried but can't yet be drunk from).

## Gathering

Built.

- **Foraging** (the Survival tree's entry, Intuition) finds food with `forage`; **Herbalism** (the Nature tree's entry, Intuition) finds herbs with `gather`. Both are data: `Find` aimed at the room ([Every skill as data](effects.md#every-skill-as-data)). The herbalist teaches both, and the Poacher, Farmhand and Herbalist's Apprentice backgrounds start with them.
- **What there is** comes from the room's [environment](world.md#environment-and-finds). Finding anything is your skill (level plus Intuition) against 3 (the skill's `against`), by opposed roll; what you find is picked from the room's table with Luck, so luck favours the rarer end. It goes to your hand, or your feet if your hands are full, and is saved at once.
- **Each room holds three finds** of each kind, growing one back every 10 minutes.
- **Finds today:** the Darkwood has wild berries, mushrooms and hazelnuts (food that speeds recovery), and yarrow, kingsfoil and moonpetal (herbs); the sewers have the odd stale crust, and glowcap.
- **Herbs have no use yet:** they're the makings of Brewing, draughts and poultices, which come later.

## Open questions

- [ ] **Is the four-tier shape right,** and are the level gates (15, 30, 45) the right spacing for a 0–50 skill?
- [ ] **How many skills per tree** to start: the five or so above, or fewer?
- [ ] **What stops one character learning every tree?** Still the biggest open question in [Skills](skills.md#open-questions). Trainers make it pressing: with free lessons, anyone can open every tree, though now only at its entry.
- [x] **Do lessons cost money?** Spells do, except each tradition's starter; skills don't (2026-10-26). See [Lessons](economy.md#lessons).
- [ ] **Shields and Archery:** in from the start, or later?
- [ ] **Crafting:** one tree, or one per craft?
