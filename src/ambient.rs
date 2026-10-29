//! Ambient lines: a little life between fights. A mob template or a room can have `chatter`,
//! lines one of which now and then reaches the players there: a crow cawing overhead, water
//! dripping in the sewers, the hum of the Rift-Scar. A room with a player in it says one about
//! every `chatter_minutes` (`rules.ron`), picked from its own lines and those of the living mobs
//! there that aren't hidden. The pick isn't plain chance: a line just said is the least likely to
//! come again, and the others grow likelier each time they're passed over, so every line comes
//! round and repeats are rare.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::combat::{Conditions, Dead};
use crate::dice::Dice;
use crate::mobs::{Mob, MobTemplates};
use crate::output::{Outbox, Outgoing};
use crate::time::heartbeat;
use crate::world::{InRoom, Player};

/// A room's own ambient lines (`RoomDef::chatter`).
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Chatter(pub Vec<String>);

/// How much likelier a line is for each line said since it last was, up to the cap: a line just
/// said has weight 1, one passed over four times 5, as one never said.
const FRESHEST: u32 = 4;

/// What each room has said: for each line, how many lines since it was said there.
#[derive(Resource, Default, Debug)]
pub struct Said(HashMap<Entity, HashMap<String, u32>>);

impl Said {
    /// Pick one of `lines` for `room`, the longer since one was said the likelier, and remember
    /// it was.
    fn pick(&mut self, room: Entity, lines: &[&String], dice: &mut Dice) -> Option<String> {
        let since = self.0.entry(room).or_default();
        let table: Vec<(&String, u32)> = lines
            .iter()
            .map(|line| (*line, 1 + since.get(*line).copied().unwrap_or(FRESHEST)))
            .collect();
        let line = dice.pick(0, &table)?.to_string();
        for count in since.values_mut() {
            *count = (*count + 1).min(FRESHEST);
        }
        since.insert(line.clone(), 0);
        Some(line)
    }
}

pub struct AmbientPlugin;

impl Plugin for AmbientPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Said>()
            .add_systems(Update, chatter.run_if(heartbeat()));
    }
}

/// The chance a room with someone in it says a line in `secs` seconds, at one about every
/// `minutes`.
pub fn chance_in(secs: f64, minutes: f64) -> f64 {
    if minutes <= 0.0 {
        return 0.0;
    }
    1.0 - (-secs / (minutes * 60.0)).exp()
}

/// Now and then, a line from a room with a player in it: its own, or a mob's there.
#[allow(clippy::too_many_arguments)]
fn chatter(
    time: Res<Time>,
    mut last: Local<Option<f64>>,
    players: Query<&InRoom, With<Player>>,
    rooms: Query<&Chatter>,
    mobs: Query<(&Mob, &InRoom, Option<&Conditions>), Without<Dead>>,
    templates: Res<MobTemplates>,
    mut dice: ResMut<Dice>,
    mut said: ResMut<Said>,
    mut outbox: ResMut<Outbox>,
) {
    let now = time.elapsed_secs_f64();
    let secs = now - last.replace(now).unwrap_or(now);
    let p = chance_in(secs, crate::rules::rules().chatter_minutes);
    if p <= 0.0 {
        return;
    }
    let mut heard: Vec<Entity> = players.iter().map(|r| r.0).collect();
    heard.sort();
    heard.dedup();
    for room in heard {
        let lines: Vec<&String> = rooms
            .get(room)
            .map_or(&[][..], |c| &c.0)
            .iter()
            .chain(
                mobs.iter()
                    .filter(|(_, at, conditions)| {
                        at.0 == room && conditions.is_none_or(|c| c.hidden().is_none())
                    })
                    .filter_map(|(mob, ..)| templates.0.get(&mob.key))
                    .flat_map(|def| &def.chatter),
            )
            .collect();
        if lines.is_empty() || !dice.chance(0, p) {
            continue;
        }
        if let Some(line) = said.pick(room, &lines, &mut dice) {
            outbox.0.push(Outgoing::Room {
                room,
                except: Entity::PLACEHOLDER,
                text: format!("{line}\r\n"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_about_every_so_often() {
        // Over a minute, at one a minute, more likely than not; never without a rate.
        let p = chance_in(60.0, 1.0);
        assert!((0.6..0.65).contains(&p), "{p}");
        assert!(chance_in(1.0, 1.0) < 0.02);
        assert_eq!(chance_in(60.0, 0.0), 0.0);
    }

    #[test]
    fn a_line_just_said_is_the_least_likely_to_come_again() {
        let mut said = Said::default();
        let mut dice = Dice::seeded(11);
        let room = Entity::from_raw_u32(1).unwrap();
        let lines: Vec<String> = ["caw", "rustle", "drip"].map(String::from).into();
        let lines: Vec<&String> = lines.iter().collect();
        let mut repeats = 0;
        let mut last = None;
        for _ in 0..600 {
            let line = said.pick(room, &lines, &mut dice);
            repeats += usize::from(line == last);
            last = line;
        }
        // By plain chance a third would repeat the one before; weighted, about one in eleven.
        assert!(repeats < 100, "{repeats} repeats in 600");
    }
}
