# Attributes, Karma and Luck

[Design summary](README.md) · Status: built, except earning Karma from character level (waits on skills)

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

- **Starting values:** every attribute starts at 1, except Magic at 0, and every character starts with 10 Karma to spend. Karma can go on Magic too, as latent talent.
- **Derived values:** health 8 + Body / 2, fatigue 8 + Willpower / 2, initiative Reaction + Intuition, carrying capacity from Strength + Body.
- **Attributes never go down** and Karma is never refunded, except through rare in-world events.

Players see their attributes with `score` and spend Karma with `raise <attribute>`, which accepts any unambiguous prefix (`raise agi`). Admins can set an online player's attribute with `setattr <player> <attribute> <rating>`, which ignores Karma and is meant for testing.

## Karma

- **Earning:** character level is total skill levels divided by 5, rounded down, and each character level grants 1 Karma. Karma is therefore scarce and every point is a small event. Until skills exist, the 10 starting Karma is the only source.
- **Cost:** raising an attribute from r to r + 1 costs 1 + floor(r / 5): 1 Karma up to 5, then 2, 3, 4 per point for each further band of five. Concentrating is possible; spreading goes further.
- **Estimates, to be checked by simulation:** about 10 Karma after a first week, 60 after a few months, 200 or more for long-term veterans.
- **Karma spent is not stored.** It is worked out from current ratings and starting ratings, so it can never drift out of sync. If races end up adjusting starting attributes, each character's starting ratings will be stored so those points are not counted as spent.

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

## Data

| Table | Columns | Notes |
| --- | --- | --- |
| `character_attributes` | `character_id`, `attribute`, `rating` | Only attributes that differ from their starting value have rows |

An `Attributes` component on each player entity, loaded at login, holds the ratings in memory.

## Open questions

- [ ] **Attribute cost curve:** the gently rising curve above is built; switch to a flat 1 Karma per point?
- [ ] **Karma rate:** 1 Karma per 5 total skill levels is a guess, to be checked by the progression simulation.
