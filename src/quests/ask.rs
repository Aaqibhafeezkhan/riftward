//! `ask <mob> about <topic>`: the first piece of dialogue. A mob answers from the topics the
//! quest stages you're on give it; asking is a game event (`Asked`), which an `Ask` objective
//! counts.

use bevy::prelude::*;

use super::def::Quests;
use super::journal::{HELD, QuestLog};
use crate::combat::Conditions;
use crate::command::Invocation;
use crate::events::GameEvent;
use crate::items::capitalized;
use crate::mobs::MobQuery;
use crate::output::{Actor, Characters, Outbox, Outgoing, deliver};
use crate::stealth::visible;
use crate::targeting::{Keywords, Target};
use crate::world::MobKey;

/// `ask <mob> [about] <topic>`.
#[allow(clippy::too_many_arguments)]
pub(super) fn ask(
    In(inv): In<Invocation>,
    players: Characters,
    mobs: MobQuery,
    (keywords, hidden): (Query<&Keywords>, Query<&Conditions>),
    logs: Query<&QuestLog>,
    quests: Res<Quests>,
    mut events: MessageWriter<GameEvent>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut words = inv.args.trim().splitn(2, ' ');
    let (Some(whom), Some(rest)) = (words.next().and_then(Target::parse), words.next()) else {
        deliver(
            vec![Outgoing::line(actor.entity, inv.usage_error())],
            &mut outbox,
        );
        return;
    };
    let rest = rest.trim();
    let topic = rest
        .strip_prefix("about ")
        .unwrap_or(rest)
        .trim()
        .to_lowercase();
    let here = visible(
        actor.entity,
        crate::mobs::targets_in(actor.room, &mobs, &keywords),
        &hidden,
    );
    let Some((key, name)) = whom
        .find_one(actor.entity, here)
        .and_then(|e| mobs.get(e).ok())
        .map(|(_, mob, _)| (mob.key.clone(), crate::combat::the(&mob.name)))
    else {
        deliver(
            vec![Outgoing::line(
                actor.entity,
                format!("You don't see {} here.", whom.describe()),
            )],
            &mut outbox,
        );
        return;
    };
    let answer = logs
        .get(actor.entity)
        .ok()
        .and_then(|log| answer(log, &quests, &key, &topic));
    let (topic, text) = match answer {
        Some((topic, text)) => (topic, text),
        None => (
            topic.clone(),
            format!("{} has nothing to say about {topic}.", capitalized(&name)),
        ),
    };
    events.write(GameEvent::Asked {
        who: actor.entity,
        mob: key,
        topic: topic.clone(),
    });
    deliver(
        vec![
            Outgoing::line(actor.entity, text),
            Outgoing::Room {
                room: actor.room,
                except: actor.entity,
                text: format!("{} asks {name} about {topic}.\r\n", actor.name),
            },
        ],
        &mut outbox,
    );
}

/// What a mob of template `mob` says about `topic` (or a topic it begins) to someone with this
/// log: from the stage they're on of each quest they hold. The topic as written, and the answer.
fn answer(log: &QuestLog, quests: &Quests, mob: &MobKey, topic: &str) -> Option<(String, String)> {
    log.0
        .iter()
        .filter(|r| r.state == HELD)
        .filter_map(|r| {
            let quest = quests.0.get(&r.quest)?;
            let (_, stage) = quest.stage_at(&r.stage)?;
            Some((quest, stage))
        })
        .flat_map(|(quest, stage)| {
            stage
                .topics
                .iter()
                .filter(|(id, _)| MobKey::parse(id, quest.zone()) == *mob)
                .flat_map(|(_, topics)| topics)
        })
        .find(|(about, _)| {
            let about = about.to_lowercase();
            about == topic || about.starts_with(topic)
        })
        .map(|(about, text)| (about.to_lowercase(), text.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::QuestRecord;
    use crate::quests::def::{Objective, QuestDef, Stage};
    use crate::quests::testing::rats;
    use std::collections::{BTreeMap, HashMap};

    #[test]
    fn a_mob_answers_from_the_stage_youre_on() {
        let quest = QuestDef {
            stages: vec![
                Stage {
                    text: "Ask Marin.".into(),
                    objectives: vec![Objective::Ask("clerk".into(), "crystals".into())],
                    topics: BTreeMap::from([(
                        "clerk".to_string(),
                        vec![("Crystals".to_string(), "\"Three this week.\"".to_string())],
                    )]),
                    ..Default::default()
                },
                Stage {
                    text: "Ask in the alley.".into(),
                    objectives: vec![Objective::Ask("talla".into(), "crystals".into())],
                    topics: BTreeMap::from([(
                        "talla".to_string(),
                        vec![("crystals".to_string(), "\"The culvert.\"".to_string())],
                    )]),
                    ..Default::default()
                },
            ],
            ..rats()
        };
        assert!(quest.check().is_ok());
        let quests = Quests(HashMap::from([(quest.id.clone(), quest)]));
        let on = |stage: &str| {
            QuestLog(vec![QuestRecord {
                quest: "riftwatch:rats".into(),
                state: HELD.into(),
                stage: stage.into(),
                taken_at: 0,
                due_at: None,
                finished_at: None,
            }])
        };
        let (clerk, talla) = (
            MobKey::parse("riftwatch:clerk", ""),
            MobKey::parse("riftwatch:talla", ""),
        );
        assert_eq!(
            answer(&on("1"), &quests, &clerk, "cryst"),
            Some(("crystals".into(), "\"Three this week.\"".into())),
            "the start of a topic will do"
        );
        assert_eq!(
            answer(&on("1"), &quests, &talla, "crystals"),
            None,
            "not yet"
        );
        assert!(answer(&on("2"), &quests, &talla, "crystals").is_some());
        assert_eq!(
            answer(&on("2"), &quests, &clerk, "crystals"),
            None,
            "that stage is done"
        );
        assert_eq!(
            answer(&QuestLog::default(), &quests, &clerk, "crystals"),
            None
        );
    }
}
