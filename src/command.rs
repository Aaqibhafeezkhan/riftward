//! The command system. Every command is registered with a `CommandSpec` (name, short forms,
//! category, permission, usage and help text) and its own Bevy system, which receives an
//! `Invocation` and asks for exactly the resources it needs.
//!
//! The dispatcher turns each line of input into an invocation: it expands aliases, hides commands
//! the player isn't allowed to use, and queues the command's system to run. `help` is generated
//! from the registry. Prompts are the connection's job (`net.rs`), not the commands'.
//!
//! Commands marked as actions (attacking, fleeing, using skills) take a round: a player gets at
//! most one per round, and any typed sooner wait their turn in an `ActionQueue`. In a fight, the
//! combat round takes the next one from the queue in place of the auto-attack. Everything else
//! (looking, talking, moving) happens at once.

use std::collections::VecDeque;

use bevy::ecs::system::SystemId;
use bevy::prelude::*;

use crate::aliases::Aliases;
use crate::combat::Conditions;
use crate::combat::the;
use crate::db::{Permission, Role};
use crate::events::GameEvent;
use crate::mobs::Mob;
use crate::net::{Connection, PlayerInput};
use crate::output::{Actor, Characters, Outbox, Outgoing, Puppeting, deliver};
use crate::puppet::OWN_COMMANDS;
use crate::stealth::reveal;
use crate::world::{InRoom, Player};

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CommandRegistry>()
            .add_systems(Update, (dispatch, run_queued_actions).chain())
            .add_command(
                CommandSpec::new("help", Category::Information)
                    .shortcuts(&["?"])
                    .usage("help [<command>]")
                    .summary("list commands, or explain one"),
                help,
            );
    }
}

/// Groups commands in `help`, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Information,
    Movement,
    Interaction,
    Communication,
    Character,
    Building,
    Admin,
}

impl Category {
    fn title(self) -> &'static str {
        match self {
            Category::Information => "Information",
            Category::Movement => "Movement",
            Category::Interaction => "Interaction",
            Category::Communication => "Communication",
            Category::Character => "Character",
            Category::Building => "Building",
            Category::Admin => "Admin",
        }
    }
}

/// Everything the game knows about a command apart from what it does.
#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub name: &'static str,
    pub shortcuts: &'static [&'static str],
    pub category: Category,
    /// `None` means every logged-in player can use it.
    pub permission: Option<Permission>,
    pub usage: &'static str,
    pub summary: &'static str,
    /// Whether `help` lists it. Unlisted commands still work and still have `help <name>`.
    pub listed: bool,
    /// Takes a round: at most one per round, queued if typed sooner.
    pub action: bool,
    /// Can be done while hidden without being seen, like `whisper` or `search`. Information,
    /// character, movement, building and admin commands are always quiet; anything else shows a
    /// hidden player unless it says it's quiet, so a new command can't forget to.
    pub quiet: bool,
}

impl CommandSpec {
    pub fn new(name: &'static str, category: Category) -> Self {
        Self {
            name,
            shortcuts: &[],
            category,
            permission: None,
            usage: name,
            summary: "",
            listed: true,
            action: false,
            quiet: false,
        }
    }

    pub fn action(mut self) -> Self {
        self.action = true;
        self
    }

    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Whether doing it shows someone hidden.
    fn reveals(&self) -> bool {
        !self.quiet
            && matches!(
                self.category,
                Category::Interaction | Category::Communication
            )
    }

    pub fn shortcuts(mut self, shortcuts: &'static [&'static str]) -> Self {
        self.shortcuts = shortcuts;
        self
    }

    pub fn usage(mut self, usage: &'static str) -> Self {
        self.usage = usage;
        self
    }

    pub fn summary(mut self, summary: &'static str) -> Self {
        self.summary = summary;
        self
    }

    pub fn permission(mut self, permission: Permission) -> Self {
        self.permission = Some(permission);
        self
    }

    pub fn unlisted(mut self) -> Self {
        self.listed = false;
        self
    }

    fn available_to(&self, role: Role) -> bool {
        self.permission.is_none_or(|p| role.can(p))
    }

    fn answers_to(&self, verb: &str) -> bool {
        self.name == verb || self.shortcuts.contains(&verb)
    }
}

/// What a command's system receives.
pub struct Invocation {
    pub entity: Entity,
    /// The command's registered name, whichever short form was typed.
    pub command: &'static str,
    pub usage: &'static str,
    /// Everything after the command word, trimmed.
    pub args: String,
}

impl Invocation {
    pub fn usage_error(&self) -> String {
        format!("Usage: {}", self.usage)
    }
}

