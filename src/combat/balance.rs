//! Fight balance, from the data files as they are: parties of characters built for a character
//! level against whole encounters (a den as it spawns, not one wolf), fought round by round with
//! the real `combat_round` many times over. A report, not a check, so it's ignored by `cargo test`:
//!
//! ```sh
//! cargo test balance_ -- --ignored --nocapture --test-threads 1
//! ```
//!
//! Each place has its report (`balance_town`, `balance_sewer_rats`, `balance_sewer_slimes`, `balance_deep_wood`),
//! so one can be run on its own.
//!
//! What a character of a level has comes from simulating the skill-gain rules
//! (`docs/progression.md#what-a-character-of-a-level-has`): their main weapon, Dodge, Toughness
//! and Heavy Armour, Karma spent on Strength, Reaction and Body, and town armour to match.

use std::collections::HashMap;

use bevy::ecs::message::Messages;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use super::*;
use crate::characters::{RaceDef, Races};
use crate::items::{ItemLocation, ItemTemplates, Quality};
use crate::skills::{Skill, SkillState, SkillTreeDef};
use crate::vitals::{Maxima, Vitals};
use crate::world::{Exit, Exits, ItemKey, MobKey, Player, Room, RoomKey, Slot, Terrain, ZoneDef};

/// What a party member of a character level has.
struct Build {
    level: u32,
    strength: i32,
    reaction: i32,
    body: i32,
    weapon: u32,
    dodge: u32,
    toughness: u32,
    armour: u32,
    /// Town gear: worn armour, and the sword in hand.
    gear: &'static [&'static str],
}

/// A new character (level 0: a sword from a trainer, a little Karma on attributes), then
/// character levels 1, 2, 3, 6, 8, 10 and 12: skills from the progression simulation (a main weapon used
/// every round, Dodge about a third of rounds, Toughness and Heavy Armour as they're hit), and
/// about two thirds of a human's Karma on attributes, at a raise's cost of the new rating.
const BUILDS: [Build; 8] = [
    Build {
        level: 0,
        strength: 4,
        reaction: 4,
        body: 4,
        weapon: 2,
        dodge: 1,
        toughness: 0,
        armour: 0,
        gear: &["riftwatch:short_sword"],
    },
    Build {
        level: 1,
        strength: 5,
        reaction: 4,
        body: 4,
        weapon: 6,
        dodge: 5,
        toughness: 2,
        armour: 0,
        gear: &["riftwatch:short_sword"],
    },
    Build {
        level: 2,
        strength: 5,
        reaction: 5,
        body: 4,
        weapon: 9,
        dodge: 8,
        toughness: 5,
        armour: 3,
        gear: &["riftwatch:short_sword", "riftwatch:leather_jerkin"],
    },
    Build {
        level: 3,
        strength: 5,
        reaction: 5,
        body: 4,
        weapon: 13,
        dodge: 11,
        toughness: 8,
        armour: 6,
        gear: &["riftwatch:short_sword", "riftwatch:leather_jerkin"],
    },
    Build {
        level: 6,
        strength: 6,
        reaction: 6,
        body: 5,
        weapon: 20,
        dodge: 17,
        toughness: 13,
        armour: 10,
        gear: &[
            "riftwatch:short_sword",
            "riftwatch:studded_leather",
            "riftwatch:leather_cap",
        ],
    },
    Build {
        level: 8,
        strength: 7,
        reaction: 6,
        body: 6,
        weapon: 23,
        dodge: 21,
        toughness: 16,
        armour: 13,
        gear: &[
            "riftwatch:short_sword",
            "riftwatch:mail_shirt",
            "riftwatch:iron_helm",
            "riftwatch:leather_leggings",
        ],
    },
    Build {
        level: 10,
        strength: 7,
        reaction: 7,
        body: 6,
        weapon: 25,
        dodge: 23,
        toughness: 18,
        armour: 15,
        gear: &[
            "riftwatch:short_sword",
            "riftwatch:mail_shirt",
            "riftwatch:iron_helm",
            "riftwatch:leather_leggings",
        ],
    },
    Build {
        level: 12,
        strength: 8,
        reaction: 7,
        body: 7,
        weapon: 27,
        dodge: 25,
        toughness: 20,
        armour: 17,
        gear: &[
            "riftwatch:short_sword",
            "riftwatch:mail_shirt",
            "riftwatch:iron_helm",
            "riftwatch:leather_leggings",
        ],
    },
];

