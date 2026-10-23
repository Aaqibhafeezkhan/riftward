//! Per-character command aliases: `alias fb use fireball` makes `fb goblin` run
//! `use fireball goblin`. Aliases are saved in the database and loaded at login.

use std::collections::BTreeMap;

use bevy::prelude::*;

use crate::db::Database;

pub const MAX_ALIASES: usize = 50;
pub const MAX_EXPANSION_LEN: usize = 200;
const MAX_NAME_LEN: usize = 20;

/// A character's aliases, name -> expansion. Lives on the player entity.
#[derive(Component, Default)]
pub struct Aliases(pub BTreeMap<String, String>);

impl Aliases {
    /// The command an alias stands for, with whatever was typed after the alias name appended.
    /// The result is not expanded again, so aliases can't chain or loop.
    pub fn expand(&self, verb: &str, rest: &str) -> Option<String> {
        let expansion = self.0.get(verb)?;
        Some(if rest.is_empty() {
            expansion.clone()
        } else {
            format!("{expansion} {rest}")
        })
    }
}

/// `alias` lists aliases, `alias <name>` shows one, `alias <name> <command>` creates or replaces one.
/// `is_builtin` says whether a name is a command this player can already use.
pub fn alias_command(
    character_id: i64,
    aliases: &mut Aliases,
    args: &str,
    is_builtin: impl Fn(&str) -> bool,
    db: &Database,
) -> String {
    if args.is_empty() {
        if aliases.0.is_empty() {
            return "You have no aliases. Create one with: alias <name> <command>".into();
        }
        let mut text = format!("Your aliases ({}/{MAX_ALIASES}):", aliases.0.len());
        for (name, expansion) in &aliases.0 {
            text += &format!("\r\n  {name:<12} {expansion}");
        }
        return text;
    }

    let (name, expansion) = args
        .split_once(' ')
        .map(|(n, e)| (n, e.trim()))
        .unwrap_or((args, ""));
    let name = name.to_ascii_lowercase();

    if expansion.is_empty() {
        return match aliases.0.get(&name) {
            Some(expansion) => format!("{name} = {expansion}"),
            None => format!("You have no alias '{name}'."),
        };
    }

    let valid_name = (1..=MAX_NAME_LEN).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid_name {
        return format!(
            "Alias names are 1-{MAX_NAME_LEN} letters, digits, dashes, or underscores."
        );
    }
    if is_builtin(&name) {
        return format!("'{name}' is already a command, so it can't be an alias.");
    }
    if expansion.len() > MAX_EXPANSION_LEN {
        return format!("Aliases can be at most {MAX_EXPANSION_LEN} characters long.");
    }
    let replacing = aliases.0.contains_key(&name);
    if !replacing && aliases.0.len() >= MAX_ALIASES {
        return format!("You already have {MAX_ALIASES} aliases. Remove one with 'unalias <name>'.");
    }

    if let Err(err) = db.set_alias(character_id, &name, expansion) {
        error!("failed to save alias for character {character_id}: {err}");
        return "Something went wrong saving that alias. Please try again.".into();
    }
    aliases.0.insert(name.clone(), expansion.into());
    let verb = if replacing { "Updated" } else { "Added" };
    format!("{verb} alias: {name} = {expansion}")
}

/// `unalias <name>` removes an alias.
pub fn unalias_command(
    character_id: i64,
    aliases: &mut Aliases,
    args: &str,
    db: &Database,
) -> String {
    if args.is_empty() {
        return "Usage: unalias <name>".into();
    }
    let name = args.to_ascii_lowercase();
    if !aliases.0.contains_key(&name) {
        return format!("You have no alias '{name}'.");
    }
    if let Err(err) = db.remove_alias(character_id, &name) {
        error!("failed to remove alias for character {character_id}: {err}");
        return "Something went wrong removing that alias. Please try again.".into();
    }
    aliases.0.remove(&name);
    format!("Removed alias '{name}'.")
}
