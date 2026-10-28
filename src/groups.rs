//! Following and groups: characters who move and fight together (see `docs/groups.md`).
//!
//! `follow` sets someone to go where another goes: when the one they follow walks out of a room,
//! they walk after them with the same `go`, by the same rules. A group is formed the ROM way: its
//! leader takes in those following them with `group <name>`. Groups and following live in the
//! game only; `nofollow` and `autoassist` are settings saved with the character. What a group
//! shares in a fight (experience, coins, Luck, assisting) is in `combat`.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::attributes::{Attributes, progression};
use crate::combat::{Conditions, Dead, Fighting};
use crate::command::{AddCommand, Category, CommandRegistry, CommandSpec, Invocation};
use crate::db::{Database, Role};
use crate::events::{GameEvent, Way};
use crate::mobs::{self, MobQuery};
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver, targets_in_room};
use crate::skills::{SkillDefs, Skills};
use crate::stealth::visible;
use crate::targeting::{Keywords, Target};
use crate::vitals::{BaseHealth, Maxima, Resting, Vitals};

pub struct GroupsPlugin;

impl Plugin for GroupsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Joined>()
            // A tick after the leader goes, so those following see them leave before they
            // follow.
            .add_systems(
                Update,
                (follow_leaders, tidy)
                    .chain()
                    .before(crate::command::dispatch),
            )
            .add_command(
                CommandSpec::new("follow", Category::Movement)
                    .usage("follow [<someone> | self]")
                    .summary("go where someone goes; 'follow self' to stop, and leave your group"),
                follow,
            )
            .add_command(
                CommandSpec::new("nofollow", Category::Movement)
                    .summary("turn followers away, or let them follow again"),
                nofollow,
            )
            .add_command(
                CommandSpec::new("group", Category::Communication)
                    .usage("group [<follower>]")
                    .summary("see your group, or (leading) take a follower in or put them out")
                    .quiet(),
                group,
            )
            .add_command(
                CommandSpec::new("gtell", Category::Communication)
                    .shortcuts(&["gt"])
                    .usage("gtell <text>")
                    .summary("speak to your whole group, wherever they are")
                    .quiet(),
                gtell,
            )
            .add_command(
                CommandSpec::new("disband", Category::Communication)
                    .summary("(leading a group) end it")
                    .quiet(),
                disband,
            )
            .add_command(
                CommandSpec::new("autoassist", Category::Character)
                    .usage("autoassist [on | off]")
                    .summary("whether you join your group's fights by yourself"),
                autoassist,
            );
    }
}

/// Who someone follows.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Following(pub Entity);

/// Turns followers away.
#[derive(Component)]
pub struct NoFollow;

/// Doesn't join the group's fights by themselves: `autoassist off`.
#[derive(Component)]
pub struct NoAssist;

/// In a group, led by `leader` (the leader has one too). `joined` orders the members, so the lead
/// passes to whoever joined first.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct InGroup {
    pub leader: Entity,
    pub joined: u64,
}

/// Counts joinings, for `InGroup::joined`.
#[derive(Resource, Default)]
struct Joined(u64);

