//! Port of `device-service-core/src/listing-rows.ts`: asks for the newest `sessions.list` row
//! version (`rows: 7`) first and steps down one version per `invalid_request`, remembering the
//! accepted version for future listings.

use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

use crate::local_api::{ListingResult, LocalApiClient, LocalApiError};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const NEWEST_LISTING_ROWS_VERSION: u64 = 7;

pub fn older_rows_version(rows: Option<u64>) -> Option<Option<u64>> {
    match rows {
        Some(7) => Some(Some(6)),
        Some(6) => Some(Some(5)),
        Some(5) => Some(Some(4)),
        Some(4) => Some(Some(3)),
        Some(3) => Some(Some(2)),
        Some(2) => Some(None),
        _ => None,
    }
}

pub trait SessionListClient: Send + Sync {
    fn list(
        &self,
        filter: Option<Value>,
        rows: Option<u64>,
    ) -> BoxFuture<'_, Result<ListingResult, LocalApiError>>;
}

impl SessionListClient for LocalApiClient {
    fn list(
        &self,
        filter: Option<Value>,
        rows: Option<u64>,
    ) -> BoxFuture<'_, Result<ListingResult, LocalApiError>> {
        Box::pin(self.sessions_list(filter, rows))
    }
}

impl<T: SessionListClient + ?Sized> SessionListClient for &T {
    fn list(
        &self,
        filter: Option<Value>,
        rows: Option<u64>,
    ) -> BoxFuture<'_, Result<ListingResult, LocalApiError>> {
        (**self).list(filter, rows)
    }
}

/// Remembers the newest `sessions.list` `rows` version accepted by a device service.
pub struct NewestRowsListing<C> {
    client: C,
    rows: Option<u64>,
}

impl<C: SessionListClient> NewestRowsListing<C> {
    pub fn new(client: C) -> Self {
        Self {
            client,
            rows: Some(NEWEST_LISTING_ROWS_VERSION),
        }
    }

    pub fn current_rows(&self) -> Option<u64> {
        self.rows
    }

    pub async fn list(&mut self, filter: Option<Value>) -> Result<ListingResult, LocalApiError> {
        loop {
            match self.client.list(filter.clone(), self.rows).await {
                Ok(result) => return Ok(result),
                Err(error) if error.is("invalid_request") => {
                    let Some(next) = older_rows_version(self.rows) else {
                        return Err(error);
                    };
                    self.rows = next;
                }
                Err(error) => return Err(error),
            }
        }
    }
}
