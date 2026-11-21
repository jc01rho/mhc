//! Headers memory-core sends on every sync request (A3 engine contract sec 8 and A1.1-M3).
//! `SYNC_CONTRACT_VERSION` copies omo-service `packages/contract/src/version.ts` CONTRACT_VERSION at
//! e39e23ad; that file is outside the vendored sync-format set. `SYNC_CAPABILITIES` lives here,
//! beside the parser, so only a build that parses recovery-phrase keys says so; the service floor
//! reads it, never the contract version.

pub const SYNC_CONTRACT_VERSION: &str = "1.44.0";
pub const SYNC_CAPABILITIES: &str = "phrase";

/// The routes the service's client floor guards: the `/v1/sync/*` families (keys, pending,
/// recovery, entries, batches, state, objects, uploads). There a request that does not declare the
/// phrase capability is refused 426 client_update_required for a phrase account (#13691). Account
/// routes (`/v1/me`, `/v1/devices`, `/v1/account/*`) are not floored today, and their clients use
/// [`sync_request_headers`] all the same.
pub const FLOORED_ROUTE_PREFIX: &str = "/v1/sync/";

/// Whether a request to `path` would be refused by the floor: a floored route without this build's
/// contract headers. Header names compare case-insensitively, as HTTP does.
pub fn misses_sync_contract(path: &str, headers: &[(String, String)]) -> bool {
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    path.starts_with(FLOORED_ROUTE_PREFIX)
        && !(header("omo-contract-version") == Some(SYNC_CONTRACT_VERSION)
            && header("omo-sync-capabilities")
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .any(|capability| capability == SYNC_CAPABILITIES))
}

/// The headers of every sync-path request: the contract headers and the device's bearer token,
/// built only here so no client can send one without the other. The service's floor refuses a
/// request from a phrase account 426 when it lacks the phrase capability (#13691).
pub fn sync_request_headers(token: &str) -> Vec<(String, String)> {
    vec![
        (
            "omo-contract-version".to_string(),
            SYNC_CONTRACT_VERSION.to_string(),
        ),
        (
            "omo-sync-capabilities".to_string(),
            SYNC_CAPABILITIES.to_string(),
        ),
        ("authorization".to_string(), format!("Bearer {token}")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_headers_pass_the_floor_on_every_sync_route() {
        let headers = sync_request_headers("t0k");
        for path in [
            "/v1/sync/batches?after=0",
            "/v1/sync/state",
            "/v1/sync/objects/o1",
            "/v1/sync/uploads",
            "/v1/sync/entries?after=3",
            "/v1/sync/keys",
            "/v1/sync/pending",
            "/v1/sync/recovery/join",
        ] {
            assert!(!misses_sync_contract(path, &headers), "{path}");
        }
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "authorization" && v == "Bearer t0k")
        );
    }

    #[test]
    fn a_floored_route_without_the_capability_misses_the_contract() {
        let bare = vec![("authorization".to_string(), "Bearer t".to_string())];
        assert!(misses_sync_contract("/v1/sync/state", &bare));
        let wrong_version = vec![
            ("omo-contract-version".to_string(), "1.0.0".to_string()),
            ("omo-sync-capabilities".to_string(), "phrase".to_string()),
        ];
        assert!(misses_sync_contract("/v1/sync/state", &wrong_version));
        let listed = vec![
            ("Omo-Contract-Version".to_string(), "1.44.0".to_string()),
            ("omo-sync-capabilities".to_string(), "x, phrase".to_string()),
        ];
        assert!(!misses_sync_contract("/v1/sync/state", &listed));
        // Account routes are not floored.
        assert!(!misses_sync_contract("/v1/me/events", &bare));
    }
}
