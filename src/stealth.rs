//! Stealth: hiding, sneaking, and noticing those who do.
//!
//! `hide` (Stealth) makes someone hidden: unseen, untargetable and unannounced to everyone who
//! didn't notice them. Noticing is an opposed roll, the observer's Intuition against the hider's
//! Stealth plus Agility, made when they hide, when anyone walks in, and when someone `search`es
//! (with double their Intuition). Mobs see by the same rules, so an aggressive rat ignores a
//! player it hasn't noticed. Acting openly or walking away shows a hidden character again (see
//! `reveal`), unless they're sneaking: `sneak` keeps them hidden as they move, rolling afresh in
//! every room.

use std::collections::HashSet;

use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::combat::{Dead, Fighting, the};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::mobs::Mob;
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};
use crate::skills::{Learner, Practice, Skills, fill};
use crate::targeting::Keywords;
use crate::world::InRoom;

/// The skill that hides and sneaks.
pub const STEALTH: &str = "stealth:basics";
/// How much better a deliberate search is than a passing glance: Intuition times this.
const SEARCH_BONUS: f64 = 2.0;

pub struct StealthPlugin;

impl Plugin for StealthPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, notice_newcomers)
            .add_command(
                CommandSpec::new("hide", Category::Interaction)
                    .summary("slip out of sight, if you know Stealth")
                    .quiet()
                    .action(),
                hide,
            )
            .add_command(
                CommandSpec::new("sneak", Category::Interaction)
                    .summary("move quietly, staying hidden as you go, if you know Stealth")
                    .quiet(),
                sneak,
            )
            .add_command(
                CommandSpec::new("search", Category::Interaction)
                    .summary("search the shadows for anyone hiding here")
                    .quiet()
                    .action(),
                search,
            );
    }
}

/// Out of sight: `stealth` is how well they hid (Stealth plus Agility, with their `luck`), and
/// `seen_by` everyone who noticed them anyway.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Hidden {
    pub stealth: f64,
    pub luck: i32,
    pub seen_by: HashSet<Entity>,
}

/// Moving quietly: they stay hidden as they go.
#[derive(Component, Debug)]
pub struct Sneaking;

/// Whether `viewer` can see `target`: anyone not hidden, or hidden but noticed by them.
pub fn sees(hidden: &Query<&Hidden>, viewer: Entity, target: Entity) -> bool {
    viewer == target
        || hidden
            .get(target)
            .ok()
            .is_none_or(|h| h.seen_by.contains(&viewer))
}

/// Those of `candidates` that `viewer` can see, for choosing a target.
pub fn visible<'a>(
    viewer: Entity,
    candidates: Vec<(Entity, &'a Keywords)>,
    hidden: &Query<&Hidden>,
) -> Vec<(Entity, &'a Keywords)> {
    candidates
        .into_iter()
        .filter(|(target, _)| sees(hidden, viewer, *target))
        .collect()
}

/// How well someone hides: Stealth plus Agility.
pub fn stealth_rating(skills: &Skills, attributes: &Attributes) -> f64 {
    let level = skills.0.get(STEALTH).map_or(0, |s| s.level as i32);
    f64::from((level + attributes.get(Attribute::Agility)).max(1))
}

/// Whether an observer with `intuition` and `luck` notices someone hidden, by opposed roll.
pub fn notices(dice: &mut Dice, intuition: f64, luck: i32, hidden: &Hidden) -> bool {
    dice.opposed(intuition.max(1.0), luck, hidden.stealth, hidden.luck)
        .won
}

/// Show someone hidden again: they're no longer hidden, and the room is told.
pub fn reveal(
    commands: &mut Commands,
    events: &mut MessageWriter<GameEvent>,
    who: Entity,
    name: &str,
    room: Entity,
) {
    commands.entity(who).remove::<Hidden>();
    events.write(GameEvent::Revealed {
        who,
        name: name.to_string(),
        room,
    });
}

/// Everyone hiding, where they are.
type Hiders<'w, 's> = Query<'w, 's, (Entity, &'static InRoom, &'static mut Hidden)>;

