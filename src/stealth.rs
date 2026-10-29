//! Stealth: hiding, sneaking, and noticing those who do.
//!
//! `hide` (Stealth) makes someone hidden: unseen, untargetable and unannounced to everyone who
//! didn't notice them. Hidden is a condition (`hidden`, in `assets/conditions/`): its strength
//! is how well they hid, and its `seen_by` everyone who noticed them anyway. Noticing is an opposed roll, the observer's Intuition against the hider's
//! Stealth plus Agility, made when they hide, when anyone walks in, and when someone `search`es
//! (with double their Intuition). Mobs see by the same rules, so an aggressive rat ignores a
//! player it hasn't noticed. Acting openly or walking away shows a hidden character again (see
//! `reveal`), unless they're sneaking: `sneak` keeps them hidden as they move, rolling afresh in
//! every room.
//!
//! A mob born hidden (a spider in its web) springs from hiding at whoever comes near unaware, a
//! blow that can't be avoided (`combat`), and once its fight is over and some time has passed
//! (`rehide_seconds`, `rules.ron`), it hides again, as well as it was born.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;

use crate::attributes::{Attribute, Attributes};
use crate::combat::{ConditionDefs, Conditions, HIDDEN, Lasts, the};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::Role;
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::mobs::{Mob, MobTemplates};
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::skills::Skills;
use crate::targeting::Keywords;
use crate::world::InRoom;

/// The skill that hides and sneaks.
pub const STEALTH: &str = "stealth:basics";

pub struct StealthPlugin;

impl Plugin for StealthPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, notice_newcomers)
            .add_systems(Update, rehide.run_if(crate::time::heartbeat()))
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

/// Moving quietly: they stay hidden as they go.
#[derive(Component, Debug)]
pub struct Sneaking;

/// Whether `viewer` can see `target`: not in the dark themselves, and the target not hidden, or
/// hidden but noticed by them.
pub fn sees(hidden: &Query<&Conditions>, viewer: Entity, target: Entity) -> bool {
    viewer == target
        || (!hidden
            .get(viewer)
            .is_ok_and(|c| c.has(crate::light::IN_THE_DARK))
            && hidden.get(target).ok().is_none_or(|c| c.seen_by(viewer)))
}

/// Whether `viewer` can see someone with these `conditions`, for those holding them already.
pub fn can_see(viewer: Entity, target: Entity, conditions: &Conditions) -> bool {
    viewer == target || conditions.seen_by(viewer)
}

/// Those of `candidates` that `viewer` can see, for choosing a target.
pub fn visible<'a>(
    viewer: Entity,
    candidates: Vec<(Entity, &'a Keywords)>,
    hidden: &Query<&Conditions>,
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

/// Whether an observer with `intuition` and `luck` notices someone who hid this well
/// (`stealth`, the `hidden` condition's strength) with `their_luck`, by opposed roll.
pub fn notices(dice: &mut Dice, intuition: f64, luck: i32, stealth: i32, their_luck: i32) -> bool {
    dice.opposed(
        intuition.max(1.0),
        luck,
        f64::from(stealth.max(1)),
        their_luck,
    )
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
    unhide(commands, who);
    events.write(GameEvent::Revealed {
        who,
        name: name.to_string(),
        room,
    });
}

/// Take `hidden` off someone, quietly: walking away, a fight.
pub fn unhide(commands: &mut Commands, who: Entity) {
    commands.entity(who).queue(|mut e: EntityWorldMut| {
        if let Some(mut conditions) = e.get_mut::<Conditions>() {
            conditions.remove(HIDDEN);
        }
    });
}

/// Mobs alive and out of a fight: those that might hide again.
type Idle<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Mob, &'static Conditions, &'static InRoom),
    (
        Without<crate::combat::Fighting>,
        Without<crate::combat::Dead>,
    ),
>;

/// Mobs born hidden that have been in sight, out of a fight, for `rehide_seconds` hide again, as
/// well as they were born, unseen by all: the spider back in its web.
pub(crate) fn rehide(
    mut commands: Commands,
    time: Res<Time>,
    mut since: Local<HashMap<Entity, f64>>,
    mobs: Idle,
    templates: Res<MobTemplates>,
    mut outbox: ResMut<Outbox>,
) {
    let now = time.elapsed_secs_f64();
    let wait = crate::rules::rules().rehide_seconds;
    let mut seen = HashSet::new();
    for (entity, mob, conditions, at) in &mobs {
        let Some(born) = templates
            .0
            .get(&mob.key)
            .and_then(|def| def.conditions.iter().find(|c| c.condition == HIDDEN))
        else {
            continue;
        };
        if conditions.hidden().is_some() {
            continue;
        }
        seen.insert(entity);
        let out_since = *since.entry(entity).or_insert(now);
        if now - out_since >= wait {
            hide_as(&mut commands, entity, born.strength, HashSet::new());
            outbox.0.push(Outgoing::Room {
                room: at.0,
                except: entity,
                text: format!(
                    "{} slips back out of sight.\r\n",
                    crate::items::capitalized(&the(&mob.name))
                ),
            });
            seen.remove(&entity);
        }
    }
    since.retain(|entity, _| seen.contains(entity));
}

