# Groups

[Design summary](README.md) · Status: designed (2026-10-27), not built. How characters travel and fight together: following, groups formed the ROM way, shared experience and coin, assisting, and taunting to hold a pack's attention.

Riftward is meant to be played alone or together, and nothing should push a player into a group to make progress. A group is a few characters who've agreed to move and fight as one: they share what a fight is worth, its coins, and their contracts, and they can take on what none of them could alone.

## Following

- **`follow <someone>`** sets you to follow them. When they leave a room, you go with them, by the same rules as moving yourself: you can't follow through a door you can't pass, up a climb you fail, or while you're fighting, resting or held. "You follow Ada north."
- **`follow self`** (or `follow me`) stops following. Following ends too if the one you follow logs off, dies, or leaves you behind (a climb you fail).
- **`nofollow`** turns away followers: anyone following you stops, and no one can start. A per-player setting, saved like the others.
- **Mobs can follow** by the same rule, so a puppeted mob can join a group, a GM can lead a party, and companions (a charmed beast, a hireling) can come later.
- **Shadowing** (the Stealth tree) is following unseen; it comes with the Stealth tree's later skills.

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
- **Everyone sees it:** the room sees "Bob joins Ada's group.", and `look` at a member could say whose group they're in.

## Fighting together

- **Assist is a setting,** on by default (`autoassist`, saved like `combine`, [#3](https://github.com/Foxworks-Studios/riftward/issues/3)): when a group member in your room is in a fight and you aren't, you join it at the start of the next round, against whoever is fighting them. Off, you choose your own fights.
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

- **Experience:** every member in the room when a foe dies earns its victory experience, in full, but worked out **side against side**: the group's combined strength against the foe's, by the [ratio rule](attributes.md#the-ratio-rule), as [Progression](progression.md#tough-fights) planned. Five against an ogre isn't a tough fight, so each earns what an easy win is worth. Nothing is divided, and nothing rewards a crowd. The same odds speed skill gains during the fight.
- **Coins** a foe carried are split evenly among the members in the room, the odd coins to whoever struck the last blow.
- **Loot** falls to the ground as now, for whoever takes it; a loot table rolls with the group's average Luck, among the members present ([Luck](attributes.md#luck)), so there's no sending the luckiest to open every chest.
- **Contracts:** accepting a guild contract while grouped offers it to every member in the room whose rank allows it. Kills and finds by any member who holds it count for all who do; each earns the contract's full merit, and its coin is split among them ([Quests](quests.md#group-contracts)).
- **Not shared:** skill practice (each learns from what they do), and what's harvested or picked up.

## Saved

Nothing about a group is saved: groups, following and leadership live in the game only. `nofollow` and `autoassist` are per-player settings, saved with the character.

## Open questions

- [ ] **Group size:** 6 to start; does a bigger group need anything else (a raid of two groups)?
- [ ] **Experience out of the room:** a member who fled, or stood guard next door, earns nothing; is that right, or should anyone who struck a blow earn it?
- [ ] **Taunting's numbers:** how long it lasts, what it costs, and whether a mob that's been taunted resists the next taunt better.
- [ ] **Leaving the group in a fight:** can you, and do you still earn a share of what's dying?
- [ ] **Group recall or a meeting point:** a way to gather a scattered group, or not at all.
