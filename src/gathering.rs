//! Gathering: what a room's ground yields. `forage` (Foraging) finds food and `gather`
//! (Herbalism) finds herbs, from the room's finds: its own, or its zone's for its environment.
//!
//! Finding anything is the gatherer's skill against a fixed difficulty, by opposed roll; what's found is picked from the room's table with Luck. Each room holds only a few
//! finds of each kind, growing back one at a time, so no one strips one bush forever.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Database;
use crate::items::{ItemQuery, ItemTemplates, make_for};
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};
use crate::skills::{Learner, Practice, fill};
use crate::world::{Finds, ItemKey, RoomKey, Table};

/// The skill `forage` uses.
const FORAGING: &str = "survival:foraging";
/// The skill `gather` uses.
const HERBALISM: &str = "nature:herbalism";
/// How hard anything is to find, against a gatherer's skill (level plus attribute).
const DIFFICULTY: f64 = 3.0;
/// How many finds of each kind a room holds when fully grown.
const PATCH_SIZE: u32 = 3;
/// Seconds for one find to grow back.
const REGROW_SECS: f64 = 600.0;

pub struct GatheringPlugin;

impl Plugin for GatheringPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Patches>()
            .add_command(
                CommandSpec::new("forage", Category::Interaction)
                    .summary("search the ground here for something to eat, if you know Foraging")
                    .action(),
                forage,
            )
            .add_command(
                CommandSpec::new("gather", Category::Interaction)
                    .summary("search here for herbs, if you know Herbalism")
                    .action(),
                gather,
            );
    }
}

/// What's gathered: food or herbs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Forage,
    Herbs,
}

impl Kind {
    fn skill(self) -> &'static str {
        match self {
            Kind::Forage => FORAGING,
            Kind::Herbs => HERBALISM,
        }
    }

    fn table(self, finds: &Finds) -> &Table {
        match self {
            Kind::Forage => &finds.def.forage,
            Kind::Herbs => &finds.def.herbs,
        }
    }

    fn none_here(self) -> &'static str {
        match self {
            Kind::Forage => "There's nothing to forage here.",
            Kind::Herbs => "No herbs grow here.",
        }
    }
}

/// What's left to find in each room, by kind. Not saved: a restart lets everything grow back.
#[derive(Resource, Default)]
pub struct Patches(HashMap<(Entity, Kind), Patch>);

/// What's left in one room of one kind, and since when it's been growing back.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Patch {
    left: u32,
    since: f64,
}

impl Patch {
    /// Grow back one find for every `REGROW_SECS` since `since`, up to full. A full patch doesn't
    /// bank growth: its clock starts when something is next taken.
    fn regrow(&mut self, now: f64) {
        let grown = ((now - self.since) / REGROW_SECS).floor().max(0.0) as u32;
        self.left = (self.left + grown).min(PATCH_SIZE);
        self.since += f64::from(grown) * REGROW_SECS;
        if self.left == PATCH_SIZE {
            self.since = now;
        }
    }
}

impl Patches {
    fn patch(&mut self, room: Entity, kind: Kind, now: f64) -> &mut Patch {
        let patch = self.0.entry((room, kind)).or_insert(Patch {
            left: PATCH_SIZE,
            since: now,
        });
        patch.regrow(now);
        patch
    }
}

fn forage(In(inv): In<Invocation>, gatherer: Gatherer) {
    gather_from(inv, Kind::Forage, gatherer);
}

fn gather(In(inv): In<Invocation>, gatherer: Gatherer) {
    gather_from(inv, Kind::Herbs, gatherer);
}

/// What gathering needs.
#[derive(bevy::ecs::system::SystemParam)]
struct Gatherer<'w, 's> {
    commands: Commands<'w, 's>,
    players: PlayerQuery<'w, 's>,
    rooms: Query<'w, 's, (&'static RoomKey, Option<&'static Finds>)>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    practice: Practice<'w, 's>,
    patches: ResMut<'w, Patches>,
    time: Res<'w, Time>,
    db: Res<'w, Database>,
}

/// Search the room for `kind`: if there's anything left and the skill finds it, it's picked from
/// the room's table and goes to the gatherer's hand, or their feet.
fn gather_from(inv: Invocation, kind: Kind, mut g: Gatherer) {
    let Some(actor) = Actor::get(&g.players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let mut gains = Vec::new();
    let reply = (|| {
        let (here, finds) = g.rooms.get(actor.room).map_err(|_| String::new())?;
        let finds = finds
            .filter(|f| !kind.table(f).is_empty())
            .ok_or(kind.none_here())?;
        let now = g.time.elapsed_secs_f64();
        if g.patches.patch(actor.room, kind, now).left == 0 {
            return Err("This place has been picked clean. Give it time to grow back.".into());
        }
        let ready = g.practice.begin(actor.entity, kind.skill())?;
        let skill = f64::from(ready.effectiveness.max(1));
        let found = if g.practice.opposed(skill, ready.luck, DIFFICULTY, 0).won {
            g.practice
                .pick(ready.luck, kind.table(finds))
                .cloned()
                .flatten()
        } else {
            None
        };
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
        };
        gains = g.practice.exercise(actor.entity, learner, kind.skill());

        // Told in the skill's words, with what was found.
        let text = g
            .practice
            .defs
            .skills
            .get(kind.skill())
            .and_then(|skill| skill.def.text.clone())
            .ok_or_else(String::new)?;
        let (lines, item, note) = match found {
            Some(id) => {
                g.patches.patch(actor.room, kind, now).left -= 1;
                let key = ItemKey::parse(&id, &finds.zone);
                let (name, where_to) = make_for(
                    &mut g.commands,
                    &g.templates,
                    &g.db,
                    &g.items,
                    &actor,
                    here,
                    key,
                )?;
                let note = if where_to == "here" {
                    " Your hands are full, so it's on the ground."
                } else {
                    ""
                };
                (&text.hit, name, note)
            }
            None => (text.miss.as_ref().unwrap_or(&text.hit), String::new(), ""),
        };
        let with = [("actor", actor.name.as_str()), ("item", item.as_str())];
        out.push(Outgoing::Room {
            room: actor.room,
            except: actor.entity,
            text: fill(&lines.room, &with) + "\r\n",
        });
        Ok(fill(&lines.actor, &with) + note)
    })();
    match reply {
        Ok(text) | Err(text) if !text.is_empty() => {
            out.insert(0, Outgoing::line(actor.entity, text))
        }
        _ => {}
    }
    out.extend(
        gains
            .into_iter()
            .map(|line| Outgoing::line(actor.entity, line)),
    );
    deliver(out, &mut g.players);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picked_patches_grow_back_one_at_a_time() {
        let mut patches = Patches::default();
        let room = Entity::from_raw_u32(1).unwrap();
        // Full at first; take all three.
        for _ in 0..PATCH_SIZE {
            let patch = patches.patch(room, Kind::Forage, 0.0);
            assert!(patch.left > 0);
            patch.left -= 1;
        }
        assert_eq!(patches.patch(room, Kind::Forage, 1.0).left, 0);
        // Herbs are a separate patch.
        assert_eq!(patches.patch(room, Kind::Herbs, 1.0).left, PATCH_SIZE);
        // One grows back per REGROW_SECS, never past full.
        assert_eq!(patches.patch(room, Kind::Forage, REGROW_SECS).left, 1);
        assert_eq!(patches.patch(room, Kind::Forage, REGROW_SECS * 2.5).left, 2);
        assert_eq!(
            patches.patch(room, Kind::Forage, REGROW_SECS * 100.0).left,
            PATCH_SIZE
        );
    }
}
