//! Resolving what a player means by a target word: `goblin`, `2.goblin`, `all.coin`, `all`, `me`.
//!
//! Anything that can be targeted carries `Keywords`. A word matches a thing when it is the start of
//! any of its keywords, so `fro` finds Frodo. Callers supply the candidates (the room's occupants,
//! an inventory, everyone online) in the order they should be searched.

use bevy::prelude::*;

/// Lowercase words a thing can be referred to by.
#[derive(Component, Clone, Debug, Default)]
pub struct Keywords(pub Vec<String>);

impl Keywords {
    /// Keywords from a display name: "Old Tom" can be found as `old` or `tom`.
    pub fn from_name(name: &str) -> Self {
        Self(name.split_whitespace().map(str::to_lowercase).collect())
    }

    pub fn matches(&self, word: &str) -> bool {
        let word = word.to_lowercase();
        !word.is_empty() && self.0.iter().any(|keyword| keyword.starts_with(&word))
    }
}

/// A parsed target argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// `me` or `self`.
    Me,
    /// `goblin` (the first) or `2.goblin` (the second).
    Nth { n: usize, word: String },
    /// `all`, or `all.coin` for every match.
    All { word: Option<String> },
}

impl Target {
    /// Parse a target word. Returns `None` for an empty argument or a nonsense index like `0.x`.
    pub fn parse(arg: &str) -> Option<Self> {
        let arg = arg.trim().to_lowercase();
        if arg.is_empty() {
            return None;
        }
        if arg == "me" || arg == "self" {
            return Some(Target::Me);
        }
        if arg == "all" {
            return Some(Target::All { word: None });
        }
        if let Some(word) = arg.strip_prefix("all.") {
            return (!word.is_empty()).then(|| Target::All {
                word: Some(word.into()),
            });
        }
        if let Some((n, word)) = arg.split_once('.')
            && let Ok(n) = n.parse::<usize>()
        {
            return (n >= 1 && !word.is_empty()).then(|| Target::Nth {
                n,
                word: word.into(),
            });
        }
        Some(Target::Nth { n: 1, word: arg })
    }

    /// The single thing this target names, among `candidates`. `viewer` is who is asking: `me`
    /// resolves to them, and keyword searches prefer everyone else, falling back to the viewer only
    /// when nothing else matches. `all` targets resolve to nothing here.
    pub fn find_one<'a>(
        &self,
        viewer: Entity,
        candidates: impl IntoIterator<Item = (Entity, &'a Keywords)>,
    ) -> Option<Entity> {
        match self {
            Target::Me => Some(viewer),
            Target::Nth { n, word } => {
                let mut viewer_matches = false;
                let found = candidates
                    .into_iter()
                    .filter(|(entity, keywords)| {
                        let matches = keywords.matches(word);
                        if *entity == viewer {
                            viewer_matches |= matches;
                            return false;
                        }
                        matches
                    })
                    .nth(n - 1)
                    .map(|(entity, _)| entity);
                found.or((*n == 1 && viewer_matches).then_some(viewer))
            }
            Target::All { .. } => None,
        }
    }

    /// Everything this target names: all matches for `all` and `all.word`, otherwise at most one.
    // First used by item commands (`get all.coin`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn find_all<'a>(
        &self,
        viewer: Entity,
        candidates: impl IntoIterator<Item = (Entity, &'a Keywords)>,
    ) -> Vec<Entity> {
        match self {
            Target::All { word } => candidates
                .into_iter()
                .filter(|(entity, keywords)| {
                    *entity != viewer && word.as_ref().is_none_or(|w| keywords.matches(w))
                })
                .map(|(entity, _)| entity)
                .collect(),
            _ => self.find_one(viewer, candidates).into_iter().collect(),
        }
    }

    /// How to describe this target in a "you don't see ..." message.
    pub fn describe(&self) -> String {
        match self {
            Target::Me => "yourself".into(),
            Target::Nth { n: 1, word } => word.clone(),
            Target::Nth { n, word } => format!("{n}.{word}"),
            Target::All { word: Some(word) } => format!("any {word}"),
            Target::All { word: None } => "anything".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (World, Vec<(Entity, Keywords)>) {
        let mut world = World::new();
        let things = ["Frodo", "Fredegar", "goblin warrior", "goblin archer", "Gandalf"]
            .into_iter()
            .map(|name| (world.spawn_empty().id(), Keywords::from_name(name)))
            .collect();
        (world, things)
    }

    fn refs(things: &[(Entity, Keywords)]) -> impl Iterator<Item = (Entity, &Keywords)> {
        things.iter().map(|(e, k)| (*e, k))
    }

    #[test]
    fn parses_target_forms() {
        assert_eq!(Target::parse(""), None);
        assert_eq!(Target::parse("ME"), Some(Target::Me));
        assert_eq!(Target::parse("all"), Some(Target::All { word: None }));
        assert_eq!(
            Target::parse("all.coin"),
            Some(Target::All {
                word: Some("coin".into())
            })
        );
        assert_eq!(
            Target::parse("2.goblin"),
            Some(Target::Nth {
                n: 2,
                word: "goblin".into()
            })
        );
        assert_eq!(Target::parse("0.goblin"), None);
        assert_eq!(Target::parse("all."), None);
        assert_eq!(
            Target::parse("Goblin"),
            Some(Target::Nth {
                n: 1,
                word: "goblin".into()
            })
        );
    }

    #[test]
    fn finds_by_keyword_prefix_and_index() {
        let (_world, things) = world();
        let viewer = things[4].0;
        let find = |arg: &str| Target::parse(arg).unwrap().find_one(viewer, refs(&things));

        assert_eq!(find("frodo"), Some(things[0].0));
        assert_eq!(find("fr"), Some(things[0].0));
        assert_eq!(find("2.fr"), Some(things[1].0));
        assert_eq!(find("3.fr"), None);
        assert_eq!(find("archer"), Some(things[3].0));
        assert_eq!(find("2.goblin"), Some(things[3].0));
        assert_eq!(find("me"), Some(viewer));
        assert_eq!(find("gandalf"), Some(viewer), "the viewer is the last resort");
        assert_eq!(find("troll"), None);
    }

    #[test]
    fn all_targets_return_every_match() {
        let (_world, things) = world();
        let viewer = things[4].0;
        let all = |arg: &str| Target::parse(arg).unwrap().find_all(viewer, refs(&things));

        assert_eq!(all("all.goblin"), vec![things[2].0, things[3].0]);
        assert_eq!(all("all").len(), 4);
        assert_eq!(all("goblin"), vec![things[2].0]);
    }
}
