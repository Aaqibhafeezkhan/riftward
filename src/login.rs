//! Connecting: log in to (or create) an account, then pick (or create) a character.
//!
//! Each connection that hasn't entered the game yet carries a `LoginState` component. Once a
//! character is chosen, the state is replaced by `Player` and `InRoom`.
//!
//! Creating a character goes: name, race, spending starting Karma, background, description. Each
//! step after the name can go `back`. Nothing but the name is saved until the end, so a character
//! left half-made (or made before races existed) goes through creation again at its next login.
//!
//! `quit` in the game leaves the character and comes back to the character menu, on a fresh
//! entity so nothing of the character lingers; `quit` at the menu disconnects.

use bevy::prelude::*;

use crate::aliases::Aliases;
use crate::attributes::{Attribute, Attributes, experience_for};
use crate::characters::{self, Backgrounds, RaceDef, Races, STARTING_ITEMS};
use crate::combat::{AutoAttack, Conditions, HideNumbers};
use crate::command::{self, ActionQueue};
use crate::commands::movement::{View, describe_room};
use crate::db::{Account, CharacterRecord, Database, ItemPlace, Permission, Role};
use crate::items::{HANDS, ItemTemplates, Quality};
use crate::money::Purse;
use crate::net::{self, Connection, JustConnected, PlayerInput};
use crate::output::{Characters, Outbox, Outgoing, deliver};
use crate::skills::{SkillDefs, Skills, UNIVERSAL_SKILLS, learn};
use crate::spells::Spells;
use crate::targeting::Keywords;
use crate::vitals::{DEFAULT_PROMPT, Maxima, PromptFormat, Vitals};
use crate::world::{InRoom, Player, RoomIndex, RoomKey};
use crate::world::{ItemKey, Slot};

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
        )
        .add_systems(PostUpdate, return_to_menu.after(net::cleanup_disconnected));
    }
}

const BANNER: &str = r"
   ____   ___  _____  _____ __        __    _     ____   ____
  |  _ \ |_ _||  ___||_   _|\ \      / /   / \   |  _ \ |  _ \
  | |_) | | | | |_     | |   \ \ /\ / /   / _ \  | |_) || | | |
  |  _ <  | | |  _|    | |    \ V  V /   / ___ \ |  _ < | |_| |
  |_| \_\|___||_|      |_|     \_/\_/   /_/   \_\|_| \_\|____/
";

const MAX_PASSWORD_ATTEMPTS: u32 = 3;
const MIN_PASSWORD_LEN: usize = 8;

#[derive(Component)]
pub enum LoginState {
    Username,
    Password {
        account: Account,
        attempts: u32,
    },
    NewPassword {
        username: String,
    },
    ConfirmPassword {
        username: String,
        password: String,
    },
    ChooseCharacter {
        account: Account,
        characters: Vec<CharacterRecord>,
    },
    NameCharacter {
        account: Account,
    },
    ChooseRace(Draft),
    SpendKarma(Draft),
    ChooseBackground(Draft),
    Describe(Draft),
}

/// A character being created, as far as the player has got.
pub struct Draft {
    account: Account,
    character: CharacterRecord,
    race: String,
    attributes: Attributes,
    background: String,
    /// A description they already have, which they can keep.
    description: String,
    /// Being made for the first time, so they get a starting kit at the end; not when choosing
    /// again after a race or background was removed.
    fresh: bool,
}

/// The most a character description can hold.
const MAX_DESCRIPTION_LEN: usize = 600;

/// The races and backgrounds creation offers, and the skills and items they grant.
struct Choices<'a> {
    races: &'a Races,
    backgrounds: &'a Backgrounds,
    skills: &'a SkillDefs,
    items: &'a ItemTemplates,
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
        conn.send(
            "Enter your account name. If it doesn't exist yet, you'll be asked to create it.",
        );
        conn.send_raw("Account: ");
        commands
            .entity(entity)
            .remove::<JustConnected>()
            .insert(LoginState::Username);
    }
}

