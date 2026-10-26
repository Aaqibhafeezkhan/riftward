//! Conditions: short-lived states on a fighter. A technique's last some of their own turns; a
//! spell's last a while, counted in time, in a fight or out of one.
//!
//! Every fighter has a `Conditions` component (it comes with `Vitals`). At the start of each of
//! their turns, bleeding hurts, a stagger or a fall costs them the turn, and conditions counted in
//! turns count down. Conditions counted in time end when their time is up (`expire`), whether or
//! not anyone is fighting, so a blessing can't be cast and saved for later.

use bevy::prelude::*;

use crate::time::span;

/// A short-lived state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Loses their next turn.
    Staggered,
    /// Loses their next turn, and defends at half until then.
    Down,
    /// Loses this much health at the start of each turn.
    Bleeding(u32),
    /// Can't flee, and defends at half.
    Held,
    /// Defends this much better.
    Guarding(i32),
    /// Defends at half until their next turn.
    Exposed,
    /// Attacks this much better: a blessing.
    Blessed(i32),
    /// A ward that takes this much more damage in their place.
    Warded(u32),
}

impl State {
    /// What it's called, and what it does: ("blessed", "attacking 2 better").
    pub fn describe(self) -> (&'static str, String) {
        match self {
            State::Staggered => ("staggered", "losing your next turn".into()),
            State::Down => (
                "down",
                "losing your next turn, and defending at half".into(),
            ),
            State::Bleeding(n) => ("bleeding", format!("losing {n} health each turn")),
            State::Held => ("held", "unable to flee, and defending at half".into()),
            State::Guarding(n) => ("guarding", format!("defending {n} better")),
            State::Exposed => ("exposed", "defending at half until your next turn".into()),
            State::Blessed(n) => ("blessed", format!("attacking {n} better")),
            State::Warded(n) => ("warded", format!("a ward that will take {n} more damage")),
        }
    }

    /// Whether two states are the same kind, so a new one replaces the old.
    fn same_kind(self, other: State) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

/// How long a condition lasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lasts {
    /// This many more of the fighter's turns.
    Turns(u32),
    /// Until this time, in seconds since the server started.
    Until(f64),
}

/// A fighter's current conditions, each with how long it has left.
#[derive(Component, Clone, Debug, Default)]
pub struct Conditions(pub Vec<(State, Lasts)>);

/// What the start of a fighter's turn brings.
#[derive(Debug, Default, PartialEq)]
pub struct TurnStart {
    /// Health lost to bleeding.
    pub bleeding: u32,
    /// The state that costs them this turn, if any.
    pub lost_to: Option<State>,
}

impl Conditions {
    /// Add a state for `turns` of the fighter's turns, replacing one of the same kind.
    pub fn add(&mut self, state: State, turns: u32) {
        self.0.retain(|(s, _)| !s.same_kind(state));
        self.0.push((state, Lasts::Turns(turns.max(1))));
    }

    /// Add a state until `until` (seconds since the server started), replacing one of the same
    /// kind.
    pub fn add_until(&mut self, state: State, until: f64) {
        self.0.retain(|(s, _)| !s.same_kind(state));
        self.0.push((state, Lasts::Until(until)));
    }

    /// End the states whose time is up at `now`. Returns them.
    pub fn expire(&mut self, now: f64) -> Vec<State> {
        let (over, left) = self
            .0
            .drain(..)
            .partition(|(_, lasts)| matches!(lasts, Lasts::Until(until) if *until <= now));
        self.0 = left;
        over.into_iter().map(|(state, _)| state).collect()
    }

    pub fn has(&self, state: State) -> bool {
        self.0.iter().any(|(s, _)| s.same_kind(state))
    }

    /// Their defence once conditions are counted: guarding adds to it; being down, held or
    /// exposed halves it.
    pub fn defence(&self, base: i32) -> i32 {
        let guard: i32 = self
            .0
            .iter()
            .map(|(s, _)| match s {
                State::Guarding(n) => *n,
                _ => 0,
            })
            .sum();
        let halved = self
            .0
            .iter()
            .any(|(s, _)| matches!(s, State::Down | State::Held | State::Exposed));
        let defence = base + guard;
        if halved { defence / 2 } else { defence }
    }

    /// How much better they attack: a blessing's worth.
    pub fn blessing(&self) -> i32 {
        self.0
            .iter()
            .map(|(s, _)| match s {
                State::Blessed(n) => *n,
                _ => 0,
            })
            .sum()
    }

    /// Let a ward take what it can of `damage`. Returns how much it took; a ward that's taken all
    /// it can is gone.
    pub fn absorb(&mut self, damage: i32) -> i32 {
        let mut taken = 0;
        for (state, _) in &mut self.0 {
            if let State::Warded(left) = state {
                taken = damage.clamp(0, *left as i32);
                *left -= taken as u32;
            }
        }
        self.0.retain(|(s, _)| *s != State::Warded(0));
        taken
    }

