# Contributing to Riftward

Thanks for helping build Riftward, a classless telnet MUD on a headless [Bevy](https://bevyengine.org) server: characters are defined by the skills they learn and use, and the world is built mostly from inside the game.

There are three main ways to contribute, and you don't need to write Rust for the first two:

- **Building the world:** zones, rooms and descriptions, in game or in zone files.
- **Game design:** answering open questions, tuning numbers, playtesting.
- **Engineering:** the server itself.

Start with the [design summary](docs/README.md). It links to every design document, says what exists today, and lists the open questions.

## Running the server

You need a recent stable Rust toolchain (the project uses the 2024 edition).

```sh
cargo run                                  # listens on 0.0.0.0:4000, database ./mud.db
MUD_ADDR=127.0.0.1:4001 cargo run          # another address or port
MUD_DB=/tmp/scratch.db cargo run           # a throwaway database
```

Run it with `cargo run` from the project root: that's how Bevy finds the `assets/` folder. The first build takes a while because dependencies are compiled with optimizations, even in debug builds.

Connect with any telnet or MUD client:

```sh
nc localhost 4000          # always available on macOS and Linux
telnet localhost 4000
```

[TinTin++](https://tintin.mudhalla.net) and [Mudlet](https://www.mudlet.org) give you command history and hide your password as you type it.

### Accounts and roles

- **The first account created on a new database becomes an admin.** On a fresh `mud.db`, create your own account first.
- Admins grant roles in game with `role <account> <player|builder|admin>`. Builders can build; admins can also manage roles and use testing commands like `setattr`.
- `mud.db` is ignored by git. Everyone has their own local database.

## Building the world

Builders build from inside the game. Type `help` to see the commands your role allows; the main ones:

| Command | Does |
| --- | --- |
| `where`, `rooms`, `zones` | Show where you are, the rooms in this zone, and all zones |
| `goto <room>` | Teleport, by `room` or `zone:room` |
| `dig <dir> <name>` | Create a room through a new exit, with a way back |
| `desc <text>`, `rename <name>` | Edit the room you're standing in |
| `link <dir> <room>`, `unlink <dir>` | Add or remove a one-way exit |
| `item`, `item new <id> <name>`, `item <field> <id> <value>` | List, show, create and edit this zone's item templates (fields: name, desc, keywords, bulk, tags, wear, fixed, container) |
| `create <item>`, `destroy <item>` | Make an item from a template, or destroy one for good |
| `door <dir> <name>`, `door <dir> lock`, `door <dir> remove` | Add a door to an exit (shared with the way back), give it a lock, or remove it; `door <dir>` shows it |
| `zone create <id> <name>`, `zone rename <name>` | Create or rename a zone |

Every change is saved straight to the zone's file in `assets/zones/`, and those files are committed to git like code. You can also edit them by hand while the server runs: it reloads them automatically, and players standing in a changed room are told their surroundings shifted.

Two things to know about zone files:

- **Room ids are local to their zone.** Link to another zone's room with `zone:room`.
- **Comments in a zone file are lost** the next time a builder command saves that zone; only the standard header is kept.

## Working on the code

The [architecture document](docs/architecture.md) describes how the server fits together: the modules, what happens each tick, and where data lives. The short version:

| Where | What |
| --- | --- |
| `src/command.rs` | The command system: registry, dispatcher, generated `help` |
| `src/commands/` | Player commands, grouped by topic |
| `src/build.rs` | Builder and admin commands |
| `src/world.rs` | Zones and rooms, and applying zone file changes live |
| `src/net.rs` | Connections, input and output, the prompt |
| `src/db.rs` | SQLite: accounts, characters, aliases, attributes |
| `src/dice.rs` | The only source of randomness; every roll goes through it |

### Adding a command

Each command is a Bevy system registered with a `CommandSpec`. The system receives an `Invocation` (who ran it, and the arguments) and asks for exactly the resources it needs:

```rust
// In a plugin's build():
app.add_command(
    CommandSpec::new("wave", Category::Communication)
        .usage("wave [<target>]")
        .summary("wave at the room, or at someone"),
    wave,
);

fn wave(In(inv): In<Invocation>, mut players: PlayerQuery) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    // ... build a Vec<Outgoing>, then:
    deliver(out, &mut players);
}
```

Registration takes care of the rest: `help` lists it, aliases can't shadow it, and `.permission(Permission::Build)` hides it from anyone without that permission. Don't write prompts yourself; the connection adds one after every batch of output. Resolve target words with `Target::parse` and `find_one` (see `src/targeting.rs`), so `2.goblin` and `me` work everywhere.

### House rules

- **All randomness goes through `Dice`,** never a random number generator of your own, so Luck applies to every roll.
- **Contests compare ratios, never differences,** because attributes are uncapped (see [the ratio rule](docs/attributes.md#the-ratio-rule)).
- **Designed content goes in data files** (zones now; skill trees, items and NPCs later). Things that change during play go in the database.
- **Database schema:** tables are currently created with `CREATE TABLE IF NOT EXISTS`. Changing an existing table needs numbered migrations, which don't exist yet; raise it before you need one.

### Before opening a pull request

```sh
cargo fmt
cargo clippy --all-targets      # should report no warnings
cargo test
```

Then try your change in a real client against a throwaway database (`MUD_DB=/tmp/scratch.db`). For anything involving more than one player, connect several clients at once; scripted `nc` sessions work well:

```sh
( printf 'myaccount\r\nmypassword\r\n'; sleep 1; printf '1\r\nlook\r\nquit\r\n'; sleep 1 ) | nc localhost 4000
```

## Design documents

The design lives in [`docs/`](docs/README.md), one document per area, each with its own open questions.

- **Decisions are recorded in the docs,** not just in discussion. A change that alters a design should update its document in the same pull request.
- **Open questions are checklists** at the end of each document. Answering one is a contribution in itself: propose an answer in an issue or pull request.
- **Keep the summary current.** `docs/README.md` lists what exists, what's decided and what's planned.

## Git

- Work on a branch and open a pull request; don't commit straight to the main branch.
- Write commit messages as a short imperative summary line ("Add item decay"), then a body explaining what changed and why.
- Commit zone files and docs; never commit `mud.db` or anything from `target/`.