impl Joined {
    fn next(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

/// Everyone in a group.
pub type Groups<'w, 's> = Query<'w, 's, (Entity, &'static InGroup)>;

/// The group `who` is in, led by its leader, the leader first and then in the order they joined;
/// just `who` if they're in none.
pub fn members(who: Entity, groups: &Groups) -> Vec<Entity> {
    let Ok((_, mine)) = groups.get(who) else {
        return vec![who];
    };
    let mut members: Vec<(Entity, &InGroup)> = groups
        .iter()
        .filter(|(_, g)| g.leader == mine.leader)
        .collect();
    members.sort_by_key(|(e, g)| (*e != mine.leader, g.joined));
    members.into_iter().map(|(e, _)| e).collect()
}

/// Which side `who` is on: their group's leader, or themselves alone.
pub fn side(who: Entity, groups: &Groups) -> Entity {
    groups.get(who).map_or(who, |(_, g)| g.leader)
}

/// The name a character goes by, player or mob: "Ada", "the town guard".
fn name_of(characters: &Characters, who: Entity) -> String {
    Actor::get(characters, who).map_or_else(|| "someone".into(), |a| a.name)
}

fn capitalized(name: &str) -> String {
    crate::items::capitalized(name)
}

/// `who` leaves their group. A member just goes; the leader hands the lead to whoever joined
/// first. A group left with one member is over. Tells everyone still in it.
fn leave_group(
    who: Entity,
    groups: &Groups,
    characters: &Characters,
    commands: &mut Commands,
    out: &mut Vec<Outgoing>,
) {
    let Ok((_, mine)) = groups.get(who) else {
        return;
    };
    let name = name_of(characters, who);
    let rest: Vec<Entity> = members(who, groups)
        .into_iter()
        .filter(|e| *e != who)
        .collect();
    commands.entity(who).remove::<InGroup>();
    out.push(Outgoing::line(who, "You leave the group."));
    if rest.len() < 2 {
        for other in rest {
            commands.entity(other).remove::<InGroup>();
            out.push(Outgoing::line(
                other,
                format!("{} leaves the group, and it breaks up.", capitalized(&name)),
            ));
        }
        return;
    }
    if mine.leader != who {
        for other in rest {
            out.push(Outgoing::line(
                other,
                format!("{} leaves the group.", capitalized(&name)),
            ));
        }
        return;
    }
    // The lead passes to whoever joined first.
    let leader = rest[0];
    let leads = name_of(characters, leader);
    for other in rest {
        if let Ok((_, g)) = groups.get(other) {
            commands.entity(other).insert(InGroup {
                leader,
                joined: g.joined,
            });
        }
        let line = if other == leader {
            format!("{} leaves the group; you lead it now.", capitalized(&name))
        } else {
            format!(
                "{} leaves the group; {leads} leads it now.",
                capitalized(&name)
            )
        };
        out.push(Outgoing::line(other, line));
    }
}

/// Stop `who` following anyone, telling them and the one they followed.
fn stop_following(
    who: Entity,
    following: &Query<&Following>,
    characters: &Characters,
    commands: &mut Commands,
    out: &mut Vec<Outgoing>,
) {
    let Ok(Following(leader)) = following.get(who) else {
        return;
    };
    commands.entity(who).remove::<Following>();
    out.push(Outgoing::line(
        who,
        format!("You stop following {}.", name_of(characters, *leader)),
    ));
    out.push(Outgoing::line(
        *leader,
        format!(
            "{} stops following you.",
            capitalized(&name_of(characters, who))
        ),
    ));
}

/// `follow`: whom you follow; `follow <someone>` to go where they go; `follow self` to stop, and
/// leave your group.
#[allow(clippy::too_many_arguments)]
fn follow(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    hidden: Query<&Conditions>,
    following: Query<&Following>,
    groups: Groups,
    refusing: Query<(), With<NoFollow>>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let words = inv.args.to_lowercase();
    if words.is_empty() {
        let line = match following.get(actor.entity) {
            Ok(Following(leader)) => {
                format!("You are following {}.", name_of(&characters, *leader))
            }
            Err(_) => "You aren't following anyone.".into(),
        };
        out.push(Outgoing::line(actor.entity, line));
        deliver(out, &mut outbox);
        return;
    }
    let target = if matches!(words.as_str(), "self" | "me") {
        Some(actor.entity)
    } else {
        Target::parse(&inv.args).and_then(|target| {
            let mut candidates = targets_in_room(actor.room, &characters, &keywords);
            candidates.extend(mobs::targets_in(actor.room, &mobs, &keywords));
            target.find_one(actor.entity, visible(actor.entity, candidates, &hidden))
        })
    };
    match target {
        None => out.push(Outgoing::line(actor.entity, "You don't see them here.")),
        Some(me) if me == actor.entity => {
            let grouped = groups.get(me).is_ok();
            if following.get(me).is_err() && !grouped {
                out.push(Outgoing::line(me, "You aren't following anyone."));
            }
            stop_following(me, &following, &characters, &mut commands, &mut out);
            leave_group(me, &groups, &characters, &mut commands, &mut out);
        }
        Some(leader) if following.get(actor.entity) == Ok(&Following(leader)) => {
            out.push(Outgoing::line(
                actor.entity,
                format!("You're already following {}.", name_of(&characters, leader)),
            ));
        }
        Some(leader) if refusing.get(leader).is_ok() => {
            out.push(Outgoing::line(
                actor.entity,
                format!(
                    "{} doesn't want to be followed.",
                    capitalized(&name_of(&characters, leader))
                ),
            ));
        }
        Some(leader) => {
            stop_following(
                actor.entity,
                &following,
                &characters,
                &mut commands,
                &mut out,
            );
            // Following someone outside your group takes you out of it.
            if groups.get(actor.entity).is_ok()
                && side(leader, &groups) != side(actor.entity, &groups)
            {
                leave_group(actor.entity, &groups, &characters, &mut commands, &mut out);
            }
            commands.entity(actor.entity).insert(Following(leader));
            let name = name_of(&characters, leader);
            out.push(Outgoing::line(
                actor.entity,
                format!("You now follow {name}."),
            ));
            out.push(Outgoing::line(
                leader,
                format!("{} now follows you.", capitalized(&actor.name)),
            ));
        }
    }
    deliver(out, &mut outbox);
}

/// `nofollow`: turn followers away (and send those following off), or let them follow again.
/// Saved with the character.
fn nofollow(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    refusing: Query<(), With<NoFollow>>,
    followers: Query<(Entity, &Following)>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    let refuse = refusing.get(actor.entity).is_err();
    if refuse {
        commands.entity(actor.entity).insert(NoFollow);
        out.push(Outgoing::line(
            actor.entity,
            "You no longer let anyone follow you.",
        ));
        for (follower, Following(leader)) in &followers {
            if *leader == actor.entity {
                commands.entity(follower).remove::<Following>();
                out.push(Outgoing::line(
                    follower,
                    format!("You stop following {}.", actor.name),
                ));
            }
        }
    } else {
        commands.entity(actor.entity).remove::<NoFollow>();
        out.push(Outgoing::line(
            actor.entity,
            "You let others follow you again.",
        ));
    }
    if let Some(id) = actor.character_id
        && let Err(err) = db.set_nofollow(id, refuse)
    {
        error!("failed to save {}'s nofollow setting: {err}", actor.name);
    }
    deliver(out, &mut outbox);
}

/// What `group` shows of one member: "Ada (leading)  14/20 health  18/20 stamina  Market Square".
type Sheets<'w, 's> = Query<
    'w,
    's,
    (
        &'static Vitals,
        &'static Attributes,
        &'static Skills,
        Option<&'static BaseHealth>,
    ),
>;

/// `group`: list your group; `group <follower>` (leading, or alone) takes a follower into it, or
/// puts a member out.
#[allow(clippy::too_many_arguments)]
fn group(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    mobs: MobQuery,
    keywords: Query<&Keywords>,
    hidden: Query<&Conditions>,
    following: Query<&Following>,
    groups: Groups,
    sheets: Sheets,
    defs: Res<SkillDefs>,
    rooms: RoomQuery,
    mut joined: ResMut<Joined>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    if inv.args.is_empty() {
        out.push(Outgoing::line(
            actor.entity,
            listing(actor.entity, &characters, &groups, &sheets, &defs, &rooms),
        ));
        deliver(out, &mut outbox);
        return;
    }
    let target = Target::parse(&inv.args).and_then(|target| {
        let mut candidates = targets_in_room(actor.room, &characters, &keywords);
        candidates.extend(mobs::targets_in(actor.room, &mobs, &keywords));
        target.find_one(actor.entity, visible(actor.entity, candidates, &hidden))
    });
    let leader = side(actor.entity, &groups);
    let reply = |text: String, out: &mut Vec<Outgoing>| {
        out.push(Outgoing::line(actor.entity, text));
    };
    match target {
        None => reply("You don't see them here.".into(), &mut out),
        _ if leader != actor.entity => {
            reply("Only the group's leader can do that.".into(), &mut out)
        }
        Some(who) if who == actor.entity => {
            reply("You can't group with yourself.".into(), &mut out)
        }
        Some(who) if groups.get(who).is_ok_and(|(_, g)| g.leader == actor.entity) => {
            // Already in: put them out.
            let name = name_of(&characters, who);
            out.push(Outgoing::line(
                who,
                format!("{} puts you out of the group.", actor.name),
            ));
            leave_group(who, &groups, &characters, &mut commands, &mut out);
            reply(format!("You put {name} out of the group."), &mut out);
        }
        Some(who) if groups.get(who).is_ok() => reply(
            format!(
                "{} is already in a group.",
                capitalized(&name_of(&characters, who))
            ),
            &mut out,
        ),
        Some(who) if following.get(who) != Ok(&Following(actor.entity)) => reply(
            format!(
                "{} isn't following you.",
                capitalized(&name_of(&characters, who))
            ),
            &mut out,
        ),
        Some(_) if members(actor.entity, &groups).len() >= progression().max_group => {
            reply("Your group is full.".into(), &mut out)
        }
        Some(who) => {
            if groups.get(actor.entity).is_err() {
                commands.entity(actor.entity).insert(InGroup {
                    leader: actor.entity,
                    joined: joined.next(),
                });
            }
            commands.entity(who).insert(InGroup {
                leader: actor.entity,
                joined: joined.next(),
            });
            let name = capitalized(&name_of(&characters, who));
            for member in members(actor.entity, &groups) {
                if member != actor.entity {
                    out.push(Outgoing::line(member, format!("{name} joins the group.")));
                }
            }
            reply(format!("{name} joins your group."), &mut out);
            out.push(Outgoing::line(
                who,
                format!("You join {}'s group.", actor.name),
            ));
            // The rest of the room, who aren't in it, see it too.
            let group = members(actor.entity, &groups);
            for (other, _, _, in_room) in &characters {
                if in_room.0 == actor.room && other != who && !group.contains(&other) {
                    out.push(Outgoing::line(
                        other,
                        format!("{name} joins {}'s group.", actor.name),
                    ));
                }
            }
        }
    }
    deliver(out, &mut outbox);
}

/// The group as `group` shows it, to `who`.
fn listing(
    who: Entity,
    characters: &Characters,
    groups: &Groups,
    sheets: &Sheets,
    defs: &SkillDefs,
    rooms: &RoomQuery,
) -> String {
    if groups.get(who).is_err() {
        return "You aren't in a group. Someone following you can join yours: 'group <name>'."
            .into();
    }
    let all = members(who, groups);
    let leader = all[0];
    let mut text = format!("{}'s group:", capitalized(&name_of(characters, leader)));
    for member in all {
        let Some(actor) = Actor::get(characters, member) else {
            continue;
        };
        let mut name = capitalized(&actor.name);
        if member == leader {
            name += " (leading)";
        }
        let vitals = sheets
            .get(member)
            .map(|(vitals, attributes, skills, base)| {
                let max = Maxima::of(attributes, skills, defs, base);
                format!(
                    "{:>4}/{:<4} health {:>4}/{:<4} stamina",
                    vitals.health.ceil() as i32,
                    max.health,
                    vitals.stamina.floor() as i32,
                    max.stamina
                )
            })
            .unwrap_or_default();
        let place = rooms
            .get(actor.room)
            .map_or(String::new(), |(room, _, _)| room.name.clone());
        text += &format!("\r\n  {name:<24} {vitals}  {place}");
    }
    text
}

/// `gtell <text>`: speak to everyone in your group, wherever they are.
fn gtell(
    In(inv): In<Invocation>,
    characters: Characters,
    groups: Groups,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let text = &inv.args;
    let out = if groups.get(actor.entity).is_err() {
        vec![Outgoing::line(actor.entity, "You aren't in a group.")]
    } else if text.is_empty() {
        vec![Outgoing::line(actor.entity, "Tell your group what?")]
    } else {
        members(actor.entity, &groups)
            .into_iter()
            .map(|member| {
                let line = if member == actor.entity {
                    format!("\x1b[1;32mYou tell the group, \"{text}\"\x1b[0m")
                } else {
                    format!(
                        "\x1b[1;32m{} tells the group, \"{text}\"\x1b[0m",
                        capitalized(&actor.name)
                    )
                };
                Outgoing::line(member, line)
            })
            .collect()
    };
    deliver(out, &mut outbox);
}

/// `disband`: the leader ends the group. Followers keep following.
fn disband(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    groups: Groups,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let out = match groups.get(actor.entity) {
        Err(_) => vec![Outgoing::line(actor.entity, "You aren't in a group.")],
        Ok((_, g)) if g.leader != actor.entity => vec![Outgoing::line(
            actor.entity,
            "Only the group's leader can disband it. ('follow self' to leave it.)",
        )],
        Ok(_) => members(actor.entity, &groups)
            .into_iter()
            .map(|member| {
                commands.entity(member).remove::<InGroup>();
                let line = if member == actor.entity {
                    "You disband the group.".to_string()
                } else {
                    format!("{} disbands the group.", capitalized(&actor.name))
                };
                Outgoing::line(member, line)
            })
            .collect(),
    };
    deliver(out, &mut outbox);
}

/// `autoassist [on | off]`: whether you join a fight a member of your group in the room is in.
/// On unless turned off; saved with the character.
fn autoassist(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    off: Query<(), With<NoAssist>>,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let on = off.get(actor.entity).is_err();
    let reply = match inv.args.trim().to_lowercase().as_str() {
        "" if on => {
            "You join your group's fights by yourself. ('autoassist off' to choose your own.)"
                .to_string()
        }
        "" => "You choose your own fights. ('autoassist on' to join your group's by yourself.)"
            .to_string(),
        word @ ("on" | "off") => {
            let on = word == "on";
            if on {
                commands.entity(actor.entity).remove::<NoAssist>();
            } else {
                commands.entity(actor.entity).insert(NoAssist);
            }
            if let Some(id) = actor.character_id
                && let Err(err) = db.set_autoassist(id, on)
            {
                error!("failed to save {}'s autoassist setting: {err}", actor.name);
            }
            if on {
                "You'll join your group's fights by yourself.".into()
            } else {
                "You'll choose your own fights.".into()
            }
        }
        _ => inv.usage_error(),
    };
    deliver(vec![Outgoing::line(actor.entity, reply)], &mut outbox);
}

/// Those who don't follow when the one they follow leaves: fighting, resting or dead.
type Busy = Or<(With<Fighting>, With<Resting>, With<Dead>)>;

/// Whoever someone follows walked out of the room: they go after them, the same way, with `go`,
/// unless they're fighting, resting or dead. Those following them follow in turn.
#[allow(clippy::too_many_arguments)]
fn follow_leaders(
    mut commands: Commands,
    mut events: MessageReader<GameEvent>,
    followers: Query<(Entity, &Following, &crate::world::InRoom)>,
    busy: Query<(), Busy>,
    characters: Characters,
    registry: Res<CommandRegistry>,
    mut outbox: ResMut<Outbox>,
) {
    let Some((spec, id)) = registry
        .find("go", Role::Player)
        .map(|(spec, id)| (spec.clone(), *id))
    else {
        return;
    };
    let mut out = Vec::new();
    for event in events.read() {
        let GameEvent::Left {
            who,
            room,
            way: Way::Walked(Some(dir)),
            ..
        } = event
        else {
            continue;
        };
        for (follower, Following(leader), in_room) in &followers {
            if leader != who || in_room.0 != *room || busy.get(follower).is_ok() {
                continue;
            }
            out.push(Outgoing::line(
                follower,
                format!("You follow {} {dir}.", name_of(&characters, *who)),
            ));
            commands.run_system_with(
                id,
                Invocation {
                    entity: follower,
                    command: spec.name,
                    usage: spec.usage,
                    args: dir.clone(),
                },
            );
        }
    }
    deliver(out, &mut outbox);
}

/// Following ends when the one followed is gone (logged off) or dead; a group whose leader is
/// gone passes to whoever joined first, and a group of one is over.
fn tidy(
    mut commands: Commands,
    following: Query<(Entity, &Following)>,
    groups: Groups,
    present: Query<(), Without<Dead>>,
    characters: Characters,
    mut outbox: ResMut<Outbox>,
) {
    let mut out = Vec::new();
    for (follower, Following(leader)) in &following {
        if characters.get(*leader).is_err() || present.get(*leader).is_err() {
            commands.entity(follower).remove::<Following>();
            out.push(Outgoing::line(
                follower,
                "You've lost the one you were following.",
            ));
        }
    }
    let mut by_leader: HashMap<Entity, Vec<(Entity, u64)>> = HashMap::new();
    for (member, g) in &groups {
        by_leader
            .entry(g.leader)
            .or_default()
            .push((member, g.joined));
    }
    for (leader, mut all) in by_leader {
        all.sort_by_key(|(_, joined)| *joined);
        let gone = characters.get(leader).is_err();
        let rest: Vec<(Entity, u64)> = all.into_iter().filter(|(e, _)| *e != leader).collect();
        if !gone && !rest.is_empty() {
            continue;
        }
        if rest.len() < 2 {
            // Only one left: the group is over.
            for (member, _) in rest {
                commands.entity(member).remove::<InGroup>();
                out.push(Outgoing::line(member, "Your group has broken up."));
            }
            if !gone {
                commands.entity(leader).remove::<InGroup>();
            }
            continue;
        }
        let new = rest[0].0;
        let name = name_of(&characters, new);
        for (member, joined) in rest {
            commands.entity(member).insert(InGroup {
                leader: new,
                joined,
            });
            let line = if member == new {
                "Your leader is gone; you lead the group now.".to_string()
            } else {
                format!("Your leader is gone; {name} leads the group now.")
            };
            out.push(Outgoing::line(member, line));
        }
    }
    deliver(out, &mut outbox);
}

#[cfg(test)]
mod tests {
    use bevy::ecs::message::Messages;
    use bevy::ecs::system::RunSystemOnce;

    use super::*;
    use crate::world::{InRoom, Player};

    /// Who ran `go`, and which way.
    #[derive(Resource, Default)]
    struct Went(Vec<(Entity, String)>);

    fn went(In(inv): In<Invocation>, mut went: ResMut<Went>) {
        went.0.push((inv.entity, inv.args));
    }

    fn player(world: &mut World, name: &str, room: Entity) -> Entity {
        world
            .spawn((
                Player {
                    name: name.into(),
                    character_id: 1,
                    account: name.to_lowercase(),
                    role: Role::Player,
                    race: "human".into(),
                    background: String::new(),
                    description: String::new(),
                },
                InRoom(room),
            ))
            .id()
    }

    fn world() -> (World, Entity, Entity) {
        let mut world = World::new();
        world.init_resource::<Outbox>();
        world.init_resource::<Went>();
        world.init_resource::<Messages<GameEvent>>();
        let go = world.register_system(went);
        world.insert_resource(CommandRegistry::of(vec![(
            CommandSpec::new("go", Category::Movement),
            go,
        )]));
        let (market, street) = (world.spawn_empty().id(), world.spawn_empty().id());
        (world, market, street)
    }

    fn leave(world: &mut World, who: Entity, room: Entity, dir: &str) {
        world.write_message(GameEvent::Left {
            who,
            name: String::new(),
            room,
            way: Way::Walked(Some(dir.into())),
            witnesses: None,
        });
    }

    #[test]
    fn followers_walk_after_whoever_they_follow() {
        let (mut world, market, street) = world();
        let ada = player(&mut world, "Ada", street);
        let bob = player(&mut world, "Bob", market);
        let cal = player(&mut world, "Cal", market);
        let dee = player(&mut world, "Dee", street);
        world.entity_mut(bob).insert(Following(ada));
        // Cal follows Ada too, but is fighting; Dee follows her but wasn't in the market.
        world
            .entity_mut(cal)
            .insert((Following(ada), Fighting(ada)));
        world.entity_mut(dee).insert(Following(ada));
        leave(&mut world, ada, market, "north");
        world.run_system_once(follow_leaders).unwrap();
        world.flush();
        assert_eq!(world.resource::<Went>().0, vec![(bob, "north".to_string())]);
    }

    #[test]
    fn a_group_whose_leader_is_gone_passes_to_who_joined_first() {
        let (mut world, market, _) = world();
        let ada = player(&mut world, "Ada", market);
        let bob = player(&mut world, "Bob", market);
        let cal = player(&mut world, "Cal", market);
        for (joined, member) in [ada, cal, bob].into_iter().enumerate() {
            world.entity_mut(member).insert(InGroup {
                leader: ada,
                joined: joined as u64,
            });
        }
        world.entity_mut(bob).insert(Following(ada));
        world.despawn(ada);
        world.run_system_once(tidy).unwrap();
        world.flush();
        assert_eq!(world.get::<InGroup>(cal).map(|g| g.leader), Some(cal));
        assert_eq!(world.get::<InGroup>(bob).map(|g| g.leader), Some(cal));
        assert!(
            world.get::<Following>(bob).is_none(),
            "lost the one he followed"
        );
        // Then Bob logs off, and Cal is a group of one: it's over.
        world.despawn(bob);
        world.run_system_once(tidy).unwrap();
        world.flush();
        assert!(world.get::<InGroup>(cal).is_none());
    }
}
