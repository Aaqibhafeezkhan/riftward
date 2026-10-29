//! Tracking: reading who has passed through a room, and which way they went.
//!
//! Every room remembers the tracks of whoever walks in or out of it, player or mob, for half an
//! hour; a teleport leaves none, and wading water holds none. `track [<who>]` (or `use tracking`)
//! reads them with Tracking, an active skill made of effects resolved by `trackwork`: each track
//! is found by the tracker's skill against how hard the ground is to read, harder the older the
//! track, as its `against` formula says. Tracks show what made them, not who: "a human" (a player or a
//! townsperson alike), "a sewer rat".

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;

use crate::characters::{Races, article};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::Role;
use crate::events::{GameEvent, Way, from};
use crate::formula::Side;
use crate::mobs::Mob;
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::skills::{Learner, Practice, SkillKind, fill};
use crate::targeting::Keywords;
use crate::world::{Player, Room, Terrain};

/// The skill `track` uses.
const TRACKING: &str = "survival:tracking";
/// How long tracks last, in seconds (`rules.ron`).
fn track_secs() -> f64 {
    crate::rules::rules().tracks.minutes * 60.0
}

/// The most tracks a room remembers; the oldest go first (`rules.ron`).
fn max_tracks() -> usize {
    crate::rules::rules().tracks.most
}

pub struct TrackingPlugin;

impl Plugin for TrackingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tracks>()
            .add_systems(Update, lay_tracks)
            .add_command(
                CommandSpec::new("track", Category::Interaction)
                    .usage("track [<who>]")
                    .summary("read the tracks here, or only those of someone, if you know Tracking")
                    .quiet()
                    .action(),
                track,
            )
            // Run by `use` for an active skill that reads the room's tracks; players don't type
            // it.
            .add_command(
                CommandSpec::new("trackwork", Category::Interaction)
                    .usage("use <skill> [<who>]")
                    .summary("read the tracks here with an active skill")
                    .quiet()
                    .unlisted(),
                trackwork,
            );
    }
}

/// One set of tracks: what made them, which way, and when.
#[derive(Clone, Debug, PartialEq)]
struct Track {
    who: Entity,
    /// What it looks like made them: "a sewer rat", "a human".
    what: String,
    /// Words to pick it out by: a mob's keywords, or a player's race.
    words: Vec<String>,
    going: Going,
    at: f64,
}

/// Which way tracks lead.
#[derive(Clone, Debug, PartialEq)]
enum Going {
    /// Out, by this exit.
    Out(String),
    /// In, from this exit.
    In(String),
}

/// Every room's tracks, newest last. Not saved: a restart washes them away.
#[derive(Resource, Default)]
pub struct Tracks(HashMap<Entity, VecDeque<Track>>);

impl Tracks {
    fn lay(&mut self, room: Entity, track: Track) {
        let tracks = self.0.entry(room).or_default();
        tracks.push_back(track);
        while tracks.len() > max_tracks() {
            tracks.pop_front();
        }
    }

    /// The tracks in `room` still to be seen at `now`, newest first.
    fn in_room(&mut self, room: Entity, now: f64) -> Vec<Track> {
        let Some(tracks) = self.0.get_mut(&room) else {
            return Vec::new();
        };
        let last = track_secs();
        tracks.retain(|t| now - t.at < last);
        tracks.iter().rev().cloned().collect()
    }
}

/// How hard a room's ground is to read a fresh track on (`rules.ron`); `None` where it holds none.
fn ground(terrain: Terrain) -> Option<f64> {
    let ground = crate::rules::rules().tracks.ground;
    match terrain {
        Terrain::Mud => Some(ground.mud),
        Terrain::Uneven => Some(ground.uneven),
        Terrain::Rough => Some(ground.rough),
        Terrain::Road => Some(ground.road),
        Terrain::Paved => Some(ground.paved),
        Terrain::Wading => None,
    }
}

/// How a track's age reads: "fresh", "recent", "old" (`rules.ron`).
fn age(secs: f64) -> &'static str {
    let tracks = &crate::rules::rules().tracks;
    tracks
        .ages
        .iter()
        .find(|(under, _)| secs / 60.0 < *under)
        .map_or(&tracks.older, |(_, words)| words)
}

