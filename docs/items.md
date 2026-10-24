# Items, containers and locks

[Design summary](README.md) · Status: designed, not built; next in the [build order](architecture.md#proposed-build-order)

Items are entities defined by templates in zone files. They can be carried, worn, dropped, or put inside containers. An item becomes **real**, saved to the database, the moment a player touches it, and most items slowly **decay** when left lying around. Doors and containers share one **lock** model: open, closed or locked, with keys, lockpicking, bashing and traps.

## Templates and location

Zone files define item templates; [spawns](world.md#spawns-and-resets) place them in the world.

```ron
items: {
    "longsword": (name: "a longsword", keywords: ["longsword", "sword"],
        description: "A plain, well-balanced blade.", weight: 3),
    "bread": (name: "a loaf of bread", keywords: ["bread", "loaf"], weight: 1,
        decay: Perishable(hours: 6, message: "The bread has gone mouldy and crumbles away.")),
    "cellar_key": (name: "a small iron key", keywords: ["key", "iron"], weight: 0),
}
```

One **location** component, replacing `InRoom`, says where anything is: in a room, carried by someone, worn, or inside another item. Items are found with the usual [targeting](architecture.md#command-system-and-targeting-built) (`sword`, `2.key`, `all.coin`).

| Command | Does |
| --- | --- |
| `get <item>`, `get <item> <container>` | Pick up from the room, or take out of a container |
| `drop <item>` | Put down in the room |
| `put <item> <container>` | Put into a container |
| `inventory` (`i`) | List what you carry |
| `look <item>`, `examine <item>` | Describe it; examining a container lists its contents |

`all` forms work where they make sense: `get all`, `get all.coin chest`, `drop all.bread`.

## What survives a restart

**An item becomes "real" the moment a player touches it.**

- **Untouched spawns are temporary.** Items a reset created that no player has handled live only in memory. A restart clears them, and resets repopulate.
- **Touching makes an item real.** Getting, dropping, putting, giving or wearing an item gives it a row in an `items` table: its template, where it is, when it was dropped, and when it decays. From then on every move updates that row, and the item survives restarts wherever it ends up. Looking at or examining an item doesn't count.
- **Startup restores real items** to their saved locations, removing any whose decay time has passed.
- **Resets only manage their own spawns.** Once a spawned item is picked up, it is real and no longer the reset's; the next reset spawns a fresh one.
- **NPCs stay temporary** and reset with their zone.

## Decay

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
- **Once found,** it is a real item like any other, saved wherever it goes. Its spawn checks the `items` table for a real copy before spawning one, so a second copy never appears.

## Containers

A container is an item with a `container` section, optionally with a [lock](#locks-and-keys):

```ron
"oak_chest": (name: "an old oak chest", keywords: ["chest", "oak"],
    fixed: true,                                          // can't be picked up
    container: (capacity: 20,
        state: Locked, lock: Some((key: None, pick: Some(6), bash: Some(15)))),
    contents: Table([("gold_coins", 60), ("healing_draught", 30), ("angorim", 1)]),
),
"satchel": (name: "a leather satchel", keywords: ["satchel", "bag"],
    container: (capacity: 10)),
```

- **Fixed containers belong to their spawn and are never real.** A chest's name is stable (its spawn's id), and each reset refills its contents and resets its lock.
- **Contents follow the usual rule:** temporary until a player takes them, then real.
- **Players can store things in fixed containers.** Real items saved as inside `tavern:oak_chest` survive restarts, because the chest's name is stable. That's the basis for player storage later.
- **Loot tables** roll through [`Dice.pick`](attributes.md#the-dice-service), so Luck applies; for a group, the group's average Luck.
- **Portable containers** like bags, and corpses, are ordinary items with a `container` section.

## Locks and keys

Anything that opens and closes, a [door](world.md#doors) or a container, has a `state` it starts in (`Open`, `Closed` or `Locked`) and optionally the same lock (built for doors):

```ron
state: Locked,                        // where it starts, and returns to on a reset
lock: Some((
    key: Some("town:cellar_key"),     // an item template; any copy of it opens the lock
    pick: Some(4),                    // lockpicking difficulty; None = can't be picked
    bash: Some(12),                   // sturdiness against forcing; None = can't be forced
)),
```

A lock with no `key` has no keyhole: it can only be picked or bashed. Traps will be another field on the lock (see [Traps](#traps)).

| Command | Needs |
| --- | --- |
| `open`, `close` | It isn't locked |
| `lock`, `unlock` | Its key in your inventory. Until items exist, players are told "You don't have the key", and builders can lock and unlock anything for testing |
| `pick` | A lockpicking skill, rolled through `Dice` against `pick` (not built yet) |
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
- [ ] **Does an NPC picking up an item make it real?** Proposed: no, only players.
- [ ] **Corpses:** are player corpses real (they survive a restart) while NPC corpses stay temporary until looted?
- [ ] **Carrying limits:** by weight (Strength and Body), by count, or both?
