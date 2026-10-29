//! The game's rules in numbers, from `assets/rules.ron`: how long a round and an action take,
//! what fleeing and a riposte cost, how vitals are made and recover, and the like. Read the first
//! time they're needed; a file that's missing or wrong stops the server, as `progression.ron`
//! does, since it's better to fix the file than to run on numbers nobody chose.

use serde::Deserialize;

/// Where the rules live, from where the server runs.
const RULES_FILE: &str = "assets/rules.ron";

/// The game's rules in numbers (`assets/rules.ron`, which explains each).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Rules {
    /// Seconds in a combat round: everyone fighting acts once in each.
    pub round_seconds: f64,
    /// Seconds an action takes outside a fight, before the next can start.
    pub action_seconds: f64,
    /// Seconds a prepared spell or opener can wait for its first turn.
    pub ready_timeout_seconds: f64,
    /// Stamina it takes to flee.
    pub flee_stamina: f32,
    /// The chance of getting away, before Luck.
    pub flee_chance: f64,
    /// Stamina a riposte costs.
    pub riposte_stamina: f32,
    /// The chance, in percent, that a mob uses a combat skill on its turn, unless its template says.
    pub mob_tactics: u32,
    /// Health and stamina before attributes: most health is this plus Body over
    /// `attribute_per_vital`, most stamina this plus Willpower over it.
    pub base_vitals: i32,
    pub attribute_per_vital: i32,
    /// Real minutes for health, and for stamina, to recover from nothing to full.
    pub health_recovery_minutes: f32,
    pub stamina_recovery_minutes: f32,
    /// How much faster vitals recover while resting.
    pub rest_speedup: f32,
    /// Drain is resisted against its stamina cost times this, as a fixed difficulty.
    pub drain_difficulty: f64,
    /// How much better a deliberate search is than a passing glance: Intuition times this.
    pub search_bonus: f64,
}

static RULES: std::sync::OnceLock<Rules> = std::sync::OnceLock::new();

/// The rules, read from `assets/rules.ron` the first time they're needed.
pub fn rules() -> &'static Rules {
    RULES.get_or_init(|| {
        let text = std::fs::read_to_string(RULES_FILE)
            .unwrap_or_else(|err| panic!("can't read {RULES_FILE}: {err}"));
        crate::world::ron_options()
            .from_str(&text)
            .unwrap_or_else(|err| panic!("{RULES_FILE} doesn't parse: {err}"))
    })
}
