//! Connecting: log in to (or create) an account, then pick (or create) a character.
//!
//! Each connection that hasn't entered the game yet carries a `LoginState` component. Once a
//! character is chosen, the state is replaced by `Player` and `InRoom`.

use bevy::prelude::*;

use crate::aliases::Aliases;
use crate::attributes::Attributes;
use crate::command;
use crate::commands::movement::describe_room;
use crate::output::{Outgoing, PlayerQuery, RoomQuery, deliver};
use crate::targeting::Keywords;
use crate::db::{Account, CharacterRecord, Database, Permission, Role};
use crate::net::{Connection, JustConnected, PlayerInput};
use crate::world::{InRoom, Player, RoomIndex, RoomKey};

pub struct LoginPlugin;

impl Plugin for LoginPlugin {
    fn build(&self, app: &mut App) {
        // Run after the command handler so a connection that finishes logging in this frame
        // doesn't also have the same line treated as a game command.
        app.add_systems(
            Update,
            (greet_new_connections, handle_login)
                .chain()
                .after(command::dispatch),
        );
    }
}

const BANNER: &str = r"
  ____                    __  __ _   _ ____
 | __ )  _____   ___   _ |  \/  | | | |  _ \
 |  _ \ / _ \ \ / / | | || |\/| | | | | | | |
 | |_) |  __/\ V /| |_| || |  | | |_| | |_| |
 |____/ \___| \_/  \__, ||_|  |_|\___/|____/
                   |___/
";

const MAX_PASSWORD_ATTEMPTS: u32 = 3;
const MIN_PASSWORD_LEN: usize = 8;

#[derive(Component)]
pub enum LoginState {
    Username,
    Password { account: Account, attempts: u32 },
    NewPassword { username: String },
    ConfirmPassword { username: String, password: String },
    ChooseCharacter { account: Account, characters: Vec<CharacterRecord> },
    NameCharacter { account: Account },
}

impl LoginState {
    /// Whether the client should hide what the user types in this state.
    fn is_secret(&self) -> bool {
        matches!(
            self,
            LoginState::Password { .. }
                | LoginState::NewPassword { .. }
                | LoginState::ConfirmPassword { .. }
        )
    }
}

/// What to do with a connection after handling one line of login input.
enum Step {
    Next(LoginState),
    Enter {
        account: Account,
        character: CharacterRecord,
    },
    Disconnect,
}

fn greet_new_connections(
    mut commands: Commands,
    mut new: Query<(Entity, &mut Connection), With<JustConnected>>,
) {
    for (entity, mut conn) in &mut new {
        conn.send(BANNER);
        conn.send("Enter your account name. If it doesn't exist yet, you'll be asked to create it.");
        conn.send_raw("Account: ");
        commands
            .entity(entity)
            .remove::<JustConnected>()
            .insert(LoginState::Username);
    }
}

fn handle_login(
    mut commands: Commands,
    mut inputs: MessageReader<PlayerInput>,
    mut states: Query<&mut LoginState>,
    mut players: PlayerQuery,
    rooms: RoomQuery,
    index: Res<RoomIndex>,
    db: Res<Database>,
) {
    let mut out = Vec::new();
    for PlayerInput { entity, line } in inputs.read() {
        let entity = *entity;
        let Ok(mut state) = states.get_mut(entity) else {
            continue;
        };

        let was_secret = state.is_secret();
        let current = std::mem::replace(&mut *state, LoginState::Username);
        let step = match current {
            LoginState::Username => username(entity, line, &db, &mut out),
            LoginState::Password { account, attempts } => {
                password(entity, line, account, attempts, &db, &mut out)
            }
            LoginState::NewPassword { username } => new_password(entity, line, username, &mut out),
            LoginState::ConfirmPassword { username, password } => {
                confirm_password(entity, line, username, password, &db, &mut out)
            }
            LoginState::ChooseCharacter { account, characters } => {
                choose_character(entity, line, account, characters, &db, &mut out)
            }
            LoginState::NameCharacter { account } => {
                name_character(entity, line, account, &db, &mut out)
            }
        };

        // The client doesn't echo the newline after a hidden password either, so add one.
        if let Ok((_, mut conn, _, _)) = players.get_mut(entity) {
            if was_secret {
                conn.set_echo(true);
                conn.send_raw("\r\n");
            }
            if let Step::Next(next) = &step
                && next.is_secret()
            {
                conn.set_echo(false);
            }
        }

        match step {
            Step::Next(next) => *state = next,
            Step::Disconnect => {
                if let Ok((_, mut conn, _, _)) = players.get_mut(entity) {
                    conn.closing = true;
                }
            }
            Step::Enter { account, character } => {
                if let Some(next) = enter_game(
                    entity, account, character, &mut commands, &players, &rooms, &index, &db,
                    &mut out,
                ) {
                    *state = next;
                }
            }
        }
        deliver(std::mem::take(&mut out), &mut players);
    }
}