/// Leave tracks wherever someone walks in or out.
fn lay_tracks(
    mut events: MessageReader<GameEvent>,
    mut tracks: ResMut<Tracks>,
    rooms: Query<&Room>,
    players: Query<&Player>,
    mobs: Query<(&Mob, &Keywords)>,
    races: Res<Races>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    for event in events.read() {
        let (who, room, going) = match event {
            GameEvent::Left {
                who,
                room,
                way: Way::Walked(Some(dir)),
                ..
            } => (*who, *room, Going::Out(dir.clone())),
            GameEvent::Entered {
                who,
                room,
                way: Way::Walked(Some(dir)),
                ..
            } => (*who, *room, Going::In(dir.clone())),
            _ => continue,
        };
        if rooms.get(room).is_ok_and(|r| ground(r.terrain).is_none()) {
            continue;
        }
        // One of the peoples leaves a person's tracks, player or not; a beast its own.
        let race = players
            .get(who)
            .ok()
            .map(|p| p.race.clone())
            .or_else(|| mobs.get(who).ok().and_then(|(mob, _)| mob.race.clone()));
        let (what, words) = if let Some(race) = race {
            let race = races.name(&race).to_lowercase();
            (format!("{} {race}", article(&race)), vec![race])
        } else if let Ok((mob, keywords)) = mobs.get(who) {
            (mob.name.clone(), keywords.0.clone())
        } else {
            continue;
        };
        tracks.lay(
            room,
            Track {
                who,
                what,
                words,
                going,
                at: now,
            },
        );
    }
}

/// `track [<who>]`: Tracking, by its old name. It's an active skill made of effects
/// (`survival.skill.ron`), so this is `use tracking [<who>]`.
fn track(In(inv): In<Invocation>, mut commands: Commands, registry: Res<CommandRegistry>) {
    let Some((spec, id)) = registry.find("trackwork", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: format!("{TRACKING} {}", inv.args.trim()),
        },
    );
}

