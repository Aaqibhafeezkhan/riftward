# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Riftward is a classless telnet MUD on a headless Bevy 0.20 server (Rust 2024 edition). Characters are defined by skills they learn and use; the world is built mostly from inside the game.

**Picking the work back up?** Read `docs/STATUS.md` first (what's built, what's next, open decisions), then `docs/README.md` (design summary and index) and `docs/architecture.md` (modules, the tick, where data lives).

## Commands

```sh
cargo fmt
cargo clippy --all-targets            # must report no warnings
cargo test                            # all tests (unit tests live in #[cfg(test)] modules in src/)
cargo test combat::                   # tests in one module
cargo test <test_name>                # a single test by name substring

cargo run                             # 0.0.0.0:4000, database ./mud.db
MUD_DB=/tmp/scratch.db MUD_ADDR=127.0.0.1:4001 cargo run   # scratch server for live testing
```

- Always `cargo run` from the project root: that's how Bevy finds `assets/`.
- `fmt`, `clippy --all-targets` and `test` must all be clean before a change is done.
- `main.rs`'s `the_server_starts` test builds the whole app and sets up every system and command, so a query clash (Bevy's B0001) fails `cargo test` instead of crashing the live server.

### Testing live

- Never test against the real `mud.db`; use a scratch `MUD_DB`. The first account created on a fresh database becomes an admin.
- Script sessions with `nc` (end with `quit` twice, character then menu, or `nc` waits forever):
  `( printf 'acct\r\npass\r\n'; sleep 1; printf '1\r\nlook\r\nquit\r\nquit\r\n'; sleep 1 ) | nc 127.0.0.1 4001`
  Use a small Python socket client for things `nc` can't do (e.g. dropping the connection mid-fight).
- **Builder/admin commands rewrite zone files in `assets/zones/`.** Back the folder up before a test that uses them and restore it after.
- Stop a test server by the process on its port (`lsof -t -iTCP:4001 -sTCP:LISTEN`), never by name, so a real server on 4000 is left alone.
- Roles: player, builder, GM, admin; builders and GMs sit side by side, and each command names a `Permission` (`Role::can`, `docs/architecture.md#roles`). Admin testing commands: `grantskill`, `setattr`, `setvitals`, `condition` (`condition <who> <condition> [<strength>] [<minutes>|<turns>t]`, or `remove`), `worldflag`, `summon`, `purge`, `goto`, `create`. `create`/`summon` take a template id (`stray_dog`, or `zone:id` from anywhere) or the words it answers to (`summon dog`), this zone first, then every zone; several matches are listed by id.
- Prefer deterministic tests over chasing dice live: combat tests build a tiny world and run one `combat_round` (see tests in `src/combat/mod.rs`).

## Architecture

`src/main.rs` assembles a 30 Hz `MinimalPlugins` app with asset hot-reload, then one Bevy plugin per module (engine plugins, then game plugins). `docs/architecture.md` has the full module table.

**One tick:** `net.rs` accepts connections and queues each client's lines, emitting at most **one `PlayerInput` per player per tick** (keeps commands in typed order across systems) → `command.rs` dispatches (alias expansion, permission hiding, unknown → "Huh?") and runs that command's system → output is queued on each `Connection` and flushed once in `PostUpdate`.

**Commands** are Bevy systems registered with `app.add_command(CommandSpec::new(..).usage(..).summary(..), system)`. The system takes `In<Invocation>` plus whatever queries/resources it needs; `help` is generated from the registry, and `.permission(Permission::Build)` hides it from lower roles. Player commands live in `src/commands/` and topic modules (`items.rs`, `combat/`, `skills.rs`…); builder/admin commands in `build.rs`. See `CONTRIBUTING.md` for a worked example.

- **Never write prompts from a command.** The connection appends exactly one prompt after each batch of output; `vitals.rs` computes the prompt text just before flush.
- **Players and mobs are both characters.** Commands read them through `Characters` and get their actor with `Actor::get`, which works for either: a mob runs commands when it wanders (`go`) or a builder `puppet`s it. `actor.character_id` is `None` for a mob, and mobs aren't saved, so skip saving for them. Build output as `Vec<Outgoing>` and queue it with `deliver(out, &mut outbox)` into the `Outbox` (`output.rs`); never write to a `Connection` from a command.
- Resolve target words with `Target::parse` + `find_one`/`find_all` (`targeting.rs`) so `2.goblin`, `all.coin` and `me` work everywhere.
- To act on players only, check for `Player` (`Characters` gives `Option<&Player>`); text sent to a mob nobody is puppeting is simply dropped.
- **Action commands** (`kill`, `flee`, `use`, `pick`, `bash`, techniques) go through the per-player `ActionQueue` and take a 3-second combat round; everything else is instant.
- **Game events:** systems that make something happen write a `GameEvent` (`events.rs`); announcements, passive-skill triggers and world events listen to it. Add events rather than special cases.
- **Time:** a one-second heartbeat drives spawns, wandering, recovery, the clock. Heartbeat systems must work from elapsed time and timestamps, never by counting beats.

