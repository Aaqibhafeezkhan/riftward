//! Vitals: a character's current health and stamina, how they recover, and the status prompt that
//! shows them.
//!
//! The maximums come from attributes (health 8 + Body / 2, stamina 8 + Willpower / 2). Current
//! values recover on the heartbeat, worked out from elapsed time: three times as fast while
//! resting, and not at all in a fight. They keep recovering while the character is offline: their
//! vitals are saved with the time they were saved, and caught up at the next login.

use bevy::prelude::*;

use crate::attributes::Attributes;
use crate::combat::Fighting;
use crate::command::{AddCommand, Category, CommandSpec, Invocation};
use crate::db::{Database, Permission};
use crate::net::{self, Connection};
use crate::output::{Actor, Outgoing, PlayerQuery, Reply, deliver, reply_line, targets_online};
use crate::targeting::{Keywords, Target};
use crate::time::heartbeat;
use crate::world::Player;

/// Seconds for health to recover from nothing to full, at rest.
const HEALTH_RECOVERY_SECS: f32 = 600.0;
/// Seconds for stamina to recover from nothing to full, at rest.
const STAMINA_RECOVERY_SECS: f32 = 120.0;
/// How much faster vitals recover while resting.
const REST_SPEEDUP: f32 = 3.0;
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
                    show_prompts.before(net::flush_output),
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
                    .permission(Permission::ManageRoles),
                setvitals_cmd,
            );
    }
}

/// Current health and stamina, which can be fractional while recovering. Lives on the player
/// entity, next to the `Attributes` that set their maximums.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct Vitals {
    pub health: f32,
    pub stamina: f32,
}

impl Vitals {
    /// Full health and stamina.
    pub fn full(attributes: &Attributes) -> Self {
        Self {
            health: attributes.health() as f32,
            stamina: attributes.stamina() as f32,
        }
    }

    /// Recover for `secs` seconds at rest, never past the maximums.
    pub fn recover(&mut self, attributes: &Attributes, secs: f32) {
        let (max_health, max_stamina) = (attributes.health() as f32, attributes.stamina() as f32);
        let secs = secs.max(0.0);
        self.health = (self.health + max_health * secs / HEALTH_RECOVERY_SECS).min(max_health);
        self.stamina = (self.stamina + max_stamina * secs / STAMINA_RECOVERY_SECS).min(max_stamina);
    }

    /// Keep within 0 and the maximums, as after a maximum goes down.
    pub fn clamp(&mut self, attributes: &Attributes) {
        self.health = self.health.clamp(0.0, attributes.health() as f32);
        self.stamina = self.stamina.clamp(0.0, attributes.stamina() as f32);
    }

    /// Vitals as saved: `health` and `stamina` at unix time `saved_at`, caught up to `now`.
    /// Missing values mean full.
    pub fn restored(
        attributes: &Attributes,
        saved: Option<(f32, f32)>,
        saved_at: Option<i64>,
        now: i64,
    ) -> Self {
        let Some((health, stamina)) = saved else {
            return Self::full(attributes);
        };
        let mut vitals = Self { health, stamina };
        vitals.clamp(attributes);
        let away = saved_at.map_or(0, |at| now - at);
        vitals.recover(attributes, away as f32);
        vitals
    }
}

/// The text of a prompt, filled in from a character's vitals.
pub fn render_prompt(format: &str, vitals: &Vitals, attributes: &Attributes) -> String {
    let mut text = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            text.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => text += &(vitals.health.floor() as i32).to_string(),
            Some('H') => text += &attributes.health().to_string(),
            Some('s') => text += &(vitals.stamina.floor() as i32).to_string(),
            Some('S') => text += &attributes.stamina().to_string(),
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

/// Players' vitals and whether they're resting or fighting. Mobs don't recover yet.
type Recovering<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Vitals,
        &'static Attributes,
        Has<Resting>,
        Has<Fighting>,
    ),
    With<Player>,
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

/// Recover everyone's vitals by the time since the last heartbeat.
fn recover(time: Res<Time>, mut last: Local<Option<f64>>, mut players: Recovering) {
    let now = time.elapsed_secs_f64();
    let secs = last.map_or(0.0, |last| now - last) as f32;
    *last = Some(now);
    for (mut vitals, attributes, resting, fighting) in &mut players {
        let before = vitals.clone();
        let mut after = before.clone();
        after.clamp(attributes);
        let pace = match (fighting, resting) {
            (true, _) => 0.0,
            (false, true) => REST_SPEEDUP,
            (false, false) => 1.0,
        };
        after.recover(attributes, secs * pace);
        if after != before {
            *vitals = after;
        }
    }
}

/// Save everyone's vitals about once a minute, so a crash loses little.
fn save_vitals(
    time: Res<Time>,
    mut last: Local<f64>,
    players: Query<(&Player, &Vitals)>,
    db: Res<Database>,
) {
    let now = time.elapsed_secs_f64();
    if now - *last < SAVE_EVERY_SECS {
        return;
    }
    *last = now;
    let at = unix_now();
    for (player, vitals) in &players {
        save(&db, player, vitals, at);
    }
}

