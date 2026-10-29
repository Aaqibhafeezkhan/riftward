# Items, containers and locks

[Design summary](README.md) · Status: built, except decay and traps. Conditions on single items (a flaming blade) are built too: see [Item conditions](effects.md#item-conditions-as-built). Item spawns and unique items are built (see [The living world](world.md#items))

Items are entities defined by templates in zone files. They can be carried, worn, dropped, or put inside containers. An item becomes **real**, saved to the database, the moment a player touches it, and most items slowly **decay** when left lying around. Doors and containers share one **lock** model: open, closed or locked, with keys, lockpicking, bashing and traps.

## Templates

Zone files define item templates; [spawns](world.md#spawns-and-resets) place them in the world, and builders make them with `create`.

```ron
items: {
    "longsword": (name: "a longsword", keywords: ["longsword", "sword"],
        description: "A plain, well-balanced blade.", bulk: Bulk(1), tags: ["blade"]),
    "iron_key": (name: "an iron key", bulk: Negligible, tags: ["key"]),
    "bread": (name: "a loaf of bread", tags: ["food"],
        decay: Perishable(hours: 6, message: "The bread has gone mouldy and crumbles away.")),
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `name`, `description` | required, empty | What players see |
| `keywords` | words of the name | What players can call it |
| `bulk` | `Light` | How much space and burden it takes; see [Bulk](#bulk) |
| `tags` | none | Kinds, such as `key`, `scroll`, `potion`; specialised containers accept them |
| `wear` | none | The slot it's worn in: `Head`, `Neck`, `Body`, `Back`, `Belt`, `Feet` |
| `container` | none | It holds things; see [Containers](#containers) |
| `fixed` | `false` | Can't be picked up, like a chest bolted to the floor |

## Weapons

A template can be a weapon:

```ron
"short_sword": (name: "a short sword", bulk: Bulk(1), tags: ["blade"],
    weapon: (skill: "swordplay:basics", damage: (2, 5))),
"knife": (name: "a knife", tags: ["blade"],
    weapon: (skill: "knives:basics", damage: (1, 4))),   // what a blow looks like is its skill's text
```

Held in hand, it's what its holder fights with (see [Combat](architecture.md#combat)): its damage in anyone's hands, and better aim and more damage for those trained in its skill. `examine` shows a weapon's damage and skill.

## Armour

A template can be armour: worn pieces take damage off every hit and cost some dodging, and a shield held in hand blocks. Armour stops [physical damage](effects.md#damage-types) only, unless its `against` lists more types. `examine` shows what a piece does. See [Armour](trees.md#armour) for how it fights.

```ron
"mail_shirt": (name: "a mail shirt", bulk: Bulk(3), wear: Body, armour: (reduction: 3, penalty: 2)),
"round_shield": (name: "a round shield", bulk: Bulk(2), armour: (penalty: 1, block: 4)),
"salamander_cloak": (name: "a salamander cloak", bulk: Bulk(1), wear: Back,
    armour: (reduction: 2, against: ["physical", "fire"])),
```

## Quality and rarity

*Built.* Two item stats, set differently:

- **Rarity belongs to the template** (`rarity: Rare`), on the same scale as skills: common, uncommon, rare, epic, legendary. It colours the item's name in lists (green uncommon, blue rare, magenta epic, gold legendary), `look` says so ("It's rare."), and it multiplies the item's value: ×1, ×1.5, ×3, ×6, ×12. Today the Rat King's tooth is rare, and the silver locket, moonpetal and glowcap uncommon.
- **Quality belongs to each item:** crude, poor, standard, fine or masterwork, saved with it (`items.quality`). It shows in the name ("a fine knife"; standard has no word) and can be called by (`look fine`), and `look` says it. Things that grow (food, drink and herbs) are withered or perfect instead of crude or masterwork.

| Quality | Value | Weapon damage, armour's reduction and block |
| --- | --- | --- |
| Crude (withered) | ×0.5 | −2 |
| Poor | ×0.75 | −1 |
| Standard | ×1 | 0 |
| Fine | ×1.5 | +1 |
| Masterwork (perfect) | ×3 | +2 |

Damage never drops below 0 to 1, armour never below nothing, and armour with no reduction or block gains none.

- **Where quality comes from:** only items with a value have one. Loot rolls it with the killer's Luck, mobs' gear and spawned items with a plain roll (mostly standard: 10% crude, 20% poor, 50% standard, 15% fine, 5% masterwork). Gathering sets it by how surely the search won: a near thing finds a poor one, a decisive win a perfect one. What shops sell is standard, though they resell whatever quality they bought. Crafting will set it by skill.
- **Worth:** an item's `value` is for a common, standard one; rarity and quality multiply it, and shops and Appraisal use the result.
- **Testing:** `create <item> [crude|poor|fine|masterwork]`, by template id or the words it answers to (`create short sword fine`), this zone first, then every zone.

## Bulk

Bulk works as in Pathfinder 2e. It measures size and awkwardness together, not weight alone.

| `bulk` | Counts as | Examples |
| --- | --- | --- |
| `Negligible` | Nothing | A key, a ring, a coin |
| `Light` | A tenth of 1 Bulk | A torch, a dagger, a loaf of bread |
| `Bulk(n)` | n Bulk | A longsword is 1; armour 1 to 4 |

## Where items are

| Place | Space | Counts toward encumbrance |
| --- | --- | --- |
| In a room | | |
| **Held**, in your two hands | 2 items | Yes |
| **Worn** in a slot | One per slot, except the belt, which takes up to 3 attachments | Yes, but takes no inventory space |
| **Inside a container** | The container's capacity | Yes, wherever the container is |

Without containers, a character can carry two things, one in each hand. Ordinary clothes with pockets, a belt pouch, a keyring and eventually a backpack are what let them carry more.

### Automatic placement

When you pick something up without saying where it goes, it goes to the first of these with room:

1. **A worn container that accepts the item's tags:** keys onto the keyring, scrolls into the scroll tube, potions into the potion pouch.
2. **A general worn container,** smallest first, so a light item goes into a belt pouch before the backpack.
3. **A free hand,** as a last resort; hands are for what you're using.

Otherwise: "You have nowhere to put the iron key." The reply always says where an item went ("You put an iron key on your keyring."). `hold` and `put` choose the place yourself.

### Encumbrance

Everything held, worn and inside carried containers counts toward your total Bulk.

- **Encumbered** past 5 + Strength / 2 Bulk: walking costs twice the terrain's stamina, and even paved ground costs ½ (see [Terrain](world.md#terrain)).
- **Can't carry more** past 10 + Strength / 2 Bulk: you can't pick up, take or be given anything that would push you past it.

At Strength 1 that's 5 and 10. Every worn and stored item counts in full; there's no backpack discount.

### Commands

| Command | Does |
| --- | --- |
| `get <item>`, `get all`, `get all.<item>` | Pick up from the room, placed automatically |
| `get <item> <container>` | Take out of a container, here or carried |
| `put <item> <container>` | Put into a container, here or carried |
| `drop <item>` | Put down in the room, from anywhere you carry it |
| `give <item> <player>` | Hand over; placed automatically on their side |
| `hold <item>` | Move into your hands |
| `sheath <weapon>` (`sheathe`) | Put a weapon in a worn sheath or loop that takes it; `sheath` alone puts away what you hold. `put sword sheath` works too |
| `draw <weapon>` | Take a weapon from its sheath or loop into your hand |
| `wear <item>`, `remove <item>` | Wear in its slot, or take off into your hands |
| `inventory` (`i`) | Hands, what you wear, each container's contents and fill, and your Bulk against your limits |
| `look <item>`, `examine <item>` | Describe it; examining a container lists its contents |

## What survives a restart

**An item becomes "real" the moment a player touches it.**

- **Untouched spawns are temporary.** Items a reset created that no player has handled live only in memory. A restart clears them, and resets repopulate.
- **Touching makes an item real.** Getting, dropping, putting, giving or wearing an item gives it a row in an `items` table: its template, where it is, when it was dropped, and when it decays. From then on every move updates that row, and the item survives restarts wherever it ends up. Looking at or examining an item doesn't count.
- **Startup restores real items** to their saved locations, removing any whose decay time has passed.
- **Resets only manage their own spawns.** Once a spawned item is picked up, it is real and no longer the reset's; the next reset spawns a fresh one.
- **A mob's things aren't saved** (2026-10-28, [#55](https://github.com/Foxworks-Studios/riftward/issues/55)): a mob takes, carries, wears and gives items as a player does, and what goes to a mob (handed to it, picked up, put in its bag) is no longer real: its row, and those of anything inside it, are deleted, and it resets with the mob and falls in its corpse, as the gear it spawns with does. Handed back to a player, it's real again. A unique item given to a mob is forgotten too, so its spawn may make another.
- **Conditions put on one item** (`Imbue`) are saved with it in `item_conditions`, and come back with it.
- **Mobs stay temporary** and reset with their zone.

## Consumables

A template can be eaten or drunk (`by`): it's used up, and its `effects` happen to whoever ate or drank it, in the same vocabulary as spells and skills ([effects.md](effects.md)): `Heal` for health at once, `Apply` for a condition (on the eater, whichever `on` it names), `Remove` and `Cure` to take one off. Nothing about food or drink is in code: bread is just an item whose effect is the `nourished` condition, which speeds recovery while it lasts. A meal's wording is the condition's own. See [Healing](trees.md#healing).

```ron
"bread": (name: "a loaf of bread", tags: ["food"], consumable: (by: Eat, effects: [Apply(condition: "nourished", strength: "2", lasts: Minutes(80))])),
"healing_draught": (name: "a healing draught", tags: ["potion"], consumable: (by: Drink, effects: [Heal(amount: "roll(4, 8)")])),
```

A poisonous mushroom would `Apply` `poisoned`; an antidote would `Cure` it. The zone test checks each consumable's effects are ones that can happen to the eater and that its conditions exist.

You can only eat or drink what you carry. Bandages are items tagged `bandage`, used up by First Aid.

## Decay

Status: built for corpses only. A mob's corpse rots in 5 minutes; a player's, holding everything they had, in 30, timed from when they fell so it survives restarts. A rotting container spills its contents where it lay. Litter, perishables and torches wait.

Items rot away over time, which keeps rooms from filling with clutter and makes food, corpses and torches feel physical. It's also what keeps saved items from filling the world and the database. Each template picks one kind of decay:

| Decay | Clock runs | Examples |
| --- | --- | --- |
| Litter (the default) | Only while the item lies in the world; picking it up resets it | A dropped sword eventually crumbles |
| Perishable | Always, even carried, by real time (including while offline) | Food, herbs, corpses, torches |
| None | Never | Unique items |

- **Timestamp-based.** Each item stores when it decays; a light check every few seconds removes expired items and tells whoever can see them. Real items keep their decay time in the database, so decay continues across restarts.
- **Containers spill when they rot.** A rotting corpse drops its contents, which start their own litter timers, giving players a window to loot.

## Unique items

Some items exist only once and never respawn: once *Angorim* has been found, no reset creates another.

- **Templates are marked unique** (`unique: true`) and have no decay: a unique item stays wherever it is dropped, forever, until someone picks it up.
- **Before it's found,** a unique item is an ordinary temporary spawn; a restart before anyone touches it just spawns it again.
- **Once found,** it is a real item like any other, saved wherever it goes. Its spawn checks the `items` table for a real copy before spawning one, so a second copy never appears. *Built: `unique: true` on a template, or `item unique <id> yes`. Never decaying waits on decay.*

## Containers

A container is an item with a `container` section. Worn containers carry your things; fixed ones, like chests, sit in rooms.

```ron
"backpack": (name: "a leather backpack", bulk: Bulk(1), wear: Back,
    container: (capacity: Bulk(4))),
"belt_pouch": (name: "a belt pouch", bulk: Negligible, wear: Belt,
    container: (capacity: Light(4))),                          // four light items
"keyring": (name: "a brass keyring", bulk: Negligible, wear: Belt,
    container: (capacity: Count(10), accepts: ["key"])),
"sword_sheath": (name: "a leather sheath", wear: Belt,
    container: (capacity: Count(1), accepts: ["blade"])),
"weapon_loop": (name: "an iron weapon loop", bulk: Negligible, wear: Belt,
    container: (capacity: Count(1), accepts: ["axe", "hammer", "mace"])),
"travelers_coat": (name: "a traveller's coat", bulk: Light, wear: Body,
    container: (capacity: Count(2), max_item: Light)),         // pockets
"oak_chest": (name: "an old oak chest", fixed: true,
    container: (capacity: Bulk(10), state: Locked,
        lock: Some((key: Some("chest_key"), pick: Some(4), bash: Some(5))))),
```

| Field | Default | Meaning |
| --- | --- | --- |
| `capacity` | required | `Bulk(n)`, `Light(n)` (n light items' worth), or `Count(n)` items |
| `accepts` | anything | Tags it takes; a keyring takes only `key`. Specialised containers are filled first |
| `max_item` | no limit | The largest single item it takes, so a sword can't go in a pocket |
| `state`, `lock` | always open | Like a [door](#locks-and-keys): containers can be closed, locked, picked and bashed |

- **Any worn item can be a container,** which is how clothes get pockets.
- **Fixed items are for anything that shouldn't be carried off:** chests, signposts, furniture, a well. They can't be picked up, held or worn, and rooms list them as part of the room ("An old oak chest is here.") rather than as things lying around. Give them a large `bulk` too, for when moving furniture becomes possible.
- **A container's open, closed or locked state isn't saved,** like a door's: it returns to its template's state when the server restarts, and will reset with its zone.
- **Containers nest.** A pouch can go in a backpack, and its contents count toward the backpack's fill and your Bulk. `inventory` and `examine` show what you carry as a tree.
- **Only what's at hand can be moved:** what you hold or wear, what lies in the room, and what's directly inside an open container among those. A loaf in a pouch packed in a backpack can't be taken until the pouch comes out ("A belt pouch is packed inside your leather backpack; take it out first."). This applies to `get`, `put`, `drop`, `give`, `hold`, `wear`, `sheath`, `draw` and opening containers; looking is never restricted.
- **Automatic placement only uses containers you hold or wear directly,** and never packs one container inside another: a pouch taken out of a pack goes to your hand, ready to use.
- **Containers can't go inside themselves,** directly or through another container.
- **Fixed containers belong to their spawn and are never real.** A chest's name is stable (its spawn's id), and each reset refills its contents and resets its lock. Until spawns exist, builders `create` them, which places a fixed item in the room.
- **Contents follow the usual rule:** temporary until a player takes them, then real. Real items saved inside a fixed chest survive restarts, which is the basis for player storage later.
- **A mob's gear is its first drop.** Mobs hold and wear items from their template's `equipment` (see [Mobs](world.md)); when one dies, its gear falls into its corpse, temporary like any untouched item until a player takes it. A guard killed in the market leaves its short sword behind. A mob removed any other way (purged, its template deleted) takes its gear with it.
- **Loot tables** on mob templates roll through [`Dice.pick`](attributes.md#the-dice-service) when the mob dies, with the killer's Luck (for a group, later, the group's average), and add to what its gear already drops. See [Mobs](world.md). *Built.*
- **Corpses** will be portable containers with perishable decay.

## Locks and keys

Anything that opens and closes, a [door](world.md#doors) or a container, has a `state` it starts in (`Open`, `Closed` or `Locked`) and optionally the same lock (built for doors):

```ron
state: Locked,                        // where it starts, and returns to on a reset
lock: Some((
    key: Some("town:cellar_key"),     // an item template; any copy of it opens the lock
    pick: Some(4),                    // lockpicking difficulty; None = can't be picked
    bash: Some(6),                    // sturdiness against forcing; None = can't be forced
)),
```

A lock with no `key` has no keyhole: it can only be picked or bashed. Traps will be another field on the lock (see [Traps](#traps)).

| Command | Needs |
| --- | --- |
| `open`, `close` | It isn't locked |
| `lock`, `unlock` | Its key in your inventory. Until items exist, players are told "You don't have the key", and builders can lock and unlock anything for testing |
| `pick` | Lockpicking (level plus Agility) against `pick`, by [ratio contest](attributes.md#the-ratio-rule), with Luck. A shortcut to `use lockpicking`: Lockpicking is [data](effects.md#every-skill-as-data), taking the `locked` condition off. A lock with no `pick` can't be picked |
| `bash` | Strength against `bash`, as a [ratio contest](attributes.md#the-ratio-rule), with Luck. Only forces it open; the lock still works afterwards. Others on both sides hear it |

The target can be a **direction** for a door (`open north`), or a name (`open door`, `unlock 2.chest`); directions are checked first. Replies are short and specific:

```
> open north
It's locked.
> unlock north
You don't have the key.
> pick north
You pick the lock on the heavy oak door.
> open door
You open the heavy oak door.
```

- **Others see it happen:** "Frodo opens the heavy oak door." For a door, both rooms are told.
- **Every action fires a [game event](architecture.md#game-events)** (`Opened`, `Closed`, `Unlocked`, `LockPicked`, `Bashed`), so traps, alarms, passive skills and [world events](world.md#world-events) respond to locks like anything else.
- **Locks reset with their zone,** back to their default state: cellars relock and chests close.

## Traps

A trap sits on a lock and fires on an event, usually `Opened`:

```ron
trap: Some((on: Opened, find: 5, disarm: 7, effect: Damage(6))),
```

- **Finding** uses the same `search` as [hidden doors](world.md#doors); **disarming** uses a skill, rolled against `disarm`.
- **The effect** needs the effect system, so traps arrive with combat and skills.

## Open questions

- [ ] **Unique items and deleted characters:** if a character carrying a unique item is deleted, where does it go? For example, dropped where the character last stood.
- [ ] **Default litter decay time:** about 30 minutes on the ground? Should items spawned by a reset decay at all, since the next reset replaces them?
- [ ] **Preserving perishables:** salting, a preserving spell, or cold storage that slows or pauses decay. A good hook for skills; templates only need to allow it.
- [ ] **Does a mob picking up an item make it real?** Proposed: no, only players.
- [ ] **Corpses:** are player corpses real (they survive a restart) while mob corpses stay temporary until looted?
- [ ] **Starting equipment:** which clothes, pouch and keyring new characters start with, likely set by background.
