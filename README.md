# Riftward

A classless telnet MUD, written in Rust on a headless [Bevy](https://bevyengine.org) server.

There are no classes in Riftward. A character is what they've learned: sword and fire, stealth and herbalism, whatever they practise. Skills grow by being used, the rarest are taught by players who have mastered them, and some are found only by doing something nobody has written down. The world is built mostly from inside the game.

> **Early days.** Riftward is playable but young: one town, the sewers beneath it, the edge of the Darkwood beyond its North Gate, and the systems to grow from there. Numbers are first guesses and things change often.

## What's in it

- **Skills, not classes:** skill trees as data, practice-based progression, techniques for every weapon, and higher skills discovered through use or unlocked with Karma.
- **Fighting:** 3-second rounds in initiative order, dodging and shields, armour that stops only the damage it's made for, conditions from staggered to warded, and mobs that use techniques and spells like players do.
- **Magic:** three casting traditions (divine, hermetic and shamanic) with their first spells, cast through a school and paid for in Drain.
- **One effect vocabulary:** blows, techniques, spells, active skills and consumables all say what they do in the same terms (damage, heal, apply a condition, imbue a weapon), with amounts as formulas in data, and conditions and passive skills that react to hits, dodges and kills.
- **Playing together:** following and groups, with experience, coins and loot shared, and the Adventurers' Guild, with ranks and merit.
- **Quests:** a contract board in the Contract Hall, quests in stages with choices, escorts, items to find and hand over, deadlines and failures, kept in a journal; game masters can run them by hand and write new ones in the game.
- **A living world:** mobs that wander, breed, flee, help their allies and fight each other; creatures and races with traits; resets; day and night; world events (let the sewer rats break out and the Rat King rises).
- **Items:** weapons, armour, containers that pack themselves, quality and rarity, keys and locks, money, shops and haggling, and food and potions that are just items with effects.
- **Stealth, tracking, gathering and healing:** hide and backstab, read who walked through, forage and gather herbs, bandage wounds and drink draughts.
- **Building in game:** builders dig rooms, write descriptions and make items and doors, all saved straight to zone files; game masters can puppet mobs and run live events.
- **Designer-friendly data:** zones, skills, spells, conditions, traits, quests, damage types, races, backgrounds and the guild are RON files; most reload while the server runs.

The full list is in [What exists today](docs/README.md#what-exists-today).

## Quick start

You need a recent stable Rust toolchain (the 2024 edition).

```sh
git clone https://github.com/Foxworks-Studios/riftward.git
cd riftward
cargo run            # listens on 0.0.0.0:4000; the database is ./mud.db
```

Run it from the project root, so the server finds `assets/`. The first build takes a while.

Then connect with any telnet or MUD client:

```sh
telnet localhost 4000
```

[Mudlet](https://www.mudlet.org) and [TinTin++](https://tintin.mudhalla.net) give you command history and hide your password as you type.

**The first account created on a new database becomes an admin.** Make yours first. Then try `help`, `look`, `score`, `skills` and `time`, find the drill sergeant to `learn` a weapon, and read the contract board in the Contract Hall.

To run a throwaway copy beside a real one:

```sh
MUD_DB=/tmp/scratch.db MUD_ADDR=127.0.0.1:4001 cargo run
```

## Where things are

| Path | What's there |
| --- | --- |
| `src/` | The server: one Bevy plugin per module ([architecture](docs/architecture.md)) |
| `assets/zones/` | The world: rooms, doors, item and mob templates, spawns, world events |
| `assets/skills/`, `assets/spells/` | Skill trees and spell books |
| `assets/conditions/`, `assets/traits/` | Conditions (staggered, poisoned, nourished) and traits (what a creature or race is) |
| `assets/quests/` | Quests, one file each, by zone |
| `assets/*.ron` | Races, backgrounds, damage types, the guild, progression and rules numbers, and the world's givens (`world.ron`: start room, starting kit, death) |
| `docs/` | The design, one document per area, and what's built |

## Learn more

- [Design summary](docs/README.md): the goals, every design document, and what exists today.
- [Where we are](docs/STATUS.md): what's built, what's next, and what's waiting on a decision.
- [Contributing](CONTRIBUTING.md): building the world, game design, and engineering, with a worked example of adding a command.

## Development

```sh
cargo fmt
cargo clippy --all-targets   # no warnings
cargo test
```

## License

Riftward is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
