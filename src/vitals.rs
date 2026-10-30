//! Vitals: current health and stamina, how they recover, and the status prompt that shows them.
//! Players and mobs share them, and recover by the same rules.
//!
//! The maximums come from attributes (health 8 + Body, stamina 8 + Willpower), or a mob
//! template's own health, plus what passive skills add. Current values recover on the heartbeat,
//! worked out from elapsed time: three times as fast while resting, and not at all in a fight.
//! Players keep recovering while offline: their vitals are saved with the time they were saved,
//! and caught up at the next login.

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::combat::{ConditionDefs, Conditions, Dead, Fighting, the};
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::events::GameEvent;
use crate::mobs::Mob;
use crate::net::{self, Connection};
use crate::output::{
    Actor, Characters, Outbox, Outgoing, Puppeting, Reply, deliver, reply_line, targets_online,
};
use crate::skills::{SkillDefs, Skills, boosts};
use crate::targeting::{Keywords, Target};
use crate::time::heartbeat;
use crate::world::Player;

/// How often everyone's vitals are saved, in seconds, besides at logout.
const SAVE_EVERY_SECS: f64 = 60.0;

/// The prompt a character has until they set their own.
pub const DEFAULT_PROMPT: &str = "<%h/%Hhp %s/%Sst> ";
const MAX_PROMPT_LEN: usize = 80;

pub struct VitalsPlugin;

impl Plugin for VitalsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (recover, save_vitals).chain().run_if(heartbeat()))
            .add_systems(
                PostUpdate,
                (
                    show_prompts
                        .after(crate::output::flush_outbox)
                        .before(net::flush_output),
                    save_on_logout.before(net::cleanup_disconnected),
                ),
            )
            .add_command(
                CommandSpec::new("prompt", Category::Character)
                    .usage("prompt [<format>]")
                    .summary("set your prompt: %h/%H health, %s/%S stamina, %% a percent sign"),
                prompt_cmd,
            )
            .add_command(
                CommandSpec::new("rest", Category::Character)
                    .shortcuts(&["sit"])
                    .summary("sit down and rest, recovering three times as fast"),
                rest,
            )
            .add_command(
                CommandSpec::new("stand", Category::Character)
                    .shortcuts(&["wake"])
                    .summary("get up from resting"),
                stand,
            )
            .add_command(
                CommandSpec::new("setvitals", Category::Admin)
                    .usage("setvitals <player> <health> [<stamina>]")
                    .summary("set an online player's current health and stamina, for testing")
                    .permission(Permission::Events),
                setvitals_cmd,
            );
    }
}

/// A mob's health from its template, instead of the usual 8 + Body: a rat is small. Skills
/// like Toughness still add to it.
#[derive(Component, Debug, Clone, Copy)]
pub struct BaseHealth(pub i32);

/// The most health and stamina someone can have: from their attributes (8 + Body, 8 +
/// Willpower) or their `BaseHealth`, plus what passive skills like Toughness and Endurance add.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Maxima {
    pub health: i32,
    pub stamina: i32,
}

impl Maxima {
    pub fn of(
        attributes: &Attributes,
        skills: &Skills,
        defs: &SkillDefs,
        base: Option<&BaseHealth>,
    ) -> Self {
        let boosts = boosts(skills, defs);
        let health = base.map_or_else(|| attributes.health(), |b| b.0);
        // Whole points: a passive's fraction counts once it adds up to one.
        Self {
            health: health + boosts.max_health.floor() as i32,
            stamina: attributes.stamina() + boosts.max_stamina.floor() as i32,
        }
    }

    /// From attributes alone, with no skills.
    #[cfg(test)]
    pub fn bare(attributes: &Attributes) -> Self {
        Self {
            health: attributes.health(),
            stamina: attributes.stamina(),
        }
    }
}

/// Current health and stamina, which can be fractional while recovering. Lives on the player or
/// mob entity, next to the `Attributes` and `Skills` that set their maximums.
#[derive(Component, Clone, Debug, PartialEq)]
#[require(Conditions)]
pub struct Vitals {
    pub health: f32,
    pub stamina: f32,
}

impl Vitals {
    /// Full health and stamina.
    pub fn full(max: Maxima) -> Self {
        Self {
            health: max.health as f32,
            stamina: max.stamina as f32,
        }
    }

