# Where we are

[Design summary](README.md) · Last updated 2026-10-25, after limiting trainers to entry skills, adding discovery through use and the tough-fight boost, and making Inner Discipline the Adept's school, then giving mobs equipment and skills their own text, then armour, then healing, then loot tables and gathering, then tracking, climbing and swimming, then reworking contests and progression, with blows and Diku damage words, then stealth, then spells, then money and shops, then Karma's new costs and unlocking skills with it, then Haggling and Appraisal, then item quality and rarity, then players and mobs as one kind of character, with puppeting, then the GM role, then conditions as data

A handoff note for picking the work back up: what's built, what's next, and what's waiting on a decision. The design docs hold the detail; this page points into them.

## What's built

Riftward is a classless telnet MUD on headless Bevy 0.20. Everything below is committed, tested live on a scratch server, and documented.

- **Engine:** telnet with proper negotiation (`src/telnet.rs`), prompts marked with EOR or GA for MUD clients, one command per tick, an action queue (combat actions take a 3-second round), game events, a game clock (a day every 3 real hours), a one-second heartbeat.
- **World:** zone files (RON, hot-reloaded) for Riftwatch (25 rooms), its sewers (22) and the Darkwood (6); doors and locks; terrain that costs stamina to cross; in-game building.
- **Items:** templates, Bulk and encumbrance, hands and wear slots, containers and automatic placement, weapons, corpses that rot, real items that survive restarts, item spawns, unique items.
- **Mobs:** templates and spawns (keeping a count, breeding, after-death and reseed timers, gated by world flags), wandering and crowding, pack limits, aggression, auras, the players' vitals, technique skills used by chance, and equipment they fight with and drop.
- **Characters:** creation (race with Earthdawn-style shifts, starting Karma, a background as a past life, a description), vitals that recover (faster resting, offline too, never in a fight), `score`, `describe`, aliases, a custom prompt.
- **Skills:** trees as data (`assets/skills/*.skill.ron`), use-based progression with practice and gain cooldowns, uncapped levels with a soft cap from character level, experience and character levels that earn Karma, opposed-roll contests, active and passive skills, attacks, techniques, auras, passive stat bonuses, counters (Riposte), trainers (`learn`, entry skills only), discovery of a tree's other skills through use, a tough-fight boost to learning, Awakening to the four magic traditions.
- **Karma:** attributes cost the rating they reach (3 to 4 costs 4); characters start with 20 (humans 32); `karma <skill>` unlocks a priced skill in a tree the character has, once its requirements are met.
- **Characters:** players and mobs share commands, output and rules. Mobs can have races; wandering mobs walk with `go`, and aggressive ones start fights with `kill`; GMs can `puppet` a mob and play it.
- **Roles:** player, builder, GM and admin, with builders and GMs side by side: builders edit the world's files, GMs run live events (puppeting, summoning, world flags).
- **Conditions** are data (`assets/conditions/`): seventeen, made of modifiers, with their own wording and hints; techniques, spells and items can apply or grant any of them, and timed ones are saved.
- **Items** also have quality (crude to masterwork, per item) and rarity (common to legendary, per template), which set their worth and, for quality, a weapon's damage or armour's protection.
- **Money and shops:** a copper, silver and gold purse, coins from kills, three Riftwatch shops that sell, buy and resell, spell lessons that cost money, and the Trade tree: Haggling (a deal with each shopkeeper) and Appraisal.
- **Spells:** spell books as data (`assets/spells/`), `cast` and `spells`, Drain resisted by Willpower, the first two spells of each casting tradition taught by its teacher, cast in or out of fights by players and mobs alike, and the blessed and warded conditions.
- **Combat:** rounds in initiative order, auto-attacks, weapons (trained and untrained), techniques for every weapon tree's journeyman tier, conditions (staggered, down, bleeding, held, guarding, exposed), stamina costs, fleeing, death and respawn at the shrine, corpses that keep a player's gear, and death for anyone who quits or disconnects mid-fight.
- **World events:** saved world flags and data-driven events. A rat breakout into the city raises the Rat King, whose aura toughens the rats until he dies.

The [README](README.md) has the fuller list, and each design doc says what of it is built.

## Next up