    /// Each state, what it does and how long it has left at `now`, for `affects`: "blessed:
    /// attacking 2 better, for 1 hour 20 minutes (10 real minutes)".
    pub fn listed(&self, now: f64) -> Vec<String> {
        self.0
            .iter()
            .map(|(state, lasts)| {
                let (name, what) = state.describe();
                let left = match *lasts {
                    Lasts::Until(until) => format!("for {}", span((until - now).max(0.0))),
                    Lasts::Turns(1) => "for 1 more turn".into(),
                    Lasts::Turns(n) => format!("for {n} more turns"),
                };
                format!("{name}: {what}, {left}")
            })
            .collect()
    }

    /// Whether they can run from a fight.
    pub fn can_flee(&self) -> bool {
        !self.has(State::Held)
    }

    /// The start of their turn: bleeding hurts, a stagger or a fall costs the turn, and
    /// everything counts down.
    pub fn start_turn(&mut self) -> TurnStart {
        let mut start = TurnStart::default();
        for (state, lasts) in &mut self.0 {
            match state {
                State::Bleeding(damage) => start.bleeding += *damage,
                State::Staggered | State::Down if start.lost_to.is_none() => {
                    start.lost_to = Some(*state);
                }
                _ => {}
            }
            if let Lasts::Turns(turns) = lasts {
                *turns = turns.saturating_sub(1);
            }
        }
        // Exposure only lasts until the fighter acts again.
        self.0
            .retain(|(state, lasts)| *lasts != Lasts::Turns(0) && *state != State::Exposed);
        start
    }

    /// End a state early, whatever its turns or strength: bandaged bleeding stops.
    pub fn remove(&mut self, state: State) {
        self.0.retain(|(s, _)| !s.same_kind(state));
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_can_be_ended_early() {
        let mut conditions = Conditions::default();
        conditions.add(State::Bleeding(2), 3);
        conditions.add(State::Held, 2);
        // However hard and long the bleeding, a bandage stops it.
        conditions.remove(State::Bleeding(0));
        assert!(!conditions.has(State::Bleeding(0)));
        assert!(conditions.has(State::Held));
        assert_eq!(conditions.start_turn().bleeding, 0);
    }

    #[test]
    fn states_count_down_in_turns() {
        let mut conditions = Conditions::default();
        conditions.add(State::Bleeding(1), 2);
        conditions.add(State::Staggered, 1);
        conditions.add(State::Held, 2);
        assert!(!conditions.can_flee());
        assert_eq!(
            conditions.start_turn(),
            TurnStart {
                bleeding: 1,
                lost_to: Some(State::Staggered)
            }
        );
        assert_eq!(
            conditions.start_turn(),
            TurnStart {
                bleeding: 1,
                lost_to: None
            }
        );
        assert!(conditions.can_flee(), "the hold is over");
        assert_eq!(conditions.start_turn(), TurnStart::default());
        assert!(conditions.0.is_empty());
    }

    #[test]
    fn defence_follows_conditions() {
        let mut conditions = Conditions::default();
        assert_eq!(conditions.defence(6), 6);
        conditions.add(State::Guarding(4), 2);
        assert_eq!(conditions.defence(6), 10);
        conditions.add(State::Down, 1);
        assert_eq!(conditions.defence(6), 5);
        // A new guard replaces the old one.
        conditions.add(State::Guarding(2), 2);
        assert_eq!(conditions.defence(6), 4);
    }

    #[test]
    fn a_ward_takes_what_it_can_and_then_is_gone() {
        let mut conditions = Conditions::default();
        conditions.add(State::Warded(5), 3);
        assert_eq!(conditions.absorb(3), 3);
        assert_eq!(conditions.absorb(4), 2, "only 2 left in it");
        assert!(!conditions.has(State::Warded(0)));
        assert_eq!(conditions.absorb(4), 0);
    }

    #[test]
    fn timed_states_last_their_time_whatever_the_turns() {
        let mut conditions = Conditions::default();
        conditions.add_until(State::Blessed(2), 600.0);
        conditions.add(State::Held, 1);
        // Fighting doesn't wear a blessing down...
        for _ in 0..10 {
            conditions.start_turn();
        }
        assert_eq!(conditions.blessing(), 2);
        assert!(!conditions.has(State::Held));
        // ...time does, in a fight or out.
        assert!(conditions.expire(599.0).is_empty());
        assert_eq!(conditions.expire(600.0), vec![State::Blessed(2)]);
        assert_eq!(conditions.blessing(), 0);
    }

    #[test]
    fn exposure_ends_at_the_next_turn() {
        let mut conditions = Conditions::default();
        conditions.add(State::Exposed, 5);
        assert_eq!(conditions.defence(8), 4);
        conditions.start_turn();
        assert_eq!(conditions.defence(8), 8);
    }
}