    /// Recover for `secs` seconds at rest, never past the maximums.
    pub fn recover(&mut self, max: Maxima, secs: f32) {
        let (max_health, max_stamina) = (max.health as f32, max.stamina as f32);
        let secs = secs.max(0.0);
        let r = crate::rules::rules();
        let (health_secs, stamina_secs) = (
            r.health_recovery_minutes * 60.0,
            r.stamina_recovery_minutes * 60.0,
        );
        self.health = (self.health + max_health * secs / health_secs).min(max_health);
        self.stamina = (self.stamina + max_stamina * secs / stamina_secs).min(max_stamina);
    }

    /// Keep within 0 and the maximums, as after a maximum goes down.
    pub fn clamp(&mut self, max: Maxima) {
        self.health = self.health.clamp(0.0, max.health as f32);
        self.stamina = self.stamina.clamp(0.0, max.stamina as f32);
    }

    /// Vitals as saved: `health` and `stamina` at unix time `saved_at`, caught up to `now`.
    /// Missing values mean full.
    pub fn restored(
        max: Maxima,
        saved: Option<(f32, f32)>,
        saved_at: Option<i64>,
        now: i64,
    ) -> Self {
        let Some((health, stamina)) = saved else {
            return Self::full(max);
        };
        let mut vitals = Self { health, stamina };
        vitals.clamp(max);
        let away = saved_at.map_or(0, |at| now - at);
        vitals.recover(max, away as f32);
        vitals
    }
}

/// The text of a prompt, filled in from a character's vitals.
pub fn render_prompt(format: &str, vitals: &Vitals, max: Maxima) -> String {
    let mut text = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => text += &(vitals.health.floor() as i32).to_string(),
            Some('H') => text += &max.health.to_string(),
            Some('s') => text += &(vitals.stamina.floor() as i32).to_string(),
            Some('S') => text += &max.stamina.to_string(),
            Some('%') => text.push('%'),
            Some(other) => {
                text.push('%');
                text.push(other);
            }
            None => text.push('%'),
        }
    }
    text
}

/// Everyone's vitals, players and mobs, and whether they're resting or fighting.
type Recovering<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Vitals,
        &'static Attributes,
        &'static Skills,
        Option<&'static BaseHealth>,
        Has<Resting>,
        Has<Fighting>,
        Option<&'static Conditions>,
    ),
    Without<Dead>,
>;

/// Sitting down to rest. Walking, attacking or being attacked gets them up.
#[derive(Component, Debug)]
pub struct Resting;

/// A character's prompt format. Lives on the player entity.
#[derive(Component, Clone, Debug)]
pub struct PromptFormat(pub String);

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Recover everyone's vitals by the time since the last heartbeat: faster resting, and faster
/// for conditions that say so (`Recovery`: a meal's `nourished`).
fn recover(
    time: Res<Time>,
    mut last: Local<Option<f64>>,
    mut characters: Recovering,
    (defs, condition_defs): (Res<SkillDefs>, Res<ConditionDefs>),
) {
    let now = time.elapsed_secs_f64();
    let secs = last.map_or(0.0, |last| now - last) as f32;
    *last = Some(now);
    for (mut vitals, attributes, skills, base, resting, fighting, conditions) in &mut characters {
        let max = Maxima::of(attributes, skills, &defs, base);
        let before = vitals.clone();
        let mut after = before.clone();
        after.clamp(max);
        let pace = match (fighting, resting) {
            (true, _) => 0.0,
            (false, true) => crate::rules::rules().rest_speedup,
            (false, false) => 1.0,
        } * conditions.map_or(1.0, |c| c.recovery(&condition_defs) as f32);
        after.recover(max, secs * pace);
        if after != before {
            *vitals = after;
        }
    }
}

/// Save everyone's vitals about once a minute, so a crash loses little.
fn save_vitals(
    time: Res<Time>,
    mut last: Local<f64>,
    players: Query<(&Player, &Vitals, &Conditions)>,
    db: Res<Database>,
) {
    let now = time.elapsed_secs_f64();
    if now - *last < SAVE_EVERY_SECS {
        return;
    }
    *last = now;
    let at = unix_now();
    for (player, vitals, conditions) in &players {
        save(&db, player, vitals, conditions, now, at);
    }
}