/// Hide someone, as well as `stealth`, seen through by `seen_by` already: it replaces how they
/// were hidden before, as when a sneak rolls afresh in a new room.
pub fn hide_as(commands: &mut Commands, who: Entity, stealth: i32, seen_by: HashSet<Entity>) {
    commands.entity(who).queue(move |mut e: EntityWorldMut| {
        let id = e.id();
        e.world_scope(|world| {
            world.resource_scope(|world, defs: Mut<ConditionDefs>| {
                if let Some(mut conditions) = world.get_mut::<Conditions>(id)
                    && conditions.apply(&defs, HIDDEN, stealth, Lasts::Indefinitely)
                    && let Some(hidden) = conditions.hidden_mut()
                {
                    hidden.seen_by = seen_by;
                }
            });
        });
    });
}

/// Everyone who might be hiding, where they are, and their Luck.
type Hiders<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static InRoom,
        &'static mut Conditions,
        &'static Attributes,
    ),
>;

/// `hide`: Stealth, by its old name. It's an active skill made of effects
/// (`stealth.skill.ron`) that puts `hidden` on its user, so this is `use stealth`; how hiding
/// is noticed as it happens is in `combat/abilities.rs`.
fn hide(In(inv): In<Invocation>, mut commands: Commands, registry: Res<CommandRegistry>) {
    let Some((spec, id)) = registry.find("ability", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: STEALTH.to_string(),
        },
    );
}

/// `sneak`: start or stop moving quietly.
fn sneak(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut outbox: ResMut<Outbox>,
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
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// A name for someone, as others say it.
fn name_of(players: &Characters, mobs: &Query<&Mob>, who: Entity) -> String {
    match (Actor::get(players, who), mobs.get(who)) {
        (Some(player), _) => player.name,
        (_, Ok(mob)) => the(&mob.name),
        _ => "someone".into(),
    }
}

/// `search`: look hard for anyone hiding here, with double your Intuition.
fn search(
    In(inv): In<Invocation>,
    mut both: ParamSet<(Characters, Hiders)>,
    mut outbox: ResMut<Outbox>,
    sheets: Query<&Attributes>,
    mobs: Query<&Mob>,
    mut dice: ResMut<Dice>,
    mut events: MessageWriter<GameEvent>,
) {
    let Some(actor) = Actor::get(&both.p0(), inv.entity) else {
        return;
    };
    events.write(GameEvent::Searched {
        who: actor.entity,
        room: actor.room,
    });
    let (intuition, luck) = sheets.get(actor.entity).map_or((1.0, 0), |a| {
        (
            f64::from(a.get(Attribute::Intuition)) * crate::rules::rules().search_bonus,
            a.get(Attribute::Luck),
        )
    });
    let mut hiders = both.p1();
    let mut found = Vec::new();
    for (hider, at, mut conditions, theirs) in &mut hiders {
        let their_luck = theirs.get(Attribute::Luck);
        let Some(hidden) = conditions.hidden_mut() else {
            continue;
        };
        if at.0 != actor.room || hider == actor.entity || hidden.seen_by.contains(&actor.entity) {
            continue;
        }
        if notices(&mut dice, intuition, luck, hidden.strength, their_luck) {
            hidden.seen_by.insert(actor.entity);
            found.push(hider);
        }
    }
    let players = both.p0();
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
    // A search is plain to see; a hidden searcher's, only to those who've noticed them.
    out.push(Outgoing::Room {
        room: actor.room,
        except: actor.entity,
        text: format!("{} searches the shadows.\r\n", actor.name),
    });
    deliver(out, &mut outbox);
}

/// Anyone who walks into a room gets a chance to notice whoever is hiding there.
fn notice_newcomers(
    mut events: MessageReader<GameEvent>,
    mut both: ParamSet<(Characters, Hiders)>,
    mut outbox: ResMut<Outbox>,
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
        for (hider, at, mut conditions, theirs) in &mut hiders {
            let their_luck = theirs.get(Attribute::Luck);
            let Some(hidden) = conditions.hidden_mut() else {
                continue;
            };
            if at.0 != *room || hider == *who || hidden.seen_by.contains(who) {
                continue;
            }
            if notices(&mut dice, intuition, luck, hidden.strength, their_luck) {
                hidden.seen_by.insert(*who);
                noticed.push((*who, hider));
            }
        }
    }
    let players = both.p0();
    let out = noticed
        .into_iter()
        .map(|(who, hider)| {
            let name = name_of(&players, &mobs, hider);
            Outgoing::line(who, format!("You notice {name} hiding in the shadows."))
        })
        .collect();
    deliver(out, &mut outbox);
}
