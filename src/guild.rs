//! The Adventurers' Guild: membership, its metal ranks, and the rank tags members wear (see
//! `docs/quests.md#guild-ranks`).
//!
//! Joining is free, at the clerk: a character becomes a Bone member and is handed a Bone tag.
//! Rank rises with merit (from contracts, once there are any, or a GM's `guild merit`): on its
//! own up to Iron, and by promotion from Silver up. Rank never drops. What a member's rank opens
//! is tested with `GuildRank` in the shared requirement language, and only counts while they wear
//! a guild tag: taking it off makes them an anonymous traveller. The guild can expel a member,
//! who keeps their rank on record and can buy their way back in. The ranks, and what the clerk
//! charges, are data in `assets/guild.ron`.

use std::collections::HashSet;

use bevy::prelude::*;
use serde::Deserialize;

use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, MembershipRecord, Permission};
use crate::items::{Item, ItemLocation, ItemQuery, ItemTemplates};
use crate::mobs::{MobQuery, MobTemplates};
use crate::money::{self, Purse};
use crate::output::{Actor, Characters, Outbox, Outgoing, RoomQuery, deliver};
use crate::world::{ItemKey, MobKey, Player};

/// The guild's id, as membership is saved: other organisations can come later.
pub const GUILD: &str = "adventurers";

/// Where the guild's ranks and fees live, from where the server runs.
const GUILD_FILE: &str = "assets/guild.ron";

/// The tag every guild tag template carries.
pub const GUILD_TAG: &str = "guild_tag";

/// The guild, from `assets/guild.ron`.
#[derive(Deserialize, Debug)]
pub struct GuildDef {
    /// As said in a sentence: "the Adventurers' Guild".
    pub name: String,
    /// The mob template that keeps the guild's ledgers and tags, `zone:mob`.
    pub clerk: String,
    /// Copper for a new tag, when one is lost.
    pub tag_fee: u64,
    /// Lowest first.
    pub ranks: Vec<RankDef>,
}

#[derive(Deserialize, Debug)]
pub struct RankDef {
    pub id: String,
    /// "Bone", "Silver".
    pub name: String,
    /// The merit that makes a member eligible.
    pub merit: i64,
    /// Whether it takes a promotion as well as the merit.
    #[serde(default)]
    pub trial: bool,
    /// The tag's item template, `zone:item`.
    pub tag: String,
    /// Copper an expelled member of this rank pays to come back.
    pub reinstatement: u64,
}

impl GuildDef {
    /// Where the rank `id` stands, lowest first.
    pub fn rank(&self, id: &str) -> Option<usize> {
        self.ranks.iter().position(|r| r.id == id)
    }

    fn clerk_key(&self) -> MobKey {
        MobKey::parse(&self.clerk, "")
    }
}

static GUILD_DEF: std::sync::OnceLock<GuildDef> = std::sync::OnceLock::new();

/// The guild, read from `assets/guild.ron` the first time it's needed. A file that's missing or
/// wrong stops the server, as `progression.ron` does.
pub fn guild() -> &'static GuildDef {
    GUILD_DEF.get_or_init(|| {
        let text = std::fs::read_to_string(GUILD_FILE)
            .unwrap_or_else(|err| panic!("can't read {GUILD_FILE}: {err}"));
        crate::world::ron_options()
            .from_str(&text)
            .unwrap_or_else(|err| panic!("{GUILD_FILE} doesn't parse: {err}"))
    })
}

/// A character's standing in the guild, on the player.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct Membership {
    /// Where their rank stands in `GuildDef::ranks`.
    pub rank: usize,
    pub merit: i64,
    pub contracts: u32,
    pub expelled: bool,
}

impl Membership {
    /// A new Bone member.
    fn new() -> Self {
        Self {
            rank: 0,
            merit: 0,
            contracts: 0,
            expelled: false,
        }
    }

    /// From what was saved, if its rank still exists.
    pub fn from_record(record: &MembershipRecord) -> Option<Self> {
        Some(Self {
            rank: guild().rank(&record.rank)?,
            merit: record.merit,
            contracts: record.contracts,
            expelled: record.expelled,
        })
    }

    pub fn save(&self, character_id: Option<i64>, db: &Database) {
        let (Some(id), Some(rank)) = (character_id, guild().ranks.get(self.rank)) else {
            return;
        };
        let record = MembershipRecord {
            rank: rank.id.clone(),
            merit: self.merit,
            contracts: self.contracts,
            expelled: self.expelled,
        };
        if let Err(err) = db.save_membership(id, GUILD, &record) {
            error!("failed to save guild membership for character {id}: {err}");
        }
    }

