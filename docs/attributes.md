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

- **Starting values:** attributes start from a base of 3 (Luck 1, Magic 0), shifted by race (see [Races](characters.md#races)), and every character starts with 10 Karma to spend (humans 13). Karma can go on Magic too, as latent talent.
- **Derived values:** health 8 + Body / 2, stamina 8 + Willpower / 2, initiative Reaction + Intuition, carrying capacity from Strength + Body. Health and stamina are maximums; see [Vitals](#vitals) for the current values.
- **Attributes never go down** and Karma is never refunded, except through rare in-world events.

Players see their attributes with `score` and spend Karma with `raise <attribute>`, which accepts any unambiguous prefix (`raise agi`). Admins can set an online player's attribute with `setattr <player> <attribute> <rating>`, which ignores Karma and is meant for testing.

## Karma

- **Earning:** character level is total skill levels divided by 5, rounded down, and each character level grants 1 Karma. Karma is therefore scarce and every point is a small event; it arrives the moment the gain that earns it happens.
- **Cost:** raising an attribute from r to r + 1 costs 1 + floor(r / 5): 1 Karma up to 5, then 2, 3, 4 per point for each further band of five. Concentrating is possible; spreading goes further.
- **Estimates, to be checked by simulation:** about 10 Karma after a first week, 60 after a few months, 200 or more for long-term veterans.
- **Karma spent is not stored.** It is worked out from current ratings and the race's starting ratings, so it can never drift out of sync, and points a race gives aren't counted as spent.

## The ratio rule

Because attributes are uncapped, every contest compares ratios, never differences. A hit chance of attack / (attack + defence) gives 20 against 10 the same 67% as 40 against 20, so veterans win more often without winning automatically. Combat and every other contest must follow this rule.

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

- [ ] **Attribute cost curve:** the gently rising curve above is built; switch to a flat 1 Karma per point?
- [ ] **Karma rate:** 1 Karma per 5 total skill levels is a guess, to be checked by the progression simulation.
