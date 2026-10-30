# Progression

[Design summary](README.md) · Status: built as designed. Every number below is in `assets/progression.ron`

Skills level up only through use: each use may roll a chance to gain a level. The chance grows with recent practice, fades with time away, and each gain puts that skill on a cooldown that lengthens with level. All numbers below are starting points for tuning, and will live in `assets/config/progression.ron`.

What counts as a "use" depends on the skill's kind: activating an active skill, or a passive skill's trigger event firing. See [Skill kinds](skills.md#skill-kinds).

## The gain roll

Each known skill stores its `level`, a `practice` value from 0 to 1, `last_used`, and `next_gain_at`. On every use:

1. **Practice fades** with the time since the last use, halving every 4 hours (`practice_half_life_minutes` in `assets/progression.ron`; it was 30 minutes until 2026-10-27, which cost a player most of their warm-up over a short break):

   ```math
   p' = p \cdot 0.5^{\Delta t / 4\,\text{h}}
   ```

2. **Roll for a gain**, unless the skill is on cooldown:

   ```math
   \text{chance} = d \cdot 0.25 \cdot \frac{25}{25 + L} \cdot \left(0.2 + 0.8\,p'\right) \cdot 0.5^{\max(0,\,L - C)/5}
   ```

3. **Practice builds**, including during a cooldown, so the player is warmed up when it ends:

   ```math
   p \leftarrow p' + 0.1\,(1 - p')
   ```

4. **On a gain**, the level rises by one and the cooldown starts. Practice is kept.

In the chance formula, d is the skill's `difficulty`, L its level, and C its soft cap, 10 + 2 × character level (see [The soft cap](#the-soft-cap-skills-and-the-character)); below the cap the last factor is 1. A cold skill rolls at 20% of its base chance and a fully practised one at 100%, so a cold skill can still improve. The roll goes through the `Dice` service like every other roll (see [Luck](attributes.md#luck)).

For a skill at level 10, within its soft cap (base chance 18%):

| Situation | Practice | Chance per use |
| --- | --- | --- |
| First use, cold | 0.00 | 4% |
| After 10 uses in a row | 0.65 | 13% |
| After 20 uses in a row | 0.88 | 16% |
| Warmed up, then an hour's break | 0.74 | 14% |
| Warmed up, then 4 hours away | 0.44 | 10% |
| Warmed up, then a day away | 0.01 | 4% |

## Pacing

Decided 2026-10-29. In a fight a skill is used about once a round, every 3 seconds. Out of one, an action takes only a second, and some skills (sneaking as you walk, haggling) take none, so repeating one could teach three times as fast. To keep them even, **out of a fight a use counts for less when it follows the last one quickly**: a use t seconds after the previous counts for t / 3 of one (`practice_window_seconds` in `assets/progression.ron`), and in full after 3 seconds. Foraging every second counts a third each time, about the same as once a round in a fight.

A use's weight multiplies its gain chance, the practice it builds and its chance of a discovery. In a fight every use counts in full, since rounds pace it already, and a dodge against each of several attackers still counts. The time of the last try isn't saved, so the first use after logging in counts in full.

## Gain cooldown

After a gain, that skill cannot gain again until its cooldown ends. A starting curve is 5 minutes × 1.1<sup>level</sup>: 5 minutes at level 0, about 9 hours at level 49.

- **Uses during a cooldown still count.** They work normally and build practice; they just don't roll.
- **Spamming and botting can't outpace it.** No amount of repetition gains faster than the cooldown allows.
- **Mastery takes real time,** which is what keeps rare skills rare (see [Rarity](skills.md#rarity)).

## Skill levels have no cap

A skill can keep improving forever, but every level is harder than the last: the gain chance falls with level, and the cooldown after each gain grows. That makes a practical ceiling without a hard one.

- **The gain chance** falls as 25 / (25 + L): full at level 0, half at 25, a third at 50, a fifth at 100.
- **The cooldown** grows ×1.1 a level, as before: about 9 hours at 49, a day at 60, nearly three days at 70.
- **Mastery** is a milestone, not a cap: at a skill's `mastery` level (50 unless it says otherwise) it's mastered ("You have mastered it."), and only a master can teach a loot skill.

## The soft cap: skills and the character

A skill grows at full speed up to **10 + 2 × character level**. Above that, its gain chance halves for every 5 levels over. A new character can take any skill to about 10 easily; a level-10 character to 30, a level-20 character to 50. Skills can run ahead of the character, but slowly, so a character who spreads across many trees doesn't outgrow their own experience.

## What a character of a level has

Simulated 2026-10-29 from the numbers above (`assets/progression.ron`): a character fighting even fights (50 experience each), six rounds a fight and a rest of 90 seconds to 3 minutes between, their main weapon used every round and Dodge about a third of rounds. The gain cooldown, in real time, is what paces a skill, so skills follow hours played: resting longer between fights raises them against character level.

| Character level | Main weapon | Dodge | Hours |
| --- | --- | --- | --- |
| 0 | 0 to 2 | 0 to 1 | New |
| 1 | 5 to 7 | 5 | 1 |
| 3 | 11 to 15 | 11 | 3 |
| 5 | 15 to 19 | 16 | 5 to 6 |
| 7 | 17 to 22 | 19 | 7 to 8 |
| 10 | 21 to 26 | 23 | 6 to 11 |

Tough fights (gains up to twice as likely) and fights too easy to pay experience (which still train) push skills a little higher. Nobody reaches the soft cap (30 at level 10) this way. Content is tuned against these (`src/combat/balance.rs` builds characters of levels 0, 1, 2, 3, 6 and 10 from them): a grey wolf that rated 12 against a level-6 character's 23 gave no experience at all ([Tuned against parties](darkwood.md#tuned-against-parties)).

## Character level and experience

Character level is separate from skill levels: skills grow by use, and the character grows by deeds.

- **Experience** comes from:
  - **Victories**, worth the risk you overcame: 100 × the chance you'd have lost, from how outmatched you were by **the whole fight** (see [Tough fights](#tough-fights)). An even fight gives about 50; a fight twice your match about 89. **A fight you'd have lost less than one time in ten gives nothing:** foes under about half your strength teach you nothing, so you outgrow easy prey, and a rat, to a veteran, is worth nothing. It's paid **kill by kill:** each foe that falls is worth its share of the whole fight's experience, by its share of the foes' strength, so clearing the fight earns the fight's worth, and three rats at once pay more than three rats one at a time (2026-10-28). The side that fells a foe earns it: everyone on it in the room.
  - **Discoveries:** 50 for discovering a skill through use, 100 for Awakening.
  - **Not quests.** Contracts and quests pay coin, items and guild merit, never experience (2026-10-27): the experience in a contract is already in the fighting it sends you to do, so a quest can't become the efficient way to level.
- **Every level costs the same: 1,000 experience,** about 20 even fights. What slows a character down isn't the price of a level but the fights it takes: as they grow stronger, the same foes are worth less and then nothing, so levelling means seeking harder challenges, and how far a character can climb is set by the hardest the world offers (2026-10-26).
- **The numbers are data,** in `assets/progression.ron`: starting Karma, Karma and experience per level, victory experience and the least risk worth any, and discovery and Awakening experience. It's read when the server starts, so a change needs a restart, not a code change; a file that doesn't parse stops the server.
- **Each level grants 5 Karma** (see [Karma](attributes.md#karma)). It doesn't raise health or stamina; those come from attributes and skills.
- **Existing characters** start with the experience their old character level (total skill levels ÷ 5) would need, so they keep every point of Karma they had.

## Tough fights

A fight against the odds teaches more, and is worth more. How **outmatched** a player is comes from their whole fight, by the [ratio rule](attributes.md#the-ratio-rule): their side's attack and defence against their foes', each side's added up, combined as a geometric mean. 1 is an even fight; 4 means the foes are four times their match.

**A fight is rated as a whole, like a D&D encounter** (decided 2026-10-28). An **encounter** is one side's fight in one room, a side being a player alone or a [group](groups.md):

- **It counts everyone who has taken part:** each member of the side who has fought, and each foe who has fought them or waits its turn to (aggressive mobs in the room, held back by a [pack's limit](world.md#packs), who aren't busy fighting someone else), with their attack and defence as they joined. Reinforcements join it as they arrive.
- **Rated by skill, not gear** (decided 2026-10-30, [#8](https://github.com/Foxworks-Studios/riftward/issues/8)): attack is a fighter's skill and attribute, defence their Reaction and passive skills like Dodge, with armour's penalty and a shield's block. What armour takes off a blow isn't counted: skills, which grow with a character's level, are a rough enough measure of threat, and the one players and mobs share, since mobs have no level.
- **Mobs fighting beside the side count for it:** a guard or a dog taking on the same foes makes the fight easier, though it isn't in the group and earns nothing (its kills pay no one). Mob allies of the foes (a pack's leader, the Rat King) are foes like the rest, each weighing by its strength.
- **It never gets easier as foes fall:** the fallen still count, since the whole fight, beginning to end, is what was spent on it. Both sides only grow: a member joining partway makes it easier, a foe joining harder.
- **Each side has its own.** A lone player and a group fighting the same rats are two encounters, each rated by its own members against the foes that fought it, and each paid for its own kills.
- **It ends** when none of the side is fighting there: the foes are dead, or the side has fled. (`Encounter` in `combat/mod.rs`.)

- **Chances multiply by the odds,** from no change at even odds or better up to **double** at 2 or more, for every gain roll and every [discovery](skills.md#discovery) made during the fight, passive skills like Dodge included. Luck applies on top, as on every roll.
- **Cooldowns still hold,** so a hard fight speeds learning toward the cooldown, never past it.
- **Mobs aren't counted;** they don't learn.
- **Sides, not pairs:** five players against one ogre isn't a tough fight even if the ogre outclasses each of them, and ten rats around one player are, even if each rat is easy. The encounter's odds decide both the odds for skill gains and the victory experience ([Groups](groups.md#what-a-group-shares), built 2026-10-28).

## Implementation notes

- The fade and the cooldown are computed from stored timestamps when the skill is next used. Nothing ticks on a timer, and offline characters cost nothing.
- The roll is one pure function, `roll_skill_gain(state, skill, tuning, now, dice)`, with unit tests and a small simulation that prints gain and Karma curves for tuning.
- Each gain raises total skill levels, which raises character level and earns Karma (see [Karma](attributes.md#karma)).

## Open questions

- [ ] **Is the gain cooldown per skill or shared across all skills?** Leaning: per skill, so a varied session makes progress on several.
- [ ] **Does practice fade while logged off?** Leaning: yes, by real time.
- [ ] **Confirm:** skill levels never decay, and gaining a level keeps practice.
- [x] **Do uses need a minimum gap to build practice?** Not a gap but a ramp: out of a fight, uses closer than a round apart count for less ([Pacing](#pacing)).
- [ ] **Does Luck apply to skill-gain rolls?** It speeds progression within the cooldowns, and may make Luck the attribute everyone raises first.