    fn rank_name(&self) -> &'static str {
        guild().ranks.get(self.rank).map_or("", |r| r.name.as_str())
    }

    /// Add `merit` (or take it, never below nothing), promoting them through ranks that need no
    /// trial. Rank never drops. What to tell them.
    pub fn earn(&mut self, merit: i64) -> Vec<String> {
        let mut lines = Vec::new();
        let before = self.merit;
        self.merit = (self.merit + merit).max(0);
        while let Some(next) = guild().ranks.get(self.rank + 1) {
            if self.merit < next.merit {
                break;
            }
            if next.trial {
                if before < next.merit {
                    lines.push(format!(
                        "You've earned enough merit for {}: the guild will set you a trial.",
                        next.name
                    ));
                }
                break;
            }
            self.rank += 1;
            lines.push(format!(
                "\x1b[1;32mYou've been raised to {} in the guild! The clerk has your new tag \
                 ('guild tag').\x1b[0m",
                next.name
            ));
        }
        lines
    }
}

/// The rank a member shows: theirs, while they wear a guild tag and haven't been expelled.
/// `GuildRank` conditions, `who` and anyone checking a tag look at this.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShownRank(pub usize);

pub struct GuildPlugin;

impl Plugin for GuildPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, show_ranks)
            .add_command(
                CommandSpec::new("join", Category::Character)
                    .usage("join guild")
                    .summary("become a member of the Adventurers' Guild, at its clerk"),
                join,
            )
            .add_command(
                CommandSpec::new("guild", Category::Character)
                    .usage("guild [tag|record]")
                    .summary(
                        "your standing in the guild; 'guild tag' for a new tag, at the clerk; \
                         'guild record' for the contracts you've taken",
                    ),
                guild_cmd,
            )
            .add_command(
                CommandSpec::new("reinstate", Category::Character)
                    .summary("buy your way back into the guild after being expelled, at the clerk"),
                reinstate,
            );
    }
}

/// A player, and their standing in the guild and what they show of it.
type Standing = (
    Entity,
    Option<&'static Membership>,
    Option<&'static ShownRank>,
);

/// Keep `ShownRank` on everyone wearing a guild tag who is a member in good standing, and off
/// everyone else.
fn show_ranks(
    mut commands: Commands,
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    players: Query<Standing, With<Player>>,
) {
    let wearing: HashSet<Entity> = items
        .iter()
        .filter_map(|(_, item, location)| match location {
            ItemLocation::Worn(who, _) if is_tag(item, &templates) => Some(*who),
            _ => None,
        })
        .collect();
    for (who, membership, shown) in &players {
        let rank = membership
            .filter(|m| !m.expelled && wearing.contains(&who))
            .map(|m| m.rank);
        match (rank, shown) {
            (Some(rank), Some(ShownRank(now))) if rank == *now => {}
            (Some(rank), _) => {
                commands.entity(who).insert(ShownRank(rank));
            }
            (None, Some(_)) => {
                commands.entity(who).remove::<ShownRank>();
            }
            (None, None) => {}
        }
    }
}

fn is_tag(item: &Item, templates: &ItemTemplates) -> bool {
    templates
        .0
        .get(&item.key)
        .is_some_and(|def| def.tags.iter().any(|t| t == GUILD_TAG))
}

/// The guild tags `who` has on them, at any depth: held, worn, or in their bags.
fn tags_on<'a>(
    who: Entity,
    items: &'a ItemQuery,
    templates: &ItemTemplates,
) -> Vec<(Entity, &'a Item)> {
    let carried = |mut entity: Entity| {
        for _ in 0..32 {
            match items.get(entity).map(|(_, _, location)| *location) {
                Ok(ItemLocation::Inside(container)) => entity = container,
                Ok(location) => return location.holder() == Some(who),
                Err(_) => return false,
            }
        }
        false
    };
    items
        .iter()
        .filter(|(entity, item, _)| is_tag(item, templates) && carried(*entity))
        .map(|(entity, item, _)| (entity, item))
        .collect()
}

/// The guild's clerk, if one is in `room`: their entity and name.
pub fn clerk_in(room: Entity, mobs: &MobQuery) -> Option<(Entity, String)> {
    let key = guild().clerk_key();
    mobs.iter()
        .find(|(_, mob, in_room)| in_room.0 == room && mob.key == key)
        .map(|(entity, mob, _)| (entity, mob.name.clone()))
}