fn username(entity: Entity, line: &str, db: &Database, out: &mut Vec<Outgoing>) -> Step {
    let valid = (3..=20).contains(&line.len())
        && line.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        out.push(Outgoing::To(
            entity,
            "Account names are 3-20 letters, digits, or underscores.\r\nAccount: ".into(),
        ));
        return Step::Next(LoginState::Username);
    }

    match db.find_account(line) {
        Ok(Some(account)) => {
            out.push(Outgoing::To(entity, "Password: ".into()));
            Step::Next(LoginState::Password {
                account,
                attempts: 0,
            })
        }
        Ok(None) => {
            out.push(Outgoing::To(
                entity,
                format!(
                    "Creating a new account '{line}'.\r\n\
                     Choose a password (at least {MIN_PASSWORD_LEN} characters): "
                ),
            ));
            Step::Next(LoginState::NewPassword {
                username: line.into(),
            })
        }
        Err(err) => db_failure(entity, err, out),
    }
}

fn password(
    entity: Entity,
    line: &str,
    account: Account,
    attempts: u32,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    if account.verify_password(line) {
        return character_menu(entity, account, db, out);
    }
    if attempts + 1 >= MAX_PASSWORD_ATTEMPTS {
        warn!("too many failed logins for account '{}'", account.username);
        out.push(Outgoing::line(entity, "Too many failed attempts. Goodbye."));
        return Step::Disconnect;
    }
    out.push(Outgoing::To(entity, "Wrong password.\r\nPassword: ".into()));
    Step::Next(LoginState::Password {
        account,
        attempts: attempts + 1,
    })
}

fn new_password(entity: Entity, line: &str, username: String, out: &mut Vec<Outgoing>) -> Step {
    if line.chars().count() < MIN_PASSWORD_LEN {
        out.push(Outgoing::To(
            entity,
            format!("Passwords must be at least {MIN_PASSWORD_LEN} characters.\r\nPassword: "),
        ));
        return Step::Next(LoginState::NewPassword { username });
    }
    out.push(Outgoing::To(entity, "Confirm password: ".into()));
    Step::Next(LoginState::ConfirmPassword {
        username,
        password: line.into(),
    })
}

fn confirm_password(
    entity: Entity,
    line: &str,
    username: String,
    password: String,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    if line != password {
        out.push(Outgoing::To(
            entity,
            "Passwords don't match. Let's start over.\r\nAccount: ".into(),
        ));
        return Step::Next(LoginState::Username);
    }
    match db.create_account(&username, &password) {
        Ok(account) => {
            info!("created account '{}' ({})", account.username, account.role.as_str());
            if account.role == Role::Admin {
                out.push(Outgoing::line(
                    entity,
                    "This is the first account on the server, so it has been made an admin.",
                ));
            }
            character_menu(entity, account, db, out)
        }
        Err(err) => db_failure(entity, err, out),
    }
}

/// List the account's characters, or go straight to naming one if there are none.
fn character_menu(entity: Entity, account: Account, db: &Database, out: &mut Vec<Outgoing>) -> Step {
    let characters = match db.characters(account.id) {
        Ok(characters) => characters,
        Err(err) => return db_failure(entity, err, out),
    };

    if characters.is_empty() {
        out.push(Outgoing::To(
            entity,
            "You have no characters yet. Name your first character: ".into(),
        ));
        return Step::Next(LoginState::NameCharacter { account });
    }

    let mut text = String::from("Your characters:\r\n");
    for (i, character) in characters.iter().enumerate() {
        text += &format!("  {}. {}\r\n", i + 1, character.name);
    }
    text += "Enter a number or name to play, or 'new <name>' to create a character: ";
    out.push(Outgoing::To(entity, text));
    Step::Next(LoginState::ChooseCharacter {
        account,
        characters,
    })
}

fn choose_character(
    entity: Entity,
    line: &str,
    account: Account,
    characters: Vec<CharacterRecord>,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    if let Some(name) = line.strip_prefix("new ") {
        return name_character(entity, name.trim(), account, db, out);
    }

    let chosen = match line.parse::<usize>() {
        Ok(n) => n.checked_sub(1).and_then(|i| characters.get(i)),
        Err(_) => characters.iter().find(|c| c.name.eq_ignore_ascii_case(line)),
    };
    match chosen {
        Some(character) => Step::Enter {
            character: character.clone(),
            account,
        },
        None => {
            out.push(Outgoing::To(
                entity,
                "No such character. Enter a number, a name, or 'new <name>': ".into(),
            ));
            Step::Next(LoginState::ChooseCharacter {
                account,
                characters,
            })
        }
    }
}

