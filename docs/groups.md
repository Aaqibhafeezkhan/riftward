# Groups

[Design summary](README.md) · Status: built (2026-10-28): following, groups, `gtell`, autoassist, and sharing experience, coins and Luck (`src/groups.rs`, and `combat` for the fight). Taunting waits on the `Taunted` modifier, group contracts on the quest engine, and the ally bonus on [#29](https://github.com/Foxworks-Studios/riftward/issues/29). How characters travel and fight together: following, groups formed the ROM way, shared experience and coin, assisting, and taunting to hold a pack's attention.

Riftward is meant to be played alone or together, and nothing should push a player into a group to make progress. A group is a few characters who've agreed to move and fight as one: they share what a fight is worth, its coins, and their contracts, and they can take on what none of them could alone.

## Following

- **`follow <someone>`** sets you to follow them. When they walk out of a room, you go after them a moment later (you see them leave first), by the same rules as moving yourself: you can't follow through a door you can't pass, up a climb you fail, or while you're fighting, resting or held. "You follow Ada north."
- **`follow self`** (or `follow me`) stops following. Following ends too if the one you follow logs off, dies, or leaves you behind (a climb you fail).
- **`nofollow`** turns away followers: anyone following you stops, and no one can start. A per-player setting, saved like the others.
- **Mobs can follow** by the same rule, so a puppeted mob can join a group, a GM can lead a party, and companions (a charmed beast, a hireling) can come later.
- **Shadowing** (the Stealth tree) is following unseen; it comes with the Stealth tree's later skills.
- **Only walking is followed:** a builder's `goto` isn't, and nor is anything that moves someone without their walking. Fleeing is walking, so someone who isn't in the fight follows a leader who flees it; those fighting stay.

## Forming a group, the ROM way

A group is formed by its leader out of those following them.

| Command | Does |
| --- | --- |
| `follow ada` | Follow Ada, the first step to joining her group |
| `group bob` | (the leader) Take Bob into the group; he must be following you |
| `group bob` again | (the leader) Put Bob out of the group; he still follows |
| `group` | List the group: each member's name, health, stamina, and where they are |
| `gtell <message>` (`gt`) | Speak to the whole group, wherever they are |
| `follow self` | Leave the group: following no one, you're in no one's group |
| `disband` | (the leader) End the group |

- **A leader leads by being followed:** anyone who follows a member follows them, not the leader, as in ROM; but only the leader adds members, and members move with whoever they follow, so a line of followers moves as one.
- **The lead passes on** when the leader leaves or logs off: to the member who joined first.
- **Groups aren't saved.** They last while their members are in the game; logging off leaves your group. Following isn't saved either.
- **A size limit,** data in `progression.ron`: 6 to start.
- **Everyone sees it:** the room sees "Bob joins Ada's group.". (`look` at a member could say whose group they're in; not yet.)

## Fighting together

- **Assist is a setting,** on by default (`autoassist`, saved with the character): when a group member in your room is in a fight and you aren't, you join it at the start of the next round, against whoever they're fighting, as long as you can see them and they aren't in your group. Not while you're resting. Off, you choose your own fights.
- **Packs see each of you.** A spawn's `pack` limit is [per target](world.md#packs), so a group walking into the Warren meets three rats each, not three among them.
- **Taunting holds them.** A taunt makes a mob attack the taunter whatever its pack's limit, and stay on them for a while (below). It's how one member protects the rest.
- **An ally bonus** for fighting alongside others ([#29](https://github.com/Foxworks-Studios/riftward/issues/29)) would make grouping pay in the fight itself, not only in what's shared after it.

### Taunting

Planned with `Taunted(by)` and the `Taunt` effect in [Effects](effects.md#modifiers).

- **A taunt is a skill** (a technique, or an active skill aimed at a foe) whose effect applies `Taunted(by)`: the taunted mob turns on the taunter at once, leaving whoever it was fighting, and attacks only them while the condition lasts, a few rounds.
- **It ignores the pack rules.** Any number of mobs can be taunted onto one character; a pack's `pack` limit no longer holds them back from the taunter, or spreads them to the others. Six rats on the one in heavy armour is the point.
- **It's a contest,** as everything is: the taunter's Charisma (or a skill) against the mob's Willpower, by the ratio rule, and some things can't be taunted at all: the inanimate and the mindless (`Immune` by tag, as [traits](effects.md#traits) do).
- **Where it lives:** a Presence or Shields skill (shouting a challenge, clashing a shield), and maybe a room-wide version as its capstone, taunting everything hostile in the room at once.
- **Players aren't forced:** on a player, `Taunted` is a penalty for attacking anyone but the taunter, never a loss of control.

## What a group shares

- **Experience:** the group's fight is one [encounter](progression.md#tough-fights), rated once over its whole length: everyone in the group who fought, added up, against every foe who fought them or waited its turn, by the [ratio rule](attributes.md#the-ratio-rule). Each foe the group fells is worth its share of what that whole fight is worth, and every living member in the room earns it, in full: a member standing by earns what those fighting do; one who has died, or is elsewhere, earns nothing. Five against an ogre isn't a tough fight, so each earns what an easy win is worth. Nothing is divided, and nothing rewards a crowd. The same odds speed skill gains during the fight.
- **Two sides, two fights:** a lone player and a group (or two groups) fighting the same rats are rated and paid separately, each for the foes that fought them and the kills they made.
- **Coins** a foe carried are split evenly among the living members in the room, the odd coins to whoever struck the last blow: "You take 11 copper from the bandit and share it out: 6 copper for you."
- **Loot** falls to the ground as now, for whoever takes it; a loot table rolls with the group's average Luck, among the members present ([Luck](attributes.md#luck)), so there's no sending the luckiest to open every chest.
- **Contracts:** accepting a guild contract while grouped offers it to every member in the room whose rank allows it. Kills and finds by any member who holds it count for every holder in the same room; each earns the contract's full merit, and its coin is split among them ([Quests](quests.md#progress-in-a-group)).
- **Not shared:** skill practice (each learns from what they do), and what's harvested or picked up.

## Saved

Nothing about a group is saved: groups, following and leadership live in the game only. `nofollow` and `autoassist` are per-player settings, saved with the character.

## Open questions

- [ ] **Group size:** 6 to start; does a bigger group need anything else (a raid of two groups)?
- [ ] **Experience out of the room:** a member who fled, or stood guard next door, earns nothing; is that right, or should anyone who struck a blow earn it?
- [ ] **Taunting's numbers:** how long it lasts, what it costs, and whether a mob that's been taunted resists the next taunt better.
- [ ] **Leaving the group in a fight:** can you, and do you still earn a share of what's dying?
- [ ] **Group recall or a meeting point:** a way to gather a scattered group, or not at all.