/// Where to find the clerk, for those who aren't with them: "Marin Harrow".
fn clerk_name(templates: &MobTemplates) -> String {
    templates.0.get(&guild().clerk_key()).map_or_else(
        || "the guild's clerk".into(),
        |def| crate::combat::the(&def.name),
    )
}

/// Hand `actor` a new tag for `rank`: into a free hand, or at their feet. Real at once, as
/// anything a player is given is.
#[allow(clippy::too_many_arguments)]
fn give_tag(
    actor: &Actor,
    rank: usize,
    commands: &mut Commands,
    items: &ItemQuery,
    templates: &ItemTemplates,
    rooms: &RoomQuery,
    db: &Database,
) -> String {
    let Some(def) = guild().ranks.get(rank) else {
        return String::new();
    };
    let key = ItemKey::parse(&def.tag, "");
    let mut held = crate::items::hands_full(actor.entity, items);
    crate::items::hand_new(actor.into(), key, &mut held, commands, templates, rooms, db)
}

/// Take away every guild tag `who` has on them.
fn take_tags(
    who: Entity,
    commands: &mut Commands,
    items: &ItemQuery,
    templates: &ItemTemplates,
    db: &Database,
) {
    for (entity, item) in tags_on(who, items, templates) {
        crate::healing::used_up(commands, db, entity, item);
    }
}

/// `join guild`: become a Bone member, in front of the clerk.
#[allow(clippy::too_many_arguments)]
fn join(
    In(inv): In<Invocation>,
    mut commands: Commands,
    characters: Characters,
    mobs: MobQuery,
    mob_templates: Res<MobTemplates>,
    memberships: Query<&Membership>,
    items: ItemQuery,
    templates: Res<ItemTemplates>,
    rooms: RoomQuery,
    db: Res<Database>,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&characters, inv.entity) else {
        return;
    };
    let reply = if !inv.args.eq_ignore_ascii_case("guild") {
        "Join what? ('join guild')".to_string()
    } else if actor.character_id.is_none() {
        "Only players can join the guild.".to_string()
    } else {
        match (clerk_in(actor.room, &mobs), memberships.get(actor.entity)) {
            (None, _) => format!(
                "You'll have to see {} to join the guild.",
                clerk_name(&mob_templates)
            ),
            (Some((_, clerk)), Ok(m)) if m.expelled => format!(
                "{clerk} looks up from the ledger. \"You were put out of the guild. You can buy \
                 your way back in ('reinstate'), for {}.\"",
                money::coins(reinstatement(m))
            ),
            (Some((_, clerk)), Ok(m)) => format!(
                "{clerk} looks at you over the ledger. \"You're already one of ours: {}.\"",
                m.rank_name()
            ),
            (Some((_, clerk)), Err(_)) => {
                let membership = Membership::new();
                membership.save(actor.character_id, &db);
                let tag = give_tag(&actor, 0, &mut commands, &items, &templates, &rooms, &db);
                commands.entity(actor.entity).insert(membership);
                deliver(
                    vec![Outgoing::Room {
                        room: actor.room,
                        except: actor.entity,
                        text: format!("{} signs the guild's ledger.\r\n", actor.name),
                    }],
                    &mut outbox,
                );
                format!(
                    "{clerk} writes your name in the ledger and hands you {tag}. \"Welcome to \
                     {}. Wear it where people can see it: it's how they'll know you're one of \
                     ours.\"",
                    guild().name
                )
            }
        }
    };
    deliver(vec![Outgoing::line(actor.entity, reply)], &mut outbox);
}

/// What an expelled member pays to come back.
fn reinstatement(membership: &Membership) -> u64 {
    guild()
        .ranks
        .get(membership.rank)
        .map_or(0, |r| r.reinstatement)
}

/// What `guild` needs: everything about members, tags and the clerk.
#[derive(bevy::ecs::system::SystemParam)]
struct Ledger<'w, 's> {
    commands: Commands<'w, 's>,
    characters: Characters<'w, 's>,
    mobs: MobQuery<'w, 's>,
    mob_templates: Res<'w, MobTemplates>,
    memberships: Query<'w, 's, &'static mut Membership>,
    shown: Query<'w, 's, &'static ShownRank>,
    purses: Query<'w, 's, &'static mut Purse>,
    items: ItemQuery<'w, 's>,
    templates: Res<'w, ItemTemplates>,
    rooms: RoomQuery<'w, 's>,
    db: Res<'w, Database>,
    outbox: ResMut<'w, Outbox>,
    quests: Res<'w, crate::quests::Quests>,
}

