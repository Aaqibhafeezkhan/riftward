//! Persistent storage for accounts and characters (SQLite).
//!
//! The world itself lives in zone files under `assets/zones`; the database only holds things
//! that change during play.

use std::collections::BTreeMap;
use std::sync::Mutex;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use bevy::prelude::*;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

use crate::skills::SkillState;

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
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        -- Chosen at creation; a character without a race hasn't finished creating.
        race        TEXT,
        background  TEXT,
        -- What others see when they look at them.
        description TEXT NOT NULL DEFAULT '',
        -- Current vitals and when they were saved; NULL means full.
        health      REAL,
        stamina     REAL,
        vitals_at   INTEGER,
        -- Their prompt format; NULL means the default.
        prompt      TEXT,
        -- The attack skill they make each round of a fight; NULL means the basic attack.
        autoattack  TEXT,
        -- Experience, which sets character level; NULL for characters from before it existed.
        experience  INTEGER,
        -- 1 if they'd rather not see damage numbers in fights.
        hide_numbers INTEGER NOT NULL DEFAULT 0,
        -- Money in their purse, in copper.
        coins       INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE IF NOT EXISTS character_aliases (
        character_id INTEGER NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
        name         TEXT NOT NULL COLLATE NOCASE,
        expansion    TEXT NOT NULL,
        PRIMARY KEY (character_id, name)
    );

    CREATE TABLE IF NOT EXISTS character_attributes (
        character_id INTEGER NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
        attribute    TEXT NOT NULL,
        rating       INTEGER NOT NULL,
        PRIMARY KEY (character_id, attribute)
    );

    -- Real items: ones a player has touched. An item is in exactly one place: lying in a `room`,
    -- with a character (`character_id`, held when `slot` is 'held', else worn in that slot), or
    -- inside another item (`container_id`).
    CREATE TABLE IF NOT EXISTS items (
        id           INTEGER PRIMARY KEY,
        template     TEXT NOT NULL,        -- zone:item
        room         TEXT,                 -- zone:room
        character_id INTEGER REFERENCES characters(id) ON DELETE CASCADE,
        dropped_at   INTEGER,              -- unix seconds, when it was last put down
        slot         TEXT,
        container_id INTEGER REFERENCES items(id) ON DELETE CASCADE,
        -- Its own name, when it isn't its template's: 'the corpse of Grok'.
        name         TEXT
    );

    -- What each character has learned, and how far they've got.
    CREATE TABLE IF NOT EXISTS character_skills (
        character_id INTEGER NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
        skill        TEXT NOT NULL,        -- tree:skill
        level        INTEGER NOT NULL,
        practice     REAL NOT NULL,
        last_used    INTEGER,              -- unix seconds
        next_gain_at INTEGER NOT NULL,     -- unix seconds
        learned_via  TEXT NOT NULL,
        learned_at   INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY (character_id, skill)
    );

    -- The spells each character knows.
    CREATE TABLE IF NOT EXISTS character_spells (
        character_id INTEGER NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
        spell        TEXT NOT NULL,        -- tradition:spell
        learned_via  TEXT NOT NULL,
        learned_at   INTEGER NOT NULL DEFAULT (unixepoch()),
        PRIMARY KEY (character_id, spell)
    );

    -- Spawns waiting to return after their last mob died (`AfterDeath`, or a wiped-out
    -- breeding spawn's reseed), so a restart doesn't bring the dragon back early.
    CREATE TABLE IF NOT EXISTS spawn_state (
        spawn      TEXT PRIMARY KEY,       -- zone:spawn
        next_reset INTEGER NOT NULL        -- unix seconds
    );

    -- The world's flags, like 'rats_broke_out', set and cleared by world events.
    CREATE TABLE IF NOT EXISTS world_state (
        flag   TEXT PRIMARY KEY,
        set_at INTEGER NOT NULL DEFAULT (unixepoch())
    );

    -- How many mobs each breeding spawn has, so a restart brings back the same number.
    CREATE TABLE IF NOT EXISTS spawn_counts (
        spawn TEXT PRIMARY KEY,            -- zone:spawn
        count INTEGER NOT NULL
    );
";

/// Opens the database at `path` (`:memory:` for one that isn't kept).
pub struct DbPlugin {
    pub path: String,
}

impl Plugin for DbPlugin {
    fn build(&self, app: &mut App) {
        let path = &self.path;
        let db = Database::open(path).expect("failed to open database");
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

/// Where a real item is, as stored in the `items` table.
pub enum ItemPlace {
    /// Lying in a room (`zone:room`).
    Room(String),
    /// In a character's hands.
    Held(i64),
    /// Worn by a character, in a slot like `back` or `belt`.
    Worn(i64, &'static str),
    /// Inside another real item.
    Inside(i64),
}

impl ItemPlace {
    /// The (room, character_id, slot, container_id) columns for this place.
    fn columns(&self) -> (Option<&str>, Option<i64>, Option<&str>, Option<i64>) {
        match self {
            ItemPlace::Room(room) => (Some(room), None, None, None),
            ItemPlace::Held(character) => (None, Some(*character), Some("held"), None),
            ItemPlace::Worn(character, slot) => (None, Some(*character), Some(slot), None),
            ItemPlace::Inside(container) => (None, None, None, Some(*container)),
        }
    }

    /// Items on the ground remember when they were put down, for decay later.
    fn dropped_at(&self) -> Option<i64> {
        match self {
            ItemPlace::Room(_) => Some(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_default(),
            ),
            _ => None,
        }
    }
}

/// A real item lying in a room, as saved.
pub struct RoomItemRow {
    pub id: i64,
    pub template: String,
    /// `zone:room`.
    pub room: String,
    /// Its own name, if it has one.
    pub name: Option<String>,
    /// When it was put down, in unix seconds.
    pub dropped_at: Option<i64>,
}

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
    /// `None` until character creation is finished.
    pub race: Option<String>,
    pub background: Option<String>,
    pub description: String,
    /// Health and stamina when last saved, and when; `None` means full.
    pub vitals: Option<(f32, f32)>,
    pub vitals_at: Option<i64>,
    /// Their own prompt format, if they've set one.
    pub prompt: Option<String>,
    /// The attack they've chosen to make each round, if any.
    pub autoattack: Option<String>,
    /// Their experience; `None` for a character from before experience, worked out at login.
    pub experience: Option<u64>,
    /// Whether they'd rather fights didn't show damage numbers.
    pub hide_numbers: bool,
    /// Money in their purse, in copper.
    pub coins: u64,
}

/// SQLite connection. Queries run synchronously on the main thread; they're small and local,
/// but password hashing does take a few milliseconds per login.
#[derive(Resource)]
pub struct Database(Mutex<Connection>);

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        // Columns added since the first versions of these tables.
        for (table, column, ty) in [
            ("items", "slot", "TEXT"),
            ("items", "container_id", "INTEGER REFERENCES items(id)"),
            ("items", "name", "TEXT"),
            ("characters", "race", "TEXT"),
            ("characters", "background", "TEXT"),
            ("characters", "description", "TEXT NOT NULL DEFAULT ''"),
            ("characters", "health", "REAL"),
            ("characters", "stamina", "REAL"),
            ("characters", "vitals_at", "INTEGER"),
            ("characters", "prompt", "TEXT"),
            ("characters", "autoattack", "TEXT"),
            ("characters", "experience", "INTEGER"),
            ("characters", "hide_numbers", "INTEGER NOT NULL DEFAULT 0"),
            ("characters", "coins", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let exists: bool = conn.query_row(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1)"
                ),
                [column],
                |row| row.get(0),
            )?;
            if !exists {
                conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty};"))?;
            }
        }
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
            conn.query_row("SELECT NOT EXISTS (SELECT 1 FROM accounts)", [], |row| {
                row.get(0)
            })?;
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
        let mut stmt = conn.prepare(
            "SELECT id, name, room, race, background, description, health, stamina, vitals_at,
                 prompt, autoattack, experience, hide_numbers, coins FROM characters
                 WHERE account_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([account_id], |row| {
            Ok(CharacterRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                room: row.get(2)?,
                race: row.get(3)?,
                background: row.get(4)?,
                description: row.get(5)?,
                vitals: match (row.get::<_, Option<f64>>(6)?, row.get::<_, Option<f64>>(7)?) {
                    (Some(health), Some(stamina)) => Some((health as f32, stamina as f32)),
                    _ => None,
                },
                vitals_at: row.get(8)?,
                prompt: row.get(9)?,
                autoattack: row.get(10)?,
                experience: row.get::<_, Option<i64>>(11)?.map(|e| e.max(0) as u64),
                hide_numbers: row.get::<_, i64>(12)? != 0,
                coins: row.get::<_, i64>(13)?.max(0) as u64,
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
            "INSERT INTO characters (account_id, name, experience) VALUES (?1, ?2, 0)",
            params![account_id, name],
        )?;
        Ok(CharacterRecord {
            id: conn.last_insert_rowid(),
            name: name.into(),
            room: None,
            race: None,
            background: None,
            description: String::new(),
            vitals: None,
            vitals_at: None,
            prompt: None,
            autoattack: None,
            experience: Some(0),
            hide_numbers: false,
            coins: 0,
        })
    }

    pub fn set_hide_numbers(&self, character_id: i64, hide: bool) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET hide_numbers = ?1 WHERE id = ?2",
            params![i64::from(hide), character_id],
        )?;
        Ok(())
    }

    pub fn save_coins(&self, character_id: i64, coins: u64) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET coins = ?1 WHERE id = ?2",
            params![coins as i64, character_id],
        )?;
        Ok(())
    }

    pub fn save_experience(&self, character_id: i64, experience: u64) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET experience = ?1 WHERE id = ?2",
            params![experience as i64, character_id],
        )?;
        Ok(())
    }

    /// Save a chosen auto-attack, or `None` for fighting with what they hold.
    pub fn set_autoattack(&self, character_id: i64, skill: Option<&str>) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET autoattack = ?1 WHERE id = ?2",
            params![skill, character_id],
        )?;
        Ok(())
    }

    pub fn save_vitals(
        &self,
        character_id: i64,
        health: f32,
        stamina: f32,
        at: i64,
    ) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET health = ?1, stamina = ?2, vitals_at = ?3 WHERE id = ?4",
            params![f64::from(health), f64::from(stamina), at, character_id],
        )?;
        Ok(())
    }

    /// Save a prompt format, or `None` for the default.
    pub fn set_prompt(&self, character_id: i64, prompt: Option<&str>) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET prompt = ?1 WHERE id = ?2",
            params![prompt, character_id],
        )?;
        Ok(())
    }

    /// Save the choices that finish character creation, replacing any attributes saved before.
    pub fn finish_creation(
        &self,
        character_id: i64,
        race: &str,
        background: &str,
        description: &str,
        attributes: &[(&str, i32)],
    ) -> DbResult<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE characters SET race = ?1, background = ?2, description = ?3 WHERE id = ?4",
            params![race, background, description, character_id],
        )?;
        tx.execute(
            "DELETE FROM character_attributes WHERE character_id = ?1",
            [character_id],
        )?;
        for (attribute, rating) in attributes {
            tx.execute(
                "INSERT INTO character_attributes (character_id, attribute, rating)
                 VALUES (?1, ?2, ?3)",
                params![character_id, attribute, rating],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_description(&self, character_id: i64, description: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET description = ?1 WHERE id = ?2",
            params![description, character_id],
        )?;
        Ok(())
    }

    /// Saved attribute ratings; attributes without a row are at their starting rating.
    pub fn attributes(&self, character_id: i64) -> DbResult<Vec<(String, i32)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT attribute, rating FROM character_attributes WHERE character_id = ?1",
        )?;
        let rows = stmt.query_map([character_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_attribute(&self, character_id: i64, attribute: &str, rating: i32) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO character_attributes (character_id, attribute, rating) VALUES (?1, ?2, ?3)
             ON CONFLICT (character_id, attribute) DO UPDATE SET rating = excluded.rating",
            params![character_id, attribute, rating],
        )?;
        Ok(())
    }

    pub fn aliases(&self, character_id: i64) -> DbResult<BTreeMap<String, String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT name, expansion FROM character_aliases WHERE character_id = ?1")?;
        let rows = stmt.query_map([character_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Create or replace an alias.
    pub fn set_alias(&self, character_id: i64, name: &str, expansion: &str) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO character_aliases (character_id, name, expansion) VALUES (?1, ?2, ?3)
             ON CONFLICT (character_id, name) DO UPDATE SET expansion = excluded.expansion",
            params![character_id, name, expansion],
        )?;
        Ok(())
    }

    pub fn remove_alias(&self, character_id: i64, name: &str) -> DbResult<()> {
        self.conn().execute(
            "DELETE FROM character_aliases WHERE character_id = ?1 AND name = ?2",
            params![character_id, name],
        )?;
        Ok(())
    }

    /// Save a new real item at `location` and return its id.
    pub fn create_item(&self, template: &str, location: &ItemPlace) -> DbResult<i64> {
        let conn = self.conn();
        let (room, character, slot, container) = location.columns();
        conn.execute(
            "INSERT INTO items (template, room, character_id, slot, container_id, dropped_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                template,
                room,
                character,
                slot,
                container,
                location.dropped_at()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn move_item(&self, id: i64, location: &ItemPlace) -> DbResult<()> {
        let (room, character, slot, container) = location.columns();
        self.conn().execute(
            "UPDATE items SET room = ?1, character_id = ?2, slot = ?3, container_id = ?4,
             dropped_at = ?5 WHERE id = ?6",
            params![room, character, slot, container, location.dropped_at(), id],
        )?;
        Ok(())
    }

    pub fn delete_item(&self, id: i64) -> DbResult<()> {
        self.conn()
            .execute("DELETE FROM items WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Every real item lying in a room.
    pub fn items_in_rooms(&self) -> DbResult<Vec<RoomItemRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, template, room, name, dropped_at FROM items WHERE room IS NOT NULL
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RoomItemRow {
                id: row.get(0)?,
                template: row.get(1)?,
                room: row.get(2)?,
                name: row.get(3)?,
                dropped_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Give a real item its own name, instead of its template's.
    pub fn set_item_name(&self, id: i64, name: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE items SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(())
    }

    /// Every item a character holds or wears: (id, template, slot). The slot is `held` for
    /// items in their hands.
    pub fn items_on_character(&self, character_id: i64) -> DbResult<Vec<(i64, String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, template, COALESCE(slot, 'held') FROM items
             WHERE character_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([character_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every item inside a container: (id, template).
    pub fn items_inside(&self, container_id: i64) -> DbResult<Vec<(i64, String)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, template FROM items WHERE container_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map([container_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn spawn_timers(&self) -> DbResult<Vec<(String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT spawn, next_reset FROM spawn_state")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Remember when a spawn next resets, or forget it with `None`.
    pub fn set_spawn_timer(&self, spawn: &str, next_reset: Option<i64>) -> DbResult<()> {
        let conn = self.conn();
        match next_reset {
            Some(at) => conn.execute(
                "INSERT INTO spawn_state (spawn, next_reset) VALUES (?1, ?2)
                 ON CONFLICT (spawn) DO UPDATE SET next_reset = excluded.next_reset",
                params![spawn, at],
            )?,
            None => conn.execute("DELETE FROM spawn_state WHERE spawn = ?1", [spawn])?,
        };
        Ok(())
    }

    /// Whether any real item is made from this template (`zone:item`), anywhere.
    pub fn item_exists(&self, template: &str) -> DbResult<bool> {
        Ok(self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM items WHERE template = ?1)",
            [template],
            |row| row.get(0),
        )?)
    }

    pub fn world_flags(&self) -> DbResult<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT flag FROM world_state")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_world_flag(&self, flag: &str, set: bool) -> DbResult<()> {
        let conn = self.conn();
        if set {
            conn.execute(
                "INSERT INTO world_state (flag) VALUES (?1) ON CONFLICT (flag) DO NOTHING",
                [flag],
            )?;
        } else {
            conn.execute("DELETE FROM world_state WHERE flag = ?1", [flag])?;
        }
        Ok(())
    }

    pub fn spawn_counts(&self) -> DbResult<Vec<(String, u32)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT spawn, count FROM spawn_counts")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_spawn_count(&self, spawn: &str, count: u32) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO spawn_counts (spawn, count) VALUES (?1, ?2)
             ON CONFLICT (spawn) DO UPDATE SET count = excluded.count",
            params![spawn, count],
        )?;
        Ok(())
    }

    /// A character's skills, by `tree:skill`.
    pub fn skills(&self, character_id: i64) -> DbResult<Vec<(String, SkillState)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT skill, level, practice, last_used, next_gain_at, learned_via
             FROM character_skills WHERE character_id = ?1",
        )?;
        let rows = stmt.query_map([character_id], |row| {
            let mut state = SkillState::new(&row.get::<_, String>(5)?);
            state.level = row.get(1)?;
            state.practice = row.get(2)?;
            state.last_used = row.get(3)?;
            state.next_gain_at = row.get(4)?;
            Ok((row.get(0)?, state))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn save_skill(&self, character_id: i64, skill: &str, state: &SkillState) -> DbResult<()> {
        self.conn().execute(
            "INSERT INTO character_skills
                 (character_id, skill, level, practice, last_used, next_gain_at, learned_via)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (character_id, skill) DO UPDATE SET
                 level = excluded.level, practice = excluded.practice,
                 last_used = excluded.last_used, next_gain_at = excluded.next_gain_at",
            params![
                character_id,
                skill,
                state.level,
                state.practice,
                state.last_used,
                state.next_gain_at,
                state.learned_via
            ],
        )?;
        Ok(())
    }

    /// The spells a character knows, by `tradition:spell`.
    pub fn spells(&self, character_id: i64) -> DbResult<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT spell FROM character_spells WHERE character_id = ?1")?;
        let rows = stmt.query_map([character_id], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn save_spell(&self, character_id: i64, spell: &str, learned_via: &str) -> DbResult<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO character_spells (character_id, spell, learned_via)
             VALUES (?1, ?2, ?3)",
            params![character_id, spell, learned_via],
        )?;
        Ok(())
    }

    pub fn save_location(&self, character_id: i64, room: &str) -> DbResult<()> {
        self.conn().execute(
            "UPDATE characters SET room = ?1 WHERE id = ?2",
            params![room, character_id],
        )?;
        Ok(())
    }
}
