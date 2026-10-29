//! Reactions away from the blows of a fight: conditions and passive skills answering what their
//! bearer does, or has done to them, anywhere. Coming into a room, someone arriving, resting,
//! eating and drinking, a condition taking hold or running out, and every so often in game time.
//! Those in a fight are set off where they happen: as a blow lands (`resolve.rs`), as a turn
//! begins and as a spell is cast.
//!
//! Systems that make something happen write a `GameEvent`; `triggers` turns the events into the
//! reactions they set off, and `react` resolves them as a round of fighting would, out of a fight
//! or in one. A room's own conditions (`RoomConditions`) react too, to whoever comes in and,
//! every so often, to everyone there: the brambles, a web, a rift's hum.

use std::collections::{HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use super::Dead;
use super::fell;
use super::resolve::reactions_of;
use super::states::{ConditionDefs, Conditions, Imbued, ItemConditions};
use super::strike::{Fighters, Round};
use crate::db::Database;
use crate::dice::Dice;
use crate::effects::When;
use crate::events::GameEvent;
use crate::items::{ItemQuery, ItemTemplates};
use crate::mobs::MobTemplates;
use crate::output::Outbox;
use crate::skills::SkillDefs;
use crate::spells::SpellDefs;
use crate::vitals::Resting;
use crate::world::{InRoom, RoomConditions};

/// How often the heartbeat looks for reactions that come round every so often, in real seconds.
const EVERY_CHECK_SECS: f64 = 1.0;

/// A reaction waiting to be set off: `bearer`'s, at `other`, or for a condition's own (taking
/// hold, running out), that condition's at the strength it had, or a room's conditions' on
/// `bearer`, who is in it.
#[derive(Message, Clone, Debug)]
pub struct Trigger {
    pub bearer: Entity,
    pub other: Entity,
    pub when: When,
    pub condition: Option<(String, i32)>,
    pub room: Option<Entity>,
}

impl Trigger {
    fn new(bearer: Entity, other: Entity, when: When) -> Self {
        Self {
            bearer,
            other,
            when,
            condition: None,
            room: None,
        }
    }
}

/// The living characters, and where they are.
type Present<'w, 's> = Query<'w, 's, (Entity, &'static InRoom), (With<Conditions>, Without<Dead>)>;

/// What happened, as the reactions it sets off.
pub(super) fn triggers(
    mut events: MessageReader<GameEvent>,
    mut triggers: MessageWriter<Trigger>,
    present: Present,
    rooms: Query<(), With<RoomConditions>>,
) {
    for event in events.read() {
        match event {
            GameEvent::Entered {
                who,
                room,
                witnesses,
                ..
            } => {
                triggers.write(Trigger::new(*who, *who, When::Enters));
                // And the place itself, if it has conditions of its own.
                if rooms.contains(*room) {
                    triggers.write(Trigger {
                        room: Some(*room),
                        ..Trigger::new(*who, *who, When::Enters)
                    });
                }
                // Those already there who see them come: not someone sneaking past unnoticed.
                for (bearer, at) in &present {
                    let saw = witnesses.as_ref().is_none_or(|w| w.contains(&bearer));
                    if at.0 == *room && bearer != *who && saw {
                        triggers.write(Trigger::new(bearer, *who, When::Arrives));
                    }
                }
            }
            GameEvent::Rested { who } => {
                triggers.write(Trigger::new(*who, *who, When::Rests));
            }
            GameEvent::Consumed { who, drink } => {
                let when = if *drink { When::Drinks } else { When::Eats };
                triggers.write(Trigger::new(*who, *who, when));
            }
            // Put on by a reaction, it sets off no more.
            GameEvent::ConditionApplied {
                who,
                by,
                condition,
                strength,
                reacting: false,
            } => {
                triggers.write(Trigger {
                    condition: Some((condition.clone(), *strength)),
                    ..Trigger::new(*who, *by, When::Applied)
                });
            }
            GameEvent::ConditionFaded {
                who,
                condition,
                strength,
            } => {
                triggers.write(Trigger {
                    condition: Some((condition.clone(), *strength)),
                    ..Trigger::new(*who, *who, When::Fades)
                });
            }
            _ => {}
        }
    }
}

/// What resolving reactions needs: what a round of fighting does.
#[derive(SystemParam)]
pub(super) struct Reacting<'w, 's> {
    commands: Commands<'w, 's>,
    fighters: Fighters<'w, 's>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    mobs: Res<'w, MobTemplates>,
    defs: Res<'w, SkillDefs>,
    spells: Res<'w, SpellDefs>,
    conditions: Res<'w, ConditionDefs>,
    dice: ResMut<'w, Dice>,
    db: Res<'w, Database>,
    story: Res<'w, crate::facts::Facts>,
    events: MessageWriter<'w, GameEvent>,
    log: ResMut<'w, Outbox>,
    resting: Query<'w, 's, Entity, With<Resting>>,
    time: Res<'w, Time>,
    item_conditions: Query<'w, 's, (Entity, &'static mut ItemConditions)>,
    rooms: Query<'w, 's, (Entity, &'static RoomConditions)>,
}

/// Set off the reactions waiting, and on the heartbeat those that come round every so often.
/// Whoever a reaction kills falls to its bearer.
pub(super) fn react(
    mut triggers: MessageReader<Trigger>,
    mut checked: Local<Option<f64>>,
    mut p: Reacting,
) {
    let now = p.time.elapsed_secs_f64();
    let mut pending: Vec<Trigger> = triggers.read().cloned().collect();
    let last = *checked.get_or_insert(now);
    if now - last >= EVERY_CHECK_SECS {
        *checked = Some(now);
        pending.extend(every(&p.fighters, &p.conditions, &p.defs, last, now));
        pending.extend(every_in_rooms(
            &p.fighters,
            &p.rooms,
            &p.conditions,
            last,
            now,
        ));
    }
    if pending.is_empty() {
        return;
    }

    let watchers: Vec<(Entity, Entity)> = p
        .fighters
        .iter()
        .filter(|f| f.player.is_some())
        .map(|f| (f.entity, f.in_room.0))
        .collect();
    let targets: HashMap<Entity, Entity> = p
        .fighters
        .iter()
        .filter_map(|f| Some((f.entity, f.fighting?.0)))
        .collect();
    let resting: HashSet<Entity> = p.resting.iter().collect();
    let none = HashMap::new();
    let mut imbued = Imbued::gather(p.item_conditions.iter());
    let mut round = Round {
        fighters: &mut p.fighters,
        items: &p.items,
        templates: &p.templates,
        mobs: &p.mobs,
        defs: &p.defs,
        spells: &p.spells,
        conditions: &p.conditions,
        dice: &mut p.dice,
        db: &p.db,
        story: Some(&p.story),
        events: &mut p.events,
        log: &mut p.log,
        watchers: &watchers,
        engaged: &targets,
        targets: &targets,
        auras: &HashMap::new(),
        resting: &resting,
        outmatched: &none,
        ambushers: &HashSet::new(),
        now,
        imbued: &mut imbued,
    };
    let mut fallen = Vec::new();
    for t in pending {
        let died = match (&t.condition, t.room) {
            (_, Some(room)) => match p.rooms.get(room) {
                Ok((_, own)) => round.react_to_room(t.bearer, t.when, &own.0),
                Err(_) => Vec::new(),
            },
            (Some((key, strength)), None) => {
                round.react_as(t.bearer, t.other, (key, *strength), t.when)
            }
            (None, None) => round.react(t.bearer, t.other, t.when),
        };
        fallen.extend(died.into_iter().map(|died| (died, t.bearer)));
    }
    imbued.write_back(&mut p.item_conditions);
    for (died, slayer) in fallen {
        fell(&mut p.commands, died, slayer, &targets);
    }
}

/// Reactions that come round every so often (`Every(Minutes(10))`) and have since `last`: each
/// comes at the same game times for everyone, every so many minutes from the server's start.
fn every(
    fighters: &Fighters,
    conditions: &ConditionDefs,
    defs: &SkillDefs,
    last: f64,
    now: f64,
) -> Vec<Trigger> {
    let mut due = Vec::new();
    for f in fighters.iter().filter(|f| !f.dead) {
        let mut periods = Vec::new();
        for (reaction, ..) in reactions_of(f.conditions, f.skills, conditions, defs) {
            if let When::Every(period) = reaction.on
                && !periods.contains(&period)
            {
                let secs = period.real_secs();
                if (now / secs).floor() > (last / secs).floor() {
                    periods.push(period);
                }
            }
        }
        due.extend(
            periods
                .into_iter()
                .map(|period| Trigger::new(f.entity, f.entity, When::Every(period))),
        );
    }
    due
}

/// A room's conditions' reactions that come round every so often and have since `last`: on
/// everyone living in it, at the same game times as everyone's own.
fn every_in_rooms(
    fighters: &Fighters,
    rooms: &Query<(Entity, &RoomConditions)>,
    conditions: &ConditionDefs,
    last: f64,
    now: f64,
) -> Vec<Trigger> {
    let mut due = Vec::new();
    for (room, own) in rooms {
        let mut periods = Vec::new();
        for (key, _) in &own.0 {
            for reaction in conditions.get(key).map_or(&[][..], |d| &d.reactions) {
                if let When::Every(period) = reaction.on
                    && !periods.contains(&period)
                {
                    let secs = period.real_secs();
                    if (now / secs).floor() > (last / secs).floor() {
                        periods.push(period);
                    }
                }
            }
        }
        for period in periods {
            due.extend(
                fighters
                    .iter()
                    .filter(|f| !f.dead && f.in_room.0 == room)
                    .map(|f| Trigger {
                        room: Some(room),
                        ..Trigger::new(f.entity, f.entity, When::Every(period))
                    }),
            );
        }
    }
    due
}