/// `guild`: your standing in the guild. `guild tag`, at the clerk: a tag for your rank, free in
/// place of an old one, for a fee if you've lost yours. `guild record`: what the clerk's record
/// has against you (GMs: `guild record <player>`). GMs and admins also have `guild promote
/// <player>`, `guild merit <player> <n>`, `guild expel <player>` and `guild reinstate <player>`.
fn guild_cmd(In(inv): In<Invocation>, mut l: Ledger) {
    let Some(actor) = Actor::get(&l.characters, inv.entity) else {
        return;
    };
    let mut words = inv.args.split_whitespace();
    let first = words.next().map(str::to_lowercase);
    let mut out = Vec::new();
    let reply = match first.as_deref() {
        None => standing(&actor, &l),
        Some("tag") => new_tag(&actor, &mut l),
        Some("record") => {
            let whose = words.next();
            match whose {
                Some(name) if actor.role.can(Permission::Events) => {
                    match l.db.character_named(name) {
                        Ok(Some((id, name))) => record(id, &name, &l),
                        _ => format!("No one is called {name}."),
                    }
                }
                Some(_) => inv.usage_error(),
                None => match actor.character_id {
                    Some(id) => record(id, "you", &l),
                    None => String::new(),
                },
            }
        }
        Some(gm @ ("promote" | "merit" | "expel" | "reinstate"))
            if actor.role.can(Permission::Events) =>
        {
            let rest: Vec<&str> = words.collect();
            manage(gm, &rest, &mut l, &mut out)
        }
        Some(_) => inv.usage_error(),
    };
    out.insert(0, Outgoing::line(actor.entity, reply));
    deliver(out, &mut l.outbox);
}

/// `guild record`: the lines in Marin's record against a member: each contract accepted,
/// completed, failed or given up, and when. Never how far along they are.
fn record(character_id: i64, whom: &str, l: &Ledger) -> String {
    let lines = match l.db.guild_record(character_id) {
        Ok(lines) => lines,
        Err(err) => {
            error!("failed to read the guild's record for {character_id}: {err}");
            return "The record can't be read just now.".into();
        }
    };
    let clerk = crate::items::capitalized(&clerk_name(&l.mob_templates));
    if lines.is_empty() {
        return format!("{clerk}'s record has nothing against {whom}.");
    }
    let now = crate::time::unix_now();
    let mut text = format!("{clerk}'s record against {whom}:");
    for (quest, what, at) in lines {
        let name = l
            .quests
            .0
            .get(&quest)
            .map_or(quest.as_str(), |q| q.name.as_str());
        text += &format!(
            "\r\n  {} {name}, {}.",
            crate::items::capitalized(&what),
            ago(now - at)
        );
    }
    text
}

/// How long ago, by the day: "today", "yesterday", "3 days ago".
fn ago(secs: i64) -> String {
    match secs / 86_400 {
        ..=0 => "today".into(),
        1 => "yesterday".into(),
        days => format!("{days} days ago"),
    }
}

/// What `guild` says of someone's standing.
fn standing(actor: &Actor, l: &Ledger) -> String {
    let Ok(m) = l.memberships.get(actor.entity) else {
        return format!(
            "You aren't a member of {}. {} can sign you up ('join guild').",
            guild().name,
            crate::items::capitalized(&clerk_name(&l.mob_templates))
        );
    };
    if m.expelled {
        return format!(
            "You were expelled from {} at {} rank. {} will take you back for {} ('reinstate').",
            guild().name,
            m.rank_name(),
            crate::items::capitalized(&clerk_name(&l.mob_templates)),
            money::coins(reinstatement(m))
        );
    }
    let mut text = format!(
        "You're a {} member of {}, with {} merit and {} contract{} done.",
        m.rank_name(),
        guild().name,
        m.merit,
        m.contracts,
        if m.contracts == 1 { "" } else { "s" }
    );
    if let Some(next) = guild().ranks.get(m.rank + 1) {
        text += &if m.merit < next.merit {
            format!(" {} comes at {} merit", next.name, next.merit)
        } else {
            format!(" You have the merit for {}", next.name)
        };
        text += if next.trial { ", and a trial." } else { "." };
    }
    if l.shown.get(actor.entity).is_err() {
        text += "\r\nYou aren't wearing your tag, so no one can tell.";
    }
    text
}

