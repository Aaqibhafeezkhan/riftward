//! Game time: a clock that runs eight times faster than real time, so a full day and night pass
//! every three real hours, and the one-second heartbeat that slow systems run on.
//!
//! The clock is worked out from the real time, so it needs no saving: every restart picks up at
//! the same hour. Systems on the heartbeat work from elapsed time, never by counting beats, so the
//! heartbeat's rate is a performance setting rather than a game rule.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::common_conditions::on_timer;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::events::GameEvent;
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};

/// Real seconds in one game day.
pub const REAL_SECS_PER_GAME_DAY: i64 = 3 * 60 * 60;

/// The real time now, in unix seconds, for what's saved to end at a time.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// How often the heartbeat systems run.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// A run condition for systems that only need checking about once a second.
pub fn heartbeat() -> impl FnMut(Res<Time>) -> bool + Clone {
    on_timer(HEARTBEAT)
}

/// How long `real_secs` is, said in game time with the real time after it: "1 hour 20 minutes
/// (10 real minutes)".
pub fn span(real_secs: f64) -> String {
    let game_minutes = (real_secs * 24.0 * 60.0 / REAL_SECS_PER_GAME_DAY as f64).ceil() as i64;
    let real_minutes = (real_secs / 60.0).ceil() as i64;
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    let game = match (game_minutes / 60, game_minutes % 60) {
        (0, m) => plural(m, "minute"),
        (h, 0) => plural(h, "hour"),
        (h, m) => format!("{} {}", plural(h, "hour"), plural(m, "minute")),
    };
    format!("{game} ({})", plural(real_minutes, "real minute"))
}

pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Clock>()
            .add_systems(Update, tick_clock.run_if(heartbeat()))
            .add_command(
                CommandSpec::new("time", Category::Information)
                    .summary("see what time of day it is"),
                time_cmd,
            );
    }
}

/// The part of the day, which changes what the world is like.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    Dawn,
    Day,
    Dusk,
    Night,
}

impl Phase {
    /// Dawn from 5, day from 7, dusk from 19, night from 21.
    pub fn at(hour: u32) -> Self {
        match hour {
            5..7 => Phase::Dawn,
            7..19 => Phase::Day,
            19..21 => Phase::Dusk,
            _ => Phase::Night,
        }
    }

    /// Its name in a sentence: "it being night".
    pub fn name(self) -> &'static str {
        match self {
            Phase::Dawn => "dawn",
            Phase::Day => "day",
            Phase::Dusk => "dusk",
            Phase::Night => "night",
        }
    }

    /// What people outdoors see when this part of the day begins.
    pub fn arrives(self) -> &'static str {
        match self {
            Phase::Dawn => "The sky pales in the east, and the first birds begin to sing.",
            Phase::Day => "The sun climbs clear of the horizon. The day has begun.",
            Phase::Dusk => "The sun sinks low, and long shadows stretch across the ground.",
            Phase::Night => "Darkness falls, and the stars come out one by one.",
        }
    }
}

/// A moment in game time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameTime {
    /// Game seconds since the clock began.
    pub secs: i64,
}

impl GameTime {
    /// The game time at a real moment, in unix seconds.
    pub fn at(unix: i64) -> Self {
        Self {
            secs: unix * (86_400 / REAL_SECS_PER_GAME_DAY),
        }
    }

    pub fn now() -> Self {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        Self::at(unix)
    }

    pub fn hour(self) -> u32 {
        (self.secs.rem_euclid(86_400) / 3600) as u32
    }

    pub fn phase(self) -> Phase {
        Phase::at(self.hour())
    }

    /// "about three in the afternoon", "midnight", "noon".
    pub fn describe(self) -> String {
        let hour = self.hour();
        let word = [
            "twelve", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
            "ten", "eleven",
        ][(hour % 12) as usize];
        match hour {
            0 => "about midnight".into(),
            12 => "about noon".into(),
            1..=4 => format!("about {word} in the small hours"),
            5..=11 => format!("about {word} in the morning"),
            13..=17 => format!("about {word} in the afternoon"),
            _ => format!("about {word} in the evening"),
        }
    }
}

/// The part of the day the world was last told about.
#[derive(Resource, Default)]
pub struct Clock {
    pub phase: Option<Phase>,
}

/// Announce each new part of the day as it begins. Nothing is announced on startup.
fn tick_clock(mut clock: ResMut<Clock>, mut events: MessageWriter<GameEvent>) {
    let phase = GameTime::now().phase();
    if clock.phase.is_some_and(|last| last != phase) {
        events.write(GameEvent::PhaseChanged(phase));
    }
    clock.phase = Some(phase);
}

/// `time`: the hour, read from the sky outdoors or guessed indoors.
fn time_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    rooms: RoomQuery,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let indoors = rooms.get(actor.room).is_ok_and(|(room, _, _)| room.indoors);
    let when = GameTime::now().describe();
    let text = if indoors {
        format!("You can't see the sky from here, but you'd guess it's {when}.")
    } else {
        let sky = match GameTime::now().phase() {
            Phase::Dawn => "The sky is brightening",
            Phase::Day => "The sun is up",
            Phase::Dusk => "The light is failing",
            Phase::Night => "It's dark",
        };
        format!("{sky}. It's {when}.")
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_in_game_time_and_real() {
        assert_eq!(span(600.0), "1 hour 20 minutes (10 real minutes)");
        assert_eq!(span(90.0 * 60.0), "12 hours (90 real minutes)");
        assert_eq!(span(7.0), "1 minute (1 real minute)");
    }

    #[test]
    fn a_game_day_passes_every_three_real_hours() {
        let start = GameTime::at(0);
        assert_eq!(start.hour(), 0);
        // One real hour is eight game hours.
        assert_eq!(GameTime::at(3600).hour(), 8);
        // Three real hours later it's the same hour again.
        assert_eq!(GameTime::at(REAL_SECS_PER_GAME_DAY).hour(), 0);
        // A game hour is seven and a half real minutes.
        assert_eq!(GameTime::at(450).hour(), 1);
        assert_eq!(GameTime::at(449).hour(), 0);
    }

    #[test]
    fn hours_fall_into_parts_of_the_day() {
        let phases: Vec<Phase> = (0..24).map(Phase::at).collect();
        assert_eq!(phases[4], Phase::Night);
        assert_eq!(phases[5], Phase::Dawn);
        assert_eq!(phases[7], Phase::Day);
        assert_eq!(phases[18], Phase::Day);
        assert_eq!(phases[19], Phase::Dusk);
        assert_eq!(phases[21], Phase::Night);
    }

    #[test]
    fn times_read_naturally() {
        let at_hour = |h: i64| GameTime { secs: h * 3600 };
        assert_eq!(at_hour(0).describe(), "about midnight");
        assert_eq!(at_hour(3).describe(), "about three in the small hours");
        assert_eq!(at_hour(9).describe(), "about nine in the morning");
        assert_eq!(at_hour(12).describe(), "about noon");
        assert_eq!(at_hour(15).describe(), "about three in the afternoon");
        assert_eq!(at_hour(22).describe(), "about ten in the evening");
    }
}