/// How one simulated fight came out.
#[derive(Default)]
struct Outcome {
    won: bool,
    rounds: u32,
    /// Party members dead at the end.
    fallen: usize,
}

/// Everything in the data files a fight needs.
fn world_from_disk() -> World {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let mut defs = SkillDefs::default();
    for entry in std::fs::read_dir(root.join("skills")).unwrap() {
        let path = entry.unwrap().path();
        let tree: SkillTreeDef = crate::world::ron_options()
            .from_bytes(&std::fs::read(&path).unwrap())
            .unwrap();
        let id = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".skill.ron")
            .to_string();
        defs.trees.insert(id.clone(), tree.name.clone());
        for (skill, def) in &tree.skills {
            let key = format!("{id}:{skill}");
            defs.skills.insert(
                key.clone(),
                Skill {
                    key,
                    tree: id.clone(),
                    attribute: def.attribute.unwrap_or(tree.attribute),
                    def: def.clone(),
                },
            );
        }
    }
    let (mut items, mut mobs) = (HashMap::new(), HashMap::new());
    for entry in std::fs::read_dir(root.join("zones")).unwrap() {
        let path = entry.unwrap().path();
        let zone: ZoneDef = crate::world::ron_options()
            .from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap();
        let id = path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches(".zone.ron")
            .to_string();
        for (item, def) in zone.items {
            items.insert(ItemKey::parse(&item, &id), def);
        }
        for (mob, def) in zone.mobs {
            mobs.insert(MobKey::parse(&mob, &id), def);
        }
    }
    let races: Vec<RaceDef> = crate::world::ron_options()
        .from_str(&std::fs::read_to_string(root.join("races.ron")).unwrap())
        .unwrap();

    let mut world = World::new();
    world.insert_resource(defs);
    world.insert_resource(SpellDefs::default());
    world.insert_resource(defs_on_disk());
    world.insert_resource(ItemTemplates(items));
    world.insert_resource(MobTemplates(mobs));
    world.insert_resource(Races(races));
    world.insert_resource(Dice::seeded(11));
    world.insert_resource(Database::open(":memory:").unwrap());
    world.insert_resource(Time::<()>::default());
    world.insert_resource(Outbox::default());
    world.insert_resource(crate::spawns::Spawns::default());
    world.init_resource::<Encounters>();
    world.init_resource::<WantedLoot>();
    world.init_resource::<crate::facts::Facts>();
    world.init_resource::<crate::light::LitRooms>();
    world.init_resource::<Messages<GameEvent>>();
    world
}

/// A lit room to fight in, and one east of it, where those who might answer a call for help wait.
fn arena(world: &mut World) -> (Entity, Entity) {
    let mut room = |id: &str| {
        world
            .spawn((
                Room {
                    name: id.into(),
                    description: String::new(),
                    no_wander: false,
                    indoors: true,
                    terrain: Terrain::Paved,
                },
                RoomKey::parse(&format!("test:{id}"), ""),
            ))
            .id()
    };
    let (arena, next_door) = (room("arena"), room("next_door"));
    let exit = |to| Exit {
        to,
        door: None,
        needs: None,
    };
    world
        .entity_mut(arena)
        .insert(Exits([("east".to_string(), exit(next_door))].into()));
    world
        .entity_mut(next_door)
        .insert(Exits([("west".to_string(), exit(arena))].into()));
    (arena, next_door)
}

