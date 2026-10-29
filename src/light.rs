//! Light and darkness. A room is lit by day if it's outdoors (`Daylight`, the default), always
//! (`Lit`, the default indoors), or never (`Dark`: caves, the deep wood). One that needs light and
//! has none is dark: outdoors at night, or a dark room at any hour, unless something tagged
//! `light` is there, lying in the room (a lamppost, a campfire, a dropped torch) or carried by
//! anyone in it (a torch, a lantern).
//!
//! Anyone in a dark room who can't see in the dark (a `SeesInDark` trait or condition: a dwarf's
//! Heat Sight, a wolf's eyes) is `in_the_dark`: they see no one, and fight at a disadvantage, as
//! the condition says. `look` shows them only the exits. Mobs are blinded the same way, so a
//! rabbit can't pick a fight it can't see.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::combat::{ConditionDefs, Conditions, Lasts};
use crate::items::{ItemLocation, ItemQuery, ItemTemplates};
use crate::time::{GameTime, Phase};
use crate::world::{InRoom, Room};

/// The condition on someone who can't see for the dark.
pub const IN_THE_DARK: &str = "in_the_dark";

/// The tag that makes an item give light.
pub const LIGHT: &str = "light";

/// How a room is lit.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Light {
    /// Lit by the sun: dark at night. The default outdoors.
    Daylight,
    /// Always lit: lamps no one needs to see. The default indoors.
    Lit,
    /// Never lit, but by what's brought in: caves, the deep wood.
    Dark,
}

/// A room's own light, when it isn't its default (`Daylight` outdoors, `Lit` indoors).
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoomLight(pub Light);

impl Light {
    /// A room's light: its own, or its default.
    pub fn of(room: &Room, own: Option<&RoomLight>) -> Light {
        match own {
            Some(RoomLight(light)) => *light,
            None if room.indoors => Light::Lit,
            None => Light::Daylight,
        }
    }

    /// Whether a room lit so needs a light source in this part of the day.
    pub fn needs_light(self, phase: Phase) -> bool {
        match self {
            Light::Lit => false,
            Light::Daylight => phase == Phase::Night,
            Light::Dark => true,
        }
    }
}

/// The rooms something lights right now: a light lying there, or carried by someone there.
#[derive(Resource, Default, Debug)]
pub struct LitRooms(pub HashSet<Entity>);

pub struct LightPlugin;

impl Plugin for LightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LitRooms>()
            .add_systems(PreUpdate, (light_rooms, darken).chain());
    }
}

/// Where every light is: lying in a room, or carried (at any depth) by someone in one.
fn light_rooms(
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    places: Query<&InRoom>,
    mut lit: ResMut<LitRooms>,
) {
    lit.0.clear();
    for (entity, item, _) in &items {
        let gives_light = templates
            .0
            .get(&item.key)
            .is_some_and(|def| def.tags.iter().any(|t| t == LIGHT));
        if !gives_light {
            continue;
        }
        if let Some(room) = room_of(entity, &items, &places) {
            lit.0.insert(room);
        }
    }
}

/// The room an item is in: where it lies, or where whoever has it stands.
fn room_of(mut entity: Entity, items: &ItemQuery, places: &Query<&InRoom>) -> Option<Entity> {
    for _ in 0..32 {
        match *items.get(entity).ok()?.2 {
            ItemLocation::Room(room) => return Some(room),
            ItemLocation::Inside(container) => entity = container,
            location => return places.get(location.holder()?).ok().map(|r| r.0),
        }
    }
    None
}

/// Whether `who` holds or carries something that gives light.
pub fn carries_light(who: Entity, items: &ItemQuery, templates: &ItemTemplates) -> bool {
    crate::items::carried(who, items, templates)
        .iter()
        .any(|(_, tags)| tags.iter().any(|t| t == LIGHT))
}

/// Put `in_the_dark` on everyone in a dark room who can't see in it, and take it off everyone
/// else.
fn darken(
    mut people: Query<(&InRoom, &mut Conditions)>,
    rooms: Query<(&Room, Option<&RoomLight>)>,
    lit: Res<LitRooms>,
    defs: Res<ConditionDefs>,
) {
    let phase = GameTime::now().phase();
    for (in_room, mut conditions) in &mut people {
        let dark = rooms.get(in_room.0).is_ok_and(|(room, own)| {
            Light::of(room, own).needs_light(phase) && !lit.0.contains(&in_room.0)
        }) && !conditions.sees_in_dark(&defs);
        match (dark, conditions.has(IN_THE_DARK)) {
            (true, false) => {
                conditions.apply(&defs, IN_THE_DARK, 1, Lasts::Indefinitely);
            }
            (false, true) => {
                conditions.remove(IN_THE_DARK);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::{Item, Quality};
    use crate::world::{DoorState, ItemDef, ItemKey, Terrain};
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::HashMap;

    #[test]
    fn rooms_need_light_by_their_kind_and_the_hour() {
        assert!(!Light::Daylight.needs_light(Phase::Day));
        assert!(!Light::Daylight.needs_light(Phase::Dusk));
        assert!(Light::Daylight.needs_light(Phase::Night));
        assert!(!Light::Lit.needs_light(Phase::Night));
        assert!(Light::Dark.needs_light(Phase::Day));
    }

    #[test]
    fn the_dark_blinds_those_without_a_light_or_night_eyes() {
        let mut world = World::new();
        world.insert_resource(crate::combat::defs_on_disk());
        let torch = ItemKey::parse("test:torch", "");
        world.insert_resource(ItemTemplates(HashMap::from([(
            torch.clone(),
            ItemDef {
                tags: vec![LIGHT.into()],
                ..ItemDef::named("a torch")
            },
        )])));
        world.init_resource::<LitRooms>();
        let cave = world
            .spawn((
                Room {
                    name: "A cave".into(),
                    description: String::new(),
                    no_wander: false,
                    indoors: true,
                    terrain: Terrain::Paved,
                },
                RoomLight(Light::Dark),
            ))
            .id();
        let eyes = Conditions::default().with_traits(["heat_sight".to_string()].iter());
        let walker = world.spawn((InRoom(cave), Conditions::default())).id();
        let dwarf = world.spawn((InRoom(cave), eyes)).id();
        let run = |world: &mut World| {
            world.run_system_once(light_rooms).unwrap();
            world.run_system_once(darken).unwrap();
        };
        let dark = |world: &World, who| world.get::<Conditions>(who).unwrap().has(IN_THE_DARK);
        run(&mut world);
        assert!(dark(&world, walker));
        assert!(!dark(&world, dwarf), "Heat Sight");
        // A torch lying there lights it for everyone.
        let lit = world
            .spawn((
                Item {
                    key: torch,
                    name: "a torch".into(),
                    id: None,
                    state: DoorState::Open,
                    quality: Quality::Standard,
                },
                ItemLocation::Room(cave),
            ))
            .id();
        run(&mut world);
        assert!(!dark(&world, walker));
        // Carried away by someone who leaves, it takes the light with it.
        let elsewhere = world.spawn(Conditions::default()).id();
        let bearer = world.spawn(InRoom(elsewhere)).id();
        *world.get_mut::<ItemLocation>(lit).unwrap() = ItemLocation::Held(bearer);
        run(&mut world);
        assert!(dark(&world, walker));
    }
}
