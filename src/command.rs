//! The command system. Every command is registered with a `CommandSpec` (name, short forms,
//! category, permission, usage and help text) and its own Bevy system, which receives an
//! `Invocation` and asks for exactly the resources it needs.
//!
//! The dispatcher turns each line of input into an invocation: it expands aliases, hides commands
//! the player isn't allowed to use, and queues the command's system to run. `help` is generated
//! from the registry. Prompts are the connection's job (`net.rs`), not the commands'.

use bevy::ecs::system::SystemId;
use bevy::prelude::*;

use crate::aliases::Aliases;
use crate::db::{Permission, Role};
use crate::net::{Connection, PlayerInput};
use crate::output::{Actor, Outgoing, PlayerQuery, deliver};
use crate::world::Player;

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CommandRegistry>()
            .add_systems(Update, dispatch)
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
        }
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

/// Width of the usage column in `help`.
const USAGE_WIDTH: usize = 34;

#[derive(Resource, Default)]
pub struct CommandRegistry {
    commands: Vec<(CommandSpec, CommandId)>,
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
        let id = self.register_system(handler);
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

/// Split input into a lowercase verb and the rest. `'hello` is shorthand for `say hello`.
pub fn split_verb(line: &str) -> (String, &str) {
    if let Some(text) = line.strip_prefix('\'') {
        return ("say".into(), text.trim());
    }
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    (verb.to_ascii_lowercase(), rest.trim())
}

/// Turn each logged-in player's line into a command invocation. Lines from connections that
/// haven't logged in yet are left to `login.rs`.
pub fn dispatch(
    mut commands: Commands,
    mut inputs: MessageReader<PlayerInput>,
    mut players: Query<(&mut Connection, &Player, Option<&Aliases>)>,
    registry: Res<CommandRegistry>,
) {
    for PlayerInput { entity, line } in inputs.read() {
        let entity = *entity;
        let Ok((mut conn, player, aliases)) = players.get_mut(entity) else {
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
        match registry.find(&verb, player.role) {
            Some((spec, id)) => commands.run_system_with(
                *id,
                Invocation {
                    entity,
                    command: spec.name,
                    usage: spec.usage,
                    args: args.into(),
                },
            ),
            None => conn.send("Huh? Type 'help' for a list of commands."),
        }
    }
}

fn help(In(inv): In<Invocation>, registry: Res<CommandRegistry>, mut players: PlayerQuery) {
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
    deliver(vec![Outgoing::line(inv.entity, text)], &mut players);
}
