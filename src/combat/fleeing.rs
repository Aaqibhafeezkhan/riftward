//! Running by an effect (`Flee`): a startled deer bolting, a beaten wolf breaking off. In a fight
//! the runner tries `flee`, as a player would, with its stamina, its chance and `CantFlee`; out
//! of one they bolt with `go`, through a random way anyone could walk. Either way a mob keeps out
//! of rooms it may not wander into or that hold what it `avoids`, while it has anywhere else to
//! go ([`shuns`], which `flee` asks too).

use std::collections::HashSet;

use bevy::prelude::*;

use super::{Dead, Fighting};
use crate::command::{CommandRegistry, Invocation};
use crate::db::Role;
use crate::dice::Dice;
use crate::events::GameEvent;
use crate::mobs::{Mob, MobTemplates};
use crate::output::RoomQuery;
use crate::world::{Door, InRoom, MobKey};

/// Whether a mob of template `key` would rather not run into `room`: one it may not wander into
/// (`no_wander`), or one holding a mob of a kind it `avoids`, of `others` (each mob, and where it
/// is). Players avoid nothing.
pub(super) fn shuns<'a>(
    key: Option<&MobKey>,
    room: Entity,
    no_wander: bool,
    mut others: impl Iterator<Item = (&'a Mob, Entity)>,
    templates: &MobTemplates,
) -> bool {
    let Some((own, def)) = key.and_then(|k| Some((k, templates.0.get(k)?))) else {
        return false;
    };
    no_wander
        || others.any(|(other, there)| {
            there == room
                && templates
                    .0
                    .get(&other.key)
                    .is_some_and(|o| def.avoids_mob(own, &other.key, o))
        })
}

/// Those who might run: where they are, their template if they're a mob, and whether they're
/// fighting.
type Runners<'w, 's> =
    Query<'w, 's, (&'static InRoom, Option<&'static Mob>, Has<Fighting>), Without<Dead>>;

/// Make everyone an effect told to run since the last, run.
#[allow(clippy::too_many_arguments)]
pub(super) fn flee_from_effects(
    mut events: MessageReader<GameEvent>,
    mut commands: Commands,
    runners: Runners,
    everyone: Query<(&Mob, &InRoom)>,
    rooms: RoomQuery,
    doors: Query<&Door>,
    templates: Res<MobTemplates>,
    registry: Res<CommandRegistry>,
    mut dice: ResMut<Dice>,
) {
    let mut ran = HashSet::new();
    for event in events.read() {
        let GameEvent::Flees { who } = event else {
            continue;
        };
        let Ok((at, mob, fighting)) = runners.get(*who) else {
            continue;
        };
        if !ran.insert(*who) {
            continue;
        }
        let (verb, args) = if fighting {
            ("flee", String::new())
        } else {
            let Ok((_, _, exits)) = rooms.get(at.0) else {
                continue;
            };
            let shunned = |room: Entity| {
                shuns(
                    mob.map(|m| &m.key),
                    room,
                    rooms.get(room).is_ok_and(|(r, _, _)| r.no_wander),
                    everyone.iter().map(|(m, at)| (m, at.0)),
                    &templates,
                )
            };
            // No one bolts up a cliff, into deep water or through a closed door.
            let mut ways: Vec<(&String, Entity)> = exits
                .0
                .iter()
                .filter(|(_, exit)| {
                    exit.needs.is_none()
                        && exit
                            .door
                            .and_then(|d| doors.get(d).ok())
                            .is_none_or(Door::is_open)
                })
                .map(|(dir, exit)| (dir, exit.to))
                .collect();
            if ways.iter().any(|(_, to)| !shunned(*to)) {
                ways.retain(|(_, to)| !shunned(*to));
            }
            if ways.is_empty() {
                continue;
            }
            let pick = dice.range(0, 0..=(ways.len() as i64 - 1)) as usize;
            ("go", ways[pick].0.clone())
        };
        let Some((spec, id)) = registry.find(verb, Role::Player) else {
            continue;
        };
        commands.run_system_with(
            *id,
            Invocation {
                entity: *who,
                command: spec.name,
                usage: spec.usage,
                args,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Category, CommandSpec};
    use crate::world::{Exit, Exits, MobDef, MobKey, Room, RoomKey, Terrain};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashMap;

    /// The commands run, as `verb args`.
    #[derive(Resource, Default)]
    struct Ran(Vec<String>);

    #[test]
    fn prey_bolts_away_from_what_it_avoids_and_flees_a_fight() {
        let mut world = World::new();
        world.insert_resource(Dice::seeded(3));
        world.insert_resource(Ran::default());
        world.init_resource::<Messages<GameEvent>>();
        let recorder = |verb: &'static str| {
            move |In(inv): In<Invocation>, mut ran: ResMut<Ran>| {
                ran.0
                    .push(format!("{verb} {}", inv.args).trim().to_string());
            }
        };
        let go = world.register_system(recorder("go"));
        let flee = world.register_system(recorder("flee"));
        world.insert_resource(CommandRegistry::of(vec![
            (CommandSpec::new("go", Category::Movement), go),
            (CommandSpec::new("flee", Category::Interaction), flee),
        ]));
        let (deer_key, wolf_key) = (
            MobKey::parse("wood:deer", ""),
            MobKey::parse("wood:wolf", ""),
        );
        let mut deer_def = MobDef::named("a red deer");
        deer_def.avoids = vec!["wolf".into()];
        world.insert_resource(MobTemplates(HashMap::from([
            (deer_key.clone(), deer_def),
            (wolf_key.clone(), MobDef::named("a grey wolf")),
        ])));
        let room = |world: &mut World, id: &str| {
            world
                .spawn((
                    Room {
                        name: id.into(),
                        description: String::new(),
                        no_wander: false,
                        indoors: false,
                        terrain: Terrain::Uneven,
                    },
                    RoomKey::parse(id, ""),
                    Exits::default(),
                ))
                .id()
        };
        let (glade, east, west) = (
            room(&mut world, "wood:glade"),
            room(&mut world, "wood:east"),
            room(&mut world, "wood:west"),
        );
        let exit = |to| Exit {
            to,
            door: None,
            needs: None,
        };
        world.entity_mut(glade).insert(Exits(
            [
                ("east".to_string(), exit(east)),
                ("west".to_string(), exit(west)),
            ]
            .into(),
        ));
        let mob = |world: &mut World, key: &MobKey, at: Entity| {
            world
                .spawn((
                    Mob {
                        key: key.clone(),
                        name: key.mob.clone(),
                        here: String::new(),
                        description: String::new(),
                        race: None,
                    },
                    InRoom(at),
                ))
                .id()
        };
        mob(&mut world, &wolf_key, west);
        let deer = mob(&mut world, &deer_key, glade);
        let run = |world: &mut World| {
            world.write_message(GameEvent::Flees { who: deer });
            world.run_system_once(flee_from_effects).unwrap();
            world.flush();
            world.resource_mut::<Ran>().0.pop()
        };
        for _ in 0..5 {
            assert_eq!(
                run(&mut world).as_deref(),
                Some("go east"),
                "never toward the wolf"
            );
        }
        world.entity_mut(deer).insert(Fighting(deer));
        assert_eq!(
            run(&mut world).as_deref(),
            Some("flee"),
            "in a fight, it tries to break away"
        );
    }
}
