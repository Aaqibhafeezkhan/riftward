//! Tracking: reading who has passed through a room, and which way they went.
//!
//! Every room remembers the tracks of whoever walks in or out of it, player or mob, for half an
//! hour; a teleport leaves none, and wading water holds none. `track [<who>]` reads them with
//! Tracking: each track is found by the tracker's skill against how hard the ground is to read,
//! harder the older the track. Tracks show what made them, not who: "a human", "a sewer rat".

use std::collections::{HashMap, VecDeque};

use bevy::prelude::*;

use crate::characters::{Races, article};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::events::{GameEvent, Way, from};
use crate::mobs::Mob;
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};
use crate::skills::{Learner, Practice, fill};
use crate::targeting::Keywords;
use crate::world::{Player, Room, Terrain};

/// The skill `track` uses.
const TRACKING: &str = "survival:tracking";
/// How long tracks last, in seconds.
const TRACK_SECS: f64 = 30.0 * 60.0;
/// The most tracks a room remembers; the oldest go first.
const MAX_TRACKS: usize = 20;

pub struct TrackingPlugin;

impl Plugin for TrackingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Tracks>()
            .add_systems(Update, lay_tracks)
            .add_command(
                CommandSpec::new("track", Category::Interaction)
                    .usage("track [<who>]")
                    .summary("read the tracks here, or only those of someone, if you know Tracking")
                    .action(),
                track,
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
        while tracks.len() > MAX_TRACKS {
            tracks.pop_front();
        }
    }

    /// The tracks in `room` still to be seen at `now`, newest first.
    fn in_room(&mut self, room: Entity, now: f64) -> Vec<Track> {
        let Some(tracks) = self.0.get_mut(&room) else {
            return Vec::new();
        };
        tracks.retain(|t| now - t.at < TRACK_SECS);
        tracks.iter().rev().cloned().collect()
    }
}

/// How hard a room's ground is to read a fresh track on; `None` where it holds none.
fn ground(terrain: Terrain) -> Option<f64> {
    match terrain {
        Terrain::Mud => Some(1.0),
        Terrain::Uneven => Some(3.0),
        Terrain::Rough => Some(5.0),
        Terrain::Road => Some(6.0),
        Terrain::Paved => Some(8.0),
        Terrain::Wading => None,
    }
}

/// How a track's age reads: "fresh", "recent", "old".
fn age(secs: f64) -> &'static str {
    match secs / 60.0 {
        m if m < 5.0 => "fresh",
        m if m < 15.0 => "recent",
        _ => "old",
    }
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
        let (what, words) = if let Ok(player) = players.get(who) {
            let race = races.name(&player.race).to_lowercase();
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

/// `track [<who>]`: read the tracks here, or only those whose maker matches `who`.
fn track(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    rooms: Query<&Room>,
    mut tracks: ResMut<Tracks>,
    mut practice: Practice,
    time: Res<Time>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let reply = (|| {
        let room = rooms.get(actor.room).map_err(|_| String::new())?;
        let difficulty = ground(room.terrain).ok_or("The water holds no tracks.")?;
        let ready = practice.begin(actor.entity, TRACKING)?;
        let now = time.elapsed_secs_f64();
        let wanted = inv.args.trim().to_lowercase();
        let skill = f64::from(ready.effectiveness.max(1));
        let mut read = Vec::new();
        for t in tracks.in_room(actor.room, now) {
            if t.who == actor.entity
                || !(wanted.is_empty() || t.words.iter().any(|w| w.starts_with(&wanted)))
            {
                continue;
            }
            // Older tracks are harder: twice as hard after ten minutes.
            let hard = difficulty * (1.0 + (now - t.at) / 600.0);
            if practice.chance(ready.luck, skill / (skill + hard)) {
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
        };
        let gains = practice.exercise(actor.entity, learner, TRACKING);

        let text = practice
            .defs
            .skills
            .get(TRACKING)
            .and_then(|skill| skill.def.text.clone())
            .ok_or_else(String::new)?;
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
    deliver(out, &mut players);
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for i in 0..MAX_TRACKS + 5 {
            tracks.lay(room, track(i as f64));
        }
        let seen = tracks.in_room(room, 30.0);
        assert_eq!(seen.len(), MAX_TRACKS);
        assert_eq!(seen[0].at, (MAX_TRACKS + 4) as f64, "newest first");
        // Half an hour on, the early ones are gone.
        assert_eq!(
            tracks.in_room(room, TRACK_SECS + 10.0).len(),
            MAX_TRACKS - 6
        );
        assert!(tracks.in_room(room, TRACK_SECS * 2.0).is_empty());
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
