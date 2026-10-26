# Effects, conditions and scripting

[Design summary](README.md) · Status: step 1, conditions as data, is built (see [Conditions, as built](#conditions-as-built)); the rest is agreed and to come. ✓ marks what's built, in data or still hard-coded

**The goal: game designers add skills, spells, conditions and items without asking for code.** Today a designer can already make a new spell or technique from the effects that exist, with any numbers and wording. What still needs code is anything *new in kind*: a new effect ("poison", "drain life"), a new condition and what it does, a new active skill (each has its own command, like `pick` or `track`), a new trigger, or a formula other than the built-in ones.

This doc proposes the vocabulary that removes those needs: **conditions as data**, **one effect vocabulary** shared by spells, techniques, skills and items, **formulas** in data, **triggers** for reactions, and, last, **scripting** (Rhai) for the long tail that data can't express.

## The pieces

| Piece | What it is | Example |
| --- | --- | --- |
| [Conditions](#conditions) | A state on a character, a room or an item, made of modifiers | Poisoned: loses 2 health a turn for 5 turns |
| [Damage types](#damage-types) | What kind of harm, for armour and resistances | fire, piercing |
| [Effects](#effects) | What happens: the verbs | damage, heal, apply a condition, teleport |
| [Abilities](#abilities) | One shape for everything a character does on purpose | a spell, a technique, a skill, a potion |
| [Formulas](#formulas) | Amounts written in data | `2d5 + power / 3` |
| [Triggers](#triggers) | When reactions and passives fire | when hit, at the start of a turn |
| [Scripting](#scripting) | Rhai, for what none of the above can say | a sword that glows near orcs |

## Conditions

A condition is data, in `assets/conditions/*.conditions.ron`: a name, what it does (a list of [modifiers](#modifiers)), how long it lasts, and what it says. Blessed, Warded, Held and Bleeding stop being code.

```ron
"poisoned": (
    name: "poisoned",
    description: "losing health to poison",          // its line in `affects`
    tags: [poison, physical],
    lasts: Turns(5),
    stacking: Stack(3),                                // up to three doses
    modifiers: [HealthEachTurn("-2")],                 // a formula, per dose
    text: (
        applied: (bearer: "You feel poison burn in your veins.", room: "{bearer} looks ill."),
        fades: (bearer: "The poison works its way out of you."),
        looks: ["looks pale and sweating"],           // the hint others see on `look`
    ),
),
```

**Every condition has:**

- **Duration:** a number of the bearer's turns; a game-time span (✓ blessings and wards); until removed (cured, broken or dispelled); or while something holds (while wielding, while in the room).
- **Stacking:** replace (✓ today's rule), refresh the duration, stack up to N times, or keep the stronger.
- **Tags** that cures, immunities and dispels refer to: `magic`, `poison`, `disease`, `mental`, `physical`, `curse`, `blessing`, and any a designer adds.
- **Breaks on:** being damaged (sleep), acting (✓ hidden), moving, entering a fight, resting.
- **Text:** when applied, when it fades, a hint others see (see [What others see](#what-others-see)), and its line in `affects` (✓). Optional: a silent condition shows only in `affects`.
- **On a character, a room, or an item.** A room can be dark or consecrated; a sword can be flaming (see [Item conditions](#item-conditions)).
- **Saved when they last in time.** A condition lasting a game-time span is saved with its bearer, as the real time it ends, so logging out or a restart doesn't cut it short or stretch it (a blessing cast before logging out may be gone when you return). Conditions counted in turns belong to a fight and end with it, and a fight can't outlast a logout.

### Modifiers

The building blocks; a condition is any mix of them. Amounts are [formulas](#formulas), so they can scale with the condition's strength or its caster's power.

**Ratings**

| Modifier | Does | Today |
| --- | --- | --- |
| `Attack(n, with)` | Attacks better, or worse; optionally only with one skill or tree | ✓ Blessed |
| `Defence(n)` / `DefenceTimes(x)` | Evasion up or down, or multiplied | ✓ Guarding; ✓ Down, Held, Exposed halve it |
| `Block(n)` | A shield's block | |
| `Armour(n)` / `Pierce(n)` | Takes more off blows; finds more gaps | |
| `DamageDealt(x)` / `DamageTaken(x)` | Multiplies damage done or taken | |
| `Initiative(n)` | Acts sooner or later | |
| `Attribute(attr, n)` | Any of the ten | |
| `Skill(skill_or_tree, n)` | A skill's effective level | |
| `SpellPower(n)` / `DrainResistance(n)` | For casters | |
| `Stealth(n)` / `Notice(n)` | Hiding, and spotting the hidden | |
| `Carry(n)` / `MoveCost(x)` | Bulk limit; stamina to move | |

**Vitals**

| Modifier | Does | Today |
| --- | --- | --- |
| `MostHealth(n)` / `MostStamina(n)` | Raises or lowers the maximum | |
| `Recovery(x)` | Recovers faster or slower | ✓ Nourished (food) |
| `HealthEachTurn(n)` | Hurts (bleeding, poison, burning) or heals (regenerating) each turn | ✓ Bleeding |
| `HealthEachMinute(n)` | The same out of a fight | |
| `StaminaEachTurn(n)` | Drains or restores stamina | |

**Pools**

| Modifier | Does | Today |
| --- | --- | --- |
| `Absorb(n)` | Takes the next N damage, then is gone | ✓ Warded |
| `Reflect(n)` | Returns N damage to whoever strikes | |
| `IgnoreUnder(n)` | Blows under N do nothing | |

**Restrictions**

| Modifier | Does | Today |
| --- | --- | --- |
| `LosesTurn` | Loses its next turn | ✓ Staggered, Down |
| `CantAct` | Can't act at all (paralysed, asleep) | |
| `CantFlee` / `CantMove` | Held in place | ✓ Held |
| `CantCast` | Silenced | |
| `CantUseTechniques` | | |
| `WontAttack` | Won't start or continue a fight | ✓ Calm (one-off today) |
| `Blind` | Can't target; defends at half | |
| `Mute` | Can't speak | |
| `Untargetable` | Sanctuary | |

**Behaviour**

| Modifier | Does |
| --- | --- |
| `Taunted(by)` | Must attack whoever taunted it |
| `Feared` | Tries to flee |
| `Charmed(by)` | Won't attack the charmer, and fights for them |
| `Confused` | Picks targets at random |

**Senses and nature**

| Modifier | Does | Today |
| --- | --- | --- |
| `SeesHidden` / `SeesInvisible` | | |
| `SeesInDark` | | |
| `SensesMagic` | Sees magic conditions on people and things | |
| `Hidden` | Out of sight to those who haven't noticed | ✓ Hidden |
| `Invisible` | Unseen by all but those who see invisible | |
| `Resist(type, x)` / `Immune(tag)` | Takes less of a damage type; can't get a tag's conditions | |
| `BreathesWater` / `FreeClimb` / `FreeSwim` | | |
| `Hasted(n)` / `Slowed` | An extra action every N turns; acts every other turn | |

**Reactions:** a condition can carry [triggers](#triggers): thorns is "when hit, deal 2 to the attacker", and a death-curse "when killed, apply Cursed to the killer".

### What others see

**Others get hints, not numbers.** Each condition can have a `looks` line, shown when someone looks at its bearer: a ward is "a faint shimmer surrounds them", Shielded "their skin has a dull, stony sheen", Poisoned "they look pale and sweating", Blessed "a warm light clings to them". Its bearer sees the name and the time left with `affects` (✓); everyone else sees only the hint.

- **Hints can follow strength:** a ward's shimmer is "bright" while it has plenty left and "flickering" when it's nearly spent, so a watchful fighter can tell when it's about to fail. A condition gives a list of hints, each from a share of its strength.
- **In the room, a word:** besides the full line on `look`, a condition can give a short tag shown after its bearer in the room's list, "A town guard keeps watch. (shimmering)", for things you'd notice at a glance. Most conditions don't: a tag is for what's obvious, like flames or a glowing ward.
- **Senses show more:** `SensesMagic` shows the names and strength of magic conditions, and later skills (a Lore or Perception skill, or `consider`) can do the same for others. Hidden and silent conditions show nothing at all.
- **Items too:** an imbued or enchanted item's hint shows when you look at it, or at whoever carries it: "the blade glistens with something dark".

### Item conditions

Conditions on items come in two kinds, which behave differently.

**Granted by an item's template: permanent, and data only.** A template says what its item gives whoever wears or wields it, the way armour's protection works today:

```ron
"ring_of_the_owl": (
    name: "a silver owl ring",
    wear: Finger,
    grants: [(condition: "sees_in_dark", while: Worn)],
),
"angorim": (
    name: "Angorim",
    weapon: (...),
    grants: [(condition: "orc_bane", while: Held)],   // more damage against orcs, and a glow near them
),
```

- Put the ring on and you see in the dark; take it off and you don't. Nothing is saved: it comes from the template.
- `while` is `Worn`, `Held`, or `Carried` (anywhere on you, like a lucky charm).
- This covers most magic items: rings, amulets, enchanted weapons and armour. A cursed item is `grants` plus a `CantRemove` modifier on the item itself.

**Put on one particular item: temporary, and saved with it.** The `Imbue` effect puts a condition on *this* sword: a venom coating, a Flame Weapon spell, holy water, a blade rusted by a rust monster. These belong to the individual item, like its [quality](items.md#quality-and-rarity), and real items are already saved, so they go in an `item_conditions` table.

- **Durations:** game-time spans are saved as the real time they end, so they survive a restart; items don't take turns, so instead of turns an item condition counts **charges**, used up as it works ("your next 3 hits").
- **Where an item condition reaches:**

  | Reach | Does | Example |
  | --- | --- | --- |
  | `Bearer` | Its modifiers pass to whoever holds or wears it | sees in the dark; +2 damage |
  | `OnHit` | Its effects land on whoever the item strikes, using a charge | venom: Poisoned |
  | `Item` | It changes the item's own stats | rusted: −1 damage until repaired |

- Untouched items in the world (spawned, not yet real) can carry template grants, but an `Imbue` makes an item real, so it's saved.

### The starting conditions

| Already built (to move into data) | New |
| --- | --- |
| Staggered, Down, Bleeding, Held, Guarding, Exposed, Blessed, Warded, Hidden, Nourished | Poisoned, Burning, Chilled (slowed), Stunned, Asleep, Rooted, Silenced, Blinded, Weakened, Strengthened, Hasted, Slowed, Shielded (armour), Invisible, Feared, Taunted, Calmed, Regenerating, Inspired, Exhausted, Sanctuary |

### Conditions, as built

*Built* (step 1). Conditions are data in `assets/conditions/*.conditions.ron`, hot-reloaded; the code is `src/combat/states.rs`.

- **Seventeen conditions:** the eight that were code (staggered, down, bleeding, held, guarding, exposed, blessed, warded) with their wording unchanged, and nine new ones: poisoned (stacks to 5 doses), burning, stunned, regenerating, rooted, silenced, shielded, weakened, strengthened. Hidden and Nourished stay as they were for now.
- **Modifiers built:** `Attack`, `Defence`, `DefenceTimes`, `Armour`, `DamageDealt`, `DamageTaken`, `HealthEachTurn`, `Absorb`, `LosesTurn`, `CantFlee`, `CantMove`, `CantCast`. Additive ones count once per point of strength. The rest of the list above arrives as something needs it.
- **Stacking:** `Replace`, `Strongest`, `Stack(n)`. **Tags** are kept for cures and immunities to come.
- **Text from data:** applied (to the target of a technique), each turn, a lost turn, death, fading, a ward taking a blow and breaking, and `looks` hints, chosen by strength left. `affects` lists names, what each does and how long, and `score` the names.
- **Using any condition:** techniques have `Inflict(condition, strength, rounds)` for the target and `Gain(...)` for the user; spells have `Apply(condition, strength, lasts)`, where `lasts` is `Minutes(n)`, `Hours(n)` or `Turns(n)`. Stagger, Bleed, Bless, Ward and the rest are now shorthands for these.
- **Items grant conditions:** `grants: [(condition: "shielded", strength: 1, while: Worn)]` on a template, `Worn`, `Held` or `Carried`, kept in step whenever items move.
- **Saved:** conditions lasting in time are saved with players (`character_conditions`), as when they end, every minute and at logout, and restored at login if they still last.
- **Checked:** tests fail if a technique, spell or item names a condition that doesn't exist.

## Damage types

Every attack and harming effect names its type, so armour, resistances and immunities work. **The types are data** (`assets/damage_types.ron`), so designers can add more; these are the first:

| Group | Types | Armour helps |
| --- | --- | --- |
| Physical | `physical` | Yes (✓ every weapon and natural attack today) |
| The elements | `fire`, `cold`, `lightning`, `wind`, `earth`, `water` | Only armour that says so |
| The energies | `positive`, `negative` | Only armour that says so |
| Light and dark | `light`, `shadow` | Only armour that says so |
| Others | `force`, `poison` | Only armour that says so |

- **Every weapon and natural attack is `physical`** (2026-10-26). There's no slashing, piercing and crushing split: a sword and a hammer differ in their skills and techniques, not their damage type. Splitting physical damage can come later if armour wants to care.
- **Magic picks its type:** Spark is `force`, a fire spell `fire`; Evocation's fire and lightning and the shamanic Elements school's wind, earth and water are the elements.
- **The energies** are life and death: `positive` heals the living and hurts the undead, `negative` the reverse, once there are undead. **Light** and **shadow** are holy and unholy power, and the dark.
- **Armour stops physical damage** unless it names more (`armour: (reduction: 3, against: [physical, fire])`), so a fire-proofed cloak or a blessed shield can be made in data. Resistances and immunities come from conditions (`Resist(fire, 0.5)`, `Immune(poison)`).

## Effects

What spells, techniques, active skills, items and reactions *do*: a list, each with parameters. Amounts are [formulas](#formulas).

**Harm and healing**

| Effect | Does | Today |
| --- | --- | --- |
| `Damage(amount, type, armour, blows)` | Hurts; landing harder on a decisive win if `blows` | ✓ Harm, every attack |
| `Heal(amount)` | Health back, up to the most | ✓ Mend, bandage, draughts |
| `Restore(amount)` / `Drain(amount)` | Stamina back, or taken | ✓ draughts |
| `Leech(amount, type, share)` | Damage, and heal the user by part of it | |

**Conditions**

| Effect | Does | Today |
| --- | --- | --- |
| `Apply(condition, strength, lasts)` | Puts a condition on the target | ✓ every condition |
| `Remove(condition)` / `Cure(tag)` | Ends one, or all with a tag | ✓ bandage stops bleeding |
| `Dispel(power)` | Ends `magic` conditions, by a contest against each one's power | |
| `Extend(condition, by)` / `Shorten(condition, by)` | | |

**Fighting**

| Effect | Does | Today |
| --- | --- | --- |
| `StartFight` / `EndFight` | | ✓ openers, Calm |
| `Taunt` | Applies Taunted | |
| `Interrupt` | Cancels a readied technique or spell | |
| `KnockBack(direction)` / `Pull` | Into, or out of, the next room | |

**The world**

| Effect | Does | Today |
| --- | --- | --- |
| `Teleport(room)` / `Recall` | Somewhere, or to a remembered place | ✓ the shrine on death |
| `Move(direction)` | Through an exit | ✓ walking |
| `Door(open \| close \| lock \| unlock)` | | ✓ pick, bash |
| `Reveal` / `Hide` | Shows the hidden here; hides the target | ✓ search, hide |
| `WorldFlag(flag, on)` | Sets off a world event | ✓ world events |
| `RoomCondition(condition, lasts)` | Darkness, fog, consecrated ground | |

**Creation**

| Effect | Does | Today |
| --- | --- | --- |
| `Create(item, quality)` | | ✓ gathering |
| `Consume(item)` | Uses up something carried | ✓ bandages, food |
| `Summon(mob, lasts, allied)` | Calls a mob, for a time, on the user's side | |
| `Imbue(condition)` | Puts a condition on an item: a flaming sword | |

**Knowing**

| Effect | Does | Today |
| --- | --- | --- |
| `Identify` | An item's worth and stats | ✓ appraise |
| `Tracks` | Reads who passed through | ✓ track |
| `SenseMagic` / `Scry(room)` | | |
| `Consider` | How outmatched you'd be | ✓ worked out every round |

**Rewards**

| Effect | Does | Today |
| --- | --- | --- |
| `Coins(amount)` | | ✓ mob loot |
| `Teach(skill \| spell)` | A scroll teaching | |
| `Experience(amount)` | For quest-like effects | ✓ victories, discoveries |

**Composition**

| Effect | Does |
| --- | --- |
| `Chance(p, effects)` | Happens only some of the time |
| `OnSuccess(effects)` / `OnFailure(effects)` | Branches on the contest: a resisted spell can still do half |
| `Area(who, effects)` | All foes here, all allies, everyone, the room |
| `Repeat(n, effects)` / `Delay(turns, effects)` | |
| `Say(text)` | Text templates (✓ every skill's `text`) |
| `Script(name)` | Hands off to a [script](#scripting) |

## Abilities

One shape for everything a character does on purpose: **spells, techniques, active skills and usable items all become abilities.** What makes a spell a spell is what it's cast through (a school) and its cost (Drain), not a separate system.

```ron
"venom_strike": (
    name: "Venom Strike",
    description: "A blade coated in venom: it bites, and the poison stays.",
    target: Foe,
    range: Here,
    cost: [Stamina("2"), Consume("venom")],
    cooldown: Secs(10),
    takes: Round,
    contest: (rating: "attack(knives:basics)", against: "evasion"),
    requires: Skill("knives:basics", 15),
    effects: [
        Damage(amount: "weapon + 1", type: piercing, armour: true, blows: true),
        OnSuccess([Apply(condition: "poisoned", strength: "1", lasts: Turns(4))]),
    ],
    text: (...),
),
```

Each ability has:

- **Target:** self, an ally, a foe, any creature, an item, a door, the room, a direction, an area.
- **Range:** this room, or the next.
- **Cost:** stamina, health, items used up, coins, Drain (✓ spells).
- **Cooldown,** and **how long it takes:** instant, one round (✓ actions), or a cast over several rounds that can be interrupted.
- **Contest:** the user's formula against the target's, or against a fixed difficulty (✓ every contest, by the ratio rule), or none.
- **Requirements** in the condition language (✓).
- **Effects** and **text** (✓).

`use <ability> [<target>]` runs any of them, and `cast` stays for spells. Built-in commands like `pick` or `track` remain only for what's truly unlike anything else; most become abilities.

## Formulas

Amounts, ratings and chances are written as small expressions, checked when the file loads:

```
2d5 + power / 3          weapon + 1          level * 2          max(1, strength - 2)
```

**Inputs:**

- dice: `NdM`
- `power`: the school's or skill's rating (✓ spell power)
- `level`, `skill(key)`, and the user's attributes by name
- `target.<attribute>`, `target.health`, `target.most_health`
- `margin` and `blow`: how decisively the contest was won (✓ blows)
- `character_level`
- `quality`: an item's quality bonus
- `strength`: a condition's strength
- `weapon`: the weapon in hand's damage roll

**Operators:** `+ - * /`, `min`, `max`, `floor`, `round`.

There are no loops and no assignments. Anything that needs more is a script.

## Triggers

When reactions and passive skills fire. Passives already practise on some of these (✓ stamina spent, dodged, blocked, hit in armour, damaged, used a skill or tree).

- **In a fight:** when hitting someone, being hit, missing, dodging, blocking, killing, dying; at the start and end of the bearer's turn.
- **Health:** when hurt below a share of health.
- **Magic:** when casting, when a spell lands on you, when a condition is applied or fades.
- **Moving and living:** when entering or leaving a room, resting, eating or drinking, using a skill or tree; each heartbeat out of a fight.

A trigger can also check a condition from the condition language (✓): "only while wielding an axe", "only if health is under a quarter".

```ron
// Thorns: a condition that hurts whoever hits its bearer.
reactions: [(on: Hit, effects: [Damage(amount: "2", type: piercing, armour: false, to: Attacker)])],
```

## Scripting

**For the long tail data can't express:** Angorim glowing when orcs are near, a quest's logic, a boss changing phase at half health. Most designs should never need it; if many do, the vocabulary above is missing something, and the fix is to add to it.

- **Language: [Rhai](https://rhai.rs).** It reads like JavaScript or Rust, which designers pick up quickly, embeds easily in Rust, and is sandboxed by default: no file or network access, and limits on operations and memory, which matters once scripts can be written on a live server. (Steel, a Scheme, was the alternative: powerful, but a harder sell to non-programmers, and younger.)
- **Scripts act only through the effect verbs** (`damage`, `apply`, `teleport`, `say`…) **and read only what formulas can read.** They never reach into the game's internals, so refactoring the engine doesn't break them, and a script can't do anything an ability couldn't.
- **Where they hook in:** a `Script("name")` effect, and a script reaction on any trigger. Scripts live in `assets/scripts/`, hot-reloaded like everything else, and are checked when they load.
- **Admins only, for now** (2026-10-26): scripts are written by admins, through the repository's `assets/scripts/`. Letting builders or GMs write them in game waits until the sandbox has been proved.
- **Built last,** once conditions, effects, formulas and triggers exist: those *are* the scripting API, so building data first designs it.

## How it gets built

Each step leaves the game working and designers with more they can do alone.

1. **Conditions as data** (*built*). The built conditions move to `assets/conditions/`, made of modifiers; combat, recovery and `affects` read modifiers instead of knowing each condition. Adds the new conditions, saving time-based ones with characters, and template `grants` on items.
2. **Effects and abilities.** One effect vocabulary and one ability shape; spells and techniques become abilities, and damage types arrive. `Imbue` and saved item conditions come with it.
3. **Formulas.** The expression evaluator, and every amount moved into data.
4. **Triggers and reactions.** More game events; reactions on conditions, skills and items.
5. **Active skills through `use`.** Most bespoke commands become abilities.
6. **Scripting.** Rhai, exposing the verbs and triggers above.

## Open questions

- [x] **How do item conditions work?** Template grants are data and need no saving; conditions put on one item are saved with it, game-time spans as real end times and the rest as charges. See [Item conditions](#item-conditions) (2026-10-26).
- [x] **Do conditions save with characters?** Yes, those lasting a game-time span, as the real time they end; fight-turn conditions end with the fight (2026-10-26).
- [x] **How much is shown?** Hints by default, like a shimmer around a ward, following its strength; names and numbers only to the bearer, or with `SensesMagic` and later skills. See [What others see](#what-others-see) (2026-10-26).
- [x] **Who may write scripts?** Admins, for now (2026-10-26).
- [x] **What damage types?** Every weapon and natural attack is `physical`; magic uses the elements (fire, cold, lightning, wind, earth, water), the energies (positive, negative), light and shadow, force and poison. The list is data (2026-10-26).
