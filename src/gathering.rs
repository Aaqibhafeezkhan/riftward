//! Gathering: what a room's ground yields, to an active skill aimed at the room that `Find`s a
//! kind of thing: Foraging finds `"forage"` (food), Herbalism `"herbs"`. What there is comes from
//! the room's finds: its own, or its zone's for its environment.
//!
//! Finding anything is the gatherer's skill (level plus attribute) against the skill's `against`
//! difficulty, by opposed roll; what's found is picked from the room's table with Luck, and the
//! more surely it was found, the better made. Each room holds only a few finds of each kind,
//! growing back one at a time, so no one strips one bush forever. `forage` and `gather` are
//! shortcuts to `use foraging` and `use herbalism`.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::combat::Blow;
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::{Database, Role};
use crate::effects::Effect;
use crate::formula::Side;
use crate::items::{ItemQuery, ItemTemplates, Quality, make_for};
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::skills::{Learner, Practice, SkillKind, fill};
use crate::world::{Finds, ItemKey, RoomKey};

/// The skill `forage` uses.
const FORAGING: &str = "survival:foraging";
/// The skill `gather` uses.
const HERBALISM: &str = "nature:herbalism";
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
            )
            // Run by `use` for an active skill aimed at the room; players don't type it.
            .add_command(
                CommandSpec::new("roomwork", Category::Interaction)
                    .usage("use <skill>")
                    .summary("use an active skill on the place you're in")
                    .quiet()
                    .unlisted(),
                roomwork,
            );
    }
}

/// What's left in each room, by kind of find. Not saved: a restart lets everything grow back.
#[derive(Resource, Default)]
pub struct Patches(HashMap<(Entity, String), Patch>);

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
    fn patch(&mut self, room: Entity, kind: &str, now: f64) -> &mut Patch {
        let patch = self.0.entry((room, kind.to_string())).or_insert(Patch {
            left: PATCH_SIZE,
            since: now,
        });
        patch.regrow(now);
        patch
    }
}

/// `forage`: Foraging, by its old name.
fn forage(In(inv): In<Invocation>, commands: Commands, registry: Res<CommandRegistry>) {
    shortcut(inv, FORAGING, commands, registry);
}

/// `gather`: Herbalism, by its old name.
fn gather(In(inv): In<Invocation>, commands: Commands, registry: Res<CommandRegistry>) {
    shortcut(inv, HERBALISM, commands, registry);
}

/// Use `skill` on the room, as `use` does.
fn shortcut(inv: Invocation, skill: &str, mut commands: Commands, registry: Res<CommandRegistry>) {
    let Some((spec, id)) = registry.find("roomwork", Role::Player) else {
        return;
    };
    commands.run_system_with(
        *id,
        Invocation {
            entity: inv.entity,
            command: spec.name,
            usage: spec.usage,
            args: skill.to_string(),
        },
    );
}

/// What gathering needs.
#[derive(bevy::ecs::system::SystemParam)]
struct Gatherer<'w, 's> {
    commands: Commands<'w, 's>,
    players: Characters<'w, 's>,
    outbox: ResMut<'w, Outbox>,
    rooms: Query<'w, 's, (&'static RoomKey, Option<&'static Finds>)>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    practice: Practice<'w, 's>,
    patches: ResMut<'w, Patches>,
    time: Res<'w, Time>,
    db: Res<'w, Database>,
}