/// A party member built as `build`, in `room`.
fn member(world: &mut World, room: Entity, build: &Build, n: usize) -> Entity {
    let attributes = Attributes::starting(
        |attribute| match attribute {
            Attribute::Strength => build.strength,
            Attribute::Reaction => build.reaction,
            Attribute::Body => build.body,
            other => other.base_rating(),
        },
        0,
    );
    let skills = Skills(
        [
            ("swordplay:basics", build.weapon),
            ("athletics:dodge", build.dodge),
            ("athletics:toughness", build.toughness),
            ("armour:heavy_armour", build.armour),
            ("athletics:endurance", 5),
            ("brawling:basics", 3),
        ]
        .into_iter()
        .map(|(key, level)| {
            let mut state = SkillState::new("Template");
            state.level = level;
            (key.to_string(), state)
        })
        .collect(),
    );
    let vitals = Vitals::full(Maxima::of(
        &attributes,
        &skills,
        world.resource::<SkillDefs>(),
        None,
    ));
    let who = world
        .spawn((
            Player {
                name: format!("Fighter{n}"),
                character_id: 1000 + n as i64,
                account: format!("fighter{n}"),
                role: crate::db::Role::Player,
                race: "human".into(),
                background: String::new(),
                description: String::new(),
            },
            InRoom(room),
            attributes,
            vitals,
            skills,
            AutoAttack(Some("swordplay:basics".into())),
        ))
        .id();
    let gear: Vec<(ItemKey, ItemLocation)> = build
        .gear
        .iter()
        .map(|key| {
            let key = ItemKey::parse(key, "");
            let def = &world.resource::<ItemTemplates>().0[&key];
            let at = match def.wear {
                Some(slot) => ItemLocation::Worn(who, slot),
                None => ItemLocation::Held(who),
            };
            (key, at)
        })
        .collect();
    world
        .run_system_once(
            move |mut commands: Commands, templates: Res<ItemTemplates>| {
                for (key, at) in gear.clone() {
                    crate::items::spawn_item(
                        &mut commands,
                        &templates,
                        key,
                        None,
                        at,
                        Quality::Standard,
                    );
                }
            },
        )
        .unwrap();
    let _ = Slot::Neck;
    who
}

/// The mobs of an encounter, by template, in `room`.
fn foes(world: &mut World, room: Entity, encounter: &[(&str, usize)]) -> Vec<Entity> {
    let keys: Vec<MobKey> = encounter
        .iter()
        .flat_map(|(key, n)| std::iter::repeat_n(MobKey::parse(key, ""), *n))
        .collect();
    world
        .run_system_once(
            move |mut commands: Commands,
                  templates: Res<MobTemplates>,
                  items: Res<ItemTemplates>,
                  defs: Res<SkillDefs>,
                  races: Res<Races>,
                  mut dice: ResMut<Dice>| {
                let mut made = Vec::new();
                for key in &keys {
                    made.extend(crate::mobs::spawn_mob(
                        &mut commands,
                        &templates,
                        &items,
                        &defs,
                        key,
                        room,
                        &races,
                        &mut dice,
                    ));
                }
                made
            },
        )
        .unwrap()
}

fn alive(world: &World, who: Entity) -> bool {
    world.get::<Dead>(who).is_none() && world.get::<Vitals>(who).is_some_and(|v| v.health > 0.0)
}

/// Point everyone still standing at a foe still standing: the party all at the first, as a party
/// focuses its blows; each foe at a party member in turn.
fn aim(world: &mut World, party: &[Entity], mobs: &[Entity]) {
    let standing = |world: &World, side: &[Entity]| -> Vec<Entity> {
        side.iter().copied().filter(|&e| alive(world, e)).collect()
    };
    let (ours, theirs) = (standing(world, party), standing(world, mobs));
    for (side, foes) in [(&ours, &theirs), (&theirs, &ours)] {
        for (i, &who) in side.iter().enumerate() {
            let target = world.get::<Fighting>(who).map(|f| f.0);
            if target.is_some_and(|t| foes.contains(&t)) {
                continue;
            }
            let pick = if std::ptr::eq(side, &ours) { 0 } else { i };
            match foes.get(pick % foes.len().max(1)) {
                Some(&foe) => {
                    world.entity_mut(who).insert(Fighting(foe));
                }
                None => {
                    world.entity_mut(who).remove::<Fighting>();
                }
            }
        }
    }
}

/// Add to `mobs` whoever has joined the fight in `room`: what the encounter brought into being (an
/// ooze torn off, oozes the Slime Lord shed), and those who answered a call for help.
fn joined(world: &mut World, room: Entity, mobs: &mut Vec<Entity>) {
    let mut new: Vec<Entity> = world
        .query_filtered::<(Entity, &InRoom), (With<Vitals>, With<Fighting>, Without<Player>)>()
        .iter(world)
        .filter(|(e, at)| at.0 == room && !mobs.contains(e))
        .map(|(e, _)| e)
        .collect();
    new.sort();
    mobs.extend(new);
}

