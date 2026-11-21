//! The process-wide half of senpi `packages/coding-agent/src/core/credential-proxy/runtime.ts` (6d4426a28;
//! senpi#6828, senpi#6830): the holds every credential-proxy activation publishes, and the view of a child
//! environment they impose.
//!
//! senpi shares this state between engine copies through a realm registry; here it is one process-global
//! registry in the lowest crate that builds child environments, so the bash tool, the terminal, hooks, the tool
//! child and the eval kernels all read the same holds. Withholding is keyed on the declaration, not on the
//! proxy: a credential whose proxy failed to start is still replaced by the placeholder in every agent child
//! environment (fail closed).
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

pub const CREDENTIAL_PLACEHOLDER: &str = "senpi-credential-proxy-placeholder";

/// One activation's hold.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialHold {
    /// Variable name -> the engine's value at activation (`None` when the engine had none).
    pub withheld: Vec<(String, Option<String>)>,
    /// Child variable -> proxy route, set only once the proxy listens.
    pub routes: Vec<(String, String)>,
}

struct Holder {
    key: String,
    generation: u64,
    hold: CredentialHold,
}

struct Holds {
    next_generation: u64,
    holders: Vec<Holder>,
}

static HOLDS: Mutex<Holds> = Mutex::new(Holds {
    next_generation: 1,
    holders: Vec::new(),
});

fn holds() -> std::sync::MutexGuard<'static, Holds> {
    HOLDS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Publishes `hold` for `holder`, replacing its previous one; the returned generation identifies this hold.
pub fn set_hold(holder: &str, hold: CredentialHold) -> u64 {
    let mut holds = holds();
    let generation = holds.next_generation;
    holds.next_generation += 1;
    holds.holders.retain(|entry| entry.key != holder);
    holds.holders.push(Holder {
        key: holder.to_owned(),
        generation,
        hold,
    });
    generation
}

/// Drops `holder`'s hold; true when it had one.
pub fn release_hold(holder: &str) -> bool {
    let mut holds = holds();
    let before = holds.holders.len();
    holds.holders.retain(|entry| entry.key != holder);
    holds.holders.len() != before
}

/// Whether `generation` is still `holder`'s current hold.
pub fn is_current(holder: &str, generation: u64) -> bool {
    holds()
        .holders
        .iter()
        .any(|entry| entry.key == holder && entry.generation == generation)
}

/// Routes `name` to `route` in `holder`'s hold of `generation`; false when that hold was replaced or released.
pub fn set_route(holder: &str, generation: u64, name: &str, route: &str) -> bool {
    let mut holds = holds();
    let Some(entry) = holds
        .holders
        .iter_mut()
        .find(|entry| entry.key == holder && entry.generation == generation)
    else {
        return false;
    };
    entry.hold.routes.retain(|(existing, _)| existing != name);
    entry.hold.routes.push((name.to_owned(), route.to_owned()));
    true
}

/// `env` with every declared credential, and any other variable carrying one of their values, set to the
/// placeholder, and each configured base-URL variable set to its proxy route.
pub fn with_credential_proxy<I, C>(env: I) -> C
where
    I: IntoIterator<Item = (String, String)>,
    C: FromIterator<(String, String)>,
{
    let active: Vec<CredentialHold> = holds()
        .holders
        .iter()
        .map(|entry| entry.hold.clone())
        .collect();
    if active.is_empty() {
        return env.into_iter().collect();
    }
    let mut names = Vec::new();
    let mut values = Vec::new();
    for hold in &active {
        for (name, value) in &hold.withheld {
            names.push(name.clone());
            if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                values.push(value.clone());
            }
        }
    }
    let mut result: BTreeMap<String, String> = env
        .into_iter()
        .map(|(name, value)| {
            if names.contains(&name) || values.contains(&value) {
                (name, CREDENTIAL_PLACEHOLDER.to_owned())
            } else {
                (name, value)
            }
        })
        .collect();
    for name in names {
        result.insert(name, CREDENTIAL_PLACEHOLDER.to_owned());
    }
    for hold in &active {
        for (name, route) in &hold.routes {
            result.insert(name.clone(), route.clone());
        }
    }
    result.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn declared_values_and_their_copies_become_the_placeholder_and_routes_are_set() {
        let holder = "credential-proxy-holds-unit-test";
        let generation = set_hold(
            holder,
            CredentialHold {
                withheld: vec![
                    ("UNIT_API_KEY".into(), Some("sk-unit-secret".into())),
                    ("UNIT_ABSENT_KEY".into(), None),
                ],
                routes: Vec::new(),
            },
        );
        assert!(set_route(
            holder,
            generation,
            "UNIT_BASE_URL",
            "http://127.0.0.1:1/cap/UNIT_API_KEY"
        ));
        let child: BTreeMap<String, String> = with_credential_proxy(env(&[
            ("UNIT_API_KEY", "sk-unit-secret"),
            ("UNIT_COPY", "sk-unit-secret"),
            ("PATH", "/usr/bin"),
        ]));
        assert!(release_hold(holder));
        assert!(!set_route(holder, generation, "UNIT_BASE_URL", "x"));
        assert_eq!(child["UNIT_API_KEY"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["UNIT_COPY"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["UNIT_ABSENT_KEY"], CREDENTIAL_PLACEHOLDER);
        assert_eq!(child["PATH"], "/usr/bin");
        assert_eq!(
            child["UNIT_BASE_URL"],
            "http://127.0.0.1:1/cap/UNIT_API_KEY"
        );
        assert!(!child.values().any(|value| value == "sk-unit-secret"));
    }
}
