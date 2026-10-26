# Where we are

[Design summary](README.md) · Last updated 2026-10-25, after limiting trainers to entry skills, adding discovery through use and the tough-fight boost, and making Inner Discipline the Adept's school, then giving mobs equipment and skills their own text, then armour, then healing, then loot tables and gathering, then tracking, climbing and swimming, then reworking contests and progression, with blows and Diku damage words

A handoff note for picking the work back up: what's built, what's next, and what's waiting on a decision. The design docs hold the detail; this page points into them.

## What's built

Riftward is a classless telnet MUD on headless Bevy 0.20. Everything below is committed, tested live on a scratch server, and documented.

- **Engine:** telnet with proper negotiation (`src/telnet.rs`), prompts marked with EOR or GA for MUD clients, one command per tick, an action queue (combat actions take a 3-second round), game events, a game clock (a day every 3 real hours), a one-second heartbeat.
- **World:** zone files (RON, hot-reloaded) for Riftwatch (25 rooms), its sewers (22) and the Darkwood (6); doors and locks; terrain that costs stamina to cross; in-game building.
- **Items:** templates, Bulk and encumbrance, hands and wear slots, containers and automatic placement, weapons, corpses that rot, real items that survive restarts, item spawns, unique items.
- **Mobs:** templates and spawns (keeping a count, breeding, after-death and reseed timers, gated by world flags), wandering and crowding, pack limits, aggression, auras, the players' vitals, technique skills used by chance, and equipment they fight with and drop.
- **Characters:** creation (race with Earthdawn-style shifts, starting Karma, a background as a past life, a description), vitals that recover (faster resting, offline too, never in a fight), `score`, `describe`, aliases, a custom prompt.
- **Skills:** trees as data (`assets/skills/*.skill.ron`), use-based progression with practice and gain cooldowns, uncapped levels with a soft cap from character level, experience and character levels that earn Karma, opposed-roll contests, active and passive skills, attacks, techniques, auras, passive stat bonuses, counters (Riposte), trainers (`learn`, entry skills only), discovery of a tree's other skills through use, a tough-fight boost to learning, Awakening to the four magic traditions.
- **Combat:** rounds in initiative order, auto-attacks, weapons (trained and untrained), techniques for every weapon tree's journeyman tier, conditions (staggered, down, bleeding, held, guarding, exposed), stamina costs, fleeing, death and respawn at the shrine, corpses that keep a player's gear, and death for anyone who quits or disconnects mid-fight.
- **World events:** saved world flags and data-driven events. A rat breakout into the city raises the Rat King, whose aura toughens the rats until he dies.

The [README](README.md) has the fuller list, and each design doc says what of it is built.

## Next up

**Stealth,** step 5 of the plan in [Skill trees](trees.md#proposed-order): seeing and being seen, and Backstab from hiding. Step 4 is built: loot, gathering, tracking, climbing and swimming (see [Gathering](trees.md#gathering), [Climbing and swimming](world.md#climbing-and-swimming), [Tracking](world.md#tracking)).

After that, in order ([Proposed order](trees.md#proposed-order)): spells (their own category: known, cast through a school skill, see [Spells](magic.md#spells)), then money, shops and crafts.

## Small things noticed and not yet done

- Attacks use the margin as blows (glancing to crushing); other contests don't yet: quiet picks, finds of better quality. Later Stealth and Knife skills could add Backstab damage, or make crushing blows easier for it.
- Trained mobs now land mostly strong and crushing blows on a novice: a town guard hits a new character for up to 14. Guards aren't aggressive, but this needs a look in play.
- Experience and level numbers are first guesses: 50 for an even fight, 100 × 1.25ⁿ per level.
- The silver locket from the Sunken Hall has no use until shops.
- Difficulties are re-rated for the steeper contests and still first guesses: climbs and swims 4 (about one try in three untrained), ordinary locks 4, foraging 3.
- Herbs have no use until Brewing; rat tails and the Rat King's tooth have none until quests or shops.
- Foraging's patch numbers are first guesses: three finds a room, one back every 10 minutes.
- First Aid's numbers are first guesses too: a novice (Intuition 3) heals 2 to 4, and treats a badly hurt patient about a third of the time.
- Waterskins can't be drunk from; they need refilling, so they wait for wells and rivers.
- Armour's numbers are first guesses. Guards in studded leather and a helm (3 armour) shrug off most of a new character's punches.
- The tough-fight odds count armour's penalty and a shield's block, but not its reduction, so a heavily armoured foe counts as less tough than it is.
- Town guards drop their short swords (and now armour) and respawn, so they can be farmed for them. Fine while nothing is sold; worth a look once shops exist.
- Discovery chance (0.1% a use) and the tough-fight cap (2×) are first guesses, untested in play.
- Room descriptions don't show who's fighting whom.
- A pack attacking repeats "The sewer rat attacks you!" once per rat.
- `pick` doesn't require lockpicks, though urchins carry them.
- Most background skills (Foraging, Stealth, Haggling…) have no tree yet, so new characters only get Brawling, Dodge, Endurance and the few that exist (Toughness, Lockpicking, Pickpocketing). They're granted at login once the trees are written.
- The rat breakout has only been tested by setting the flag by hand (`worldflag`), not by a rat wandering up through an open grate.
- Expert and master tiers of every tree, Archery and Shields.
- Weather turning ground to mud is designed ([Weather](world.md#weather-planned)), not built.

## Waiting on a decision

The design docs keep their own open questions; these are the ones that shape the most:

- **What stops one character learning every tree?** Trainers now teach only entry skills, but those are still free ([Skills](skills.md#open-questions)).
- **What lessons cost,** once there's money.
- **How many spells a caster can hold,** and whether spells cost Karma ([Magic](magic.md#open-questions)).
- **The tier gates** (15, 30, 45) and how many skills per tree ([Skill trees](trees.md#open-questions)).
- **Where the GM role sits** among the roles ([Architecture](architecture.md#open-questions)).

## Working on it

- **Checks on every change:** `cargo fmt`, `cargo clippy --all-targets` and `cargo test` must be clean. Commit only when asked, with design docs updated alongside the code.
- **Testing live:** run a scratch server, never the real database:

  ```
  MUD_DB=/tmp/scratch.db MUD_ADDR=127.0.0.1:4001 cargo run
  ```

  Then script a session with `nc 127.0.0.1 4001` (end it with `quit`, or `nc` waits forever), or a small Python socket client for things `nc` can't do, like dropping the connection. The first account made on a fresh database is an admin.
- **Builder and admin commands rewrite zone files.** Back up `assets/zones` before a test that uses them, and restore it after.
- **Admin commands for testing:** `grantskill`, `setattr`, `setvitals`, `worldflag`, `summon`, `purge`, `goto`, `create`. Note that `create` and `summon` look up templates in the zone you're standing in unless given `zone:id`.
- **Stopping a test server:** stop it by the process listening on its port (`lsof -t -iTCP:4001 -sTCP:LISTEN`), never by name, so a real server on 4000 is left alone.
- **Deterministic combat tests** build a tiny world and run one `combat_round` (see the tests in `src/combat/mod.rs`), which beats chasing dice live.