/// One fight of `size` members built as `build` against `tuned`, to the end or 60 rounds.
fn fight(build: &Build, size: usize, tuned: &Tuned, seed: u64) -> Outcome {
    let mut world = world_from_disk();
    world.insert_resource(Dice::seeded(seed));
    let (room, next_door) = arena(&mut world);
    let party: Vec<Entity> = (0..size)
        .map(|n| member(&mut world, room, build, n))
        .collect();
    let mut mobs = foes(&mut world, room, tuned.foes);
    foes(&mut world, next_door, tuned.near);
    for rounds in 1..=60 {
        // Whose eyes a shown fight is seen through: the first still standing.
        let eyes = party
            .iter()
            .copied()
            .find(|&e| alive(&world, e))
            .unwrap_or(party[0]);
        aim(&mut world, &party, &mobs);
        world.run_system_once(combat_round).unwrap();
        world
            .run_system_once(super::spawning::spawn_from_effects)
            .unwrap();
        world.run_system_once(super::help::answer_calls).unwrap();
        // A system run once reads every message still kept, so each round's are let go after it.
        world.resource_mut::<Messages<GameEvent>>().clear();
        world.flush();
        let told = std::mem::take(&mut world.resource_mut::<Outbox>().0);
        if std::env::var("BALANCE_SHOW").is_ok() {
            for out in told {
                let line = match out {
                    crate::output::Outgoing::To(who, line) if who == eyes => line,
                    crate::output::Outgoing::Room {
                        room: at,
                        except,
                        text,
                    } if at == room && except != eyes => text,
                    _ => continue,
                };
                print!("[{rounds}] {line}");
            }
        }
        let before = mobs.len();
        joined(&mut world, room, &mut mobs);
        if std::env::var("BALANCE_SHOW").is_ok() {
            for &new in &mobs[before..] {
                let name = world
                    .get::<crate::mobs::Mob>(new)
                    .map_or("someone".into(), |m| m.name.clone());
                println!("[{rounds}] -- {name} joins the fight.");
            }
        }
        let standing = |side: &[Entity]| side.iter().filter(|&&e| alive(&world, e)).count();
        let (ours, theirs) = (standing(&party), standing(&mobs));
        if ours == 0 || theirs == 0 {
            return Outcome {
                won: theirs == 0,
                rounds,
                fallen: size - ours,
            };
        }
    }
    Outcome {
        won: false,
        rounds: 60,
        fallen: 0,
    }
}

/// An encounter, and what it's tuned for: (character level, party size, the share of fights
/// that party should win, in percent). The party it's meant for should win most fights and lose
/// few members (the report's deaths are party members fallen per fight); one fewer, or weaker, a
/// real gamble.
struct Tuned {
    name: &'static str,
    foes: &'static [(&'static str, usize)],
    /// Mobs in the room next door, who come if one of `foes` calls for help.
    near: &'static [(&'static str, usize)],
    targets: &'static [(u32, usize, u32)],
}

/// Riftwatch's own (tier 0 to 1), for a new character's first fights: the alley rats of Crooked
/// Alley, which leave people be, and the stray dog they're measured against; and the law: a town
/// guard kills a new character, often in a round, and is a match for one of character level 8,
/// and the keep's guards for one of level 12 (decided 2026-10-30).
const TOWN: &[Tuned] = &[
    Tuned {
        name: "an alley rat",
        foes: &[("riftwatch:alley_rat", 1)],
        near: &[],
        targets: &[(0, 1, 97)],
    },
    Tuned {
        name: "two alley rats",
        foes: &[("riftwatch:alley_rat", 2)],
        near: &[],
        targets: &[(0, 1, 75)],
    },
    Tuned {
        name: "the stray dog",
        foes: &[("riftwatch:stray_dog", 1)],
        near: &[],
        targets: &[(0, 1, 85)],
    },
    Tuned {
        name: "a town guard",
        foes: &[("riftwatch:town_guard", 1)],
        near: &[],
        targets: &[(0, 1, 0), (3, 1, 5), (6, 1, 25), (8, 1, 50), (10, 1, 75)],
    },
    Tuned {
        name: "town guard +1 near",
        foes: &[("riftwatch:town_guard", 1)],
        near: &[("riftwatch:town_guard", 1)],
        targets: &[(8, 1, 15), (8, 2, 60), (10, 1, 40)],
    },
    Tuned {
        name: "town guard +2 near",
        foes: &[("riftwatch:town_guard", 1)],
        near: &[("riftwatch:town_guard", 2)],
        targets: &[(8, 2, 35), (10, 1, 15), (10, 2, 70)],
    },
    Tuned {
        name: "a keep guard",
        foes: &[("riftwatch:keep_guard", 1)],
        near: &[],
        targets: &[(8, 1, 10), (10, 1, 25), (12, 1, 50)],
    },
    Tuned {
        name: "keep guard +1 near",
        foes: &[("riftwatch:keep_guard", 1)],
        near: &[("riftwatch:keep_guard", 1)],
        targets: &[(12, 1, 10), (12, 2, 50)],
    },
];

