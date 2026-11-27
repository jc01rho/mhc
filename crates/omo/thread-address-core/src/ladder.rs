//! The one thread-part ladder every resolver runs over its candidates (#14305 U8).
//!
//! Upstream counterpart: `ladder.ts`. The steps, in order: exact id > session id (a host routing id or a device's
//! listing key) > alias > name in the caller's scope > unique 8+ hex id prefix. The first step that matches answers, one
//! candidate or many; what many holders mean is the caller's call. Candidates are named by their position in the slice
//! the caller passed.

use crate::name::normalize_name;
use crate::parse::is_id_prefix;

/// What the ladder reads of one candidate thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LadderView<'a> {
    /// The thread id; empty when the row publishes none.
    pub id: &'a str,
    /// Session ids that also name it: a host routing id (`rpc-N`) or a device's listing key.
    pub keys: &'a [String],
    /// Account aliases it holds.
    pub aliases: &'a [String],
    /// Its own name; empty when it has none (a fallback name is never an address).
    pub name: &'a str,
}

/// The ladder steps, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LadderStep {
    Id,
    SessionId,
    Alias,
    Name,
    IdPrefix,
}

impl LadderStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::SessionId => "session_id",
            Self::Alias => "alias",
            Self::Name => "name",
            Self::IdPrefix => "id_prefix",
        }
    }
}

/// The first step that matched and the positions of the candidates it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LadderMatch {
    One {
        step: LadderStep,
        index: usize,
    },
    Many {
        step: LadderStep,
        indices: Vec<usize>,
    },
    NoMatch,
}

/// The id prefix step: off, on, or on with one thread id left out of its candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdPrefixStep<'a> {
    Off,
    On,
    Exclude(&'a str),
}

fn found(step: LadderStep, indices: Vec<usize>) -> Option<LadderMatch> {
    match indices.len() {
        0 => None,
        1 => Some(LadderMatch::One {
            step,
            index: indices[0],
        }),
        _ => Some(LadderMatch::Many { step, indices }),
    }
}

fn positions(views: &[LadderView<'_>], test: impl Fn(&LadderView<'_>) -> bool) -> Vec<usize> {
    views
        .iter()
        .enumerate()
        .filter_map(|(index, view)| test(view).then_some(index))
        .collect()
}

/// Reads one thread part against the candidates. `named` says whether the candidate at a position may match by name
/// (the caller's scope); ids, session ids, aliases and prefixes are not scoped. An exact id and a session id may be
/// held by several candidates (one thread listed on two devices); an alias names one thread, so its first holder
/// answers; a prefix counts each thread id once.
pub fn match_thread(
    views: &[LadderView<'_>],
    text: &str,
    mut named: impl FnMut(usize) -> bool,
    id_prefix: IdPrefixStep<'_>,
) -> LadderMatch {
    if !text.is_empty()
        && let Some(hit) = found(LadderStep::Id, positions(views, |view| view.id == text))
    {
        return hit;
    }
    if let Some(hit) = found(
        LadderStep::SessionId,
        positions(views, |view| view.keys.iter().any(|key| key == text)),
    ) {
        return hit;
    }
    let wanted = normalize_name(text);
    if wanted.is_empty() {
        return LadderMatch::NoMatch;
    }
    if let Some(index) = views.iter().position(|view| {
        view.aliases
            .iter()
            .any(|alias| normalize_name(alias) == wanted)
    }) {
        return LadderMatch::One {
            step: LadderStep::Alias,
            index,
        };
    }
    let by_name: Vec<usize> = views
        .iter()
        .enumerate()
        .filter_map(|(index, view)| {
            (!view.name.is_empty() && normalize_name(view.name) == wanted && named(index))
                .then_some(index)
        })
        .collect();
    if let Some(hit) = found(LadderStep::Name, by_name) {
        return hit;
    }
    let excluded = match id_prefix {
        IdPrefixStep::Off => return LadderMatch::NoMatch,
        IdPrefixStep::On => None,
        IdPrefixStep::Exclude(id) => Some(id),
    };
    if !is_id_prefix(text) {
        return LadderMatch::NoMatch;
    }
    let prefix = text.to_lowercase();
    let mut seen: Vec<&str> = Vec::new();
    let mut picked: Vec<usize> = Vec::new();
    for (index, view) in views.iter().enumerate() {
        if Some(view.id) != excluded
            && view.id.to_lowercase().starts_with(&prefix)
            && !seen.contains(&view.id)
        {
            seen.push(view.id);
            picked.push(index);
        }
    }
    found(LadderStep::IdPrefix, picked).unwrap_or(LadderMatch::NoMatch)
}