pub type CommandId = SystemId<In<Invocation>>;

/// The most actions a player can have waiting.
const MAX_QUEUED_ACTIONS: usize = 10;

/// A player's actions waiting for their turn, and when they can next act, in seconds since the
/// server started. Lives on the player entity.
#[derive(Component, Default)]
pub struct ActionQueue {
    queue: VecDeque<(CommandId, Invocation)>,
    pub next_at: f64,
}

impl ActionQueue {
    /// The next action waiting, if any.
    pub fn pop(&mut self) -> Option<(CommandId, Invocation)> {
        self.queue.pop_front()
    }

    /// The next action waiting, without taking it.
    pub fn peek(&self) -> Option<&(CommandId, Invocation)> {
        self.queue.front()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

/// Width of the usage column in `help`.
const USAGE_WIDTH: usize = 34;

#[derive(Resource, Default)]
pub struct CommandRegistry {
    commands: Vec<(CommandSpec, CommandId)>,
}

#[cfg(test)]
impl CommandRegistry {
    /// A registry of just these commands, for tests that run a world without an app.
    pub fn of(commands: Vec<(CommandSpec, CommandId)>) -> Self {
        Self { commands }
    }
}

impl CommandRegistry {
    /// The command `verb` names, if this role may use it. Commands a role can't use are invisible.
    pub fn find(&self, verb: &str, role: Role) -> Option<&(CommandSpec, CommandId)> {
        self.commands
            .iter()
            .find(|(spec, _)| spec.answers_to(verb) && spec.available_to(role))
    }

    pub fn is_command(&self, verb: &str, role: Role) -> bool {
        self.find(verb, role).is_some()
    }

    fn help(&self, role: Role) -> String {
        let mut available: Vec<_> = self
            .commands
            .iter()
            .map(|(spec, _)| spec)
            .filter(|spec| spec.listed && spec.available_to(role))
            .collect();
        available.sort_by_key(|spec| spec.category);

        let mut text = String::new();
        let mut category = None;
        for spec in available {
            if category != Some(spec.category) {
                if category.is_some() {
                    text += "\r\n";
                }
                text += &format!("{}:\r\n", spec.category.title());
                category = Some(spec.category);
            }
            let mut left = spec.usage.to_string();
            if !spec.shortcuts.is_empty() {
                left += &format!(" ({})", spec.shortcuts.join(", "));
            }
            // Usage too wide for the column puts the summary on its own line.
            if left.len() < USAGE_WIDTH {
                text += &format!("  {left:<USAGE_WIDTH$} {}\r\n", spec.summary);
            } else {
                text += &format!("  {left}\r\n  {:<USAGE_WIDTH$} {}\r\n", "", spec.summary);
            }
        }
        text + "\r\nType 'help <command>' for more about one command."
    }

    fn help_for(&self, verb: &str, role: Role) -> Option<String> {
        let (spec, _) = self.find(verb, role)?;
        let mut text = format!("Usage: {}\r\n  {}", spec.usage, spec.summary);
        if !spec.shortcuts.is_empty() {
            text += &format!("\r\n  Also: {}", spec.shortcuts.join(", "));
        }
        Some(text)
    }
}

pub trait AddCommand {
    /// Register a command and the system that runs it.
    fn add_command<M>(
        &mut self,
        spec: CommandSpec,
        handler: impl IntoSystem<In<Invocation>, (), M> + 'static,
    ) -> &mut Self;
}

impl AddCommand for App {
    fn add_command<M>(
        &mut self,
        spec: CommandSpec,
        handler: impl IntoSystem<In<Invocation>, (), M> + 'static,
    ) -> &mut Self {
        // Set the system up now, not on first use, so parameters that clash (Bevy's B0001) stop
        // the server at startup instead of when someone first types the command.
        let mut system = IntoSystem::into_system(handler);
        system.initialize(self.world_mut());
        let id = self.world_mut().register_boxed_system(Box::new(system));
        let mut registry = self.world_mut().get_resource_or_init::<CommandRegistry>();
        let names = std::iter::once(&spec.name).chain(spec.shortcuts);
        for name in names {
            assert!(
                !registry.commands.iter().any(|(s, _)| s.answers_to(name)),
                "command name '{name}' is registered twice"
            );
        }
        registry.commands.push((spec, id));
        self
    }
}

/// Split input into a lowercase verb and the rest. `'hello` is shorthand for `say hello`, and
/// `:waves` for `emote waves`.
pub fn split_verb(line: &str) -> (String, &str) {
    if let Some(text) = line.strip_prefix('\'') {
        return ("say".into(), text.trim());
    }
    if let Some(text) = line.strip_prefix(':') {
        return ("emote".into(), text.trim());
    }
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    (verb.to_ascii_lowercase(), rest.trim())
}

/// Logged-in players as the dispatcher sees them: where their output goes, what their words
/// expand to, their queued actions, and where they are and whether they're hiding.
type Speakers<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Connection,
        &'static Player,
        Option<&'static Aliases>,
        Option<&'static Puppeting>,
    ),
>;

/// Whoever acts on a player's line: the player, or the mob they puppet. Their queued actions,
/// where they are, whether they're hiding, and what they're called.
type Bodies<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static mut ActionQueue>,
        &'static InRoom,
        Option<&'static Conditions>,
        Option<&'static Player>,
        Option<&'static Mob>,
    ),