/// The sewers' east, the rats' side (tiers 1 and 3), tuned 2026-10-30: a rat, the snake or a pack
/// of three for a new character alone, the swarm in the Overflow Chamber for two new characters
/// or one at character level 1, and the Rat King, risen in the Warren among his rats, for two or
/// three at level 3. A single rat is safe even at level 0: one hour's play outgrows it.
const SEWER_RATS: &[Tuned] = &[
    Tuned {
        name: "a sewer rat",
        foes: &[("sewers:sewer_rat", 1)],
        near: &[],
        targets: &[(0, 1, 95), (1, 1, 99)],
    },
    Tuned {
        name: "a giant cockroach",
        foes: &[("sewers:giant_cockroach", 1)],
        near: &[],
        targets: &[(0, 1, 95)],
    },
    Tuned {
        name: "the sewer snake",
        foes: &[("sewers:sewer_snake", 1)],
        near: &[],
        targets: &[(0, 1, 60), (1, 1, 85)],
    },
    Tuned {
        name: "a pack of rats",
        foes: &[("sewers:sewer_rat", 3)],
        near: &[],
        targets: &[(0, 1, 50), (1, 1, 95)],
    },
    Tuned {
        name: "the rat swarm",
        foes: &[("sewers:sewer_rat", 4)],
        near: &[],
        targets: &[(0, 1, 15), (0, 2, 85), (1, 1, 90)],
    },
    Tuned {
        name: "the Rat King",
        foes: &[("sewers:rat_king", 1), ("sewers:sewer_rat", 3)],
        near: &[],
        targets: &[(2, 3, 60), (3, 1, 10), (3, 2, 60), (3, 3, 90)],
    },
];

/// The sewers' west, the slimes' side (tiers 1 to 3), tuned 2026-10-30: an ooze or two (they
/// come two at a time, `pack: 2`) for a new character, the grey slimes of the Ossuary, two at a
/// time, a fight at character level 2 and deadly at 1, and the Slime Lord in the Old Cistern for
/// two or three at level 3. Swords only: fire, which slimes fear, would make them easier.
const SEWER_SLIMES: &[Tuned] = &[
    Tuned {
        name: "two oozes",
        foes: &[("sewers:ooze", 2)],
        near: &[],
        targets: &[(0, 1, 85), (1, 1, 99)],
    },
    Tuned {
        name: "two grey slimes",
        foes: &[("sewers:grey_slime", 2)],
        near: &[],
        targets: &[(1, 1, 5), (2, 1, 80), (3, 1, 97)],
    },
    Tuned {
        name: "an ooze",
        foes: &[("sewers:ooze", 1)],
        near: &[],
        targets: &[(0, 1, 95), (1, 1, 99)],
    },
    Tuned {
        name: "a grey slime",
        foes: &[("sewers:grey_slime", 1)],
        near: &[],
        targets: &[(1, 1, 60), (2, 1, 97), (3, 1, 99)],
    },
    Tuned {
        name: "the Slime Lord",
        foes: &[("sewers:slime_lord", 1)],
        near: &[],
        targets: &[(2, 3, 45), (3, 1, 10), (3, 2, 55), (3, 3, 85)],
    },
];