fn save_on_logout(players: Query<(&Connection, &Player, &Vitals)>, db: Res<Database>) {
    let at = unix_now();
    for (conn, player, vitals) in &players {
        if conn.closing {
            save(&db, player, vitals, at);
        }
    }
}

fn save(db: &Database, player: &Player, vitals: &Vitals, at: i64) {
    if let Err(err) = db.save_vitals(player.character_id, vitals.health, vitals.stamina, at) {
        error!("failed to save vitals for {}: {err}", player.name);
    }
}

/// Give each player's connection their prompt, filled in fresh, just before output is sent.
fn show_prompts(mut players: Query<(&mut Connection, &Vitals, &Attributes, &PromptFormat)>) {
    for (mut conn, vitals, attributes, format) in &mut players {
        conn.set_prompt(render_prompt(&format.0, vitals, attributes));
    }
}

/// `prompt [<format>]`: set the prompt, or go back to the default.
fn prompt_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
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
        match db.set_prompt(actor.character_id, saved) {
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
    deliver(vec![reply_line(inv.entity, reply)], &mut players);
}

/// `rest`: sit down to recover faster.
fn rest(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    state: Query<(Has<Resting>, Has<Fighting>)>,
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
            out.push(Outgoing::line(inv.entity, "You sit down and rest."));
            out.push(Outgoing::Room {
                room: actor.room,
                except: inv.entity,
                text: format!("{} sits down to rest.\r\n", actor.name),
            });
        }
    }
    deliver(out, &mut players);
}

/// `stand`: get up from resting.
fn stand(
    In(inv): In<Invocation>,
    mut commands: Commands,
    mut players: PlayerQuery,
    resting: Query<(), With<Resting>>,
) {
    let text = if resting.get(inv.entity).is_ok() {
        commands.entity(inv.entity).remove::<Resting>();
        "You stand up."
    } else {
        "You're already on your feet."
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}

/// `setvitals <player> <health> [<stamina>]`: an admin sets someone's current vitals.
fn setvitals_cmd(
    In(inv): In<Invocation>,
    mut players: PlayerQuery,
    keywords: Query<&Keywords>,
    mut sheets: Query<(&mut Vitals, &Attributes)>,
) {
    let mut out = Vec::new();
    let reply = set_vitals(&inv, &players, &keywords, &mut sheets, &mut out);
    out.insert(0, reply_line(inv.entity, reply));
    deliver(out, &mut players);
}

fn set_vitals(
    inv: &Invocation,
    players: &PlayerQuery,
    keywords: &Query<&Keywords>,
    sheets: &mut Query<(&mut Vitals, &Attributes)>,
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
    let (mut vitals, attributes) = sheets
        .get_mut(actor.entity)
        .map_err(|_| format!("{} has no vitals loaded.", actor.name))?;
    vitals.health = health;
    if let Some(stamina) = number(stamina) {
        vitals.stamina = stamina;
    }
    vitals.clamp(attributes);
    let now = format!(
        "{}/{} health, {}/{} stamina",
        vitals.health.floor(),
        attributes.health(),
        vitals.stamina.floor(),
        attributes.stamina()
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
        attributes.set(Attribute::Body, 4);
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
        vitals.recover(&attributes, 60.0);
        assert!((vitals.health - 1.0).abs() < 1e-4, "{vitals:?}");
        assert!((vitals.stamina - 4.5).abs() < 1e-4, "{vitals:?}");
        // Many small steps add up to one big one.
        let mut stepped = Vitals {
            health: 0.0,
            stamina: 0.0,
        };
        for _ in 0..60 {
            stepped.recover(&attributes, 1.0);
        }
        assert!((stepped.health - vitals.health).abs() < 1e-3);
        // Never past the maximum.
        vitals.recover(&attributes, 100_000.0);
        assert_eq!(vitals, Vitals::full(&attributes));
    }

    #[test]
    fn offline_time_counts_toward_recovery() {
        let attributes = sheet();
        assert_eq!(
            Vitals::restored(&attributes, None, None, 1000),
            Vitals::full(&attributes)
        );
        let back = Vitals::restored(&attributes, Some((2.0, 0.0)), Some(1000), 1060);
        assert!((back.health - 3.0).abs() < 1e-4, "{back:?}");
        assert!((back.stamina - 4.5).abs() < 1e-4, "{back:?}");
        // Saved above a maximum that has since gone down: clamped.
        let over = Vitals::restored(&attributes, Some((50.0, 50.0)), Some(1000), 1000);
        assert_eq!(over, Vitals::full(&attributes));
    }

    #[test]
    fn prompts_fill_in_placeholders() {
        let attributes = sheet();
        let vitals = Vitals {
            health: 7.9,
            stamina: 3.2,
        };
        assert_eq!(
            render_prompt(DEFAULT_PROMPT, &vitals, &attributes),
            "<7/10hp 3/9st> "
        );
        assert_eq!(
            render_prompt("HP:%h %% %x%", &vitals, &attributes),
            "HP:7 % %x%"
        );
    }
}