/// `roomwork <tree:skill>`: what `use` runs for an active skill aimed at the room, which `Find`s
/// a kind of thing. If the room has any left and the skill finds it, it's picked from the room's
/// table and goes to the gatherer's hand, or their feet. Every try practises the skill.
fn roomwork(In(inv): In<Invocation>, mut g: Gatherer) {
    let Some(actor) = Actor::get(&g.players, inv.entity) else {
        return;
    };
    let key = inv.args.trim();
    let Some(skill) = g.practice.defs.skills.get(key).cloned() else {
        return;
    };
    let SkillKind::Active {
        against, effects, ..
    } = &skill.def.kind
    else {
        return;
    };
    // Checked at load: a skill aimed at the room does one Find.
    let Some(kind) = effects.iter().find_map(|e| match e {
        Effect::Find { kind } => Some(kind.as_str()),
        _ => None,
    }) else {
        return;
    };
    let mut out = Vec::new();
    let mut gains = Vec::new();
    let reply = (|| {
        let (here, finds) = g.rooms.get(actor.room).map_err(|_| String::new())?;
        let (finds, table) = finds
            .and_then(|f| f.def.0.get(kind).map(|table| (f, table)))
            .filter(|(_, table)| !table.is_empty())
            .ok_or("There's nothing of that kind to find here.")?;
        let now = g.time.elapsed_secs_f64();
        if g.patches.patch(actor.room, kind, now).left == 0 {
            return Err("This place has been picked clean. Give it time to grow back.".into());
        }
        let ready = g.practice.begin(actor.entity, key)?;
        // How surely the search won is how good a find it is.
        let (won, margin) = match against {
            None => (true, 1.0),
            Some(against) => {
                let inputs = g
                    .practice
                    .inputs(actor.entity, ready.effectiveness, Side::default());
                let difficulty = g.practice.eval(against, &inputs).max(1.0);
                let skill = f64::from(ready.effectiveness.max(1));
                let contest = g.practice.opposed(skill, ready.luck, difficulty, 0);
                (contest.won, contest.margin)
            }
        };
        let found = if won {
            g.practice.pick(ready.luck, table).cloned().flatten()
        } else {
            None
        };
        let quality = Quality::of_find(Blow::of(margin));
        let learner = Learner {
            character_id: actor.character_id,
            race: &actor.race,
            boost: 1.0,
        };
        gains = g.practice.exercise(actor.entity, learner, key);

        // Told in the skill's words, with what was found.
        let text = skill.def.text.as_ref().ok_or_else(String::new)?;
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
                    quality,
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
    deliver(out, &mut g.outbox);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::{Attribute, Attributes};
    use crate::dice::Dice;
    use crate::events::GameEvent;
    use crate::formula::Formula;
    use crate::items::{Item, ItemLocation};
    use crate::mobs::Mob;
    use crate::skills::{Cost, Lines, Skill, SkillDef, SkillDefs, SkillState, SkillText, Skills};
    use crate::spells::Aim;
    use crate::vitals::Vitals;
    use crate::world::{FindsDef, InRoom, ItemDef, MobKey};
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;
    use std::collections::BTreeMap;

    /// A clearing where berries can be foraged, and a forager at `level`. Returns the world and
    /// the forager.
    fn clearing(level: u32) -> (World, Entity) {
        let mut world = World::new();
        let lines = |actor: &str| Lines {
            actor: actor.into(),
            target: None,
            room: "{actor} searches.".into(),
            beyond: None,
        };
        let foraging = Skill {
            key: FORAGING.into(),
            tree: "survival".into(),
            attribute: Attribute::Intuition,
            def: SkillDef {
                name: "Foraging".into(),
                description: "Finding food.".into(),
                kind: SkillKind::Active {
                    command: None,
                    quiet: false,
                    provokes: false,
                    cost: Cost::Free,
                    cooldown: 0.0,
                    aim: Aim::Room,
                    uses: None,
                    resist: None,
                    against: Some(Formula::number(3.0)),
                    tags: Vec::new(),
                    effects: vec![Effect::Find {
                        kind: "forage".into(),
                    }],
                },
                text: Some(SkillText {
                    hit: lines("You find {item}."),
                    miss: Some(lines("You find nothing.")),
                }),
                mastery: 50,
                attribute: None,
                rarity: None,
                difficulty: 1.0,
                requires: None,
                karma: None,
            },
        };
        world.insert_resource(SkillDefs {
            skills: HashMap::from([(FORAGING.to_string(), foraging)]),
            trees: HashMap::new(),
        });
        let berries = ItemKey::parse("forest:berries", "");
        world.insert_resource(ItemTemplates(HashMap::from([(
            berries,
            ItemDef {
                tags: vec!["food".into()],
                ..ItemDef::named("a handful of berries")
            },
        )])));
        world.insert_resource(Dice::seeded(9));
        world.init_resource::<crate::facts::Facts>();
        world.insert_resource(Database::open(":memory:").unwrap());
        world.insert_resource(Time::<()>::default());
        world.insert_resource(Outbox::default());
        world.insert_resource(Patches::default());
        world.init_resource::<Messages<GameEvent>>();
        let room = world
            .spawn((
                RoomKey::parse("forest:clearing", ""),
                Finds {
                    zone: "forest".into(),
                    def: FindsDef(BTreeMap::from([(
                        "forage".to_string(),
                        vec![(Some("berries".to_string()), 1)],
                    )])),
                },
            ))
            .id();
        let mut skill = SkillState::new("Template");
        skill.level = level;
        let forager = world
            .spawn((
                Mob {
                    key: MobKey::parse("forest:forager", ""),
                    name: "a forager".into(),
                    here: String::new(),
                    description: String::new(),
                    race: None,
                },
                InRoom(room),
                Attributes::starting(|_| 1, 0),
                Vitals {
                    health: 10.0,
                    stamina: 10.0,
                },
                Skills(BTreeMap::from([(FORAGING.to_string(), skill)])),
            ))
            .id();
        (world, forager)
    }

    fn search(world: &mut World, who: Entity, skill: &str) {
        let inv = Invocation {
            entity: who,
            command: "roomwork",
            usage: "",
            args: skill.into(),
        };
        world.run_system_once_with(roomwork, inv).unwrap();
        world.flush();
    }

    fn said(world: &World, text: &str) -> bool {
        world.resource::<Outbox>().0.iter().any(|out| match out {
            Outgoing::To(_, line) | Outgoing::Room { text: line, .. } => line.contains(text),
        })
    }

    #[test]
    fn foraging_finds_what_the_room_holds_until_its_picked_clean() {
        let (mut world, forager) = clearing(1000);
        search(&mut world, forager, FORAGING);
        // Found as surely as this, they're as good as berries get.
        assert!(said(&world, "You find a perfect handful of berries."));
        let held = world
            .query::<(&Item, &ItemLocation)>()
            .iter(&world)
            .filter(|(_, l)| **l == ItemLocation::Held(forager))
            .count();
        assert_eq!(held, 1, "in hand");
        // Three to a patch: two more, then nothing.
        search(&mut world, forager, FORAGING);
        search(&mut world, forager, FORAGING);
        search(&mut world, forager, FORAGING);
        assert!(said(&world, "picked clean"));
    }

    #[test]
    fn a_search_that_fails_finds_nothing_and_leaves_the_patch() {
        let (mut world, forager) = clearing(0);
        // A difficulty no one beats.
        if let Some(skill) = world.resource_mut::<SkillDefs>().skills.get_mut(FORAGING)
            && let SkillKind::Active { against, .. } = &mut skill.def.kind
        {
            *against = Some(Formula::number(1e6));
        }
        search(&mut world, forager, FORAGING);
        assert!(said(&world, "You find nothing."));
        let room = world.get::<InRoom>(forager).unwrap().0;
        let now = world.resource::<Time>().elapsed_secs_f64();
        let left = world
            .resource_mut::<Patches>()
            .patch(room, "forage", now)
            .left;
        assert_eq!(left, PATCH_SIZE);
    }

    #[test]
    fn a_room_without_that_kind_of_find_says_so() {
        let (mut world, forager) = clearing(1000);
        let room = world.get::<InRoom>(forager).unwrap().0;
        world.get_mut::<Finds>(room).unwrap().def.0.clear();
        search(&mut world, forager, FORAGING);
        assert!(said(&world, "There's nothing of that kind to find here."));
    }

    #[test]
    fn picked_patches_grow_back_one_at_a_time() {
        let mut patches = Patches::default();
        let room = Entity::from_raw_u32(1).unwrap();
        // Full at first; take all three.
        for _ in 0..PATCH_SIZE {
            let patch = patches.patch(room, "forage", 0.0);
            assert!(patch.left > 0);
            patch.left -= 1;
        }
        assert_eq!(patches.patch(room, "forage", 1.0).left, 0);
        // Herbs are a separate patch.
        assert_eq!(patches.patch(room, "herbs", 1.0).left, PATCH_SIZE);
        // One grows back per REGROW_SECS, never past full.
        assert_eq!(patches.patch(room, "forage", REGROW_SECS).left, 1);
        assert_eq!(patches.patch(room, "forage", REGROW_SECS * 2.5).left, 2);
        assert_eq!(
            patches.patch(room, "forage", REGROW_SECS * 100.0).left,
            PATCH_SIZE
        );
    }
}
