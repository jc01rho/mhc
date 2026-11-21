//! Where mhc reaches a hosted service, if anywhere.
//!
//! mhc assumes no hosted service. Every call site that could talk to one (the report intake, the
//! account API behind memory sync, the device link) asks [`resolve_service_api`] for an origin, and
//! gets `None` unless the user configured one: that is local mode, and the caller keeps its work on
//! this machine. A configured origin must pass [`check_service_origin`] (HTTPS, or plain HTTP only
//! toward a loopback or tailnet host). The process-wide local-only mark ([`LocalOnlyGuard`]) lets a
//! command whose whole path is local (`mhc remote status` and its siblings) refuse any network call
//! a shared helper would otherwise make.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod guard;
pub mod local_only;
pub mod resolver;

pub use guard::{
    EndpointRefusal, allows_plain_text, check_peer_host, check_service_origin, is_loopback_host,
    is_loopback_ip, is_tailnet_host, is_tailnet_ip,
};
pub use local_only::{
    LocalOnlyGuard, LocalOnlyNetworkRefused, is_local_only_process, mark_local_only_process,
    refuse_if_local_only,
};
pub use resolver::{
    EndpointError, EndpointSource, LOCAL_MODE_NOTICE, OMO_SERVICE_API_URL_ENV, SERVICE_API_URL_ENV,
    ServiceOrigin, resolve_service_api, resolve_service_api_from_process, service_trace_hosts,
};
