# Attributes, Karma and Luck

[Design summary](README.md) · Status: built

Characters have ten uncapped attributes, raised with Karma. Skills say what a character can do; attributes say how well their body and mind do it. Luck gives every random roll in the game a chance to be rolled twice, keeping the better result.

## Attributes

| Group | Attribute | Governs |
| --- | --- | --- |
| Physical | Body | Health, resisting poison and disease, carrying |
| Physical | Agility | Precision: blades, bows, stealth, lockpicking |
| Physical | Reaction | Speed: initiative, dodging |
| Physical | Strength | Force: heavy weapons, lifting, climbing |
| Mental | Willpower | Resolve: resisting magic and fear, stamina |
| Mental | Logic | Reasoning: crafting, lore, learned magic |
| Mental | Intuition | Instinct: perception, tracking, reading people |
| Mental | Charisma | Presence: persuasion, leadership, teaching |
| Special | Luck | Every random roll; see [Luck](#luck) |
| Special | Magic | Starts at 0; latent until the character knows a casting skill (see [Magic](magic.md)) |

- **Starting values:** attributes start from a base of 3 (Luck 1, Magic 0), shifted by race (see [Races](characters.md#races)), and every character starts with 20 Karma to spend (humans 32). Karma can go on Magic too, as latent talent.
- **Derived values:** health 8 + Body / 2, stamina 8 + Willpower / 2, initiative Reaction + Intuition, carrying capacity from Strength + Body. Health and stamina are maximums; see [Vitals](#vitals) for the current values.
- **Attributes never go down** and Karma is never refunded, except through rare in-world events.

Players see their attributes with `score` and spend Karma with `raise <attribute>`, which accepts any unambiguous prefix (`raise agi`). Admins can set an online player's attribute with `setattr <player> <attribute> <rating>`, which ignores Karma and is meant for testing.

## Karma

- **Earning:** each [character level](progression.md#character-level-and-experience) grants 5 Karma, the moment it's reached (`karma_per_level` in `assets/progression.ron`, with the starting Karma). Character level comes from experience, earned by deeds, not from skill levels, so Karma is scarce and every point is a small event.
- **Cost:** raising an attribute costs the rating it reaches: 3 to 4 costs 4 Karma, 4 to 5 costs 5, and so on, so the total grows with the square of the rating. That suits the ratio rule, where doubling a rating is a big step. Latent Magic stays cheap: 0 to 1 costs 1.
- **Karma also unlocks skills** in trees a character already has, at each skill's own price, once its requirements are met: see [Unlocking with Karma](skills.md#unlocking-with-karma). Attributes, deeper skills and Magic all compete for it.
- **Estimates, to be checked in play:** a level is about 20 even fights, so a first week (levels 3 to 5) earns 15 to 25 Karma, enough for a first raise or unlock; level 15 has 95 in all, with the starting 20; level 20, 120: a main attribute to 10 (49), a second to 7 (22) and the unlocks of two trees (about 45). How many levels a veteran reaches depends on the hardest fights the world offers.
- **Karma spent on attributes is not stored.** It is worked out from current ratings and the race's starting ratings, so it can never drift out of sync, and points a race gives aren't counted as spent. Karma spent unlocking a skill is saved with the skill (`karma_paid`).

## The ratio rule

Every contest is an **opposed roll**: each side rolls a performance around their rating, the higher wins, and how far ahead the winner finished is the **margin**. A contest against a fixed difficulty (a lock, a climb) is the same, with the difficulty rolling as a rating and no Luck.

- **Ratios, never differences.** Ratings have no cap, so a gap is measured as a ratio: 5 against 10 is the same contest as 50 against 100. Early levels count for a lot; a five-level edge between masters counts for little.
- **Steep, k = 3.** The weaker side wins (weaker / stronger)³ as often as the stronger, so being twice as good wins 8 times in 9:

  | Contest | Weaker side wins |
  | --- | --- |
  | Even | 50% |
  | Masters, 48 against 53 | 43% |
  | Novices, 5 against 10 | 11% |
  | A novice lockpicker (3) on an ordinary lock (4) | 30% |
  | A fresh fighter (3) against a veteran guard (25) | 0.2%, effectively never |
  | A sewer rat (2) against a level-20 fighter (25) | 0.05% |
  | A master (55) on an ordinary lock (4) | fails 0.04% |

- **Always rolled.** Nothing is decided without a roll, so even a one-sided fight plays out; a huge gap just makes the upset effectively never. Upsets that matter (fate, surpassing limits) come from the story, not the dice.
- **How it's rolled:** each side's performance is its rating times a random factor (k·ln rating plus a Gumbel draw, which makes the odds exactly the curve above), and a side with Luck rolls twice and keeps the better. The margin is the winner's performance over the loser's: 1.1 is a near thing, 2 a decisive win. Margins are there for degrees of success: glancing and crushing blows, critical Backstabs, picks that leave no trace. *Built:* `Dice::opposed`; attacks read the margin as [blows](architecture.md#combat), glancing to crushing.
- **Combat and every other contest must follow this rule.** Plain chances that aren't contests (fleeing, a skill's gain roll) stay plain chances.

## Luck

Every random roll gets a chance, based on the roller's Luck, to be rolled a second time, keeping the better result:

```math
\text{lucky chance} = \frac{\text{Luck}}{\text{Luck} + 50}
```

Luck 10 gives 17%, Luck 50 gives 50% and Luck 150 gives 75%. However high Luck goes, its effect never exceeds best of two, so a 1% legendary drop tops out just under 2%. The 50 is a tuning value (`LUCK_SCALE` in `src/dice.rs`).

| Roll | Better means |
| --- | --- |
| Skill gain | A gain |
| Hit, damage, dodge | The hit or dodge succeeds, or more damage |
| Loot tier | The rarer tier |
| Loot quantity and quality | More, or higher |
| Traps, hazards, critical failures | The roller escapes the bad outcome |
| Chance-based epiphany triggers | The trigger fires |

- **Contested rolls** give each side its own lucky chance. If both trigger, they cancel and the original roll stands.
- **Group loot uses the group's average Luck,** so there is no reason to send the luckiest member to open every chest.

### The Dice service

`Dice` (`src/dice.rs`) is the only source of randomness in the game. No system owns a random number generator, so Luck cannot be forgotten, tests can use a seeded generator, and rebalancing Luck is a single change.

```rust
dice.chance(luck, 0.18)              // yes or no
dice.range(luck, 3..=12)             // a value; higher is better
dice.pick(luck, &loot_table)         // weighted table, worst to best
dice.contest(luck_a, luck_b, p)      // both sides' Luck
```

## Vitals

Each character has **current** health and stamina, below or at the maximums above (`src/vitals.rs`). Mobs have them too, by the same rules, except that a mob template's `health` replaces 8 + Body / 2 and mobs don't rest.

- **Recovery** happens on the one-second heartbeat, worked out from elapsed time: health refills from empty in 10 minutes, stamina in 2. **`rest`** makes it three times as fast (walking, attacking or being attacked gets you up, and being caught resting is dangerous; see [Combat](architecture.md#combat)), and nothing recovers in a fight. First Aid and healing draughts restore health at once, and food and drink speed recovery for a while (see [Healing](trees.md#healing)).
- **It continues offline.** Vitals are saved with the time they were saved (at logout, and every minute), and caught up at the next login, so a player who logs out hurt comes back rested.
- **Fractions count.** Current values are kept as fractions and shown rounded down, so slow recovery still adds up.
- **A maximum that goes down** (should one ever) clamps the current value to it.
- **Seen in** `score` (`Health 7/10`) and the status prompt, `<7/10hp 9/9st>`. `prompt <format>` changes it, with `%h`/`%H` for current and maximum health, `%s`/`%S` for stamina and `%%` for a percent sign; `prompt` alone goes back to the default.
- **Admins** can set someone's current vitals with `setvitals <player> <health> [<stamina>]`, for testing until combat can do it.
- **Not yet:** what spends stamina (active skills, running, carrying too much) and what happens at 0 health; both arrive with combat.

## Data

| Table | Columns | Notes |
| --- | --- | --- |
| `character_attributes` | `character_id`, `attribute`, `rating` | Every rating, written when creation finishes; attributes without a row are at their race's start |
| `characters` | `health`, `stamina`, `vitals_at`, `prompt` | Current vitals when saved, and when (`NULL` means full); their prompt format (`NULL` means the default) |

An `Attributes` component on each player entity, loaded at login, holds the ratings in memory.

## Open questions

- [x] **Attribute cost curve:** a point costs the rating it reaches (2026-10-26).
- [x] **Karma per level:** 5, with every level costing the same 1,000 experience (2026-10-26).
- [ ] **Karma rate:** 1 Karma per 5 total skill levels is a guess, to be checked by the progression simulation.
