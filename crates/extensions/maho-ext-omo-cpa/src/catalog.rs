//! Port of omo-cpa `src/catalog.ts`. Shared catalog cache.
//!
//! Two providers are registered (primary and last-resort) and each owns a refresh, while `/cpa` can
//! ask for the catalog too. Every uncached call fans out to four list endpoints, so this layer keeps
//! one refresh round from producing eight or more list requests:
//!  - serve a result younger than the TTL without touching the network
//!  - collapse concurrent callers onto one in-flight request
//!
//! Only a successful fetch is cached; a failure stays retryable.

use crate::endpoint::{CatalogFetchOk, CatalogFetchResult, fetch_catalog};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// A model list does not churn minute to minute.
pub const DEFAULT_TTL_MS: i64 = 5 * 60 * 1000;

pub const CATALOG_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone)]
struct CacheEntry {
    result: CatalogFetchOk,
    fetched_at: i64,
}

#[derive(Default)]
struct Shared {
    cached: Option<(String, CacheEntry)>,
}

/// Process-wide catalog cache keyed by `root\0apiKey`.
#[derive(Clone, Default)]
pub struct CatalogCache {
    shared: Arc<Mutex<Shared>>,
    /// Serializes network fetches so concurrent callers join one fan-out.
    inflight: Arc<tokio::sync::Mutex<()>>,
}

impl CatalogCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Age of the cached catalog in ms, or `None` when nothing is cached.
    pub fn cached_age(&self, now_ms: i64) -> Option<i64> {
        let shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        shared
            .cached
            .as_ref()
            .map(|(_, entry)| now_ms - entry.fetched_at)
    }

    /// Drop the cached catalog. Used by an explicit refresh.
    pub fn clear(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        shared.cached = None;
    }

    /// Fetch the merged catalog, reusing a recent result.
    ///
    /// A failure stays uncached so the next caller can retry immediately. A list omission is not
    /// evidence that inference stopped working, so known ids survive for this connection; fresh
    /// records still replace their previous metadata.
    pub async fn get(
        &self,
        client: &reqwest::Client,
        root: &str,
        api_key: &str,
        force: bool,
        now_ms: i64,
        ttl_ms: i64,
    ) -> CatalogFetchResult {
        let key = cache_key(root, api_key);
        if !force {
            let shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((cached_key, entry)) = &shared.cached
                && *cached_key == key
                && now_ms - entry.fetched_at < ttl_ms
            {
                return CatalogFetchResult::Ok(entry.result.clone());
            }
        }

        let _guard = self.inflight.lock().await;
        // Another caller may have completed the fetch while this one waited.
        if !force {
            let shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((cached_key, entry)) = &shared.cached
                && *cached_key == key
                && now_ms - entry.fetched_at < ttl_ms
            {
                return CatalogFetchResult::Ok(entry.result.clone());
            }
        }

        let fetched = fetch_catalog(client, root, api_key, CATALOG_TIMEOUT_MS).await;
        let CatalogFetchResult::Ok(mut result) = fetched else {
            return fetched;
        };

        let previous = {
            let shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match &shared.cached {
                Some((cached_key, entry)) if *cached_key == key => entry.result.models.clone(),
                _ => Vec::new(),
            }
        };
        let listed: BTreeSet<&str> = result.models.iter().map(|model| model.id.as_str()).collect();
        let mut carried = Vec::new();
        for model in previous {
            if !listed.contains(model.id.as_str()) {
                carried.push(model);
            }
        }
        result.retained_ids = carried.iter().map(|model| model.id.clone()).collect();
        result.models.extend(carried);

        let mut shared = self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        shared.cached = Some((
            key,
            CacheEntry {
                result: result.clone(),
                fetched_at: now_ms,
            },
        ));
        CatalogFetchResult::Ok(result)
    }
}

/// Distinct servers and credentials must never share a cached catalog.
fn cache_key(root: &str, api_key: &str) -> String {
    format!("{root}\u{0}{api_key}")
}

/// `OMO_CPA_CATALOG_TTL_MS`, falling back to the module default.
pub fn ttl_from_env() -> i64 {
    std::env::var("OMO_CPA_CATALOG_TTL_MS")
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or(DEFAULT_TTL_MS, |value| value as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tier_types::CatalogModel;

    #[test]
    fn distinct_credentials_never_share_a_key() {
        assert_ne!(cache_key("http://a", "k1"), cache_key("http://a", "k2"));
        assert_ne!(cache_key("http://a", "k"), cache_key("http://b", "k"));
        assert_eq!(cache_key("http://a", "k"), cache_key("http://a", "k"));
    }

    #[test]
    fn a_missing_cache_has_no_age_and_clear_drops_it() {
        let cache = CatalogCache::new();
        assert_eq!(cache.cached_age(1_000), None);
        cache.clear();
        assert_eq!(cache.cached_age(1_000), None);
    }

    #[test]
    fn ttl_env_falls_back_when_unparseable() {
        // The process env is shared by every test in this binary, so only the fallback path is
        // asserted: an unset variable yields the module default.
        if std::env::var("OMO_CPA_CATALOG_TTL_MS").is_err() {
            assert_eq!(ttl_from_env(), DEFAULT_TTL_MS);
        }
    }

    #[test]
    fn the_cache_reports_age_of_the_stored_entry() {
        let cache = CatalogCache::new();
        {
            let mut shared = cache.shared.lock().expect("lock");
            shared.cached = Some((
                cache_key("http://a", "k"),
                CacheEntry {
                    result: CatalogFetchOk {
                        models: vec![CatalogModel::new("m")],
                        issues: Vec::new(),
                        failures: Default::default(),
                        retained_ids: Vec::new(),
                    },
                    fetched_at: 500,
                },
            ));
        }
        assert_eq!(cache.cached_age(900), Some(400));
    }
}