/// Players who quit their character, once everything that happens when someone leaves has:
/// their connection moves to a fresh entity at the character menu, and the old one goes.
fn return_to_menu(
    mut commands: Commands,
    leaving: Query<(Entity, &Connection, Option<&Player>)>,
    db: Res<Database>,
) {
    for (entity, conn, player) in &leaving {
        if !(conn.closing && conn.to_menu) {
            continue;
        }
        let account = player.and_then(|p| match db.find_account(&p.account) {
            Ok(account) => account,
            Err(err) => {
                error!("failed to find {}'s account: {err}", p.account);
                None
            }
        });
        let Some(account) = account else {
            commands.entity(entity).despawn();
            continue;
        };
        commands.queue(move |world: &mut World| {
            let Some(mut conn) = world.entity_mut(entity).take::<Connection>() else {
                return;
            };
            world.despawn(entity);
            conn.back_to_login();
            let fresh = world.spawn(conn).id();
            let mut out = Vec::new();
            match character_menu(fresh, account, world.resource::<Database>(), &mut out) {
                Step::Next(state) => {
                    world.entity_mut(fresh).insert(state);
                }
                _ => {
                    if let Some(mut conn) = world.get_mut::<Connection>(fresh) {
                        conn.closing = true;
                    }
                }
            }
            deliver(out, &mut world.resource_mut::<Outbox>());
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_login(
    mut commands: Commands,
    mut inputs: MessageReader<PlayerInput>,
    mut states: Query<&mut LoginState>,
    players: Characters,
    mut outbox: ResMut<Outbox>,
    mut connections: Query<&mut Connection>,
    time: Res<Time>,
    view: View,
    index: Res<RoomIndex>,
    db: Res<Database>,
    races: Res<Races>,
    backgrounds: Res<Backgrounds>,
    skill_defs: Res<SkillDefs>,
    item_templates: Res<ItemTemplates>,
) {
    let choices = Choices {
        races: &races,
        backgrounds: &backgrounds,
        skills: &skill_defs,
        items: &item_templates,
    };
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
            LoginState::ChooseCharacter {
                account,
                characters,
            } => choose_character(entity, line, account, characters, &choices, &db, &mut out),
            LoginState::NameCharacter { account } => {
                name_character(entity, line, account, &choices, &db, &mut out)
            }
            LoginState::ChooseRace(draft) => choose_race(entity, line, draft, &choices, &mut out),
            LoginState::SpendKarma(draft) => spend_karma(entity, line, draft, &choices, &mut out),
            LoginState::ChooseBackground(draft) => {
                choose_background(entity, line, draft, &choices, &mut out)
            }
            LoginState::Describe(draft) => describe(entity, line, draft, &choices, &db, &mut out),
        };

        // The client doesn't echo the newline after a hidden password either, so add one.
        if let Ok(mut conn) = connections.get_mut(entity) {
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
                if let Ok(mut conn) = connections.get_mut(entity) {
                    conn.closing = true;
                }
            }
            Step::Enter { account, character } => {
                match enter_game(
                    entity,
                    account,
                    character,
                    &mut commands,
                    &players,
                    &view,
                    &index,
                    &choices,
                    &db,
                    time.elapsed_secs_f64(),
                    &mut out,
                ) {
                    Some(next) => *state = next,
                    None => {
                        if let Ok(mut conn) = connections.get_mut(entity) {
                            conn.enable_prompt();
                        }
                    }
                }
            }
        }
        deliver(std::mem::take(&mut out), &mut outbox);
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
            info!(
                "created account '{}' ({})",
                account.username,
                account.role.as_str()
            );
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
fn character_menu(
    entity: Entity,
    account: Account,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
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
    text += "Enter a number or name to play, 'new <name>' to create a character, or 'quit': ";
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
    choices: &Choices,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    if line.eq_ignore_ascii_case("quit") {
        out.push(Outgoing::line(entity, "Farewell!"));
        return Step::Disconnect;
    }
    if let Some(name) = line.strip_prefix("new ") {
        return name_character(entity, name.trim(), account, choices, db, out);
    }

    let chosen = match line.parse::<usize>() {
        Ok(n) => n.checked_sub(1).and_then(|i| characters.get(i)),
        Err(_) => characters
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(line)),
    };
    let race = chosen.and_then(|c| c.race.as_deref());
    let background = chosen.and_then(|c| c.background.as_deref());
    match chosen {
        Some(character) if race.is_none() => {
            out.push(Outgoing::line(
                entity,
                format!("{} isn't finished yet. Let's finish them.", character.name),
            ));
            start_creation(entity, account, character.clone(), choices, out)
        }
        // A race or background removed from the data files since: choose again before playing.
        Some(character) if race.is_some_and(|r| choices.races.get(r).is_none()) => {
            out.push(Outgoing::line(
                entity,
                format!(
                    "\x1b[33m{}'s race, '{}', no longer exists in this world. Choose a new one; \
                     your starting Karma is yours to spend again.\x1b[0m",
                    character.name,
                    race.unwrap_or_default()
                ),
            ));
            start_creation(entity, account, character.clone(), choices, out)
        }
        Some(character) if background.is_none_or(|b| choices.backgrounds.get(b).is_none()) => {
            out.push(Outgoing::line(
                entity,
                format!(
                    "\x1b[33m{}'s background, '{}', no longer exists. Choose a new one; \
                     everything else stays as it was.\x1b[0m",
                    character.name,
                    background.unwrap_or_default()
                ),
            ));
            let race = race.unwrap_or_default().to_string();
            let attributes = choices
                .races
                .get(&race)
                .map(RaceDef::starting_attributes)
                .unwrap_or_default()
                .with_rows(db.attributes(character.id).unwrap_or_default());
            out.push(Outgoing::To(
                entity,
                characters::background_menu(choices.backgrounds, choices.races, &race, &attributes),
            ));
            Step::Next(LoginState::ChooseBackground(Draft {
                description: character.description.clone(),
                fresh: false,
                account,
                character: character.clone(),
                race,
                attributes,
                background: String::new(),
            }))
        }
        Some(character) => Step::Enter {
            character: character.clone(),
            account,
        },
        None => {
            out.push(Outgoing::To(
                entity,
                "No such character. Enter a number, a name, or 'new <name>', or 'quit': ".into(),
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
    choices: &Choices,
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
                start_creation(entity, account, character, choices, out)
            }
            Err(err) => db_failure(entity, err, out),
        },
        Err(err) => db_failure(entity, err, out),
    }
}

/// Begin creating a named character, at the choice of race.
fn start_creation(
    entity: Entity,
    account: Account,
    character: CharacterRecord,
    choices: &Choices,
    out: &mut Vec<Outgoing>,
) -> Step {
    out.push(Outgoing::To(entity, characters::race_menu(choices.races)));
    Step::Next(LoginState::ChooseRace(Draft {
        description: character.description.clone(),
        fresh: character.race.is_none(),
        account,
        character,
        race: String::new(),
        attributes: Attributes::default(),
        background: String::new(),
    }))
}

fn choose_race(
    entity: Entity,
    line: &str,
    mut draft: Draft,
    choices: &Choices,
    out: &mut Vec<Outgoing>,
) -> Step {
    let Some(race) = choices.races.choose(line) else {
        out.push(Outgoing::To(
            entity,
            "That isn't one of the races. Enter a number or name: ".into(),
        ));
        return Step::Next(LoginState::ChooseRace(draft));
    };
    // Choosing again (after 'back') starts the Karma over, since the starting ratings change.
    if draft.race != race.id {
        draft.race = race.id.clone();
        draft.attributes = race.starting_attributes();
    }
    out.push(Outgoing::To(
        entity,
        characters::karma_sheet(race, &draft.attributes),
    ));
    Step::Next(LoginState::SpendKarma(draft))
}

fn spend_karma(
    entity: Entity,
    line: &str,
    mut draft: Draft,
    choices: &Choices,
    out: &mut Vec<Outgoing>,
) -> Step {
    let Some(race) = choices.races.get(&draft.race) else {
        return start_creation(entity, draft.account, draft.character, choices, out);
    };
    let (verb, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    let result = match verb.to_ascii_lowercase().as_str() {
        "back" => {
            out.push(Outgoing::To(entity, characters::race_menu(choices.races)));
            return Step::Next(LoginState::ChooseRace(draft));
        }
        "done" => {
            out.push(Outgoing::To(
                entity,
                characters::background_menu(
                    choices.backgrounds,
                    choices.races,
                    &draft.race,
                    &draft.attributes,
                ),
            ));
            return Step::Next(LoginState::ChooseBackground(draft));
        }
        "raise" | "lower" => Attribute::parse(rest.trim()).and_then(|attribute| {
            if verb.eq_ignore_ascii_case("raise") {
                draft.attributes.raise(attribute).map(|_| ())
            } else {
                draft.attributes.lower(attribute)
            }
        }),
        _ => Err("Type 'raise <attribute>', 'lower <attribute>', 'done' or 'back'.".into()),
    };
    let mut text = characters::karma_sheet(race, &draft.attributes);
    if let Err(err) = result {
        text = format!("\x1b[31m{err}\x1b[0m\r\n{text}");
    }
    out.push(Outgoing::To(entity, text));
    Step::Next(LoginState::SpendKarma(draft))
}

fn choose_background(
    entity: Entity,
    line: &str,
    mut draft: Draft,
    choices: &Choices,
    out: &mut Vec<Outgoing>,
) -> Step {
    if line.trim().eq_ignore_ascii_case("back") {
        if let Some(race) = choices.races.get(&draft.race) {
            out.push(Outgoing::To(
                entity,
                characters::karma_sheet(race, &draft.attributes),
            ));
        }
        return Step::Next(LoginState::SpendKarma(draft));
    }
    let Some(background) = choices.backgrounds.choose(line) else {
        out.push(Outgoing::To(
            entity,
            "That isn't one of the backgrounds. Enter a number or name, or 'back': ".into(),
        ));
        return Step::Next(LoginState::ChooseBackground(draft));
    };
    if let Some(refusal) =
        characters::background_refusal(background, choices.races, &draft.race, &draft.attributes)
    {
        out.push(Outgoing::To(entity, refusal));
        return Step::Next(LoginState::ChooseBackground(draft));
    }
    draft.background = background.id.clone();
    let text = if draft.description.is_empty() {
        format!(
            "\r\nLast, describe {} as others see them when they look: a sentence or two about \
             their looks and manner. You can change it later with 'describe'.\r\n\
             Type the description, 'skip' to leave it for now, or 'back': ",
            draft.character.name
        )
    } else {
        format!(
            "\r\nOthers see {} as:\r\n{}\r\nType a new description, 'keep' to keep it, or \
             'back': ",
            draft.character.name, draft.description
        )
    };
    out.push(Outgoing::To(entity, text));
    Step::Next(LoginState::Describe(draft))
}

fn describe(
    entity: Entity,
    line: &str,
    mut draft: Draft,
    choices: &Choices,
    db: &Database,
    out: &mut Vec<Outgoing>,
) -> Step {
    let line = line.trim();
    if line.eq_ignore_ascii_case("back") {
        out.push(Outgoing::To(
            entity,
            characters::background_menu(
                choices.backgrounds,
                choices.races,
                &draft.race,
                &draft.attributes,
            ),
        ));
        return Step::Next(LoginState::ChooseBackground(draft));
    }
    if line.chars().count() > MAX_DESCRIPTION_LEN {
        out.push(Outgoing::To(
            entity,
            format!("Keep it under {MAX_DESCRIPTION_LEN} characters. Try again: "),
        ));
        return Step::Next(LoginState::Describe(draft));
    }
    let description = match line.to_ascii_lowercase().as_str() {
        "skip" => String::new(),
        "keep" => draft.description.clone(),
        _ => line.to_string(),
    };
    let id = draft.character.id;
    if let Err(err) = db.finish_creation(
        id,
        &draft.race,
        &draft.background,
        &description,
        &draft.attributes.rows(),
    ) {
        return db_failure(entity, err, out);
    }
    info!(
        "{} finished creation: {} {}",
        draft.character.name, draft.race, draft.background
    );
    if draft.fresh {
        let kit = choices
            .backgrounds
            .get(&draft.background)
            .map(|b| b.items.clone())
            .unwrap_or_default();
        give_kit(id, &kit, choices.items, db);
    }
    draft.character.race = Some(draft.race);
    draft.character.background = Some(draft.background);
    draft.character.description = description;
    Step::Enter {
        account: draft.account,
        character: draft.character,
    }
}

/// Give a new character everyone's starting items and their background's kit: worn if it's worn
/// and the slot is free, otherwise in hand while hands are free, otherwise in the first container
/// they're wearing. Their items load from the database as they enter the game.
fn give_kit(character_id: i64, kit: &[String], templates: &ItemTemplates, db: &Database) {
    let mut worn: Vec<Slot> = Vec::new();
    let mut held = 0;
    let mut pouch: Option<i64> = None;
    let all = STARTING_ITEMS
        .iter()
        .map(|s| s.to_string())
        .chain(kit.iter().cloned());
    for template in all {
        let Some(def) = templates.0.get(&ItemKey::parse(&template, "")) else {
            warn!("starting item [{template}] doesn't exist");
            continue;
        };
        let place = match def.wear {
            Some(slot) if worn.iter().filter(|s| **s == slot).count() < slot.capacity() => {
                worn.push(slot);
                ItemPlace::Worn(character_id, slot.name())
            }
            _ if held < HANDS => {
                held += 1;
                ItemPlace::Held(character_id)
            }
            _ => match pouch {
                Some(container) => ItemPlace::Inside(container),
                None => {
                    warn!("no room for starting item [{template}]");
                    continue;
                }
            },
        };
        match db.create_item(&template, &place, Quality::Standard) {
            Ok(id) => {
                if pouch.is_none()
                    && def.container.is_some()
                    && matches!(place, ItemPlace::Worn(..))
                {
                    pouch = Some(id);
                }
            }
            Err(err) => error!("failed to give starting item [{template}]: {err}"),
        }
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
    players: &Characters,
    view: &View,
    index: &RoomIndex,
    choices: &Choices,
    db: &Database,
    now: f64,
    out: &mut Vec<Outgoing>,
) -> Option<LoginState> {
    let back_to_menu = |out: &mut Vec<Outgoing>, reason: &str| {
        out.push(Outgoing::To(
            entity,
            format!("{reason}\r\nEnter a number or name to play, or 'new <name>', or 'quit': "),
        ));
        let characters = db.characters(account.id).unwrap_or_default();
        Some(LoginState::ChooseCharacter {
            account: account.clone(),
            characters,
        })
    };

    let online = players
        .iter()
        .any(|(_, p, _, _)| p.is_some_and(|p| p.character_id == character.id));
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
        && let Ok((_, key, _)) = view.rooms.get(room)
        && let Err(err) = db.save_location(character.id, &key.to_string())
    {
        error!("failed to save location for {}: {err}", character.name);
    }

    let aliases = db.aliases(character.id).unwrap_or_else(|err| {
        error!("failed to load aliases for {}: {err}", character.name);
        Default::default()
    });

    let race = character.race.clone().unwrap_or_default();
    let start = choices
        .races
        .get(&race)
        .map(RaceDef::starting_attributes)
        .unwrap_or_else(|| {
            warn!("{} is of unknown race '{race}'", character.name);
            Attributes::default()
        });
    let mut attributes = start.with_rows(db.attributes(character.id).unwrap_or_else(|err| {
        error!("failed to load attributes for {}: {err}", character.name);
        Vec::new()
    }));

    // Skills, plus any starting skills from race and background they don't have yet: ones that
    // didn't exist when they were made, or that this is their first time in the game to get.
    let mut skills = Skills(
        db.skills(character.id)
            .unwrap_or_else(|err| {
                error!("failed to load skills for {}: {err}", character.name);
                Vec::new()
            })
            .into_iter()
            .collect(),
    );
    let spells = Spells(
        db.spells(character.id)
            .unwrap_or_else(|err| {
                error!("failed to load spells for {}: {err}", character.name);
                Vec::new()
            })
            .into_iter()
            .collect(),
    );
    // What was on them when they left and still lasts.
    let saved = db.conditions(character.id).unwrap_or_else(|err| {
        error!("failed to load conditions for {}: {err}", character.name);
        Vec::new()
    });
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    // And what their race is.
    let conditions = Conditions::from_saved(saved, now, unix)
        .with_traits(choices.races.get(&race).into_iter().flat_map(|r| &r.traits));
    let background = character.background.clone().unwrap_or_default();
    let starting = choices
        .races
        .get(&race)
        .map(|r| r.skills.clone())
        .into_iter()
        .chain(
            choices
                .backgrounds
                .get(&background)
                .map(|b| b.skills.clone()),
        )
        .flatten()
        .chain(UNIVERSAL_SKILLS.iter().map(|key| key.to_string()));
    let mut learned = Vec::new();
    for key in starting {
        if let Some(skill) = choices.skills.skills.get(&key)
            && learn(&mut skills, &key, "Starting", Some(character.id), db)
        {
            learned.push(skill.def.name.clone());
        }
    }
    // Characters from before experience start with what their old level (total skill levels over
    // five) would need, so they keep every point of Karma.
    let experience = character.experience.unwrap_or_else(|| {
        let old_level = skills.0.values().map(|s| s.level).sum::<u32>() / 5;
        let experience = experience_for(old_level);
        if let Err(err) = db.save_experience(character.id, experience) {
            error!("failed to save experience for {}: {err}", character.name);
        }
        experience
    });
    attributes.set_experience(experience);
    attributes.set_karma_on_skills(skills.0.values().map(|s| s.karma_paid as i32).sum());
    let karma = attributes.karma_unspent();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();
    let max = Maxima::of(&attributes, &skills, choices.skills, None);
    let vitals = Vitals::restored(max, character.vitals, character.vitals_at, now);
    let prompt = PromptFormat(
        character
            .prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_PROMPT.into()),
    );

    let name = character.name.clone();
    commands.entity(entity).remove::<LoginState>().insert((
        Aliases(aliases),
        attributes,
        vitals,
        prompt,
        skills,
        spells,
        conditions,
        Purse(character.coins),
        ActionQueue::default(),
        AutoAttack(character.autoattack.clone()),
        Keywords::from_name(&character.name),
        Player {
            name: name.clone(),
            character_id: character.id,
            account: account.username.clone(),
            role: account.role,
            race,
            background: character.background.clone().unwrap_or_default(),
            description: character.description.clone(),
        },
        InRoom(room),
    ));
    if character.hide_numbers {
        commands.entity(entity).insert(HideNumbers);
    }
    if character.nofollow {
        commands.entity(entity).insert(crate::groups::NoFollow);
    }
    if !character.autoassist {
        commands.entity(entity).insert(crate::groups::NoAssist);
    }
    info!("{name} entered the game (account '{}')", account.username);

    out.push(Outgoing::line(
        entity,
        format!("\r\nWelcome, {name}! Type 'help' for a list of commands.\r\n"),
    ));
    if !learned.is_empty() {
        out.push(Outgoing::line(
            entity,
            format!(
                "\x1b[1;32mFrom your life before, you know: {}. Type 'skills' to see them.\x1b[0m\r\n",
                learned.join(", ")
            ),
        ));
    }
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
            view,
            account.role.can(Permission::Stage),
        ),
    ));
    out.push(Outgoing::Room {
        room,
        except: entity,
        text: format!("{name} has entered the game.\r\n"),
    });
    None
}