fn save_on_logout(
    players: Query<(&Connection, &Player, &Vitals, &Conditions)>,
    db: Res<Database>,
    time: Res<Time>,
) {
    let at = unix_now();
    for (conn, player, vitals, conditions) in &players {
        if conn.closing {
            save(&db, player, vitals, conditions, time.elapsed_secs_f64(), at);
        }
    }
}

/// Save a player's vitals, and the conditions on them that last in time.
fn save(
    db: &Database,
    player: &Player,
    vitals: &Vitals,
    conditions: &Conditions,
    now: f64,
    at: i64,
) {
    if let Err(err) = db.save_vitals(player.character_id, vitals.health, vitals.stamina, at) {
        error!("failed to save vitals for {}: {err}", player.name);
    }
    if let Err(err) = db.save_conditions(player.character_id, &conditions.to_saved(now, at)) {
        error!("failed to save conditions for {}: {err}", player.name);
    }
}

/// A player, as their prompt shows them, and what they're puppeting, if anything.
type Prompted = (
    &'static Vitals,
    &'static Attributes,
    &'static Skills,
    &'static PromptFormat,
    Option<&'static Puppeting>,
);

/// A puppeted mob, as its puppeteer's prompt shows it.
type Puppet = (
    &'static Vitals,
    &'static Attributes,
    &'static Skills,
    Option<&'static BaseHealth>,
    &'static Mob,
);

/// Give each player's connection their prompt, filled in fresh, just before output is sent. A
/// builder puppeting a mob sees the mob's vitals, and whose they are.
fn show_prompts(
    mut players: Query<(&mut Connection, Prompted)>,
    puppets: Query<Puppet>,
    defs: Res<SkillDefs>,
) {
    for (mut conn, (vitals, attributes, skills, format, puppeting)) in &mut players {
        let prompt = match puppeting.and_then(|p| puppets.get(p.0).ok()) {
            Some((vitals, attributes, skills, base, mob)) => {
                let max = Maxima::of(attributes, skills, &defs, base);
                format!(
                    "[{}] {}",
                    the(&mob.name),
                    render_prompt(&format.0, vitals, max)
                )
            }
            None => {
                let max = Maxima::of(attributes, skills, &defs, None);
                render_prompt(&format.0, vitals, max)
            }
        };
        conn.set_prompt(prompt);
    }
}

/// `prompt [<format>]`: set the prompt, or go back to the default.
fn prompt_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut formats: Query<&mut PromptFormat>,
    db: Res<Database>,
) {
    let (Some(actor), Ok(mut format)) = (
        Actor::get(&players, inv.entity),
        formats.get_mut(inv.entity),
    ) else {
        return;
    };
    // Keep a space after the prompt, so typing doesn't run into it.
    let wanted = match inv.args.trim_end() {
        "" => DEFAULT_PROMPT.to_string(),
        text => format!("{text} "),
    };
    let reply: Reply = if wanted.chars().count() > MAX_PROMPT_LEN {
        Err(format!("Keep it under {MAX_PROMPT_LEN} characters."))
    } else {
        let saved = (wanted != DEFAULT_PROMPT).then_some(wanted.as_str());
        match actor
            .character_id
            .map_or(Ok(()), |id| db.set_prompt(id, saved))
        {
            Ok(()) => {
                let custom = saved.is_some();
                format.0 = wanted.clone();
                Ok(if custom {
                    "Prompt set.".into()
                } else {
                    format!("Prompt back to the default, {}", DEFAULT_PROMPT.trim_end())
                })
            }
            Err(err) => {
                error!("failed to save prompt for {}: {err}", actor.name);
                Err("Something went wrong saving that. Please try again.".into())
            }
        }
    };
    deliver(vec![reply_line(inv.entity, reply)], &mut outbox);
}

/// `rest`: sit down to recover faster.
fn rest(
    In(inv): In<Invocation>,
    mut commands: Commands,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    state: Query<(Has<Resting>, Has<Fighting>)>,
    mut events: MessageWriter<GameEvent>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let mut out = Vec::new();
    match state.get(inv.entity) {
        Ok((_, true)) => out.push(Outgoing::line(inv.entity, "You can't rest in a fight!")),
        Ok((true, _)) => out.push(Outgoing::line(inv.entity, "You're already resting.")),
        _ => {
            commands.entity(inv.entity).insert(Resting);
            events.write(GameEvent::Rested { who: inv.entity });
            out.push(Outgoing::line(inv.entity, "You sit down and rest."));
            out.push(Outgoing::Room {
                room: actor.room,
                except: inv.entity,
                text: format!("{} sits down to rest.\r\n", actor.name),
            });
        }
    }
    deliver(out, &mut outbox);
}