/// `trackwork <tree:skill> [<who>]`: what `use` runs for an active skill aimed at the room that
/// `Reveal`s its tracks: Tracking. Each track here (or only those whose maker matches `who`) is
/// read by its own roll, the user's skill against the skill's `against` formula, which sees the
/// ground's difficulty as `target.ground` and the track's age in minutes as `age`. Those read are
/// listed after the skill's line. Every try practises the skill.
fn trackwork(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    rooms: Query<&Room>,
    mut tracks: ResMut<Tracks>,
    mut practice: Practice,
    time: Res<Time>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let (key, words) = inv.args.split_once(' ').unwrap_or((&inv.args, ""));
    let Some(skill) = practice.defs.skills.get(key).cloned() else {
        return;
    };
    let SkillKind::Active { against, .. } = &skill.def.kind else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| {
        let room = rooms.get(actor.room).map_err(|_| String::new())?;
        let ground = ground(room.terrain).ok_or("The water holds no tracks.")?;
        let ready = practice.begin(actor.entity, key)?;
        let now = time.elapsed_secs_f64();
        let wanted = words.trim().to_lowercase();
        let skill_rating = f64::from(ready.effectiveness.max(1));
        let mut read = Vec::new();
        for t in tracks.in_room(actor.room, now) {
            if t.who == actor.entity
                || !(wanted.is_empty() || t.words.iter().any(|w| w.starts_with(&wanted)))
            {
                continue;
            }
            let seen = match against {
                None => true,
                Some(against) => {
                    let numbers = Side {
                        ground,
                        ..Side::default()
                    };
                    let mut inputs = practice.inputs(actor.entity, ready.effectiveness, numbers);
                    inputs.age = (now - t.at) / 60.0;
                    let hard = practice.eval(against, &inputs).max(1.0);
                    practice.opposed(skill_rating, ready.luck, hard, 0).won
                }
            };
            if seen {
                let way = match &t.going {
                    Going::Out(dir) => format!("headed {dir}"),
                    Going::In(dir) => format!("came in {}", from(dir)),
                };
                read.push(format!("  {} {way} ({})", t.what, age(now - t.at)));
            }
        }
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
            fighting: false,
        };
        let gains = practice.exercise(actor.entity, learner, key);

        let text = skill.def.text.as_ref().ok_or_else(String::new)?;
        let lines = match (read.is_empty(), &text.miss) {
            (true, Some(miss)) => miss,
            _ => &text.hit,
        };
        let with = [("actor", actor.name.as_str())];
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: fill(&lines.room, &with) + "\r\n",
        });
        let mut told = fill(&lines.actor, &with);
        for line in read {
            told += &format!("\r\n{line}");
        }
        for line in gains {
            told += &format!("\r\n{line}");
        }
        Ok(told)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    deliver(out, &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::{Attribute, Attributes};
    use crate::db::Database;
    use crate::dice::Dice;
    use crate::effects::{Effect, Revealed};
    use crate::formula::Formula;
    use crate::skills::{Cost, Lines, Skill, SkillDef, SkillDefs, SkillState, SkillText, Skills};
    use crate::spells::Aim;
    use crate::vitals::Vitals;
    use crate::world::{InRoom, MobKey};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::{BTreeMap, HashMap};

    /// A muddy lane with a rat's and a dog's fresh tracks, and a tracker at `level`, as Tracking
    /// is in its file. Returns the world, the tracker and the lane.
    fn lane(level: u32) -> (World, Entity, Entity) {
        let mut world = World::new();
        let lines = |actor: &str| Lines {
            actor: actor.into(),
            target: None,
            room: "{actor} studies the ground.".into(),
            beyond: None,
        };
        let tracking = Skill {
            key: TRACKING.into(),
            tree: "survival".into(),
            attribute: Attribute::Intuition,
            def: SkillDef {
                name: "Tracking".into(),
                description: "Reading tracks.".into(),
                kind: SkillKind::Active {
                    command: None,
                    quiet: true,
                    provokes: false,
                    cost: Cost::Free,
                    cooldown: 0.0,
                    aim: Aim::Room,
                    uses: None,
                    resist: None,
                    against: Some(Formula::parse("target.ground * (1 + age / 10)").unwrap()),
                    tags: Vec::new(),
                    effects: vec![Effect::Reveal(Revealed::Tracks)],
                },
                text: Some(SkillText {
                    hit: lines("You read the signs:"),
                    miss: Some(lines("You can't make out any tracks.")),
                }),
                mastery: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires: None,
                karma: None,
            },
        };
        world.insert_resource(SkillDefs {
            skills: HashMap::from([(TRACKING.to_string(), tracking)]),
            trees: HashMap::new(),
        });
        world.insert_resource(Dice::seeded(4));
        world.init_resource::<crate::facts::Facts>();
        world.insert_resource(Database::open(":memory:").unwrap());
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Outbox::default());
        world.init_resource::<Messages<GameEvent>>();
        let room = world
            .spawn(Room {
                name: "Lane".into(),
                description: String::new(),
                no_wander: false,
                indoors: false,
                terrain: Terrain::Mud,
            })
            .id();
        let mut tracks = Tracks::default();
        tracks.lay(room, track(0.0));
        tracks.lay(
            room,
            Track {
                what: "a stray dog".into(),
                words: vec!["dog".into()],
                ..track(0.0)
            },
        );
        world.insert_resource(tracks);
        let mut skill = SkillState::new("Template");
        skill.level = level;
        let tracker = world
            .spawn((
                Mob {
                    key: MobKey::parse("test:tracker", ""),
                    name: "a tracker".into(),
                    here: String::new(),
                    description: String::new(),
                    race: None,
                },
                InRoom(room),
                Attributes::starting(|_| 1, 0),
                Vitals {
                    health: 10.0,
                    stamina: 10.0,
                },
                Skills(BTreeMap::from([(TRACKING.to_string(), skill)])),
            ))
            .id();
        (world, tracker, room)
    }

    fn read(world: &mut World, who: Entity, words: &str) -> String {
        let inv = Invocation {
            entity: who,
            command: "trackwork",
            usage: "",
            args: format!("{TRACKING} {words}"),
        };
        world.run_system_once_with(trackwork, inv).unwrap();
        world
            .resource::<Outbox>()
            .0
            .iter()
            .filter_map(|out| match out {
                Outgoing::To(_, line) => Some(line.clone()),
                Outgoing::Room { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn tracking_reads_the_tracks_here_or_only_those_asked_for() {
        let (mut world, tracker, _) = lane(1000);
        let told = read(&mut world, tracker, "");
        assert!(told.contains("You read the signs:"));
        assert!(told.contains("a sewer rat headed north (fresh)"), "{told}");
        assert!(told.contains("a stray dog headed north"), "{told}");
        world.resource_mut::<Outbox>().0.clear();
        let told = read(&mut world, tracker, "dog");
        assert!(told.contains("a stray dog"));
        assert!(!told.contains("sewer rat"), "only the dog's");
    }

    #[test]
    fn water_holds_no_tracks() {
        let (mut world, tracker, lane) = lane(1000);
        world.get_mut::<Room>(lane).unwrap().terrain = Terrain::Wading;
        assert_eq!(
            read(&mut world, tracker, "").trim_end(),
            "The water holds no tracks."
        );
    }

    fn track(at: f64) -> Track {
        Track {
            who: Entity::PLACEHOLDER,
            what: "a sewer rat".into(),
            words: vec!["rat".into()],
            going: Going::Out("north".into()),
            at,
        }
    }

    #[test]
    fn tracks_fade_and_the_oldest_go_first() {
        let mut tracks = Tracks::default();
        let room = Entity::PLACEHOLDER;
        for i in 0..max_tracks() + 5 {
            tracks.lay(room, track(i as f64));
        }
        let seen = tracks.in_room(room, 30.0);
        assert_eq!(seen.len(), max_tracks());
        assert_eq!(seen[0].at, (max_tracks() + 4) as f64, "newest first");
        // Half an hour on, the early ones are gone.
        assert_eq!(
            tracks.in_room(room, track_secs() + 10.0).len(),
            max_tracks() - 6
        );
        assert!(tracks.in_room(room, track_secs() * 2.0).is_empty());
    }

    #[test]
    fn soft_ground_reads_easier_and_water_not_at_all() {
        assert!(ground(Terrain::Mud) < ground(Terrain::Uneven));
        assert!(ground(Terrain::Uneven) < ground(Terrain::Paved));
        assert_eq!(ground(Terrain::Wading), None);
        assert_eq!(age(60.0), "fresh");
        assert_eq!(age(600.0), "recent");
        assert_eq!(age(1200.0), "old");
    }
}
