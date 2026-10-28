# Quests and the Adventurers' Guild

[Design summary](README.md) · Status: designed, not built. Quests wait on mobs and game events; guild contracts also need combat for most goals.

Quests are data, written by builders and GMs, and tracked per character. Most of them come from the **Adventurers' Guild**, whose contracts raise a **guild rank** that is entirely separate from a character's power. A character can ignore quests completely, hunt monsters from the first day and grow just as strong; what they give up is the guild's trust, and the doors it opens.

## Two separate tracks

| | Character | Guild rank |
| --- | --- | --- |
| Measures | What you can do | What the guild thinks of you |
| Rises by | Using skills (see [Progression](progression.md)), and Karma | Completing guild contracts, and nothing else |
| Required | Always | Never: joining is optional |
| Unlocks | Abilities | Trainers, harder contracts, guild rooms, vendors and other [perks](#guild-perks), a title |

- **Quests never grant experience, skill levels or Karma** (2026-10-27). Power comes from use, and experience from the fights a contract sends you into, which pay the same whether or not you took the contract; so a quest can't become the efficient way to level, and the player who never takes one isn't behind. Quests pay in coin, items, flags, titles and guild merit.
- **Killing monsters on your own earns no merit,** even the very monster a contract asks for. The guild rewards work it handed out.

## Guild ranks

The guild ranks its members by metal, and every member wears a **rank tag** stamped with theirs.

| Rank | Merit | Typical work |
| --- | --- | --- |
| **Bone** | 0, on joining | Sewer rats, errands, lost cats |
| **Copper** | 50 | Pests outside the walls, escort to the next village |
| **Iron** | 200 | Bandits, wolves, clearing a cellar |
| **Silver** | 600 | Rift-touched beasts, expeditions |
| **Gold** | 1,500 | Dungeons, named monsters |
| **Platinum** | 4,000 | What the guild can't send anyone else to do |

The names and numbers are data in the guild's file, to tune. Letter grades (F to S) would be the same system with different names.

- **Joining is free:** `join guild` in the Guildhall makes a character a Bone member and hands them a Bone tag.
- **Merit comes from contracts:** each guild contract's rewards include merit, scaled to its rank.
- **Promotion needs a trial from Silver up.** Reaching the merit threshold makes a character *eligible*; the clerk then offers a promotion contract (a trial, or a test set by a senior member). Bone to Iron are automatic. That keeps the higher ranks meaningful and gives GMs a natural place to run an event.
- **Rank never drops.** Merit can fall (abandoning or failing a contract costs some), but rank stays at the highest reached. A setback stings without undoing a character's history.
- **Contracts are offered up to one rank above your own,** the genre's usual rule: a Copper member can take Iron work, at Iron risk.
- **Expulsion, with paid reinstatement** (decided 2026-10-27). The guild can throw a member out: for theft in the Guild Quarters, for being caught smuggling on the trails past the Fort ([The setting](setting.md#the-wastes)), or by a GM's hand. An expelled character loses everything membership gives (contracts, perks, lodging, the Fort's gate) and their tag is taken. But **rank never drops:** the guild remembers what they were, and they can buy their way back in at the clerk, for a fee scaled to their rank (and to what they did), returning at the rank they had. `who` and `score` show them as expelled until then.

### The rank tag

The tag is an item, worn at the neck: `bone_tag`, `copper_tag` and so on, each a template with the tag `guild_tag`.

- **Showing it matters.** Guild mobs (the clerk, guild trainers, guards at the guild's inner doors) check the tag a character is *wearing*, not their record, so taking it off makes you an anonymous traveller. Most of the time that's a nuisance; occasionally it's the point.
- **It's bound to its owner.** A stolen tag doesn't let the thief pass as a member: the check is the wearer's own rank, shown by their own tag. Wearing someone else's tag does nothing, or gets you arrested.
- **Lost tags are reissued** by the clerk for a fee. Promotion swaps the old tag for the new one.
- **`who` and `score` show the rank,** `who` only while the tag is worn.

### What rank unlocks

Rank is one more term in the shared [condition language](skills.md#hidden-skills): `GuildRank("iron")` is met at Iron or above. So it works anywhere conditions already do:

```ron
// A trainer who only teaches proven members.
"guild_weaponmaster": (
    teaches: ["swordplay:riposte", "swordplay:disarm"],
    requires: GuildRank("silver"),
    refuses: "The weaponmaster glances at your tag. \"Come back when it's silver.\"",
),
```

| Where | Example |
| --- | --- |
| Trainers | A guild weaponmaster for Silver members; the archive's senior lorekeeper for Gold |
| Contracts | Higher-tier work appears on the board as rank rises |
| Rooms | A members-only door into the Guild Quarters; the archive's inner stacks |
| Dialogue | A mob who only speaks frankly to Gold members |
| Vendors and services | See [Guild perks](#guild-perks) |
| Backgrounds and hidden skills | Any `requires` or `unlock` can test it, like any other condition |

### Guild perks

Beyond trainers, rank buys standing in town. Like everything else here, each perk checks the tag a character is wearing.

| Perk | From | Example |
| --- | --- | --- |
| **Vendors who won't serve a nobody** | Copper and up | The armourer on Guild Row ignores anyone without a tag: *"I don't sell to drifters. Come back with a guild tag."* |
| **Better stock** | Rises with rank | A vendor's goods can each require a rank, so a Silver member sees blades the Iron member doesn't |
| **Guild rates** | Iron and up | A discount at guild-friendly shops, larger at higher ranks |
| **Lodging and storage** | Bone | Rooms in the Guild Quarters at decent rates: a shared dormitory with a locking chest at Bone, private rooms higher up. See [Lodging](#lodging) |
| **Credit** | Silver | Buy now from guild vendors, paid from the next contract's reward |
| **Doors that open** | Rises with rank | Guards and gatekeepers wave tagged members through: the Fort's gate onto the road to the barrier for Silver ([The setting](setting.md#the-wastes)), the keep gatehouse for Gold, perhaps |

A vendor's attitude is data on the mob, in the same condition language:

```ron
"guild_row_armourer": (
    serves: GuildRank("copper"),
    refuses: "The armourer doesn't look up. \"I don't sell to drifters.\"",
    stock: [
        (item: "iron_sword"),
        (item: "steel_longsword", requires: GuildRank("silver")),
    ],
    discount: [(GuildRank("iron"), 5), (GuildRank("gold"), 15)],   // percent
),
```

- **Most of town still serves anyone.** The market stalls, the herbalist and the tinkers don't care who you are. Perks add options; they don't take away the basics a non-member needs.
- **Snobbery works both ways.** The same condition can turn people *against* the guild: a fence in the Back Alley who won't deal with anyone wearing a tag, or a smuggler who charges members double. Taking the tag off is then a real choice.
- **Perks wait on what they need:** vendors on money and shops, lodging on rent and the time system.

### Lodging

The Guildhall rents rooms in the Guild Quarters to members, at decent rates, paid to the clerk by the week. Rank decides what kind of room:

| Rank | Room | Storage |
| --- | --- | --- |
| Bone, Copper | A bunk in the shared dormitory | A locking chest at the foot of the bunk |
| Iron, Silver | A shared room with one or two others | A locking chest |
| Gold and up | A private room with its own door and key | The chest, plus whatever fits in the room |

- **The chest is the point for new characters.** Carrying space is tight (see [Bulk](items.md)), so a cheap, safe place to leave things is the first real perk of joining. A Bone member can stash a spare sword and their loot between trips.
- **Each chest is its owner's.** It's an ordinary container with a lock (see [Locks](items.md)), placed in the dormitory when the character first rents and keyed to a key the clerk hands over. Other sleepers can see the row of chests but not open them without the key, picking or bashing.
- **Thieves are a guild matter.** Picking or bashing a chest in the Guild Quarters is possible, it's an ordinary lock, but the guild notices: it sets a flag that the clerk, guards and guild vendors react to, and can cost merit or rank standing. The open question is how far that goes.
- **Rooms are real places.** The dormitory is a room in the `riftwatch` zone; Gold members' private rooms are doors off the quarters, each locked to its tenant. There's no instancing: the dormitory is shared and players meet there.
- **Unpaid rent.** If the rent lapses, the chest stays locked and its contents wait with the clerk for a while (collected for a fee) before being sold off. Nothing vanishes silently.
- **Resting there** will matter once health and stamina regenerate over time: a bunk is a safe place to recover, and to log out.
- **Non-members have the inn,** once Riftwatch has one: dearer, and with no chest.

**Remote trainers teach as a reward.** Beyond the entry skills town trainers teach, a trainer out in the world (an old axeman in the Darkwood) teaches a higher skill only to someone who has done what they ask: a quest whose reward is the lesson.

**Common trainers stay open to everyone.** The training yard, the herbalist and the shrine teach anyone; guild rank gates the better-than-common instruction, alongside rarity and the mastery rules for player teaching (see [Acquiring skills](skills.md#acquiring-skills)).

### Saved per character

A `character_guild` table holds rank, merit, and the number of contracts completed. The design leaves room for other organisations later (the garrison, a temple, a thieves' guild) by keying it on an organisation id, with the Adventurers' Guild as the first one.

## Quests

A quest is a RON file in `assets/quests/`, hot-reloaded like zones. Guild contracts are quests with `guild: Contract(rank, merit)`.

```ron
// assets/quests/sewer_rats.quest.ron
(
    name: "Rats in the Drains",
    summary: "The guild wants the rats in the sewers thinned out before they reach the streets.",
    offered_by: Board("riftwatch:contract_hall"),
    guild: Contract(rank: "bone", merit: 10),
    requires: None,
    repeat: Every(Days(1)),
    stages: [
        (text: "Kill ten sewer rats.", goal: Count(Killed("sewer_rat"), 10)),
        (text: "Report to the contract clerk.", goal: TalkTo("contract_clerk")),
    ],
    rewards: [Coin(15)],
)
```

### Goals

Every goal listens to the shared [game events](architecture.md#game-events), the same ones skill triggers use.

| Goal | Complete when |
| --- | --- |
| `Count(event, n)` | An event happens `n` times: `Killed(mob)`, `Gathered(item)`, `UsedSkill(skill)` |
| `TalkTo(mob)` | The character talks to the mob (through its dialogue) |
| `GaveItem(item, to: mob)` | The character gives the mob the item |
| `EnteredRoom(room)` | The character reaches a room |
| `Reach(condition)` | Any condition is met: a flag, a skill level, a world counter |
| `Manual` | A GM advances it by hand, for live events |

### Where quests come from

| `offered_by` | How players find it |
| --- | --- |
| `Board(room)` | Listed on a board: the Contract Hall's for guild contracts; a tavern or temple board for others |
| `Mob(id)` | A mob offers it through dialogue |
| `Item(id)` | Reading or examining an item starts it: a torn map, a dying man's letter |
| `Event` | A [world event](world.md) offers it to everyone in a zone: the rats have broken through the grates |

### Commands

```
contracts              -> the board here, at your rank and one above
accept 2               -> take the second contract, for its token
report                 -> turn in a finished contract at the clerk, with its token
quests                 -> your quests and their current stage
abandon rats           -> give one up (contracts cost some merit)
join guild             -> become a Bone member, in the Guildhall
reinstate              -> buy your way back in after expulsion, at the clerk
```

### Tokens and reporting

The guild's way, from [the setting](setting.md#places): a contract is **claimed at the counter for a token**, and **paid when the token comes back with proof**.

- **`accept`** hands over a contract token, an item naming the contract, bound to whoever took it. Losing it means asking the clerk for another, for a fee.
- **`report`** at the clerk, with the token, turns the contract in: if every stage is done, the clerk takes the token (and any proof the contract asks to be handed over) and pays out.
- **Proof is what the work leaves:** kills are counted as they happen (`Count(Killed(...))`), and things to bring back are items handed over on reporting (`GaveItem`), such as rat tails or a slime's heart, which is where [harvesting](darkwood.md#harvest-whats-cut-loot-whats-left) comes in.
- **This replaces `TalkTo`** for guild contracts until there's dialogue: reporting is the conversation.

### Group contracts

Accepting a contract while in a [group](groups.md) offers it to every member in the room whose rank allows it, each of whom can take it or not. For those who hold it, progress is shared: a rat any of them kills counts for all, and a find any of them brings back fills the order. When it's reported, each earns the contract's full merit, and its coin is split evenly among them. Nothing is shared with a member who didn't take the contract.

### Rewards

`Coin(n)`, `Item(id)`, `Flag(id)` (which hidden skills and world events can test), `Title(id)`, and for contracts the guild merit. Not skill levels or Karma.

### Saved per character

A `character_quests` table: quest id, current stage, progress counter, when it started and when it was last completed (for `repeat`).

## GM tools

GMs and admins make and run quests from inside the game, like builders do rooms:

| Command | Does |
| --- | --- |
| `quest create <id>` / `quest edit <id> <field> ...` | Write a quest, saved to its RON file |
| `quest grant <player> <id>` | Give a quest to a player directly |
| `quest advance <player> <id>` | Push a player past a stage, for `Manual` goals or a broken one |
| `quest reward <player> <id>` | Pay out the rewards by hand |
| `guild promote <player>` / `guild merit <player> <n>` | Run a promotion trial live, or correct merit |
| `guild expel <player>` / `guild reinstate <player>` | Throw a member out, or take them back without the fee |

These pair with [puppeting](architecture.md#game-masters-and-puppeting): a GM can play the contract clerk, hand out a one-off contract with `quest grant`, then puppet the monster at the end of it.

## Open questions

- [ ] **Rank names and thresholds:** the metals are the setting's own (the Silver Hearth Guildhall's quest board is divided Bone to Platinum: [The setting](setting.md#places)); the Fort lets Silver members and up onto the road to the barrier ([The setting](setting.md#the-wastes), 2026-10-27); still open: are there ranks above Platinum (Mithril, Adamant) reserved for legendary deeds, and do higher ranks open deeper parts of the Wastes?
- [ ] **Is the Guild Archive members-only?** It holds the public Hermetic Study path to Awakening ([Magic](magic.md#which-paths-are-known)). Members-only would make the public path to magic "join the guild": free and fitting, but a choice to make deliberately.
- [ ] **Do contracts pay coin at all,** or only merit and items, until there's an economy to balance against?
- [ ] **Theft in the Guild Quarters:** expulsion with paid reinstatement is the guild's answer (2026-10-27); still open: whether a first offence is only a fine or lost merit, and how much reinstatement costs.
- [x] **Group contracts:** every member who holds the contract earns its full merit, and the coin is split (2026-10-27): see [Group contracts](#group-contracts).