**Step 2 of [Effects, conditions and scripting](effects.md#how-it-gets-built):** one effect vocabulary and one ability shape for spells, techniques and active skills. Step 1, conditions as data, is built, and so are damage types (2a): typed weapons, attacks and spells, armour that says what it stops, and `Resist`, `Weakness` and `Immune`.

Then **the crafts,** the rest of step 7 in [Skill trees](trees.md#proposed-order). Money, shops, Haggling and Appraisal are built: see [Money and shops](economy.md).

## Small things noticed and not yet done

- Attacks use the margin as blows (glancing to crushing), and so does gathering for the quality of what's found; quiet picks don't yet. Later Stealth and Knife skills could add Backstab damage, or make crushing blows easier for it.
- Trained mobs now land mostly strong and crushing blows on a novice: a town guard hits a new character for up to 14. Guards aren't aggressive, but this needs a look in play.
- Experience and level numbers are first guesses: 50 for an even fight, nothing below a one-in-ten risk, 1,000 a level, 5 Karma a level. With levels no longer slowing on their own, the soft cap (10 + 2 × level) and how high levels go both depend on how hard the world's fights get, which needs watching as content grows.
- Difficulties are re-rated for the steeper contests and still first guesses: climbs and swims 4 (about one try in three untrained), ordinary locks 4, foraging 3.
- A calmed animal stays calm until it dies or resets: there's no time limit yet.
- Spells come only from teachers: scrolls and grimoires, a spell-holding limit, more schools and great spells costing health are still to come. An admin can know every tradition's foundation; players can't yet, an open question.
- Spell numbers are first guesses: Drain 2 or 3, resisted at twice that, power = school level + its attribute + Magic, a third of which adds to damage, healing and wards. A teacher at school 20 with Magic 4 casts a Spark or Ward that a new character can't match.
- A spell readied out of a fight (`Casting`), like an opener, lingers if the fight never starts, and fires at the next one.
- Races are uneven at the new Karma prices (shifts worth 14 to 22 Karma, humans 12 and Versatility), left until racial skills are built and each race can be balanced as a whole ([Races](characters.md#races)). No racial skill exists yet, Versatility included.
- Karma numbers are first guesses: 20 starting (32 for humans), 5 a level, and skill prices from 6 to 16.
- Quality odds and multipliers are first guesses (half of found things standard, masterwork ×3 and +2), and so are the rarity multipliers.
- Haggling's numbers are first guesses: deals of a twentieth to a fifth, lasting ten minutes, and shopkeepers at 5 to 10. Deals aren't saved, and a deal is only struck by those who know Haggling.
- Prices are first guesses (see [Prices](economy.md#prices)), and so is buying at a third. A shop's resale stock and a player's money on death are open questions.
- Puppeting is for GMs and admins (builders can't). `help` still lists GM commands under "Building" and "Admin". While puppeting, the builder hears nothing of their own body, even if it's attacked. Lessons (`learn`) and account commands (`raise`, `alias`, `describe`) are for players only.
- Conditions: Hidden and Nourished are still their own components, not data. Modifiers beyond the twelve built (initiative, attributes, blind, fear, haste…) come when something needs them. No content uses the nine new conditions yet; designers can, with `Inflict`, `Apply` and `grants`.
- No mob hides yet, and `search` doesn't find hidden doors yet (they aren't built).
- Hiding isn't saved: logging out shows you, and a restart forgets everyone's hiding.
- Herbs, rat tails and the Rat King's tooth can only be sold until Brewing and quests use them.
- Foraging's patch numbers are first guesses: three finds a room, one back every 10 minutes.
- First Aid's numbers are first guesses too: a novice (Intuition 3) heals 2 to 4, and treats a badly hurt patient about a third of the time.
- Waterskins can't be drunk from; they need refilling, so they wait for wells and rivers.
- Armour's numbers are first guesses. Guards in studded leather and a helm (3 armour) shrug off most of a new character's punches.
- The tough-fight odds count armour's penalty and a shield's block, but not its reduction, so a heavily armoured foe counts as less tough than it is.
- Town guards drop their short swords and armour and respawn, and the tinker buys anything: a guard is worth about 1 gold 6 silver in gear, plus his coins. Worth a look in play ([open question](economy.md#open-questions)).
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
