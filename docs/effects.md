# Effects, conditions and scripting

[Design summary](README.md) · Status: steps 1 to 4 are built: conditions as data, damage types, one effect vocabulary, one resolver, resisting by tag, and item conditions (see the "as built" sections), formulas (step 3), and reactions in a fight and out of one (step 4). Step 5 has begun: active skills can be made of effects (5a); the rest is agreed and to come. ✓ marks what's built, in data or still hard-coded

**The goal: game designers add skills, spells, conditions and items without asking for code.** Today a designer can already make a new spell or technique from the effects that exist, with any numbers and wording. What still needs code is anything *new in kind*: a new effect ("poison", "drain life"), a new condition and what it does, an active skill that needs a new kind of target or effect (every skill in the world is data now: First Aid, Lockpicking, Foraging, Pickpocketing, Tracking, Hiding), a new trigger, or a formula other than the built-in ones.

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
| `Taunted(by)` | Must attack whoever taunted it, whatever its pack's limit ([Groups](groups.md#taunting)) |
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
| `Resist(type, x)` / `Weakness(type, x)` / `Immune(tag)` | Stops a share `x` of a damage type (0.8 = 80%); takes `x` more of one (1 = double); can't get a tag's conditions | |
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

#### Item conditions, as built

Step 2d (2026-10-26). The `reach` column above became simpler: a condition on an item works by what it has, so there's nothing to choose.