**Effects:** spells and techniques list what they do in one vocabulary (`effects.rs`: `Damage`, `Apply`, `Imbue`…), and `combat/resolve.rs` resolves every blow, technique and spell the same way: the contest (what the target `resist`s with, plus skills that resist its tags), then damage through armour and resistances, then conditions, then the bearers' `reactions` (conditions and passive skills reacting to hits, dodges, kills; a reaction never sets off another). Reactions outside a blow (entering a room, resting, a condition taking hold or running out, every so often) come from `GameEvent`s, through `combat/triggers.rs`. Amounts in effects are formulas (`formula.rs`), parsed at load: put new numbers and scaling in data, not code. Active skills that do something to someone are effects in their `.skill.ron` too (`combat/abilities.rs`; First Aid); only those like nothing else (`pick`, `track`) have their own command. Add an effect or modifier there rather than special-casing one spell or skill (`docs/effects.md`).

**Data split:** designed content is RON in `assets/` (zones with rooms/doors/item & mob templates/spawns/world events in `assets/zones/*.zone.ron`, skill trees in `assets/skills/*.skill.ron`, spell books in `assets/spells/*.spells.ron`, quests in `assets/quests/<zone>/*.quest.ron` (one each), conditions in `assets/conditions/*.conditions.ron`, traits (what a creature or race *is*, the same shape as conditions but permanent) in `assets/traits/*.traits.ron`, `races.ron`, `damage_types.ron`, `progression.ron` (Karma and experience numbers, read at startup), `backgrounds.ron`), hot-reloaded; `world.rs::sync_world` applies zone changes live. Anything that changes during play is in SQLite (`db.rs`). Items become "real" (persisted) the moment a player touches them; untouched spawns and mobs reset.

## House rules

- **All randomness goes through `Dice`** (`dice.rs`), never your own RNG, so Luck applies to every roll.
- **Contests are opposed rolls by the ratio rule**: `Dice::opposed` (or `Practice::opposed`), never a hand-made chance or a difference, since ratings are uncapped (`docs/attributes.md#the-ratio-rule`). Fixed difficulties roll too, with no Luck.
- **Designed content goes in data files**, not code.
- **A skill's wording lives in its `.skill.ron`, never in code.** Every skill needs a `description`; attacks, techniques, counters and active skills need `text` templates (`{actor}`, `{target}`, and `{harm}` in every damaging hit) for each viewpoint, on hit and miss. `SkillDef::check` enforces this at load and the tree-file test runs it (`docs/skills.md#skill-text`). Code that shows a skill being used fills in its templates with `skills::fill`.
- **A spell's wording and numbers live in its `.spells.ron`** (`assets/spells/`), checked at load by `SpellDef::check` like skills. Spells resolve inside the combat round: `combat/casting.rs` pays for them and `combat/resolve.rs` resolves them like any blow or technique, so they share blows' damage, deaths and experience; out of a fight, `cast` builds a `Round` to cast at once.
- **Commands reveal hidden players unless quiet.** Interaction and communication commands show a hidden player when used; mark one `.quiet()` on its `CommandSpec` if doing it shouldn't (whisper, search). Anything choosing a target should filter with `stealth::visible` / `sees` (`src/stealth.rs`).
- **Schema:** tables use `CREATE TABLE IF NOT EXISTS`; there are no migrations yet. Changing an existing table needs numbered migrations (`PRAGMA user_version`); raise it before doing so.
- **Zone files:** room ids are local to their zone (cross-zone links use `zone:room`), and comments in a zone file are lost the next time a builder command saves it.
- **Design docs are the record of decisions.** A change that alters a design updates its doc in `docs/` in the same change; keep `docs/README.md` ("What exists today") and `docs/STATUS.md` current.

## Git

- **Everything reaches `master` through a pull request**, ours as well as contributors' (since 2026-10-27); never push to `master` directly. Work on a feature branch off `master`. Commit only when asked; "commit, merge and push" means: commit on the branch, push it, open a PR (`gh pr create`), wait for CI (`gh pr checks <n> --watch`), merge it by rebase (`gh pr merge <n> --rebase --delete-branch`), then `git checkout master && git pull --ff-only`. The remote is `origin`, github.com/Foxworks-Studios/riftward, public.
- CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, clippy with `-D warnings` and `cargo test` on every push and pull request. A first-time contributor's run from a fork waits for a maintainer to approve it ("Approve and run" on the PR, or `gh api -X POST repos/Foxworks-Studios/riftward/actions/runs/<id>/approve`). If our work leaves a contributor's PR conflicting, maintainers may merge `master` into their branch (they allow maintainer edits). Contributors' PRs are squash-merged (`gh pr merge <n> --squash`), since a branch with merge commits can't be rebased.
- `master` is protected: a PR and a passing CI check are required, history stays linear, and only rebase and squash merges are allowed; branches are deleted on merge.
- Commit as the repository's configured `user.email`; never another address.
- Commit messages: short imperative summary ("Add item decay"), then a body explaining what and why.
- Commit zone files and docs; never `mud.db` or `target/`.