/// `guild tag`: at the clerk, a tag for your rank.
fn new_tag(actor: &Actor, l: &mut Ledger) -> String {
    let Some((_, clerk)) = clerk_in(actor.room, &l.mobs) else {
        return format!(
            "You'll have to see {} for a tag.",
            clerk_name(&l.mob_templates)
        );
    };
    let Ok(m) = l.memberships.get(actor.entity).cloned() else {
        return format!("{clerk} frowns. \"You're not one of ours. ('join guild')\"");
    };
    if m.expelled {
        return format!("{clerk} closes the ledger. \"Not until you're back in the guild.\"");
    }
    let mine = guild()
        .ranks
        .get(m.rank)
        .map(|r| ItemKey::parse(&r.tag, ""));
    let tags = tags_on(actor.entity, &l.items, &l.templates);
    if tags
        .iter()
        .any(|(_, item)| Some(&item.key) == mine.as_ref())
    {
        return format!("{clerk} glances at you. \"You've got your tag already.\"");
    }
    // An old tag is swapped for the new one free; a lost one costs.
    let swap = !tags.is_empty();
    if !swap {
        let fee = guild().tag_fee;
        let Ok(mut purse) = l.purses.get_mut(actor.entity) else {
            return String::new();
        };
        if !purse.pay(fee) {
            return format!(
                "{clerk} taps the ledger. \"A new tag is {}.\" You have {}.",
                money::coins(fee),
                money::coins(purse.0)
            );
        }
        purse.save(actor.character_id, &l.db);
    }
    take_tags(actor.entity, &mut l.commands, &l.items, &l.templates, &l.db);
    let tag = give_tag(
        actor,
        m.rank,
        &mut l.commands,
        &l.items,
        &l.templates,
        &l.rooms,
        &l.db,
    );
    if swap {
        format!("{clerk} takes your old tag and hands you {tag}.")
    } else {
        format!(
            "{clerk} notes it in the ledger and hands you {tag}, for {}.",
            money::coins(guild().tag_fee)
        )
    }
}

/// The GMs' `guild promote|merit|expel|reinstate <player> [<n>]`.
fn manage(gm: &str, rest: &[&str], l: &mut Ledger, out: &mut Vec<Outgoing>) -> String {
    let usage = "Usage: guild promote|expel|reinstate <player>, or guild merit <player> <n>";
    let Some(name) = rest.first() else {
        return usage.into();
    };
    let Some(target) = l
        .characters
        .iter()
        .find(|(_, p, _, _)| p.is_some_and(|p| p.name.eq_ignore_ascii_case(name)))
        .and_then(|(e, ..)| Actor::get(&l.characters, e))
    else {
        return format!("No one called {name} is in the game.");
    };
    let Ok(mut m) = l.memberships.get_mut(target.entity) else {
        return format!("{} isn't a member of the guild.", target.name);
    };
    let (reply, told) = match gm {
        "promote" => match guild().ranks.get(m.rank + 1) {
            None => (
                format!("{} is as high as the guild goes.", target.name),
                None,
            ),
            Some(next) => {
                m.rank += 1;
                m.merit = m.merit.max(next.merit);
                (
                    format!("{} is raised to {}.", target.name, next.name),
                    Some(format!(
                        "\x1b[1;32mYou've been raised to {} in the guild! The clerk has your \
                         new tag ('guild tag').\x1b[0m",
                        next.name
                    )),
                )
            }
        },
        "merit" => match rest.get(1).and_then(|n| n.parse::<i64>().ok()) {
            None => (usage.into(), None),
            Some(n) => {
                let lines = m.earn(n);
                (
                    format!("{} has {} merit now.", target.name, m.merit),
                    (!lines.is_empty()).then(|| lines.join("\r\n")),
                )
            }
        },
        "expel" if m.expelled => (format!("{} is already expelled.", target.name), None),
        "expel" => {
            m.expelled = true;
            take_tags(
                target.entity,
                &mut l.commands,
                &l.items,
                &l.templates,
                &l.db,
            );
            (
                format!(
                    "{} is expelled from the guild, and their tag taken.",
                    target.name
                ),
                Some(format!(
                    "\x1b[1;31mYou've been expelled from {}, and your tag taken. You can buy \
                     your way back in at the clerk ('reinstate').\x1b[0m",
                    guild().name
                )),
            )
        }
        _ if !m.expelled => (format!("{} isn't expelled.", target.name), None),
        _ => {
            m.expelled = false;
            let tag = give_tag(
                &target,
                m.rank,
                &mut l.commands,
                &l.items,
                &l.templates,
                &l.rooms,
                &l.db,
            );
            (
                format!("{} is back in the guild.", target.name),
                Some(format!(
                    "You've been taken back into {}, and given {tag}.",
                    guild().name
                )),
            )
        }
    };
    m.save(target.character_id, &l.db);
    if let Some(told) = told {
        out.push(Outgoing::line(target.entity, told));
    }
    reply
}