>;

/// Turn each logged-in player's line into a command invocation. Lines from connections that
/// haven't logged in yet are left to `login.rs`. A builder puppeting a mob acts as it, with their
/// own permissions, except for the commands that are theirs alone (`puppet::OWN_COMMANDS`).
pub fn dispatch(
    mut commands: Commands,
    mut inputs: MessageReader<PlayerInput>,
    mut players: Speakers,
    mut bodies: Bodies,
    registry: Res<CommandRegistry>,
    mut events: MessageWriter<GameEvent>,
) {
    for PlayerInput { entity, line } in inputs.read() {
        let entity = *entity;
        let Ok((mut conn, player, aliases, puppeting)) = players.get_mut(entity) else {
            continue;
        };
        if conn.closing {
            continue;
        }

        let expanded;
        let (mut verb, mut args) = split_verb(line);
        if !registry.is_command(&verb, player.role)
            && let Some(expansion) = aliases.and_then(|a| a.expand(&verb, args))
        {
            expanded = expansion;
            (verb, args) = split_verb(&expanded);
        }

        if verb.is_empty() {
            continue;
        }
        let Some((spec, id)) = registry.find(&verb, player.role) else {
            conn.send("Huh? Type 'help' for a list of commands.");
            continue;
        };
        let actor = match puppeting {
            Some(Puppeting(mob)) if !OWN_COMMANDS.contains(&spec.name) => {
                // Everything done through a puppet is logged.
                info!("{} as a puppet: {line}", player.name);
                *mob
            }
            _ => entity,
        };
        let Ok((queue, in_room, conditions, as_player, as_mob)) = bodies.get_mut(actor) else {
            continue;
        };
        let hidden = conditions.is_some_and(|c| c.hidden().is_some());
        // Acting openly shows someone hiding.
        if hidden && spec.reveals() {
            let name = match (as_player, as_mob) {
                (Some(player), _) => player.name.clone(),
                (_, Some(mob)) => the(&mob.name),
                _ => String::new(),
            };
            reveal(&mut commands, &mut events, actor, &name, in_room.0);
        }
        let invocation = Invocation {
            entity: actor,
            command: spec.name,
            usage: spec.usage,
            args: args.into(),
        };
        match queue {
            Some(mut queue) if spec.action => {
                if queue.len() >= MAX_QUEUED_ACTIONS {
                    conn.send("You can't plan that far ahead. (Type 'stop' to clear your plans.)");
                } else {
                    queue.queue.push_back((*id, invocation));
                }
            }
            _ => commands.run_system_with(*id, invocation),
        }
    }
}

/// How long an action takes outside a fight, before the next can start: short, so eating or
/// picking a lock doesn't feel slow. In a fight each takes a whole round.
pub const ACTION_SECS: f64 = 1.0;

/// Run each player's next queued action once they're free to act, outside a fight. In a fight,
/// combat rounds take actions from the queue instead.
fn run_queued_actions(
    mut commands: Commands,
    time: Res<Time>,
    mut queues: Query<&mut ActionQueue, Without<crate::combat::Fighting>>,
) {
    let now = time.elapsed_secs_f64();
    for mut queue in &mut queues {
        if queue.next_at > now {
            continue;
        }
        if let Some((id, invocation)) = queue.pop() {
            queue.next_at = now + ACTION_SECS;
            commands.run_system_with(id, invocation);
        }
    }
}

fn help(
    In(inv): In<Invocation>,
    registry: Res<CommandRegistry>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
) {
    let Some(actor) = Actor::get(&players, inv.entity) else {
        return;
    };
    let text = if inv.args.is_empty() {
        registry.help(actor.role)
    } else {
        let verb = inv.args.to_ascii_lowercase();
        registry
            .help_for(&verb, actor.role)
            .unwrap_or_else(|| format!("There is no command '{verb}'."))
    };
    deliver(vec![Outgoing::line(inv.entity, text)], &mut outbox);
}
