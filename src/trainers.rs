//! Trainers: mobs who teach skills. A mob template lists what it `teaches`; next to one, `learn`
//! lists their lessons and `learn <skill>` takes one, if the student meets the skill's
//! requirements. Fighting is learned this way (the drill sergeant teaches every weapon's basics),
//! and so is magic: each tradition's teacher Awakens a student with latent talent by teaching them
//! its foundation skill.
//!
//! Skill lessons are free, so no one is locked out of a way to play. Spells cost money, as each
//! spell says (`price`), except each tradition's starter spell.

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::Database;
use crate::items::capitalized;
use crate::mobs::{MobQuery, MobTemplates};
use crate::money::{self, Purse};
use crate::output::{Characters, Outbox, Outgoing, deliver};
use crate::requirements::Subject;
use crate::skills::{Skill, SkillDefs, Skills, learn};
use crate::spells::{self, Spell, SpellDefs, Spells};

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
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mobs: MobQuery,
    templates: Res<MobTemplates>,
    (defs, spell_defs): (Res<SkillDefs>, Res<SpellDefs>),
    mut sheets: Query<(&mut Skills, &mut Attributes, &mut Spells, &mut Purse)>,
    db: Res<Database>,
    (shown, story): (Query<&crate::guild::ShownRank>, Res<crate::facts::Facts>),
) {
    // Lessons are for players: a mob has no purse to pay with, and nothing to save.
    let Ok((_, Some(player), _, in_room)) = players.get(inv.entity) else {
        return;
    };
    let (room, race, name, character_id) = (
        in_room.0,
        player.race.clone(),
        player.name.clone(),
        player.character_id,
    );
    let Ok((mut skills, mut attributes, mut spells, mut purse)) = sheets.get_mut(inv.entity) else {
        return;
    };

    // The trainers here, and what each teaches.
    let mut trainers: Vec<(Entity, String, Vec<Lesson>)> = mobs
        .iter()
        .filter(|(_, _, r)| r.0 == room)
        .filter_map(|(entity, mob, _)| {
            let teaches = &templates.0.get(&mob.key)?.teaches;
            let lessons: Vec<Lesson> = teaches
                .iter()
                .filter_map(|key| match (defs.skills.get(key), spell_defs.0.get(key)) {
                    (Some(skill), _) => Some(Lesson::Skill(skill)),
                    (None, Some(spell)) => Some(Lesson::Spell(spell)),
                    (None, None) => None,
                })
                .collect();
            (!lessons.is_empty()).then(|| (entity, mob.name.clone(), lessons))
        })
        .collect();
    trainers.sort_by_key(|(entity, _, _)| *entity);

    let facts = Subject {
        attributes: &attributes,
        race: &race,
        skills: Some(&skills),
        guild_rank: crate::guild::shown_rank(inv.entity, &shown),
        story: Some(&story),
        character: Some(character_id),
    };
    let needs = |lesson: &Lesson| match lesson {
        Lesson::Skill(skill) => skill
            .def
            .requires
            .as_ref()
            .filter(|c| !c.met(&facts))
            .map(|c| c.describe(&|id| id.to_string())),
        // A spell needs its school, at its level.
        Lesson::Spell(spell) => {
            let school = defs
                .skills
                .get(&spell.def.school)
                .map_or(spell.def.school.as_str(), |s| s.def.name.as_str());
            match (skills.0.get(&spell.def.school), spell.def.min_level) {
                (Some(state), min) if state.level >= min => None,
                (_, 0) => Some(school.to_string()),
                (_, min) => Some(format!("{school} {min}")),
            }
        }
    };
    let known = |lesson: &Lesson| match lesson {
        Lesson::Skill(skill) => skills.0.contains_key(&skill.key),
        Lesson::Spell(spell) => spells.0.contains(&spell.key),
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
                .map(|lesson| match (known(lesson), needs(lesson)) {
                    (true, _) => match lesson {
                        Lesson::Spell(spell) => format!("the spell {} (known)", spell.def.name),
                        Lesson::Skill(skill) => format!("{} (known)", skill.def.name),
                    },
                    (false, Some(req)) => format!("{} (requires {req})", lesson.title()),
                    (false, None) => lesson.title(),
                })
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
        // A word that names several lessons means one they don't know yet: `learn spirit`
        // next to the hedge-shaman is Spirit Grasp to someone who knows Spirit Sight.
        let matching: Vec<(String, Lesson)> = trainers
            .iter()
            .flat_map(|(_, trainer, lessons)| {
                lessons
                    .iter()
                    .filter(|lesson| lesson.matches(&wanted))
                    .map(|lesson| (trainer.clone(), *lesson))
            })
            .collect();
        let found = matching
            .iter()
            .find(|(_, lesson)| !known(lesson))
            .or(matching.first())
            .cloned();
        let text = match found {
            None => format!("No one here teaches '{}'.", inv.args.trim()),
            Some((_, lesson)) if known(&lesson) => {
                format!("You already know {}.", lesson.name())
            }
            Some((trainer, lesson)) => match (needs(&lesson), lesson) {
                (Some(req), _) => format!(
                    "{} can't teach you {} yet: it requires {req}.",
                    capitalized(&trainer),
                    lesson.name()
                ),
                (None, Lesson::Spell(spell)) if !purse.pay(u64::from(spell.def.price)) => {
                    format!(
                        "{} asks {} to teach you {}, and you have {}.",
                        capitalized(&trainer),
                        money::coins(u64::from(spell.def.price)),
                        spell.def.name,
                        money::coins(purse.0)
                    )
                }
                (None, Lesson::Spell(spell)) => {
                    if spell.def.price > 0 {
                        purse.save(Some(character_id), &db);
                    }
                    spells.0.insert(spell.key.clone());
                    if let Err(err) = db.save_spell(character_id, &spell.key, "Taught") {
                        error!("failed to save {name}'s new spell {}: {err}", spell.key);
                    }
                    out.push(Outgoing::Room {
                        room,
                        except: inv.entity,
                        text: format!(
                            "{} teaches {name} the spell {}.\r\n",
                            capitalized(&trainer),
                            spell.def.name
                        ),
                    });
                    format!(
                        "{} takes you through the words and the shape of {}.\r\n\x1b[1;32mYou have learned the spell {}.\x1b[0m Type 'spells' to see the spells you know.",
                        capitalized(&trainer),
                        spell.def.name,
                        spell.def.name
                    )
                }
                (None, Lesson::Skill(skill)) => {
                    let key = skill.key.clone();
                    learn(&mut skills, &key, "Taught", Some(character_id), &db);
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
                        for line in attributes.earn(
                            crate::attributes::progression().awakening_experience,
                            &deed,
                            character_id,
                            &db,
                        ) {
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
    deliver(out, &mut outbox);
}

/// Something a trainer teaches: a skill, or a spell.
#[derive(Clone, Copy)]
enum Lesson<'a> {
    Skill(&'a Skill),
    Spell(&'a Spell),
}

impl Lesson<'_> {
    fn name(&self) -> &str {
        match self {
            Lesson::Skill(skill) => &skill.def.name,
            Lesson::Spell(spell) => &spell.def.name,
        }
    }

    /// How it's listed: a spell says so, and what it costs.
    fn title(&self) -> String {
        match self {
            Lesson::Skill(skill) => skill.def.name.clone(),
            Lesson::Spell(spell) if spell.def.price > 0 => format!(
                "the spell {} for {}",
                spell.def.name,
                money::coins(u64::from(spell.def.price))
            ),
            Lesson::Spell(spell) => format!("the spell {} (free)", spell.def.name),
        }
    }

    fn matches(&self, wanted: &str) -> bool {
        match self {
            Lesson::Skill(skill) => matches_name(skill, wanted),
            Lesson::Spell(spell) => {
                spell.key == wanted || spells::matches_name(&spell.def.name, wanted)
            }
        }
    }
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