/// `reinstate`: an expelled member buys their way back in, at their old rank, in front of the
/// clerk.
fn reinstate(In(inv): In<Invocation>, mut l: Ledger) {
    let Some(actor) = Actor::get(&l.characters, inv.entity) else {
        return;
    };
    let reply = (|| {
        let Some((_, clerk)) = clerk_in(actor.room, &l.mobs) else {
            return format!(
                "You'll have to see {} about that.",
                clerk_name(&l.mob_templates)
            );
        };
        let Ok(m) = l.memberships.get(actor.entity).cloned() else {
            return format!("{clerk} frowns. \"You were never one of ours. ('join guild')\"");
        };
        if !m.expelled {
            return format!("{clerk} raises an eyebrow. \"You're in good standing.\"");
        }
        let fee = reinstatement(&m);
        let Ok(mut purse) = l.purses.get_mut(actor.entity) else {
            return String::new();
        };
        if !purse.pay(fee) {
            return format!(
                "{clerk} doesn't look up. \"Coming back costs {} at {}. Come back when you \
                 have it.\" You have {}.",
                money::coins(fee),
                m.rank_name(),
                money::coins(purse.0)
            );
        }
        purse.save(actor.character_id, &l.db);
        let back = Membership {
            expelled: false,
            ..m
        };
        back.save(actor.character_id, &l.db);
        let tag = give_tag(
            &actor,
            back.rank,
            &mut l.commands,
            &l.items,
            &l.templates,
            &l.rooms,
            &l.db,
        );
        let rank = back.rank_name();
        l.commands.entity(actor.entity).insert(back);
        format!(
            "You pay {}. {clerk} strikes the mark beside your name and hands you {tag}. \"Back \
             at {rank}. Don't make me do that again.\"",
            money::coins(fee)
        )
    })();
    deliver(vec![Outgoing::line(actor.entity, reply)], &mut l.outbox);
}

/// The rank `who` shows, for `GuildRank` conditions: theirs while they wear a tag.
pub fn shown_rank(who: Entity, shown: &Query<&ShownRank>) -> Option<usize> {
    shown.get(who).ok().map(|s| s.0)
}

/// The name of the rank `who` shows, for `who`.
pub fn shown_rank_name(who: Entity, shown: &Query<&ShownRank>) -> Option<&'static str> {
    let rank = shown_rank(who, shown)?;
    guild().ranks.get(rank).map(|r| r.name.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merit_raises_rank_up_to_the_first_trial() {
        let mut m = Membership::new();
        assert!(m.earn(49).is_empty());
        assert_eq!(m.rank, 0);
        let lines = m.earn(1);
        assert_eq!(m.rank, 1, "Copper at 50");
        assert!(lines[0].contains("Copper"));
        // Enough for Iron and Silver at once: Iron comes by itself, Silver needs a trial.
        let lines = m.earn(600);
        assert_eq!(m.rank, 2);
        assert!(lines.iter().any(|l| l.contains("Iron")));
        assert!(lines.iter().any(|l| l.contains("trial")));
        // Losing merit never loses rank.
        m.earn(-10_000);
        assert_eq!((m.rank, m.merit), (2, 0));
    }

    #[test]
    fn every_rank_has_a_tag_that_exists() {
        let zone: crate::world::ZoneDef = crate::world::ron_options()
            .from_str(&std::fs::read_to_string("assets/zones/riftwatch.zone.ron").unwrap())
            .unwrap();
        for rank in &guild().ranks {
            let key = ItemKey::parse(&rank.tag, "");
            assert_eq!(key.zone, "riftwatch", "{}", rank.id);
            let def = zone.items.get(&key.item);
            assert!(
                def.is_some_and(|d| d.tags.iter().any(|t| t == GUILD_TAG)),
                "{}'s tag [{}] is a guild tag",
                rank.id,
                rank.tag
            );
        }
        assert_eq!(guild().rank("silver"), Some(3));
    }
}