/// Everyone else in `room`: players and mobs, living.
type Others<'w, 's> = Query<'w, 's, (Entity, &'static InRoom), (With<Attributes>, Without<Dead>)>;

fn others_in(others: &Others, room: Entity, but: Entity) -> Vec<Entity> {
    others
        .iter()
        .filter(|(e, r)| r.0 == room && *e != but)
        .map(|(e, _)| e)
        .collect()
}

/// `hide`: slip out of sight. Everyone here rolls to notice; those who do still see you.
fn hide(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut seen: ParamSet<(PlayerQuery, Others)>,
    mut practice: Practice,
    fighting: Query<(), With<Fighting>>,
) {
    let Some(actor) = Actor::get(&seen.p0(), inv.entity) else {
        return;
    };
    let watchers = others_in(&seen.p1(), actor.room, actor.entity);
    let mut out = Vec::new();
    let reply = (|| {
        if fighting.get(actor.entity).is_ok() {
            return Err("You can't hide in the middle of a fight!".to_string());
        }
        let ready = practice.begin(actor.entity, STEALTH)?;
        let mut hidden = Hidden {
            stealth: f64::from(ready.effectiveness.max(1)),
            luck: ready.luck,
            seen_by: HashSet::new(),
        };
        for &watcher in &watchers {
            let Some((_, _, attributes)) = practice.sheet(watcher) else {
                continue;
            };
            let (intuition, luck) = (
                f64::from(attributes.get(Attribute::Intuition)),
                attributes.get(Attribute::Luck),
            );
            let hider = hidden.clone();
            if practice
                .opposed(intuition.max(1.0), luck, hider.stealth, hider.luck)
                .won
            {
                hidden.seen_by.insert(watcher);
            }
        }
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
        };
        let gains = practice.exercise(actor.entity, learner, STEALTH);
        let text = practice
            .defs
            .skills
            .get(STEALTH)
            .and_then(|skill| skill.def.text.clone())
            .ok_or_else(String::new)?;
        // Seen by everyone here: that's no hiding at all.
        let all_saw = !watchers.is_empty() && hidden.seen_by.len() == watchers.len();
        let lines = match (all_saw, &text.miss) {
            (true, Some(miss)) => miss,
            _ => &text.hit,
        };
        let with = [("actor", actor.name.as_str())];
        let seen = fill(&lines.room, &with);
        for &watcher in &hidden.seen_by {
            out.push(Outgoing::line(watcher, seen.clone()));
        }
        if !all_saw {
            commands.entity(actor.entity).insert(hidden);
        }
        let mut told = fill(&lines.actor, &with);
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
    deliver(out, &mut seen.p0());
}

/// `sneak`: start or stop moving quietly.
fn sneak(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    sheets: Query<&Skills>,
    sneaking: Query<(), With<Sneaking>>,
) {
    let text = if sneaking.get(inv.entity).is_ok() {
        commands.entity(inv.entity).remove::<Sneaking>();
        "You stop sneaking, and walk openly again."
    } else if sheets
        .get(inv.entity)
        .is_ok_and(|skills| skills.0.contains_key(STEALTH))
    {
        commands.entity(inv.entity).insert(Sneaking);
        "You start moving quietly: you'll stay hidden as you go."
    } else {
        "You don't know how to do that (Stealth)."
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// A name for someone, as others say it.
fn name_of(players: &PlayerQuery, mobs: &Query<&Mob>, who: Entity) -> String {
    match (Actor::get(players, who), mobs.get(who)) {
        (Some(player), _) => player.name,
        (_, Ok(mob)) => the(&mob.name),
        _ => "someone".into(),
    }
}

/// `search`: look hard for anyone hiding here, with double your Intuition.
fn search(
    In(inv): In<Invocation>,
    mut both: ParamSet<(PlayerQuery, Hiders)>,
    sheets: Query<&Attributes>,
    mobs: Query<&Mob>,
    mut dice: ResMut<Dice>,
) {
    let Some(actor) = Actor::get(&both.p0(), inv.entity) else {
        return;
    };
    let (intuition, luck) = sheets.get(actor.entity).map_or((1.0, 0), |a| {
        (
            f64::from(a.get(Attribute::Intuition)) * SEARCH_BONUS,
            a.get(Attribute::Luck),
        )
    });
    let mut hiders = both.p1();
    let mut found = Vec::new();
    for (hider, at, mut hidden) in &mut hiders {
        if at.0 != actor.room || hider == actor.entity || hidden.seen_by.contains(&actor.entity) {
            continue;
        }
        if notices(&mut dice, intuition, luck, &hidden) {
            hidden.seen_by.insert(actor.entity);
            found.push(hider);
        }
    }
    let searcher_hidden = hiders.get(actor.entity).is_ok();
    let mut players = both.p0();
    let mut out = Vec::new();
    let told = if found.is_empty() {
        "You search the shadows, but find no one.".to_string()
    } else {
        found
            .iter()
            .map(|who| {
                let name = name_of(&players, &mobs, *who);
                format!("You spot {name} hiding in the shadows!")
            })
            .collect::<Vec<_>>()
            .join("\r\n")
    };
    out.push(Outgoing::line(actor.entity, told));
    for hider in found {
        out.push(Outgoing::line(hider, format!("{} spots you!", actor.name)));
    }
    // A search is plain to see, unless the searcher is hidden themselves.
    if !searcher_hidden {
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: format!("{} searches the shadows.\r\n", actor.name),
        });
    }
    deliver(out, &mut players);
}

/// Anyone who walks into a room gets a chance to notice whoever is hiding there.
fn notice_newcomers(
    mut events: MessageReader<GameEvent>,
    mut both: ParamSet<(PlayerQuery, Hiders)>,
    sheets: Query<&Attributes>,
    mobs: Query<&Mob>,
    mut dice: ResMut<Dice>,
) {
    let mut noticed = Vec::new();
    let mut hiders = both.p1();
    for event in events.read() {
        let GameEvent::Entered { who, room, .. } = event else {
            continue;
        };
        let Ok(attributes) = sheets.get(*who) else {
            continue;
        };
        let (intuition, luck) = (
            f64::from(attributes.get(Attribute::Intuition)),
            attributes.get(Attribute::Luck),
        );
        for (hider, at, mut hidden) in &mut hiders {
            if at.0 != *room || hider == *who || hidden.seen_by.contains(who) {
                continue;
            }
            if notices(&mut dice, intuition, luck, &hidden) {
                hidden.seen_by.insert(*who);
                noticed.push((*who, hider));
            }
        }
    }
    let mut players = both.p0();
    let out = noticed
        .into_iter()
        .map(|(who, hider)| {
            let name = name_of(&players, &mobs, hider);
            Outgoing::line(who, format!("You notice {name} hiding in the shadows."))
        })
        .collect();
    deliver(out, &mut players);
}
