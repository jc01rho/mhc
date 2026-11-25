//! Port of `device-service-core/src/local-api/` (vendored upstream from omo-relay-daemon
//! `packages/local-api`): the local API v1 contract, `docs/local-api.md` upstream.

pub mod access;
pub mod chunking;
pub mod client;
pub mod errors;
pub mod framing;
pub mod limits;
pub mod policy;
pub mod proof;
pub mod protocol;
pub mod transport;
pub mod vocabulary;

pub use client::{
    CommandOutcome, ConnectOptions, HANDSHAKE_TIMEOUT_MS, ListingResult, LocalApiClient,
    REQUEST_TIMEOUT_MS, connect_local_api,
};
pub use errors::{InsecureStateCause, LocalApiError};
pub use policy::{Policy, PolicyBlocker, PolicyUnacked};
pub use protocol::{
    ClientInfo, LOCAL_API_VERSION, ListingRow, ListingSettledBy, ListingWait, ListingWaitKind,
    Status, Welcome, validate_listing_row, validate_sessions_list_rows,
};
