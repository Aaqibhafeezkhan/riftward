# Facts, requirements and rules

[Design summary](README.md) · Status: designed (2026-10-28); the store is built (`src/facts.rs`, the `facts` table), world flags are kept in it, and GMs list facts with `facts world|<player>`. One store for what the game remembers (world flags, what a character has done, where they are in a quest, a mob's mood), one language for testing it (requirements), and rules that react to events by changing it. Quests are built on these; world events and hidden skills move onto them.

Content feels alive when it reacts to things nobody scripted together. A quest that sets "this character sold out the guild" shouldn't need to know which other quests, mobs and doors will care. So everything the game remembers about its story goes in one place, as **facts**, and everything that reacts tests facts, without knowing who set them.

## Facts

A fact is a named value in a **scope**:

| Scope | Belongs to | Examples |
| --- | --- | --- |
| `world.` | The whole game | `world.rats_broke_out`, `world.breaches` (a count), `world.slime_lord_slain` |
| `char.` | One character | `char.sold_out_the_guild`, `char.title.rat_bane`, `char.trust.marin` (a number), `char.found_door.town:bookcase` |
| `quest.` | One character's one quest | `quest.courier_found`, `quest.rats` (a counter), `quest.ending` |
| `npc.` | A mob template, everywhere | `npc.marin.mood`, `npc.narina.secret_told` |

- **Values** are a yes (`true`), a whole number, or a word (`quest.ending = "sold"`). A fact never set is false, zero, or empty, so nothing needs declaring.
- **Saved,** in one `facts` table: scope, owner (a character, a character's quest, a mob template, or none for the world), key and value. Facts survive restarts; that's their point.
- **A fact can lapse:** set with `for: Hours(2)`, it's gone after (a rumour, a guard who remembers you for a while).
- **What already exists moves here:** world flags (`world_state`) become `world.` facts (*done*, 2026-10-28: flags saved before then are moved over once, at startup), and the character flags and titles hidden skills were designed with become `char.` facts. `WorldFlag("x")` and `Flag("x")` stay as short ways of writing them.

## One requirement language

There are two today: `Condition` (attributes, race, skills, guild rank; backgrounds, skill requirements, trainers) and world events' `When` (world flags and events). They merge into one, used by every gate in the game (*done* 2026-10-28; a world event's `when` is tested with no one, so whatever asks about a character is false there), and it's renamed **`Requirement`**, after the `requires:` it fills: "condition" then means only a [status condition](effects.md#conditions) on a body, like poisoned or blessed (decided 2026-10-28).

**Status conditions aren't facts.** Poisoned, hidden or formless are mechanics: a strength, a duration in rounds or minutes, modifiers, reactions and hints, on mobs as much as players, read by every round of combat. Facts are what the story remembers. The two meet only here: a requirement can test a condition with `Has`.

| Condition | Met when |
| --- | --- |
| `Stat(attribute, min)`, `Race(id)`, `Skill(id, level)`, `GuildRank(id)` | As now |
| `Fact(key, op, value)` | A fact compares (`Eq`, `Ne`, `Gt`, `Gte`, `Lt`, `Lte`): `Fact("char.trust.marin", Gte, 3)`, `Fact("npc.riftwatch:contract_clerk.mood", Eq, "cold")`. Numbers compare as numbers; yes and words only for being equal or not (*built*) |
| `Flag(key)` | A fact is set at all: `Flag("world.rats_broke_out")`, `Not(Flag("char.sold_out_the_guild"))` (*built*) |
| `WorldFlag(id)` | A world flag is set: `Flag("world.id")` by its old name, as zone files write it (*built*) |
| `Event(happening)` | Something is happening right now: `Entered(mob, zone)`, `Died(mob)`; only true while a world event is handling it (*built*) |
| `Quest(id, state)` | A character's quest is held, done (with a given ending, or any), or failed: `Quest("riftwatch:crystals_gone_missing", Ended("sold"))` |
| `HasItem(Item(id) or Tag(tag), n)` | They carry at least that many, anywhere on them: `HasItem(Item("sewers:rat_tail"), 3)`, `HasItem(Tag("herb"), 5)` (*built*) |
| `Has(condition)` | A status condition or trait is on them: `Has("poisoned")`, `Not(Has("rift_taint"))` (*built*) |
| `InRoom(room)`, `InZone(zone)` | They're there (*built*) |
| `All`, `Any`, `Not` | As now |

Trainers, Karma and discovering a hidden skill test facts today (*built*); backgrounds, chosen before a character has any, don't. Trainers and Karma also know what's on someone, where they are and what they carry; discovery doesn't yet, so `Has`, `HasItem`, `InRoom` and `InZone` are false there. Where a condition is tested decides whose facts `char.` and `quest.` mean: the character at the trainer, the player reading the board, the one a rule fired for.

## Rules

A **rule** is: when an event happens, if a condition holds, do these actions.

```ron
(
    // Someone on the quest walks into the culvert at night, and the courier is there.
    on: Entered("sewers:stalls_culvert"),
    if: All([Fact("world.night"), Not(Fact("quest.courier_came"))]),
    do: [Spawn("courier", "sewers:stalls_culvert"), Set("quest.courier_came", true)],
)
```

(`world.night` is the clock's: the [time of day](architecture.md#time) sets it as night falls and clears it at dawn.)

**Events** a rule can match, each written by the system that makes it happen ([game events](architecture.md#game-events)):

| Event | When |
| --- | --- |
| `Killed(mob or tag)` | A mob of that template (or tag) dies; credited to the side that killed it |
| `Obtained(item or tag)` | Someone picks one up, is given one or buys one (the game event is *built*) |
| `Gave(item, to: mob)` | Someone gives a mob an item (the game event is *built*) |
| `Entered(room)` | Someone walks in |
| `Asked(mob, topic)` | `ask <mob> about <topic>` |
| `Examined(thing)`, `Searched(room)` | `examine` (or `look` at an item), `search` (the game events are *built*; things are items for now) |
| `Used(skill)` | Someone uses a skill |
| `Every(minutes)`, `StageBegan`, `StageEnded` | Time passing; a quest moving |

**Actions:** `Set(key, value)`, `Add(key, n)`, `Clear(key)`; `Give(item)`, `Take(item)`; `Spawn(mob, room)`; `Say(mob, text)`; `Announce(scope, text)`; and within a quest, `Advance(stage)`, `End(ending)`, `Fail`. Where an action means what an [effect](effects.md) means, it's the same word.

Rules live in quests (active only while the quest is on that stage), and later in world events and on mobs. A [script](effects.md#scripting) can be a rule's action, as the last resort it already is.

## How it gets built

First, before the quest engine, in five small pull requests: the store, world flags moved onto it, `Fact` in the requirement language, one requirement language (renamed, with `Has`), and `facts` for GMs. See [Quests: how it gets built](quests.md#how-it-gets-built).

## Why did or didn't it happen

Rule-driven content lives or dies on answering that. GMs and admins get **`why`**: `why <player> <quest>` shows the quest's stage, each objective and rule, each condition in it, and whether it held, with the facts it read; `facts <player>` and `facts world` list what's set (*built*: a player's own facts and their quests', online or not).

## Open questions

- [ ] **Storylets:** small reactive pieces chosen by matching facts (Valve's dynamic dialogue; Fallen London's storylets) are the right model for `ask` topics and ambient lines ([#24](https://github.com/Foxworks-Studios/riftward/issues/24)): a mob's answer is the best-matching response for the facts in play. After `ask` itself.
- [ ] **Templated quests:** "kill 10 of any vermin, anywhere", filled in at the board. Cheap variety, beside hand-written ones.
- [ ] **Who may set `npc.` and `world.` facts in game:** GMs, for live events; builders?
