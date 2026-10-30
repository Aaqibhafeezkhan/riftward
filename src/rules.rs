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
    /// About how many real minutes between ambient lines in a room with a player in it.
    pub chatter_minutes: f64,
    /// Real seconds a mob born hidden stays in sight, out of a fight, before it hides again.
    pub rehide_seconds: f64,
    /// The damage of a fall, rolled for each room fallen through: low and high.
    pub fall_damage_per_room: (u32, u32),
    /// Tracks rooms keep, for Tracking.
    pub tracks: TrackRules,
    /// Finds rooms hold, for Foraging and Herbalism.
    pub finds: FindRules,
    /// Shops' terms, and what Haggling wins.
    pub trade: TradeRules,
    /// How much harder fine and rare ingredients are to craft with.
    pub crafting: CraftingRules,
}

/// How much harder fine and rare ingredients make crafting.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct CraftingRules {
    /// The difficulty is multiplied by this for each step the makings' average quality is above
    /// standard.
    pub fine_makings: f64,
    /// And by this for each step the rarest of them is above common.
    pub rare_makings: f64,
}

/// How long rooms remember tracks, and how they read.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct TrackRules {
    /// Real minutes tracks last.
    pub minutes: f64,
    /// The most a room remembers; the oldest go first.
    pub most: usize,
    /// How hard each ground is to read a fresh track on; wading water holds none.
    pub ground: Ground,
    /// How a track's age reads, youngest first: under so many minutes, these words.
    pub ages: Vec<(f64, String)>,
    /// How an older track reads.
    pub older: String,
}

/// A number for each kind of ground that holds tracks.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Ground {
    pub mud: f64,
    pub uneven: f64,
    pub rough: f64,
    pub road: f64,
    pub paved: f64,
}

/// How many finds a room holds, and how fast they grow back.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct FindRules {
    /// Finds of each kind a room holds when fully grown.
    pub patch: u32,
    /// Real minutes for one find to grow back.
    pub regrow_minutes: f64,
}

/// What shops pay, and what a won haggle moves prices by.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct TradeRules {
    /// What a shop pays for an item before haggling: this share of its value.
    pub buying_share: f64,
    /// Real minutes a deal with a shopkeeper lasts.
    pub deal_minutes: f64,
    /// The share of an item's value a won haggle moves prices by, and how it reads, by how surely
    /// it was won.
    pub glancing: Deal,
    pub solid: Deal,
    pub strong: Deal,
    pub crushing: Deal,
}

/// How far a haggle moved prices, and how that reads ("a tenth").
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Deal {
    pub share: f64,
    pub words: String,
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
