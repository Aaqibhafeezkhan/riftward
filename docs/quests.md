# Quests and the Adventurers' Guild

[Design summary](README.md) · Status: guild membership is built (2026-10-28: joining, ranks and merit, rank tags, `GuildRank` conditions, expulsion and reinstatement, and the GMs' `guild` tools; `src/guild.rs`, the ranks in `assets/guild.ron`). Quests, contracts and the board, perks and lodging are designed, not built.

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

- **Joining is free:** `join guild` in front of the clerk (Marin Harrow, in the Contract Hall) makes a character a Bone member and hands them a Bone tag. `guild` shows a member's rank, merit, contracts done and what the next rank takes.
- **The ranks are data,** in `assets/guild.ron`: each rank's merit, whether it needs a trial, its tag, and what reinstatement costs at it; and the clerk, and the fee for a lost tag.
- **Merit comes from contracts:** each guild contract's rewards include merit, scaled to its rank.
- **Promotion needs a trial from Silver up.** Reaching the merit threshold makes a character *eligible*; the clerk then offers a promotion contract (a trial, or a test set by a senior member). Bone to Iron are automatic. That keeps the higher ranks meaningful and gives GMs a natural place to run an event.
- **Rank never drops.** Merit can fall (abandoning or failing a contract costs some), but rank stays at the highest reached. A setback stings without undoing a character's history.
- **Contracts are offered up to one rank above your own,** the genre's usual rule: a Copper member can take Iron work, at Iron risk.
- **Expulsion, with paid reinstatement** (decided 2026-10-27). The guild can throw a member out: for theft in the Guild Quarters, for being caught smuggling on the trails past the Fort ([The setting](setting.md#the-wastes)), or by a GM's hand. An expelled character loses everything membership gives (contracts, perks, lodging, the Fort's gate) and their tag is taken. But **rank never drops:** the guild remembers what they were, and they can buy their way back in at the clerk, for a fee scaled to their rank (and to what they did), returning at the rank they had. `who` and `score` show them as expelled until then.

### The rank tag

The tag is an item, worn at the neck: `bone_tag`, `copper_tag` and so on, each a template with the tag `guild_tag`.

- **Showing it matters.** Guild mobs (the clerk, guild trainers, guards at the guild's inner doors) check the tag a character is *wearing*, not their record, so taking it off makes you an anonymous traveller. Most of the time that's a nuisance; occasionally it's the point.
- **It's bound to its owner.** A stolen tag doesn't let the thief pass as a member: the check is the wearer's own rank, shown by their own tag. Wearing someone else's tag does nothing, or gets you arrested. (*Built* as: a rank shows only for a member in good standing wearing a guild tag, any guild tag; a non-member wearing one shows nothing.)
- **Lost tags are reissued** by the clerk for a fee (`guild tag`, 5 silver to start). Promotion makes a new tag due: `guild tag` swaps the old one for it free.
- **`who` and `score` show the rank,** `who` only while the tag is worn.

### What rank unlocks

Rank is one more term in the shared [requirement language](skills.md#hidden-skills): `GuildRank("iron")` is met at Iron or above. So it works anywhere conditions already do:

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

A vendor's attitude is data on the mob, in the same requirement language:

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

One engine runs every quest: a guild contract is a quest the guild posts, with a rank and merit. **Designed 2026-10-28, not built.** The examples below are written as real data on purpose: [what they need](#what-the-first-contracts-need) that the code can't do yet is listed at the end.

### Quest files

**Each quest is its own RON file,** in a folder named for the zone it belongs to: `assets/quests/riftwatch/rats_in_the_drains.quest.ron` is the quest `riftwatch:rats_in_the_drains`, named the way rooms and mobs are. Bare ids inside it (`sewer_rat`, `contract_hall`) are the zone's own; anything else is `zone:id`. Files are hot-reloaded, and each is checked when it loads, as skills and spells are: every mob, item, room and condition it names must exist, and its text must be there.

```ron
// assets/quests/riftwatch/rats_in_the_drains.quest.ron
(
    name: "Rats in the Drains",
    // One line, as the board lists it.
    summary: "The guild wants the sewer rats thinned before they reach the streets.",
    // Read out in full when someone looks at the notice, and kept in the journal.
    description: "Twenty sewer rats, killed in the sewers under the market. Bring their tails as proof.",
    // Where it's offered: a board, a mob, an item that starts it, or a world event.
    posted: Board("contract_hall"),
    // Only shown, and only takeable, while this holds (any world-event condition).
    posted_when: None,
    // A guild contract: its rank, and the merit it pays.
    contract: Some((rank: "bone", merit: 10)),
    // One holder at a time, off the board while claimed, back on it if not done in time.
    availability: Claimed(for: Days(1)),
    // Who may take it, beyond the rank: any condition.
    requires: None,
    // How soon a character can take it again once they've finished it.
    repeat: Every(Days(1)),
    stages: [
        (
            text: "Kill twenty sewer rats and bring back ten of their tails.",
            objectives: [Kill("sewer_rat", 20), Bring(Item("sewers:rat_tail"), 10)],
        ),
    ],
    // Who it's reported to: the clerk, for the guild's contracts, unless it says otherwise.
    report_to: None,
    rewards: [Coin(200)],
)
```

- **Stages run in order; the objectives in a stage are done in any order,** all of them needed. Most quests are one stage.
- **Text is the quest's own,** in its file: the summary, the description, each stage's text. Nothing a player reads is written in code.

### Open and claimed quests

A quest is one of two kinds, by its `availability`:

| | `Open` | `Claimed(for: ...)` |
| --- | --- | --- |
| On the board | Always | Only while no one has it |
| Held by | Anyone, any number at once | One character (or one group) at a time |
| Example | Narina always needs herbs; the kitchen always needs meat | Clear the sewers; the Rat King's bounty |
| Time limit | None, unless `within` sets one | `for`: not reported in time, it **fails** and goes back on the board |

- **A claim that runs out fails:** the holder is told, Marin's record marks it failed, a contract costs some merit, and the notice is back on the board for someone else.
- **A character can't hold the same quest twice;** `repeat` says how soon they can take it again once it's done (`Once`, `Every(Days(1))`, `Always`).

### The board, Marin's record and your journal

**The Guildhall is where open work comes from.** The board in the Contract Hall lists what's on offer; **Marin Harrow logs what each member takes,** and how it ends, in the guild's record. How the work is going is the member's own business, kept in their journal.

- **`board`** (or `contracts`) in the Contract Hall lists what's posted and not claimed, at the reader's rank and one above: the number, the rank, the name and the one-line summary. Non-members see it too, but can't take guild contracts.
- **`board <n>`** reads a notice in full: its description, what it asks, what it pays, how long a claim lasts.
- **`accept <n>`**, with Marin at her counter: "You take the notice down and hand it to Marin Harrow, who writes it into the guild's record against your name." A claimed notice comes off the board. Without the clerk there, nothing can be taken.
- **Marin's record** is the guild's ledger, and holds only what the guild needs to know: each contract **accepted, completed or failed** (or given up), with the date. It never says how far along you are, only that you took it and how it came out. **`guild record`** shows your entries, and GMs see anyone's.
- **`journal`** (or `quests`) is your own account, anywhere: each quest you're on, where you are in it in its own words (the stage's journal text), each objective's progress, how long a claim has left; and below, what you've finished. Quests from outside the guild are kept in it too, and in no ledger.
- **`report`** to whoever the quest is reported to (Marin, unless it names someone else, like Narina for her herbs): if every objective is done, any items it asks to be brought are handed over, and the rewards are paid.
- **`abandon <quest>`** gives one up, anywhere: it's written off in Marin's record as given up (a contract costs some merit) and a claimed notice goes back on the board.

This replaces the earlier plan of a physical token (2026-10-28): the record is the proof of who took what.

### Objectives

| Objective | Done when | Counted |
| --- | --- | --- |
| `Kill(mob, n)` | `n` of that mob template, or of mobs with that tag (`Kill(Tag("vermin"), 20)`), are killed by you or your group | As they die |
| `Bring(item, n)` | You have `n` of that item, or of items with that tag (`Bring(Item("sewers:rat_tail"), 10)`, `Bring(Tag("herb"), 5)`, matched as `HasItem` matches), when you report: they're handed over | When reported |
| `Obtain(item, n)` | You pick up, or are given, `n` of that item or tag; unlike `Bring`, nothing is handed over (the letter you're to carry; a clue) | As it happens |
| `Reach(room)` | You enter the room | As it happens |
| `Use(skill, n)` | You use the skill `n` times | As it happens |
| `Meet(condition)` | A condition holds: a flag, a skill level, a world flag | When reported |
| `Manual` | A GM says so (`quest advance`) | By hand |

- **Objectives are shorthand for [rules](facts.md#rules).** `Kill("sewer_rat", 20)` is a rule (on `Killed("sewer_rat")`, `Add("quest.kill.sewer_rat", 1)`) and a condition (that fact at 20 or more); the stage ends when all its objectives' conditions hold. Designers write objectives for the common cases, and a rule of their own for anything odd, with no engine change.
- **What counts for whom in a group** is set out in [Progress in a group](#progress-in-a-group). A stranger's deeds never count.
- **`Bring` is the proof,** and needs nothing new: rat tails and the Rat King's tooth drop as loot today; harvested parts will count the same way once harvesting exists.
- **Talking to someone** waits on dialogue; until then, `report` is the conversation.

### Beyond collecting: quests with steps, choices and consequences

Collection (kill so many, bring so many) is the simplest kind, and the first contracts are mostly that. The engine is designed for the rest from the start, so that it grows by adding objectives and actions, never by being rebuilt.

**The kinds of quest it has to carry:**

| Kind | Example | Needs |
| --- | --- | --- |
| **Collection** | Twenty rats; five herbs | `Kill`, `Bring` |
| **Delivery** | A letter to the hunter | An item handed over on taking it; `Give` it to someone |
| **Exploration** | Reach the ravine floor; find the old city's sealed door | `Reach`, `Search` (hidden exits exist) |
| **Investigation** | Who is taking the guild's crystals? | `Ask` someone about something, `Examine` a thing, clues that open the next step |
| **Choice** | Return the crystals to the guild, or sell them to Talla Mirk | Steps that branch, each way ending differently |
| **Escort** | Bring Narina safely to the Rift-Scar and back | A mob that follows you (following is built), and failing if it dies |
| **Bounty on someone** | The courier who carries the stolen crystals | A mob that appears for the quest, and whose death counts for it |
| **Defence** | Hold the culvert while the rats break out | A world event's waves, and lasting a while |
| **Chain** | Narina's secret, told only once you've helped her twice | A quest that needs another one done (`Quest(id)` as a condition) |
| **Discovered** | Pick up the dying courier's letter, and a quest begins | `posted: Item(...)`, or a world event offering it |
| **Shared by the town** | Everyone's kills push back a breach | A world counter that everyone's deeds feed (world events have them) |

**What that asks of a quest file,** beyond the collection example above:

- **Steps by name, not only in order.** A list of stages runs in order, as before; a stage can instead say where it leads (`next: "alley"`), or offer **choices**, each its own set of objectives and its own next stage: whichever is done first is the way taken, and the other closes. A quest can end in more than one way (`End("returned")`, `End("sold")`), each with its own rewards and its own line in the journal.
- **Actions when a stage begins or ends,** in one small vocabulary, shared with the [effects](effects.md) where the meaning is the same: `Give(item)` and `Take(item)`, `Spawn(mob, room)` (a mob that exists for the quest), `Set`, `Add` and `Clear` on [facts](facts.md) (`char.sold_out_the_guild`, `world.courier_caught`, `quest.warned`), `Say(mob, text)` (someone speaks to the holder), `Fail`. They're the actions of [rules](facts.md#rules), and a stage can carry rules of its own besides its objectives.
- **Ways to fail,** besides a claim running out: `fails_if: [Dies("narina"), Lost("letter")]`, an escort killed or a carried item gone.
- **The journal follows the story:** each stage has its own `journal` text, so your journal says what's happening now ("Talla Mirk says the crystals go down the culvert at night."), not a list of counters. Hidden objectives stay hidden until their stage.
- **Quest mobs are real.** No instancing: a mob spawned for a quest is in the world for anyone to see, and anyone can fight it; only its holder's side gets the quest's credit for it. It's tagged to the quest, so `Kill(Quest("courier"))` means *that* courier, and it's cleared away if the quest ends without it.

**New objectives:**

| Objective | Done when |
| --- | --- |
| `Ask(mob, topic)` | You ask someone about something: `ask talla about crystals`. While a stage is active it gives that mob that topic and what they say, so this is the first, small piece of [dialogue](#dialogue) |
| `Give(item, to: mob)` | You give someone an item (`give` exists) |
| `Examine(thing)` | You look closely at an item or a room's feature |
| `Search(room)` | You search the room (`search` exists), finding what the quest has put there |
| `Escort(mob, to: room)` | A mob following you reaches the room alive |
| `Last(minutes)` | You hold out for so long: in a defence, while a world event runs |

#### Dialogue

There's no dialogue yet, and quests are what first need it. The smallest version that carries investigations: **`ask <someone> about <topic>`.** A mob answers from its own topics (data on its template, later) and from the topics any quest stage you're on gives it. An answer can move a quest on (`Ask`), and nothing else is needed for "Marin points you to the back alley". Branching conversation trees can come later, if ever.

#### Worked example: Crystals Gone Missing

A Copper contract, claimed, three days. It uses most of the above, and is the target the engine heads for.

```ron
// assets/quests/riftwatch/crystals_gone_missing.quest.ron
(
    name: "Crystals Gone Missing",
    summary: "Vitae crystals are leaving the guild's stores, and Marin wants to know how.",
    posted: Board("contract_hall"),
    contract: Some((rank: "copper", merit: 30)),
    availability: Claimed(for: Days(3)),
    stages: [
        (
            id: "start",
            journal: "Marin Harrow says crystals are going missing between the cellar and the ledger. Ask around.",
            objectives: [Ask("contract_clerk", "crystals")],
            topics: {
                "contract_clerk": [("crystals", "\"Three this week. Nobody comes down to the cellar but us, and the rats. Ask in the back alley: anything stolen in this town passes through there.\"")],
            },
            next: "alley",
        ),
        (
            id: "alley",
            journal: "Someone in the back alley may know where stolen crystals go.",
            objectives: [Ask("hooded_figure", "crystals")],
            topics: {
                "hooded_figure": [("crystals", "Talla Mirk's hands go still. \"They come up the culvert by night, from the sewers. A courier. Don't tell Marin I said so.\"")],
            },
            on_end: [Spawn("courier", "sewers:stalls_culvert")],
            next: "courier",
        ),
        (
            id: "courier",
            journal: "A courier brings the crystals up the culvert at night. Find them.",
            objectives: [Kill(Quest("courier"), 1)],
            on_end: [Give("stolen_crystals")],
            next: "choice",
        ),
        (
            id: "choice",
            journal: "You have the stolen crystals. Marin wants them back; Talla Mirk would pay well for them.",
            choices: [
                (text: "Return them to Marin.", objectives: [Give("stolen_crystals", to: "contract_clerk")], next: End("returned")),
                (text: "Sell them to Talla Mirk.", objectives: [Give("stolen_crystals", to: "hooded_figure")], next: End("sold")),
            ],
        ),
    ],
    endings: {
        "returned": (journal: "You returned the crystals to the guild.", rewards: [Coin(300), Merit(30)]),
        "sold": (journal: "You sold the guild's crystals to Talla Mirk.", rewards: [Coin(900), Flag("sold_out_the_guild")]),
    },
    fails_if: [Lost("stolen_crystals")],
)
```

The `sold` ending pays no merit and sets a flag the guild can learn of later (a [hidden skill](skills.md#hidden-skills), a world event, Marin's coldness), which is where consequences start: what a quest leaves behind on the character and the town is what other quests, mobs and doors can test.

**Narina's Secret** is the other target: a chain (after two *Herbs for the Herbalist*), an escort to the Rift-Scar with Narina following and failing if she dies, `Examine` the corrupted plants, and an ending that teaches nothing but sets the flag her later quests need.

#### What that needs, beyond the first contracts

| Needed | Exists? |
| --- | --- |
| Stages by id, `next`, choices and endings | The quest engine's own |
| Actions on a stage's start and end (`Give`, `Take`, `Spawn`, `Flag`, `WorldFlag`, `Say`, `Fail`) | Some exist as effects (`Spawn`), world flags exist; the rest are the engine's |
| `ask <mob> about <topic>` | No: the first piece of dialogue |
| Events for asking, giving, examining and searching | `give`, `examine` and `search` exist; none writes a game event yet |
| Quest mobs: spawned for a quest, tagged to it, cleared when it ends | Spawning exists; tying a mob to a quest is new |
| Escort: a mob following the holder | Following is built; mobs following a player on a quest's say-so is new |
| `Quest(id)` and character flags in the requirement language | Conditions exist; neither term does yet |
| Failing on a death or a lost item | No |

### Progress in a group

A group on a quest moves through it together, **as long as they're together:** what any of them does for it counts for every member of the [group](groups.md) who holds the same quest **and is in the same room** when it happens (decided 2026-10-28). A member off in town, or in another zone, gets nothing from what the others do meanwhile, as with a victory's [experience](groups.md#what-a-group-shares).

- **Taking it:** accepting a quest while grouped offers it to every member in the room who may take it, each of whom can take it or not. A claimed contract is held by those who took it together, with one timer.
- **Kills:** one kill counts once, for every holder in the room. Twenty rats killed by the group finish "Kill twenty rats" for each holder who was there for all twenty, not twenty each; a holder who arrived halfway has their share.
- **Finding and picking up:** when one member picks up the letter (`Obtain`), every holder in the room moves on, though the letter itself is in one member's pack. The same for `Reach`, `Examine`, `Search` and `Ask`: one of them doing it is all of them there doing it.
- **Facts a quest sets:** the `quest.` and `char.` facts a stage or rule sets are set for every holder in the room: if the group sold the crystals to Talla, all of them standing there did.
- **Choices:** the way taken is the way taken for every holder in the room; for a holder elsewhere, the choice is still to come.
- **Handing over and reporting:** `Bring` items are handed over once, by whoever reports; reporting finishes the quest for every holder in the room with them who has done their part. Each earns its full merit, and the coin is split among them, as a [foe's coins](groups.md#what-a-group-shares) are. A claimed contract, once reported, is over for those who weren't there too, unpaid: the work's been done.
- **Leaving the group** keeps what's been done so far, and the quest goes on alone from there. Someone joining partway can take a quest the group holds and starts it fresh, except a claimed one, which only those who took it hold.
- **Only those who hold it:** a member who didn't take the quest gets nothing from it, and their deeds don't move it.

### Rewards

`Coin(n)`, `Item(id)`, `Set(fact, value)` (which hidden skills, world events, doors and other quests can test; a title is `char.title.<id>`), and for contracts the guild merit. Never experience, skill levels or Karma.

### Saved

- **`character_quests`:** character, quest id, its state (held, done with which ending, failed, abandoned), the stage **by its id**, when it was taken and when it's due, and when it was last finished (for `repeat`). Objectives' counts and the quest's own facts are `quest.` [facts](facts.md).
- **Editing a quest someone is partway through** is safe as long as their stage still exists: progress is kept by stage id, never by position. If an edit removes the stage they're on, the quest is marked broken in their journal and GMs are told, rather than guessing.
- **`guild_record`:** Marin's ledger: character, quest, what happened (accepted, completed, failed, given up) and when. Only ever added to.
- **`quest_claims`:** which claimed quests are out, held by whom, until when, so a restart doesn't put a taken notice back on the board or forget a deadline.

### The first contracts

Written as data, to find what the engine needs. Rank and merit are first guesses.

| Quest | Kind | Asks | Report to | Pays |
| --- | --- | --- | --- | --- |
| **Rats in the Drains** (Bone) | Claimed, a day | Kill 20 sewer rats, bring 10 rat tails | Marin | Coin, 10 merit |
| **Herbs for the Herbalist** (Bone) | Open, always | Bring 5 herbs (any with the `herb` tag) | Narina Greenthorn | Coin, 2 merit |
| **Roaches in the Cellar** (Bone) | Claimed, a day | Kill 6 giant cockroaches | Marin | Coin, 8 merit |
| **The Rat King** (Copper) | Claimed, two days; posted only while the rats have broken out | Kill the Rat King, bring his tooth | Marin | Coin, 25 merit, a title |
| **What Feeds the Slimes** (Copper) | Claimed, two days | Kill the Slime Lord, bring the heart of the deep | Marin | Coin, 25 merit |
| **Word to the Hunter** (Bone) | Claimed, a day | Carry a letter to the hunter in the woods (handed over on taking it) | The hunter | Coin, 5 merit |
| **The Ravine** (Iron) | Claimed, two days | Reach the ravine floor and come back | Marin | Coin, 15 merit |
| **Meat for the Kitchen** (Bone) | Open, always | Bring 4 portions of meat | Marin | Coin, 2 merit |

### What the first contracts need

What the engine itself is (quest files, the board, Marin's record, `accept`, `journal`, `report`, `abandon`, claims and their timers) is the work. Beyond that, the first contracts need:

| Needed | For | Exists? |
| --- | --- | --- |
| **Who killed it, on a death:** the `Died` event names its `killer` (and so their side) | Every `Kill` | Yes (2026-10-28): whoever struck the blow, or put on the bleeding or poison that did it |
| **Kills by tag** | `Kill(Tag("vermin"))` | Mobs have tags; matching them is new |
| **Bring by template or tag, and taking the items** | Rat tails, herbs, the King's tooth | Items have tags; handing over is new |
| **Report to any mob,** in any zone | Narina, the hunter | No |
| **Items handed over on taking a quest,** taken back if it's abandoned | The letter to the hunter | No: and the letter itself, an item template |
| **Posting only while a world condition holds** | The Rat King, posted only while the rats are out | The requirement language exists (world events); boards using it is new |
| **A deadline that runs out on its own,** across restarts | Every claimed quest | A heartbeat system and `quest_claims` |
| **A skill-use event** | `Use(skill, n)` | No: none of the first contracts needs it yet |
| **Meat** | Meat for the Kitchen | No: [harvesting](darkwood.md#harvest-whats-cut-loot-whats-left) (Butchering). The contract can be written now and come into its own then |
| **Titles** | The Rat King's reward | No: a title is a flag on the character for now |

## GM tools

GMs and admins make and run quests from inside the game, like builders do rooms:

| Command | Does |
| --- | --- |
| `quest create <id>` / `quest edit <id> <field> ...` | Write a quest, saved to its RON file |
| `quest grant <player> <id>` | Give a quest to a player directly |
| `quest advance <player> <id>` | Push a player past a stage, for `Manual` goals or a broken one |
| `quest reward <player> <id>` | Pay out the rewards by hand |
| `quest release <id>` | Put a claimed notice back on the board, its holder's claim written off |
| `why <player> <quest>` | Show the quest's stage, each objective and rule, each condition and whether it held, and the facts it read ([Facts](facts.md#why-did-or-didnt-it-happen)) |
| `facts <player>` / `facts world` | List the facts set: a player's own and their quests', online or not, or the world's (*built*) |
| `guild promote <player>` / `guild merit <player> <n>` | Run a promotion trial live, or correct merit (*built*: merit raises rank through Iron by itself, and says when a trial rank is earned) |
| `guild expel <player>` / `guild reinstate <player>` | Throw a member out, taking their tag, or take them back without the fee (*built*) |

These pair with [puppeting](architecture.md#game-masters-and-puppeting): a GM can play the contract clerk, hand out a one-off contract with `quest grant`, then puppet the monster at the end of it.

## How it gets built

In the smallest pull requests that each leave the game working, every one with its tests and its docs (planned 2026-10-28). Each line is one pull request; the first contracts arrive as soon as the engine can carry them, so content tests the engine as it grows. [Facts](facts.md) come first, since quests are built on them.

**Facts**

1. *Built.* **The fact store:** the `facts` table, scopes (`world.`, `char.`, `quest.`, `npc.`), values, setting, adding, clearing and lapsing, loaded at startup. Nothing uses it yet.
2. *Built.* **World flags become facts:** `world_state` moves into the store, and world events, gated spawns and `worldflag` read and write it. Nothing a player sees changes.
3. *Built.* **`Fact` in the requirement language:** `Fact(key, op, value)` and `Flag(key)`, with `world.`, `char.` and `npc.` facts, tested by trainers, Karma and (3b) discovery. (Backgrounds are chosen before a character has facts of their own, and test none.)
    - *Built.* **3b. Discovery reads facts too:** stumbling on a hidden skill tests its requirement with the character's facts, which means the fact store reaching skill practice in commands and in combat.
4. **One requirement language**, in three small steps:
    - *Built.* **4a. The rename:** `Condition` becomes `Requirement` (`requirements.rs`), what every `requires:` holds, so "condition" means only what's on a body, like poisoned.
    - *Built.* **4b. World events' conditions merge into it:** their `When` becomes requirements plus their events as triggers; who's tested becomes optional, since a world event has no one.
    - *Built.* **4c. New terms:** `HasItem`, `InRoom`, `InZone`, and `Has(condition)`: a status condition or trait on the character.
5. *Built.* **`facts <player>` and `facts world`,** for GMs: a player's facts (online or not) and each of their quests', or the world's.

**Events quests listen to**

6. *Built.* **`Died` names who killed it** (and so their side, `groups::side(killer)`): whoever struck the blow, or put on the bleeding or poison that did it, which is their kill for experience and loot too.
7. *Built.* **`Obtained`:** picking up, being given and buying write it, for the item and everything in it. Taking something out of your own bag isn't obtaining it.
8. *Built.* **`Gave`, `Examined` and `Searched`:** `give`, `examine` (and `look` at an item) and `search` write them. Rooms have no features to examine yet, so `Examined` is for items; giving to a mob waits on mobs holding items ([#55](https://github.com/Foxworks-Studios/riftward/issues/55)).

**The engine, with the first contracts**

9. **Quest files:** loading `assets/quests/<zone>/*.quest.ron`, checking them, and hot reload. One file, *Rats in the Drains*. No commands yet.
10. **The board:** `board` and `board <n>` in the Contract Hall, read-only: what's posted, at your rank and one above.
11. **Taking a quest:** `accept <n>` with Marin, the `character_quests` table, Marin's record (`guild record`: accepted, completed, failed), and `journal`. Open quests only; nothing counts yet.
12. **Rules and `Kill`:** objectives as rules over `quest.` facts, `Kill` by template and tag, progress in the journal, and moving from stage to stage.
13. **`report` and rewards:** coin, merit and `Set`, paid to whoever the quest is reported to. *Rats in the Drains* can be done start to finish. Plus `Bring`, so it asks for the tails. Adds *Roaches in the Cellar*.
14. **Claimed quests:** `quest_claims`, off the board while held, deadlines run out and fail on their own (across restarts), `abandon`, and what failing costs.
15. **Progress in a group:** offering to the group, one claim and timer, and the progress of every holder in the room moving together.
16. **`report_to`, `Obtain`, and items given on taking a quest:** adds *Herbs for the Herbalist* and *Word to the Hunter*.
17. **`Reach`, `Meet` and `posted_when`:** adds *The Ravine*, *The Rat King* and *What Feeds the Slimes*.
18. **`why`,** for GMs, once there are rules to explain.

**Steps, choices and consequences**

19. **Stages by id, `next`, and actions** (`Give`, `Take`, `Set`, `Say`) when a stage begins or ends.
20. **`ask <mob> about <topic>`** and the `Ask` objective: topics given by quest stages.
21. **Choices and endings,** each with its own rewards and journal line.
22. **Quest mobs:** `Spawn` for a quest, tied to it, counted by `Kill(Quest(...))`, cleared when it ends. *Crystals Gone Missing* can be written in full.
23. **`fails_if`:** a death, a lost item.
24. **Escort:** a mob following the holder on the quest's say-so, `Escort(mob, to: room)`. *Narina's Secret*.
25. **GM quest tools:** `quest grant`, `advance`, `reward`, `release`; writing quests in game (`quest create`, `quest edit`) last.

Meat for the Kitchen waits on harvesting; it can be written with step 13 and works once Butchering exists.

## Open questions

- [ ] **Rank names and thresholds:** the metals are the setting's own (the Silver Hearth Guildhall's quest board is divided Bone to Platinum: [The setting](setting.md#places)); the Fort lets Silver members and up onto the road to the barrier ([The setting](setting.md#the-wastes), 2026-10-27); still open: are there ranks above Platinum (Mithril, Adamant) reserved for legendary deeds, and do higher ranks open deeper parts of the Wastes?
- [ ] **Is the Guild Archive members-only?** It holds the public Hermetic Study path to Awakening ([Magic](magic.md#which-paths-are-known)). Members-only would make the public path to magic "join the guild": free and fitting, but a choice to make deliberately.
- [ ] **Do contracts pay coin at all,** or only merit and items, until there's an economy to balance against?
- [ ] **Theft in the Guild Quarters:** expulsion with paid reinstatement is the guild's answer (2026-10-27); still open: whether a first offence is only a fine or lost merit, and how much reinstatement costs.
- [ ] **How much a failed or abandoned contract costs:** a share of its merit (half?), or a set amount by rank?
- [ ] **Board refresh:** are some contracts posted only at certain times or for a while (a bounty that's up for three days), beyond `posted_when`?
- [x] **Group contracts:** every member who holds the contract earns its full merit, and the coin is split (2026-10-27): see [Progress in a group](#progress-in-a-group): what any holder does counts for every holder in the same room (2026-10-28).