/// `stand`: get up from resting.
fn stand(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut outbox: ResMut<Outbox>,
    resting: Query<(), With<Resting>>,
) {
    let text = if resting.get(inv.entity).is_ok() {
        commands.entity(inv.entity).remove::<Resting>();
        "You stand up."
    } else {
        "You're already on your feet."
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}

/// `setvitals <player> <health> [<stamina>]`: an admin sets someone's current vitals.
fn setvitals_cmd(
    In(inv): In<Invocation>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    keywords: Query<&Keywords>,
    mut sheets: Query<(&mut Vitals, &Attributes, &Skills)>,
    defs: Res<SkillDefs>,
) {
    let mut out = Vec::new();
    let reply = set_vitals(&inv, &players, &keywords, &mut sheets, &defs, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut outbox);
}

fn set_vitals(
    inv: &Invocation,
    players: &Characters,
    keywords: &Query<&Keywords>,
    sheets: &mut Query<(&mut Vitals, &Attributes, &Skills)>,
    defs: &SkillDefs,
    out: &mut Vec<Outgoing>,
) -> Reply {
    let mut parts = inv.args.split_whitespace();
    let number = |word: Option<&str>| word.and_then(|w| w.parse::<f32>().ok());
    let (Some(target), Some(health), stamina, None) = (
        parts.next(),
        number(parts.next()),
        parts.next(),
        parts.next(),
    ) else {
        return Err(inv.usage_error());
    };
    let target = Target::parse(target).ok_or_else(|| inv.usage_error())?;
    let actor = target
        .find_one(inv.entity, targets_online(players, keywords))
        .and_then(|e| Actor::get(players, e))
        .ok_or_else(|| format!("No one matching {} is online.", target.describe()))?;
    let (mut vitals, attributes, skills) = sheets
        .get_mut(actor.entity)
        .map_err(|_| format!("{} has no vitals loaded.", actor.name))?;
    let max = Maxima::of(attributes, skills, defs, None);
    vitals.health = health;
    if let Some(stamina) = number(stamina) {
        vitals.stamina = stamina;
    }
    vitals.clamp(max);
    let now = format!(
        "{}/{} health, {}/{} stamina",
        vitals.health.floor(),
        max.health,
        vitals.stamina.floor(),
        max.stamina
    );
    if actor.entity != inv.entity {
        out.push(Outgoing::line(
            actor.entity,
            format!("You are now at {now}."),
        ));
    }
    Ok(format!("{} is now at {now}.", actor.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::Attribute;

    fn sheet() -> Attributes {
        // Health 10, stamina 9.
        let mut attributes = Attributes::default();
        attributes.set(Attribute::Body, 2);
        attributes.set(Attribute::Willpower, 1);
        attributes
    }

    #[test]
    fn recovery_follows_elapsed_time_and_stops_at_full() {
        let attributes = sheet();
        let mut vitals = Vitals {
            health: 0.0,
            stamina: 0.0,
        };
        // A tenth of the way to full health, half of stamina.
        vitals.recover(Maxima::bare(&attributes), 60.0);
        assert!((vitals.health - 1.0).abs() < 1e-4, "{vitals:?}");
        assert!((vitals.stamina - 4.5).abs() < 1e-4, "{vitals:?}");
        // Many small steps add up to one big one.
        let mut stepped = Vitals {
            health: 0.0,
            stamina: 0.0,
        };
        for _ in 0..60 {
            stepped.recover(Maxima::bare(&attributes), 1.0);
        }
        assert!((stepped.health - vitals.health).abs() < 1e-3);
        // Never past the maximum.
        vitals.recover(Maxima::bare(&attributes), 100_000.0);
        assert_eq!(vitals, Vitals::full(Maxima::bare(&attributes)));
    }

    #[test]
    fn offline_time_counts_toward_recovery() {
        let attributes = sheet();
        assert_eq!(
            Vitals::restored(Maxima::bare(&attributes), None, None, 1000),
            Vitals::full(Maxima::bare(&attributes))
        );
        let back = Vitals::restored(
            Maxima::bare(&attributes),
            Some((2.0, 0.0)),
            Some(1000),
            1060,
        );
        assert!((back.health - 3.0).abs() < 1e-4, "{back:?}");
        assert!((back.stamina - 4.5).abs() < 1e-4, "{back:?}");
        // Saved above a maximum that has since gone down: clamped.
        let over = Vitals::restored(
            Maxima::bare(&attributes),
            Some((50.0, 50.0)),
            Some(1000),
            1000,
        );
        assert_eq!(over, Vitals::full(Maxima::bare(&attributes)));
    }

    #[test]
    fn a_base_health_stands_in_for_body() {
        let max = Maxima::of(
            &sheet(),
            &Skills::default(),
            &SkillDefs::default(),
            Some(&BaseHealth(3)),
        );
        assert_eq!(
            max,
            Maxima {
                health: 3,
                stamina: 9
            }
        );
    }

    #[test]
    fn mobs_recover_too_but_not_in_a_fight() {
        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(SkillDefs::default());
        world.init_resource::<ConditionDefs>();
        let hurt = Vitals {
            health: 0.0,
            stamina: 0.0,
        };
        let rat = world
            .spawn((hurt.clone(), sheet(), Skills::default(), BaseHealth(3)))
            .id();
        let fighter = world
            .spawn((hurt.clone(), sheet(), Skills::default(), BaseHealth(3)))
            .id();
        world.entity_mut(fighter).insert(Fighting(rat));
        let recover = world.register_system(recover);
        world.run_system(recover).unwrap();
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(1000));
        world.run_system(recover).unwrap();
        // Full, at its own health rather than 8 + Body.
        assert_eq!(
            world.get::<Vitals>(rat),
            Some(&Vitals {
                health: 3.0,
                stamina: 9.0
            })
        );
        assert_eq!(world.get::<Vitals>(fighter), Some(&hurt));
    }

    #[test]
    fn a_meal_speeds_recovery_while_its_nourishment_lasts() {
        use crate::combat::{Lasts, defs_on_disk};
        let mut world = World::new();
        world.insert_resource(Time::<()>::default());
        world.insert_resource(SkillDefs::default());
        let defs = defs_on_disk();
        world.insert_resource(defs_on_disk());
        let hurt = Vitals {
            health: 0.0,
            stamina: 0.0,
        };
        let plain = world.spawn((hurt.clone(), sheet(), Skills::default())).id();
        // Nourished at 4: a quarter faster for each point, twice as fast.
        let mut fed = Conditions::default();
        fed.apply(&defs, "nourished", 4, Lasts::Until(600.0));
        // A lesser meal after changes nothing.
        fed.apply(&defs, "nourished", 1, Lasts::Until(60.0));
        assert_eq!(fed.recovery(&defs), 2.0);
        let fed = world
            .spawn((hurt.clone(), sheet(), Skills::default(), fed))
            .id();
        let recover = world.register_system(recover);
        world.run_system(recover).unwrap();
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(20));
        world.run_system(recover).unwrap();
        let health = |world: &World, who| world.get::<Vitals>(who).unwrap().health;
        // Health 10 refills in 600 seconds: 20 seconds is a third of a point, or two thirds fed.
        assert!((health(&world, plain) - 1.0 / 3.0).abs() < 1e-4);
        assert!((health(&world, fed) - 2.0 / 3.0).abs() < 1e-4);
        // Its nourishment gone, they recover alike from here.
        world.get_mut::<Conditions>(fed).unwrap().0.clear();
        world
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(20));
        world.run_system(recover).unwrap();
        assert!((health(&world, fed) - health(&world, plain) - 1.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn prompts_fill_in_placeholders() {
        let attributes = sheet();
        let vitals = Vitals {
            health: 7.9,
            stamina: 3.2,
        };
        assert_eq!(
            render_prompt(DEFAULT_PROMPT, &vitals, Maxima::bare(&attributes)),
            "<7/10hp 3/9st> "
        );
        assert_eq!(
            render_prompt("HP:%h %% %x%", &vitals, Maxima::bare(&attributes)),
            "HP:7 % %x%"
        );
    }
}