/// The deep wood, tuned against parties (decided 2026-10-29): its edge solo at character level 4
/// to 6, the night pack for a pair or three, the den and the bear for three or four at 6 to 8,
/// and the rift-touched stag for four or five at 10.
const DEEP_WOOD: &[Tuned] = &[
    Tuned {
        name: "a grey wolf",
        foes: &[("forest:grey_wolf", 1)],
        near: &[],
        targets: &[(3, 1, 40), (6, 1, 90)],
    },
    Tuned {
        name: "a forest spider",
        foes: &[("forest:forest_spider", 1)],
        near: &[],
        targets: &[(3, 1, 50), (6, 1, 95)],
    },
    Tuned {
        name: "the night pack",
        foes: &[("forest:grey_wolf", 3)],
        near: &[],
        targets: &[(6, 2, 60), (6, 3, 90), (10, 2, 90)],
    },
    Tuned {
        name: "the den",
        foes: &[("forest:grey_wolf", 3), ("forest:old_she_wolf", 1)],
        near: &[],
        targets: &[(6, 3, 45), (6, 4, 80), (10, 4, 95)],
    },
    Tuned {
        name: "the bear",
        foes: &[("forest:brown_bear", 1)],
        near: &[],
        targets: &[(6, 1, 5), (6, 3, 80), (10, 1, 30)],
    },
    Tuned {
        name: "the rift stag",
        foes: &[("forest:rift_stag", 1)],
        near: &[],
        targets: &[(6, 4, 10), (10, 3, 45), (10, 4, 80)],
    },
];

/// Fight each of `encounters` at each of its targets, and print how it went.
fn report(encounters: &[Tuned]) {
    let trials: u64 = std::env::var("BALANCE_TRIALS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(150);
    println!(
        "\n{:<18} {:>5} {:>5} {:>6} {:>7} {:>7} {:>8}",
        "encounter", "level", "party", "wins", "target", "rounds", "deaths"
    );
    for tuned in encounters {
        for &(level, size, target) in tuned.targets {
            let build = BUILDS.iter().find(|b| b.level == level).unwrap();
            let outcomes: Vec<Outcome> = (0..trials)
                .map(|seed| fight(build, size, tuned, seed))
                .collect();
            let wins = outcomes.iter().filter(|o| o.won).count() as u64 * 100 / trials;
            let rounds = outcomes.iter().map(|o| o.rounds).sum::<u32>() as f64 / trials as f64;
            let deaths = outcomes.iter().map(|o| o.fallen).sum::<usize>() as f64 / trials as f64;
            let off = if wins.abs_diff(u64::from(target)) > 15 {
                "  <--"
            } else {
                ""
            };
            println!(
                "{:<18} {level:>5} {size:>5} {wins:>5}% {target:>6}% {rounds:>7.1} {deaths:>8.2}{off}",
                tuned.name
            );
        }
    }
}

#[test]
#[ignore = "a report: cargo test balance_town -- --ignored --nocapture"]
fn balance_town() {
    report(TOWN);
}

#[test]
#[ignore = "a report: cargo test balance_sewer_rats -- --ignored --nocapture"]
fn balance_sewer_rats() {
    report(SEWER_RATS);
}

#[test]
#[ignore = "a report: cargo test balance_sewer_slimes -- --ignored --nocapture"]
fn balance_sewer_slimes() {
    report(SEWER_SLIMES);
}

#[test]
#[ignore = "a report: cargo test balance_deep_wood -- --ignored --nocapture"]
fn balance_deep_wood() {
    report(DEEP_WOOD);
}

/// One fight told blow by blow, as the first party member still standing saw it: `BALANCE_SHOW` names the
/// encounter ("the den"), `BALANCE_LEVEL` and `BALANCE_PARTY` the party (level 6, four), and
/// `BALANCE_SEED` the dice (3).
#[test]
#[ignore = "a look: BALANCE_SHOW='the den' cargo test balance_one -- --ignored --nocapture"]
fn balance_one() {
    let var = |name: &str| std::env::var(name).ok();
    let name = var("BALANCE_SHOW").unwrap_or_else(|| "the den".into());
    let level: u32 = var("BALANCE_LEVEL")
        .and_then(|n| n.parse().ok())
        .unwrap_or(6);
    let size: usize = var("BALANCE_PARTY")
        .and_then(|n| n.parse().ok())
        .unwrap_or(4);
    let tuned = [TOWN, SEWER_RATS, SEWER_SLIMES, DEEP_WOOD]
        .iter()
        .flat_map(|area| area.iter())
        .find(|t| t.name == name)
        .unwrap();
    let build = BUILDS.iter().find(|b| b.level == level).unwrap();
    let seed: u64 = var("BALANCE_SEED")
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let o = fight(build, size, tuned, seed);
    println!("won {} in {} rounds, {} fell", o.won, o.rounds, o.fallen);
}
