# Progression

[Design summary](README.md) · Status: built as designed (tuning values are constants in `src/skills.rs` for now, not yet a config file)

Skills level up only through use: each use may roll a chance to gain a level. The chance grows with recent practice, fades with time away, and each gain puts that skill on a cooldown that lengthens with level. All numbers below are starting points for tuning, and will live in `assets/config/progression.ron`.

What counts as a "use" depends on the skill's kind: activating an active skill, or a passive skill's trigger event firing. See [Skill kinds](skills.md#skill-kinds).

## The gain roll

Each known skill stores its `level`, a `practice` value from 0 to 1, `last_used`, and `next_gain_at`. On every use:

1. **Practice fades** with the time since the last use, halving every 30 minutes:

   ```math
   p' = p \cdot 0.5^{\Delta t / 30\,\text{min}}
   ```

2. **Roll for a gain**, unless the skill is on cooldown:

   ```math
   \text{chance} = d \cdot 0.25 \left(1 - \frac{L}{L_{max}}\right) \left(0.2 + 0.8\,p'\right)
   ```

3. **Practice builds**, including during a cooldown, so the player is warmed up when it ends:

   ```math
   p \leftarrow p' + 0.1\,(1 - p')
   ```

4. **On a gain**, the level rises by one and the cooldown starts. Practice is kept.

In the chance formula, d is the skill's `difficulty`, L its level and L<sub>max</sub> its `max_level`. A cold skill rolls at 20% of its base chance and a fully practised one at 100%, so a cold skill can still improve. The roll goes through the `Dice` service like every other roll (see [Luck](attributes.md#luck)).

For a skill at level 10 of 50 (base chance 20%):

| Situation | Practice | Chance per use |
| --- | --- | --- |
| First use, cold | 0.00 | 4% |
| After 10 uses in a row | 0.65 | 14% |
| After 20 uses in a row | 0.88 | 18% |
| Warmed up, then a 30-minute break | 0.44 | 11% |
| Warmed up, then 2 hours away | 0.06 | 5% |

## Gain cooldown

After a gain, that skill cannot gain again until its cooldown ends. A starting curve is 5 minutes × 1.1<sup>level</sup>: 5 minutes at level 0, about 9 hours at level 49.

- **Uses during a cooldown still count.** They work normally and build practice; they just don't roll.
- **Spamming and botting can't outpace it.** No amount of repetition gains faster than the cooldown allows.
- **Mastery takes real time,** which is what keeps rare skills rare (see [Rarity](skills.md#rarity)).

## Tough fights

A fight against the odds teaches more. Each combat round works out how far each player is **outmatched** by the foe they're fighting, by the [ratio rule](attributes.md#the-ratio-rule): the foe's attack against the player's defence, and the foe's defence against the player's attack, combined as a geometric mean. 1 is an even fight; 4 means the foe is four times their match.

- **Chances multiply by the odds,** from no change at even odds or better up to **double** at 2 or more, for every gain roll and every [discovery](skills.md#discovery) made during the fight, passive skills like Dodge included. Luck applies on top, as on every roll.
- **Cooldowns still hold,** so a hard fight speeds learning toward the cooldown, never past it.
- **Mobs aren't counted;** they don't learn.
- **Groups, once they exist,** should compare sides rather than pairs: five players against one ogre isn't a tough fight even if the ogre outclasses each of them. Likely the combined strength of each side, by the same ratio rule. To design with grouping.

## Implementation notes

- The fade and the cooldown are computed from stored timestamps when the skill is next used. Nothing ticks on a timer, and offline characters cost nothing.
- The roll is one pure function, `roll_skill_gain(state, skill, tuning, now, dice)`, with unit tests and a small simulation that prints gain and Karma curves for tuning.
- Each gain raises total skill levels, which raises character level and earns Karma (see [Karma](attributes.md#karma)).

## Open questions

- [ ] **Is the gain cooldown per skill or shared across all skills?** Leaning: per skill, so a varied session makes progress on several.
- [ ] **Does practice fade while logged off?** Leaning: yes, by real time.
- [ ] **Confirm:** skill levels never decay, and gaining a level keeps practice.
- [ ] **Do uses need a minimum gap to build practice?** Only if cooldowns alone prove too easy to exploit.
- [ ] **Does Luck apply to skill-gain rolls?** It speeds progression within the cooldowns, and may make Luck the attribute everyone raises first.