fn name_character(
    entity: Entity,
    line: &str,
    account: Account,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    let valid = (2..=16).contains(&line.len()) && line.chars().all(|c| c.is_ascii_alphabetic());
    if !valid {
        out.push(Outgoing::To(
            entity,
            "Character names are 2-16 letters. Try again: ".into(),
        ));
        return Step::Next(LoginState::NameCharacter { account });
    }

    let mut name = line.to_ascii_lowercase();
    name[..1].make_ascii_uppercase();

    match db.character_name_taken(&name) {
        Ok(true) => {
            out.push(Outgoing::To(
                entity,
                format!("The name {name} is taken. Choose another: "),
            ));
            Step::Next(LoginState::NameCharacter { account })
        }
        Ok(false) => match db.create_character(account.id, &name) {
            Ok(character) => {
                info!("account '{}' created character {name}", account.username);
                Step::Enter { account, character }
            }
            Err(err) => db_failure(entity, err, out),
        },
        Err(err) => db_failure(entity, err, out),
    }
}

fn db_failure(entity: Entity, err: crate::db::DbError, out: &mut Vec<Outgoing>) -> Step {
    error!("database error during login: {err}");
    out.push(Outgoing::line(
        entity,
        "Something went wrong on our end. Please try again later.",
    ));
    Step::Disconnect
}

/// Spawn the character into the world. Returns a state to stay in if they can't enter yet.
#[allow(clippy::too_many_arguments)]
fn enter_game(
    entity: Entity,
    account: Account,
    character: CharacterRecord,
    commands: &mut Commands,
    players: &PlayerQuery,
    rooms: &RoomQuery,
    index: &RoomIndex,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Option<LoginState> {
    let back_to_menu = |out: &mut Vec<Outgoing>, reason: &str| {
        out.push(Outgoing::To(
            entity,
            format!("{reason}\r\nEnter a number or name to play, or 'new <name>': "),
        ));
        let characters = db.characters(account.id).unwrap_or_default();
        Some(LoginState::ChooseCharacter {
            account: account.clone(),
            characters,
        })
    };

    let online = players
        .iter()
        .any(|(_, _, p, _)| p.is_some_and(|p| p.character_id == character.id));
    if online {
        return back_to_menu(out, &format!("{} is already playing.", character.name));
    }

    let saved_room = character
        .room
        .as_deref()
        .and_then(|r| index.0.get(&RoomKey::parse(r, "")).copied());
    let Some(room) = saved_room.or_else(|| index.start()) else {
        return back_to_menu(out, "The world is still loading. Try again in a moment.");
    };
    if saved_room.is_none()
        && let Ok((_, key, _)) = rooms.get(room)
        && let Err(err) = db.save_location(character.id, &key.to_string())
    {
        error!("failed to save location for {}: {err}", character.name);
    }

    let aliases = db.aliases(character.id).unwrap_or_else(|err| {
        error!("failed to load aliases for {}: {err}", character.name);
        Default::default()
    });

    let attributes = Attributes::from_rows(db.attributes(character.id).unwrap_or_else(|err| {
        error!("failed to load attributes for {}: {err}", character.name);
        Vec::new()
    }));
    let karma = attributes.karma_unspent();

    let name = character.name.clone();
    commands.entity(entity).remove::<LoginState>().insert((
        Aliases(aliases),
        attributes,
        Keywords::from_name(&character.name),
        Player {
            name: name.clone(),
            character_id: character.id,
            account: account.username.clone(),
            role: account.role,
        },
        InRoom(room),
    ));
    info!("{name} entered the game (account '{}')", account.username);

    out.push(Outgoing::line(
        entity,
        format!("\r\nWelcome, {name}! Type 'help' for a list of commands.\r\n"),
    ));
    if karma > 0 {
        out.push(Outgoing::line(
            entity,
            format!(
                "\x1b[33mYou have {karma} Karma to spend. Type 'score' to see your attributes and \
                 'raise <attribute>' to improve one.\x1b[0m\r\n"
            ),
        ));
    }
    out.push(Outgoing::To(
        entity,
        describe_room(
            room,
            entity,
            players,
            rooms,
            account.role.can(Permission::Build),
        ),
    ));
    out.push(Outgoing::Room {
        room,
        except: entity,
        text: format!("{name} has entered the game.\r\n"),
    });
    out.push(Outgoing::prompt(entity));
    None
}
