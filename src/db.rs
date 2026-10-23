//! Persistent storage for accounts and characters (SQLite).
//!
//! The world itself lives in zone files under `assets/zones`; the database only holds things
//! that change during play.

use std::sync::Mutex;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use bevy::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

/// Database file, unless overridden by the `MUD_DB` environment variable.
pub const DEFAULT_DB_PATH: &str = "mud.db";

const SCHEMA: &str = "
    PRAGMA foreign_keys = ON;

    CREATE TABLE IF NOT EXISTS accounts (
        id            INTEGER PRIMARY KEY,
        username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
        password_hash TEXT NOT NULL,
        role          TEXT NOT NULL DEFAULT 'player',
        created_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    );

    CREATE TABLE IF NOT EXISTS characters (
        id         INTEGER PRIMARY KEY,
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
        -- Full room id (`zone:room`) the character was last in.
        room       TEXT,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    );
";

pub struct DbPlugin;

impl Plugin for DbPlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("MUD_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.into());
        let db = Database::open(&path).expect("failed to open database");
        info!("using database {path}");
        app.insert_resource(db);
    }
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("password hashing failed: {0}")]
    Hash(String),
}

pub type DbResult<T> = Result<T, DbError>;

/// What an account is allowed to do. Each role includes everything below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Player,
    Builder,
    Admin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// Create and edit rooms with the builder commands.
    Build,
    /// Change other accounts' roles.
    ManageRoles,
}

impl Role {
    pub fn can(self, permission: Permission) -> bool {
        match permission {
            Permission::Build => self >= Role::Builder,
            Permission::ManageRoles => self >= Role::Admin,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Player => "player",
            Role::Builder => "builder",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "player" => Some(Role::Player),
            "builder" => Some(Role::Builder),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Account {
    pub id: i64,
    pub username: String,
    pub role: Role,
    password_hash: String,
}

impl Account {
    pub fn verify_password(&self, password: &str) -> bool {
        PasswordHash::new(&self.password_hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
    }
}

#[derive(Clone, Debug)]
pub struct CharacterRecord {
    pub id: i64,
    pub name: String,
    /// Full room id (`zone:room`) the character was last in.
    pub room: Option<String>,
}

/// SQLite connection. Queries run synchronously on the main thread; they're small and local,
/// but password hashing does take a few milliseconds per login.
#[derive(Resource)]
pub struct Database(Mutex<Connection>);

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self(Mutex::new(conn)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap()
    }

    pub fn find_account(&self, username: &str) -> DbResult<Option<Account>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, username, role, password_hash FROM accounts WHERE username = ?1",
                [username],
                |row| {
                    Ok(Account {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        role: Role::parse(&row.get::<_, String>(2)?).unwrap_or(Role::Player),
                        password_hash: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Create an account. The very first account on a fresh database becomes an admin, so there
    /// is always someone who can hand out roles.
    pub fn create_account(&self, username: &str, password: &str) -> DbResult<Account> {
        let password_hash = Argon2::default()
            .hash_password(password.as_bytes())
            .map_err(|e| DbError::Hash(e.to_string()))?
            .to_string();

        let conn = self.conn();
        let is_first: bool =
            conn.query_row("SELECT NOT EXISTS (SELECT 1 FROM accounts)", [], |row| row.get(0))?;
        let role = if is_first { Role::Admin } else { Role::Player };
        conn.execute(
            "INSERT INTO accounts (username, password_hash, role) VALUES (?1, ?2, ?3)",
            params![username, password_hash, role.as_str()],
        )?;
        Ok(Account {
            id: conn.last_insert_rowid(),
            username: username.into(),
            role,
            password_hash,
        })
    }

    /// Returns false if no account has that username.
    pub fn set_role(&self, username: &str, role: Role) -> DbResult<bool> {
        let changed = self.conn().execute(
            "UPDATE accounts SET role = ?1 WHERE username = ?2",
            params![role.as_str(), username],
        )?;
        Ok(changed > 0)
    }

    pub fn characters(&self, account_id: i64) -> DbResult<Vec<CharacterRecord>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, name, room FROM characters WHERE account_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(CharacterRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                room: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn character_name_taken(&self, name: &str) -> DbResult<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM characters WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )?)
    }

    pub fn create_character(&self, account_id: i64, name: &str) -> DbResult<CharacterRecord> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO characters (account_id, name) VALUES (?1, ?2)",
            params![account_id, name],
        )?;
        Ok(CharacterRecord {
            id: conn.last_insert_rowid(),
            name: name.into(),
            room: None,
        })
    }

    pub fn save_location(&self, character_id: i64, room: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET room = ?1 WHERE id = ?2",
            params![room, character_id],
        )?;
        Ok(())
    }
}
