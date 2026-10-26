//! Trainers: mobs who teach skills. A mob template lists what it `teaches`; next to one, `learn`
//! lists their lessons and `learn <skill>` takes one, if the student meets the skill's
//! requirements. Fighting is learned this way (the drill sergeant teaches every weapon's basics),
//! and so is magic: each tradition's teacher Awakens a student with latent talent by teaching them
//! its foundation skill.
//!
//! Lessons are free for now; what they cost is a question for when there's money.

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::conditions::Facts;
use crate::db::Database;
use crate::items::capitalized;
use crate::mobs::{MobQuery, MobTemplates};
use crate::output::{Outgoing, PlayerQuery, deliver};
use crate::skills::{Skill, SkillDefs, Skills, learn};

/// Experience for Awakening to a magical tradition.
const AWAKENING_EXPERIENCE: u64 = 100;

pub struct TrainersPlugin;

impl Plugin for TrainersPlugin {
    fn build(&self, app: &mut App) {
        app.add_command(
            CommandSpec::new("learn", Category::Character)
                .shortcuts(&["train"])
                .usage("learn [<skill>]")
                .summary("see what a trainer here can teach you, or take a lesson"),
            learn_cmd,
        );
    }
}

/// The magical traditions, by the tree that holds their foundation skill.
pub const TRADITIONS: [(&str, &str); 4] = [
    ("divine", "Divine"),
    ("hermetic", "Hermetic"),
    ("shamanic", "Shamanic"),
    ("adept", "Adept"),
];

/// The tradition a skill Awakens a character to, if it's a foundation skill.
pub fn tradition_of(key: &str) -> Option<&'static str> {
    let (tree, skill) = key.split_once(':')?;
    TRADITIONS
        .iter()
        .find(|(t, _)| *t == tree && skill == "foundation")
        .map(|(_, name)| *name)
}

/// The tradition a character is Awakened to, if any.
pub fn awakened(skills: &Skills) -> Option<&'static str> {
    skills.0.keys().find_map(|key| tradition_of(key))
}

/// `learn [<skill>]`: list the lessons on offer here, or take one.
#[allow(clippy::too_many_arguments)]
fn learn_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    mobs: MobQuery,
    templates: Res<MobTemplates>,
    defs: Res<SkillDefs>,
    mut sheets: Query<(&mut Skills, &mut Attributes)>,
    db: Res<Database>,
) {
    let Ok((_, _, Some(player), Some(in_room))) = players.get(inv.entity) else {
        return;
    };
    let (room, race, name, character_id) = (
        in_room.0,
        player.race.clone(),
        player.name.clone(),
        player.character_id,
    );
    let Ok((mut skills, mut attributes)) = sheets.get_mut(inv.entity) else {
        return;
    };

    // The trainers here, and what each teaches.
    let mut trainers: Vec<(Entity, String, Vec<&Skill>)> = mobs
        .iter()
        .filter(|(_, _, r)| r.0 == room)
        .filter_map(|(entity, mob, _)| {
            let teaches = &templates.0.get(&mob.key)?.teaches;
            let lessons: Vec<&Skill> = teaches
                .iter()
                .filter_map(|key| defs.skills.get(key))
                .collect();
            (!lessons.is_empty()).then(|| (entity, mob.name.clone(), lessons))
        })
        .collect();
    trainers.sort_by_key(|(entity, _, _)| *entity);

    let facts = Facts {
        attributes: &attributes,
        race: &race,
        skills: Some(&skills),
    };
    let needs = |skill: &Skill| {
        skill
            .def
            .requires
            .as_ref()
            .filter(|c| !c.met(&facts))
            .map(|c| c.describe(&|id| id.to_string()))
    };

    let mut out = Vec::new();
    let wanted = inv.args.trim().to_lowercase();
    if trainers.is_empty() {
        out.push(Outgoing::line(
            inv.entity,
            "There's no one here to teach you.",
        ));
    } else if wanted.is_empty() {
        for (_, trainer, lessons) in &trainers {
            let list: Vec<String> = lessons
                .iter()
                .map(
                    |skill| match (skills.0.contains_key(&skill.key), needs(skill)) {
                        (true, _) => format!("{} (known)", skill.def.name),
                        (false, Some(req)) => format!("{} (requires {req})", skill.def.name),
                        (false, None) => skill.def.name.clone(),
                    },
                )
                .collect();
            out.push(Outgoing::line(
                inv.entity,
                format!(
                    "{} can teach you: {}.",
                    capitalized(trainer),
                    list.join(", ")
                ),
            ));
        }
        out.push(Outgoing::line(
            inv.entity,
            "Type 'learn <skill>' to take a lesson.",
        ));
    } else {
        let found = trainers.iter().find_map(|(_, trainer, lessons)| {
            lessons
                .iter()
                .find(|skill| matches_name(skill, &wanted))
                .map(|skill| (trainer.clone(), *skill))
        });
        let text = match found {
            None => format!("No one here teaches '{}'.", inv.args.trim()),
            Some((_, skill)) if skills.0.contains_key(&skill.key) => {
                format!("You already know {}.", skill.def.name)
            }
            Some((trainer, skill)) => match needs(skill) {
                Some(req) => format!(
                    "{} can't teach you {} yet: it requires {req}.",
                    capitalized(&trainer),
                    skill.def.name
                ),
                None => {
                    let key = skill.key.clone();
                    learn(&mut skills, &key, "Taught", character_id, &db);
                    let mut text = format!(
                        "{} takes you through the basics of {}.\r\n\x1b[1;32mYou have learned {}.\x1b[0m",
                        capitalized(&trainer),
                        skill.def.name,
                        skill.def.name
                    );
                    if let Some(tradition) = tradition_of(&key) {
                        text += &format!(
                            "\r\n\x1b[1;35mSomething long asleep in you wakes. You are Awakened to \
                             the {tradition} tradition.\x1b[0m"
                        );
                        let deed = format!("for Awakening to the {tradition} tradition");
                        for line in attributes.earn(AWAKENING_EXPERIENCE, &deed, character_id, &db)
                        {
                            text += &format!("\r\n{line}");
                        }
                    }
                    out.push(Outgoing::Room {
                        room,
                        except: inv.entity,
                        text: format!(
                            "{} gives {name} a lesson in {}.\r\n",
                            capitalized(&trainer),
                            skill.def.name
                        ),
                    });
                    text
                }
            },
        };
        out.insert(0, Outgoing::line(inv.entity, text));
    }
    deliver(out, &mut players);
}

/// Whether a student asking for `wanted` means this skill: its id, its tree, the start of its
/// name, or the start of a word of it. `learn sword` is Swordplay; `learn knife` is Knife
/// Fighting; `learn first aid` is First Aid.
fn matches_name(skill: &Skill, wanted: &str) -> bool {
    skill.key == wanted
        || skill.tree == wanted
        || skill.tree.starts_with(wanted)
        || skill.def.name.to_lowercase().starts_with(wanted)
        || skill
            .def
            .name
            .to_lowercase()
            .split_whitespace()
            .any(|word| word.starts_with(wanted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_skills_awaken_their_tradition() {
        assert_eq!(tradition_of("divine:foundation"), Some("Divine"));
        assert_eq!(tradition_of("shamanic:foundation"), Some("Shamanic"));
        assert_eq!(tradition_of("divine:smite"), None);
        assert_eq!(tradition_of("swordplay:basics"), None);
    }
}