- **`Imbue(condition, strength, lasts, charges, on)`** puts a condition on the weapon in hand of whoever it's `on`, replacing one of the same kind: `Imbue(condition: "flaming", lasts: Some(Minutes(80)), charges: Some(10))`. It lasts for its game time or its charges, whichever runs out first; neither, and it stays until something takes it off. `lasts: Turns(..)` is refused at load, since items don't take turns. With no weapon in hand, the user is told so.
- **Its modifiers pass to whoever holds or wears the item** (not to a pack it's in), through the same grants as template ones, so `affects` lists it "from a wooden practice sword".
- **Its `on_hit` effects ride on every blow the weapon lands,** using a charge: a condition's new `on_hit: [...]` list, in the same effect vocabulary. Their [formulas](#formulas-as-built) can use the item condition's `strength`: `flaming` is `Damage(amount: "roll(1, 3) * strength", type: "fire")`, so at strength 2 it burns for 2 to 6, and `envenomed` poisons at `strength: "strength"`. `Imbue`'s own `strength` is a formula too, so a spell can make it grow with power: `strength: "1 + floor(power / 5)"`. A charge is spent only when the blow lands; the last one is told in the condition's `fades` words.
- **Saved** in `item_conditions` (game-time ends as unix seconds, charges as a count) whenever they change, loaded when a real item appears, and ended by the heartbeat when their time is up, told to whoever has the item.
- **An imbue doesn't make an item real** (a departure from the plan above): a player's gear is real already, and a mob's weapon keeps its conditions until a player picks it up, when they're saved with it.
- **`examine`** shows them: "It's flaming: burning whatever it strikes, 1 to 3 fire damage for each point of its strength (1) (9 hits left, for 1 hour 18 minutes)."
- **The first:** `flaming` and `envenomed` (poisons what it cuts), and Hermetic's **Kindle Blade** spell, taught by the magister.
- Not yet: the `Item` reach (a condition changing the item's own stats, like rust), `CantRemove` for cursed items, and imbuing armour.

### The starting conditions

| Already built (to move into data) | New |
| --- | --- |
| Staggered, Down, Bleeding, Held, Guarding, Exposed, Blessed, Warded, Hidden, Nourished | Poisoned, Burning, Chilled (slowed), Stunned, Asleep, Rooted, Silenced, Blinded, Weakened, Strengthened, Hasted, Slowed, Shielded (armour), Invisible, Feared, Taunted, Calmed, Regenerating, Inspired, Exhausted, Sanctuary |

### Conditions, as built

*Built* (step 1). Conditions are data in `assets/conditions/*.conditions.ron`, hot-reloaded; the code is `src/combat/states.rs`.

- **Twenty-one conditions:** the eight that were code (staggered, down, bleeding, held, guarding, exposed, blessed, warded) with their wording unchanged, nine new ones (poisoned, stacking to 5 doses, burning, stunned, regenerating, rooted, silenced, shielded, weakened, strengthened), since step 2 asleep, and flaming and envenomed for weapons, and since step 4 thorns, and on locks `locked`. **Hidden** is data too (2026-10-27): `hidden`, whose strength is how well they hid and whose `seen_by` is everyone who noticed them anyway, lasting `Indefinitely` (until they act openly, walk away without sneaking, or fight). It shows in `affects`, isn't saved over a logout, and spells, items and cures can put it on or take it off like any other; noticing and who sees whom stay in `stealth.rs`. Nourished stays as it was for now ([#20](https://github.com/Foxworks-Studios/riftward/issues/20)).
- **Modifiers built:** `Attack`, `Defence`, `DefenceTimes`, `Armour`, `DamageDealt`, `DamageTaken`, `HealthEachTurn`, `Absorb`, `LosesTurn`, `CantFlee`, `CantMove`, `CantCast`, and since step 2 `Resist`, `Weakness` and `Immune`. Additive ones count once per point of strength. The rest of the list above arrives as something needs it.
- **Stacking:** `Replace`, `Strongest`, `Stack(n)`. A condition remembers who put it on, the last to renew or add to it (2026-10-28), so whoever bleeds or poisons someone to death killed them: their kill, for experience, loot and quests. **Tags** are kept for cures and immunities to come.
- **Text from data:** applied (to the target of a technique), each turn, a lost turn, death, fading, a ward taking a blow and breaking, and `looks` hints, chosen by strength left. `affects` lists names, what each does and how long, and `score` the names.
- **Using any condition:** spells and techniques alike use `Apply(condition, strength, lasts, on)`, where `lasts` is `Minutes(n)` or `Hours(n)` of game time, `Turns(n)` (the bearer's turns in a fight), `Rounds(n)` (so many combat rounds of real time, 3 seconds each, in a fight or out of one: a startled deer's window) or `Indefinitely` (until something ends it, as hiding is) and `on` is `Target` (the default) or `User`. See [Effects, as built](#effects-as-built).
- **Items grant conditions:** `grants: [(condition: "shielded", strength: 1, while: Worn)]` on a template, `Worn`, `Held` or `Carried`, kept in step whenever items move.
- **Saved:** conditions lasting in time are saved with players (`character_conditions`), as when they end, every minute and at logout, and restored at login if they still last.
- **Checked:** tests fail if a technique, spell or item names a condition that doesn't exist.

## Traits

Decided and built 2026-10-27. **A trait is what someone is, for good:** a deer's nerves, a dummy being a thing, an elf's fey mind, a slime's rubbery body, a rift-touched mutation. A skill is what someone has learned and gets better at with use; a condition is what's happening to them, and passes. Traits are the third kind.

- **The same shape as a condition** (`assets/traits/*.traits.ron`): a name, a description, `modifiers`, `reactions` and words (`looks`), so everything conditions can do, a trait can, with one engine: a trait's resistances, immunities and reactions work exactly as a condition's do. Keys are shared with conditions, and the tests fail if one is defined twice.
- **Nothing ends one:** no cure, no `Remove`, no running out, no death. They're held with the bearer's conditions (`Lasts::Trait`), kept as an item's grants are.
- **Where they come from:** a mob template's `traits: ["skittish"]`, and a race's `traits` in `races.ron` (players and mobs of the race alike), each at strength 1. They aren't saved; they come back from the template or race at every spawn and login.
- **Not put on like a condition:** `Apply` and the admin `condition` command refuse a trait, and the tests fail if an effect or a mob's `conditions` names one, or `traits` names a condition. Effects that give or take a trait on purpose (lycanthropy, a rift's mark) would be their own, `Grant` and `Revoke`, so no ordinary cure reaches them; not built until something needs them.
- **Seen:** `score` lists them as Traits, not under Affected by; `affects` leaves them out; their `looks` show when someone looks at the bearer ("The red deer watches you warily, ready to bolt.").
- **The first:** `formless` (also known by name: no corpse, and what it held and leaves lies where it died; the sewers' slimes), `rubbery`, `splitting`, `shedding`, `mending` and `venomous` for [the sewers](riftwatch.md#the-sewers-ecology), `inanimate` (the code knows it by name, as it does `hidden`: a mob with it never acts in a fight, never wanders, is worth no experience and leaves no corpse, and it's immune to poison and the mind's arts), `skittish` (prey: see [the Darkwood](darkwood.md#prey-a-window-then-gone)), and the elves' `fey_mind`.
- **`Withstand(tag, n)`,** a modifier made for traits but open to conditions: the bearer resists anything tagged so this much better in the contest, per point of strength, as a skill's `Resist(tag)` bonus does (the best tag counts once). Fey Mind is `Withstand("sleep", 4)` and `Withstand("charm", 4)`.

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
- **Armour stops physical damage** unless it names more (`armour: (reduction: 3, against: ["physical", "fire"])`), so a fire-proofed cloak or a blessed shield can be made in data. Resistances and immunities come from conditions (`Resist("fire", 0.5)`, `Immune("poison")`).

### Damage types, as built

- **The list** is `assets/damage_types.ron`: each type's name and whether ordinary armour stops it (only `physical` does). Tests fail if a weapon, attack skill, spell, armour piece or condition names a type that isn't there.
- **Weapons and attack skills** take `type:` and default to `"physical"`; Spark's `Damage` is `type: "force"`.
- **Armour:** worn and held pieces count against a hit only if they stop its type (`against`, empty meaning what ordinary armour stops). Natural armour (auras, boosts) and armour from conditions count only against types ordinary armour stops.
- **Resistances:** a condition's `Resist(type, x)` is the share of that type it stops: 0.8 stops 80%, 1 all of it. Several stack on what gets through: 50% and 50% let a quarter in.
- **Weaknesses:** `Weakness(type, x)` is extra damage of that type, added up: 0.5 takes 150%, 1 takes double, and two weaknesses of 0.5 take double. Weaknesses multiply what resistances let through, so resisting 50% of fire while weak to it by 1 takes 100%. **`Immune(tag)`** stops a condition carrying that tag from being put on at all.
- Weapon blows and harming spells go through the same armour and resistance rules, for players and mobs alike.

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
| `Imbue(condition)` | Puts a condition on an item: a flaming sword | ✓ Kindle Blade |

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

### Effects, as built

Step 2b. Spells and techniques list their effects in one vocabulary (`src/effects.rs`), and the combat round carries them out one way (`src/combat/effects.rs`):

```ron
// A technique: down on the target if it lands, open yourself either way.
effects: [Apply(condition: "down", lasts: Turns(1)), Apply(condition: "exposed", lasts: Turns(1), on: User)],
// A spell: force damage; a ward that grows with the caster's power.
effects: [Damage(amount: "roll(2, 5) + floor(power / 3)", type: "force")],
effects: [Apply(condition: "warded", strength: "4 + floor(power / 3)", lasts: Minutes(80))],
```

| Effect | Does | In |
| --- | --- | --- |
| `Damage(amount, type)` | `amount` is a [formula](#formulas-as-built), harder on a decisive win; through armour and resistances | Both |
| `Heal(amount)` | `amount` is a formula; health back, up to the most | Both |
| `Apply(condition, strength, lasts, on)` | Puts a condition on, at `strength` (a formula, 1 unless it says). Told in the condition's `applied` words | Both |
| `Remove(condition, on)` / `Cure(tag, on)` | Ends one condition, or every one with a tag, told in its `fades` words. Item grants stay | Both |
| `Pierce(n)` / `IgnoreArmour` | How damage gets past armour | Both |
| `Imbue(condition, strength, lasts, charges, on)` | Puts a condition on the weapon in hand; see [Item conditions, as built](#item-conditions-as-built) | Both |
| `Cleave` | The blow carries on into a second foe | Techniques |
| `Calm` | An animal stops fighting | Spells |
| `Take(what)` | Something lifted from the target and given to the user. `Take(Coins(amount))`: coins from their purse into the user's, as many as `amount` (a formula, in copper) says, up to what they have; the text names the haul as `{coins}`. Items and conditions would be more kinds of `what`. Not from players, for now | Active skills |
| `Spawn(mob, count)` | `count` (a formula, 1 by default, at most 10) mobs of a template appear where the user is, and join the fight against the target unless it's the user: an ooze splitting, the Slime Lord shedding slimes. `mob` is a template in the user's zone, or `zone:id`. They belong to no spawn, so once killed they're gone; brought in once the round is done (`combat/spawning.rs`), told in the reaction's own text | Reactions (`Hurt`, `Dies`), and any skill or spell |
| `Flee` | The user runs. In a fight they try to break away as the `flee` command does, with its stamina, its chance and `CantFlee`; out of one they bolt through a random way anyone could walk (no climbing, swimming or closed doors), and a mob keeps out of rooms it may not wander into or that hold what it `avoids`, while it has anywhere else to go (`combat/fleeing.rs`, once the round is done) | Reactions (*Skittish*, `startled` running out), and any skill or spell |
| `CallForHelp(rooms)` | Allies of the user within `rooms`, not busy, come running and join the fight against the target: a losing guard shouting (see [Reinforcements](world.md#mobs-fighting-mobs)) | Reactions, and any skill or spell |
| `Reveal(what)` | Something the user learns, and nothing else changes. `Reveal(Worth)`: an item's worth, named as `{worth}` (Appraisal, aimed at an item). `Reveal(Tracks)`: the tracks in the room, each read by its own roll against `against`, which sees `target.ground` and each track's `age` in minutes; those read are listed after the skill's line (Tracking, aimed at the room) | Active skills |
| `Find(kind)` | Something of a `kind` the room's [finds](world.md#environment-and-finds) list, into the user's hand, the better made the more surely it was found; see [Every skill as data](#every-skill-as-data) | Active skills aimed at the room |

- **On the user, whatever happens; on the target, if it lands** and the target is still standing.
- **Damage goes one way:** blows and spells both go through `mitigate`: worn armour by its `against`, natural armour and conditions only against what armour stops, less `Pierce`, times damage taken and resistances. A ward then takes what it can.
- **Any effect works in either** since 2c-ii: a technique can do fire damage on top of its blow, and a spell can cleave.
- The old shorthands (`Stagger`, `KnockDown`, `Bleed`, `Hold`, `Expose`, `Guard`, `Inflict`, `Gain`, `Harm`, `Bless`, `Ward`) are gone; every skill and spell file uses `Apply`.

### Resisting, as built

Step 2c-i. Spells and techniques are resisted the same way, and skills made for it help:

- **The contest:** the user's rating (a spell's power, a blow's or technique's attack) against what the target `resist`s with: `Evasion` (Reaction, Dodge and a shield, as against a blow), `Dodge` (no shield), or an `Attribute`. One opposed roll by the ratio rule, with both sides' Luck.
- **Tags:** what a spell or technique *is*, worked out from what it does, with nothing to keep in step: the tags of every condition it puts on, the types of damage it does, and its own `tags: [...]` for anything else (`"charm"`). A sleep spell is `sleep` and `mind` because the asleep condition is; Spark is `force`.
- **Skills that resist:** a passive skill's `(stat: Resist("sleep"), per: 10, base: 3)` adds to the target's rating against anything with that tag. Only skills that say so help, and only the best matching bonus counts, so a spell that's both `sleep` and `mind` doesn't count two. It's still a roll: a bonus counts for a lot against a weak caster and less against a strong one.
- **Traits that resist:** a trait's (or condition's) `Withstand("sleep", 4)` adds to the rating the same way, on top of skills. The elves' Fey Mind is one ([Traits](#traits)): it was a racial skill growing with its level until 2026-10-27, and is now flat, 4 against sleep and charm, since an elf doesn't practise being an elf.
- **Separate from conditions' `Resist` and `Immune`:** a skill helps win the contest; `Resist` takes damage off after it's lost, and `Immune` keeps a condition from taking hold at all.

### Abilities, as built

Step 2c-ii. Spells, techniques and plain blows are resolved by one piece of code (`src/combat/resolve.rs`); each keeps only what's truly its own.

- **Shared by both:** `resist`, `tags`, `effects` and `text`, and everything after the user has paid: effects on the user, the contest with the target's skills that resist it, then on a win the blow's damage and every `Damage` effect through armour, resistances and wards, `Heal`, the conditions, `Cleave`, all told in the user's words.
- **A spell's own:** its school and the level it needs, its Drain, its price, and its `aim`. Its `power` is an input to its formulas: the first spells add a third of it to their damage, healing and wards, as their data says.
- **A technique's own:** the attack it's made `with` (whose weapon must be ready), its stamina `cost`, `accuracy` and extra `damage` on the blow, whether it strikes (`Hit`, `Harmless`, `None`), and whether it's an `opener`.
- **Techniques name their contest too:** `resist` defaults to `Some(Evasion)`, as a blow; Grapple is now `Some(Attribute(Strength))`, your grip against their Strength. `None` can't be resisted.
- **Missing:** resisted by `Evasion`, it's dodged or blocked, in the defender's skill's words, and a dodge can be riposted. Resisted any other way, it just misses, in its own `miss` words, and isn't a dodge.

- **Spells are cast:** `cast` stays the verb for them, and `use <spell>` finds a spell when no skill matches and casts it, revealing a hidden caster as `cast` does (2026-10-26).

Still to come: one file shape for all of them (a spell, a technique and an active skill still live in their own files, with their own fields), and costs beyond stamina, Drain and an item used up.

### Active skills, as built

Step 5a (2026-10-27). An active skill is either a command, for what's like nothing else (none left in the world's data since 2026-10-27; the field stays for one that needs it), or made of effects, in its `.skill.ron`, and resolved by `resolve.rs` like any spell or blow. First Aid is the first:

```ron
"first_aid": (
    name: "First Aid",
    kind: Active(
        uses: "bandage",                                     // an item with this tag, used up each try
        against: "(target.most_health - target.health) / 2", // a fixed difficulty, a formula
        effects: [
            Heal(amount: "floor(power / 2) + roll(1, 3)"),
            Remove(condition: "bleeding"),
        ],
    ),
    text: (hit: (...), miss: (...)),
),
```

- **Fields:** `aim` (`Ally`, the default: yourself unless you name someone; `Foe`; `Other`; `Lock`, a door or closable container, by direction or name; `Room`, the room the user is in; `Item`, an item the user can reach, carried or here; `Me`, the user and no one else), `cost` (`Stamina(n)`), `cooldown` in seconds, `uses` (an item tag), `resist` (what the target resists with, as a spell's) or `against` (a fixed difficulty, rolled without Luck), `tags`, `effects`, `provokes`: when it fails, a mob it was tried on turns on the user, and `quiet`: using it doesn't show a hidden user (Tracking, Appraisal). Neither `resist` nor `against`: it always works. An `Other` skill needn't tell its target anything: a pickpocket's mark doesn't notice.
- **Power:** the user's skill, its level plus its attribute, is both their side of the contest and `power` in its formulas.
- **Using one:** `use first aid [<someone>]`, in a fight or out of one; it's an action, so in a fight it takes the user's turn. Old commands can stay as shortcuts: `bandage` is `use first aid`. A hidden user is revealed, as casting does.
- **Refusals cost nothing:** one that only heals and cures isn't used on someone unhurt with nothing to cure ("Bob isn't hurt."); one that `uses` an item needs one carried ("You have no bandages."); then the cooldown and the stamina. Once it's tried, the item is used up whether or not it worked, and the skill is practised.
- **At a foe** (`aim: Foe`), it starts a fight, or carries one on, and the target fights back.
- **Checked at load:** a command or effects, not both; text for its target, a `miss` when it can fail, and `{harm}` in every line if it does damage; every condition it names exists.
- **Mobs use them too** (2026-10-27): each turn of a fight, with their `tactics` chance, beside techniques and spells, any they know that's ready, affordable, with what it uses up to hand, and would do some good (a heal below half health, a condition the target hasn't got), but nothing that makes no sense mid-fight (hiding, stealing, finding, reading). A hurt mob with First Aid and a bandage binds its own wounds.
- **Still to come:** more skills made this way.

### Every skill as data

Decided 2026-10-27. Every skill becomes data, and the commands that do skills today (`pick`, `bash`, `forage`, `gather`, `steal`, `appraise`, `track`, `hide`) become shortcuts to `use`, built in as `bandage` is. What makes those skills different isn't code of their own but **what they're aimed at** and **a few more effects**.

- **Target kinds:** a character (as now), an item, a door (an exit), a room. Doors and rooms carry conditions as characters and items do; resolving one skips what only characters have (dodging, damage, health).
- **Locked is a condition on a lock** (*built*, 5b): a door's or a closable container's. How hard it is to pick or force stays the lock's own, in the zone file (`lock: (key, pick, bash)`), and formulas read it as `target.pick` and `target.sturdiness` (decided 2026-10-27). Lockpicking is data: `aim: Lock`, the user's skill `against: "target.pick"`, and on success `Remove(condition: "locked")`. A lock without the number a skill reads can't be worked that way ("That lock can't be picked."). A Knock spell would be `Remove(locked)` aimed at a lock, once spells can aim at one.
- **Keys aren't a skill.** `unlock <door>` still needs the door's key carried, and always works: it takes `locked` off, with no roll. `lock <door>` puts it back; the lock keeps its key, pick difficulty and sturdiness, so re-locking restores the same lock. `bash` isn't a skill either: it stays a command, Strength against the lock's sturdiness, forcing it open.
- **Rooms are targets** (*built*, 5c): `aim: Room` is the room the user is in, and `Find(kind: "forage")` takes something of that kind from its finds. Foraging and Herbalism are data, `forage` and `gather` shortcuts to them:

  ```ron
  kind: Active(aim: Room, cooldown: 3, against: "3", effects: [Find(kind: "herbs")]),
  ```

  A room's finds are named tables (`{"forage": [...], "herbs": [...]}`), so a new kind (ore for Mining, fish for Fishing) is data too: a table in the zone and a skill that finds it. Each room keeps a patch of each kind, three finds growing back one every 10 minutes; `roomwork` (`gathering.rs`) resolves them. Rooms don't carry conditions yet; that waits for something that needs a dark or consecrated room.
- **Stealing** (*built*, 5d, 2026-10-27): `Take(Coins(...))` moves coins from the target's purse into the user's, and the text names the haul as `{coins}`. Pickpocketing is data: `aim: Other`, the user's skill against the mark's Intuition (`resist: Attribute(Intuition)`), for a share of their purse by the margin (all of it at three times their wits), and `provokes`, so a mark who catches them attacks. `steal <someone>` is a shortcut. What's taken is its own kind (`Taken`), so stealing an item, held or carried or by tag, or a condition like a blessing, is another kind beside `Coins` rather than another effect. Coins only, from mobs only, for now (decided 2026-10-27): mobs carry a purse from their template's `coins`, so what a pickpocket lifts is no longer there for their killer. A hidden thief is revealed, as by any skill made of effects.

  ```ron
  kind: Active(aim: Other, cost: Stamina(1), cooldown: 5, resist: Attribute(Intuition), provokes: true,
      effects: [Take(Coins("max(1, floor(target.coins * min(1, margin / 3)))"))]),
  ```
- **Learning things** (*built*, 5e, 2026-10-27): `Reveal(what)` tells the user something and changes nothing else, the first effect that only informs. Appraisal is `aim: Item`, the user's skill against `"2 + 2 * log10(target.value + 1)"` (bread about 3, a short sword 6, a breastplate 8), revealing its `Worth`; something worth nothing is judged against 2, so it can now be fumbled. Tracking is `aim: Room`, revealing its `Tracks`, each read against `"target.ground * (1 + age / 10)"`, the ground's difficulty (mud 1 to paving 8) doubled after ten minutes; water holds none, and `use tracking <who>` reads only those whose maker matches. Both are `quiet`. `itemwork` (`trade.rs`) and `trackwork` (`tracking.rs`) resolve them; `appraise` and `track` are shortcuts. Formulas gained `log10`, `target.value`, `target.ground` and `age`.

  ```ron
  kind: Active(aim: Room, cooldown: 3, quiet: true,
      against: "target.ground * (1 + age / 10)", effects: [Reveal(Tracks)]),
  ```
- **Hiding** (*built*, 2026-10-27): Stealth is data, `aim: Me` (only yourself: `use stealth bob` is refused), `quiet`, and `Apply(condition: "hidden", strength: "power", lasts: Indefinitely, on: User)`; `hide` is a shortcut. What's particular to hiding stays in `combat/abilities.rs`: it can't be done in a fight, and as it's done everyone else here rolls to notice (their Intuition against its strength), before anything is told, so the skill's room line reaches only those who noticed; seen by them all, it fails, in its `miss` words. `sneak` stays a command: it's a way of moving, not a use of the skill. Within a round too, what a hidden character does is told only to those who've noticed them, so bystanders who haven't spotted an ambusher see them step out of the shadows after the blow, not the blow itself.
- **How it's kept:** on a lock, `locked` is its state (`DoorState::Locked`), so zone files, resets and the builders' `door` command are unchanged; the effects act on that state. Skills aimed at a lock can only `Apply` or `Remove` it, for now. `lockwork` (`commands/doors.rs`) resolves them, as `ability` does skills aimed at characters.
- **New effects:** `Find(table)`, a loot table rolled into the user's hands (Foraging, Gathering, aimed at a room); `Take`, an item moved from someone to the user (Pickpocketing, resisted by the victim's awareness); `Reveal`, telling the user something (Appraisal's value and quality, Tracking's trail), the first effect that only informs.
- **The order,** each step leaving the game working: locks as targets, with `locked` as a condition, and Lockpicking as data (5b, *built*); rooms as targets, with `Find`, for foraging and gathering (5c, *built*); `Take`, for stealing (5d, *built*); `Reveal`, for appraising and tracking (5e, *built*); Hiding as data (*built*, 2026-10-27).

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
- **Requirements** in the requirement language (✓).
- **Effects** and **text** (✓).

`use <ability> [<target>]` runs any of them, and `cast` stays the verb for spells (`use` on a spell casts it). **Every skill is data** (decided 2026-10-27): commands like `pick` and `forage` become shortcuts to `use`, built in as `bandage` is, and what made them special becomes target kinds (a door, an item, a room) and effects (`Find`, `Take`, `Reveal`). See [Every skill as data](#every-skill-as-data).

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

### Formulas, as built

Step 3 (2026-10-26). `src/formula.rs` parses formulas when a file loads, so a typo or an unknown name stops the file loading with a message saying what's wrong, and works them out where they're used. Every amount in an effect is one: `Damage` and `Heal` `amount`, and `Apply` and `Imbue` `strength`. A plain number still works (`strength: 4`).

- **Dice:** `2d6` adds two six-sided dice; `roll(2, 5)` is one roll from 2 to 5, what `(2, 5)` used to mean. Every roll goes through `Dice` with the user's Luck.
- **Inputs:** `power` (a caster's; nothing for a blow), `strength` (an item condition's, in its `on_hit` effects; 1 elsewhere), `margin` (how decisively the contest was won, the winner's roll over the loser's; 1 when nothing resisted), `level` (the user's character level), `user.<attribute>` and `target.<attribute>` (`user.strength`, `target.willpower`), and `user.health`, `user.most_health`, `target.health`, `target.most_health`. Attributes go through `user.` and `target.` so that `strength` can mean a condition's.
- **Operators:** `+ - * /` (division doesn't round), brackets, `min`, `max`, `floor`, `ceil`, `round`. An amount is rounded to a whole number where it's used.
- **What moved into data:** "a third of the power on top" is now written in each spell (`"roll(2, 5) + floor(power / 3)"`), `empowered` is gone (a ward is `strength: "4 + floor(power / 3)"`), and "once per point of strength" for item conditions is gone (flaming says `* strength`).
- **Still in code:** a blow's weapon damage and Strength bonus, the blow's crushing multiplier (applied to every `Damage` automatically, so formulas don't repeat it), spell power and Drain, and the numbers in `progression.ron`. `weapon`, `quality` and `skill(key)` inputs come when something needs them.

## Triggers

When reactions and passive skills fire. Passives already practise on some of these (✓ stamina spent, dodged, blocked, hit in armour, damaged, used a skill or tree).

- **In a fight:** when hitting someone (✓), being hit (✓), missing, dodging (✓), blocking, killing (✓), dying; at the start (✓) and end of the bearer's turn.
- **Health:** when hurt below a share of health (✓).
- **Magic:** when casting (✓), when a spell lands on you, when a condition is applied (✓) or fades (✓).
- **Moving and living:** when entering (✓) or leaving a room, someone arriving (✓), resting (✓), eating or drinking (✓), using a skill or tree; every so often (✓).

A trigger can also check a condition from the requirement language (✓): "only while wielding an axe", "only if health is under a quarter".

```ron
// Thorns: a condition that hurts whoever hits its bearer.
reactions: [(on: Hit, effects: [Damage(amount: "2", type: piercing, armour: false, to: Attacker)])],
```

### Reactions, as built

Steps 4a and 4b (2026-10-26). Conditions and passive skills can have `reactions`: effects that happen by themselves when the bearer does or suffers something, in a fight or out of one. Items react through the conditions they grant or are imbued with.

```ron
"thorns": (
    ...
    reactions: [(
        on: Struck,
        effects: [Damage(amount: "strength", type: "physical")],
        text: (actor: "Your thorny hide {harm} {target}.", target: "{actor}'s thorny hide {harm} you.", room: "..."),
    )],
),
```

- **Triggers (`on`) in a fight:** `Hits` (the bearer lands something meant to harm), `Struck` (the bearer is hit by it), `Dodges` (a blow, not a shield block), `Kills`, `Dies` (the bearer is killed, at whoever killed them, or themselves for bleeding to death; the one reaction that answers from beyond the grave, and the only one besides `Kills` aimed at the dead), `Hurt(share)` (a hit leaves the bearer below that share of their most health, having been above it), `TurnStarts` (each of the bearer's turns, after bleeding and the like), and `Casts` (the bearer casts a spell, in a fight or out of one, at whoever it's cast at).
- **Anywhere:** `Enters` (the bearer comes into a room), `Arrives` (someone the bearer sees comes into their room: a sneak they didn't notice sets nothing off), `Rests`, `Eats`, `Drinks`, and `Every(Minutes(n))` or `Every(Hours(n))` in game time, in a fight or out of one, at the same times for everyone.
- **A condition's own:** `Applied` (it takes hold, at whoever put it on) and `Fades` (it runs out, in time or turns; not when it's cured, removed or broken). Only the condition itself answers these, and a passive skill can't have them. Poison uses `Fades`: run its course, it leaves its bearer weakened for half an hour.
- **Who's who:** a reaction is resolved like any spell or blow, by `combat/resolve.rs`, with the bearer as the user and the other one involved as the target. Where nobody else is (entering a room, resting, eating, the heartbeat, a condition running out), the target is the bearer. Effects are `on: Target` by default, so thorns land on the attacker; `on: User` is the bearer (a last stand raising their own guard). It always lands: nothing resists a reaction, though armour and resistances still take their share of its damage.
- **Numbers:** a condition's reactions see its `strength` in their formulas, a passive skill's see its level as `power`. An optional `chance: "0.25"` (a formula) makes it happen only sometimes, and `only: Foes` only when the other one isn't the bearer's ally (for a mob, its own kind and those its `allies` name, as `MobDef::allied` counts them; for a player, other players): a deer startles at a hunter or a wolf coming in, not at another deer.
- **Text:** every reaction has its own `text`, with `{harm}` in every line if it does damage; checked at load.
- **No chains:** a reaction never sets off another, so two thorny fighters don't bounce damage back and forth forever, and a condition a reaction puts on doesn't react to taking hold. Its running out later does: that's time passing, not a chain. A reaction from a skill practises it.
- **Out of a fight it starts none:** a reaction that hurts someone doesn't start a fight, but can kill; whoever it kills falls to the reaction's bearer.
- **How it fits the code:** reactions in a fight are set off where they happen (`resolve.rs`, the start of a turn in `mod.rs`, casting in `casting.rs`). Everything else comes from game events (`ConditionApplied`, `ConditionFaded`, `Rested`, `Consumed`, `Entered`), which `combat/triggers.rs` turns into the reactions they set off and resolves as a round would, just after the combat round each tick.
- **The first:** the `thorns` condition. Nothing in the world gives it yet (a spell, item or aura can); to try it, a GM or admin can `condition guard thorns 3 5t`.
- **Still to come:** leaving a room, missing, blocking, dying, the end of a turn, a spell landing on the bearer, and using a skill, as something needs them; and more checks on a trigger from the requirement language ("only while wielding an axe"; `only: Foes` is the first).

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
2. **Effects and abilities.** Damage types (2a), one effect vocabulary (2b) and one contest with skills that resist tags (2c-i) are built. So is one resolver for spells, techniques and blows, where either can use any effect and any contest (2c-ii). So are `Imbue` and saved item conditions (2d). Step 2 is done.
3. **Formulas** (*built*). The expression evaluator, and every amount in an effect moved into data.
4. **Triggers and reactions** (*built*). Reactions on conditions, skills and items in a fight (4a), and out of one: entering a room, someone arriving, resting, eating and drinking, a condition taking hold or running out, casting, the start of a turn and every so often (4b).
5. **Every skill through `use`.** Active skills made of effects, with First Aid the first (5a, *built*). Then [every skill as data](#every-skill-as-data): locks as targets, with `locked` as a condition, and Lockpicking as data (5b, *built*), rooms as targets with `Find`, and Foraging and Herbalism as data (5c, *built*), `Take` and Pickpocketing as data (5d, *built*), `Reveal`, and Appraisal and Tracking as data (5e, *built*), and Hiding (*built*); their commands are shortcuts; and mobs use active skills in a fight, as they do techniques and spells. Step 5 is done.
6. **Scripting.** Rhai, exposing the verbs and triggers above.

## Open questions

- [x] **How do item conditions work?** Template grants are data and need no saving; conditions put on one item are saved with it, game-time spans as real end times and the rest as charges. See [Item conditions](#item-conditions) (2026-10-26).
- [x] **Do conditions save with characters?** Yes, those lasting a game-time span, as the real time they end; fight-turn conditions end with the fight (2026-10-26).
- [x] **How much is shown?** Hints by default, like a shimmer around a ward, following its strength; names and numbers only to the bearer, or with `SensesMagic` and later skills. See [What others see](#what-others-see) (2026-10-26).
- [x] **Who may write scripts?** Admins, for now (2026-10-26).
- [x] **What damage types?** Every weapon and natural attack is `physical`; magic uses the elements (fire, cold, lightning, wind, earth, water), the energies (positive, negative), light and shadow, force and poison. The list is data (2026-10-26).
