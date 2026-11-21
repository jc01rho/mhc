//! The service endpoint resolver: which account/intake service origin a call uses, if any.
//!
//! Precedence: an explicit value from the caller's own setting, then `MAHO_SERVICE_API_URL`, then
//! omo's `OMO_SERVICE_API_URL`. Empty and whitespace values count as unset. When none is set the
//! answer is `None`: local mode, and nothing is sent anywhere. A value that is set but fails the
//! guard is an error naming the setting, never a silent fall back to another origin.

use std::fmt;

use crate::guard::{EndpointRefusal, check_service_origin};

/// mhc's own variable for the service API origin.
pub const SERVICE_API_URL_ENV: &str = "MAHO_SERVICE_API_URL";
/// omo's variable for the same origin, read when mhc's is unset.
pub const OMO_SERVICE_API_URL_ENV: &str = "OMO_SERVICE_API_URL";

/// What a caller tells the user when it stays local because no origin is configured.
pub const LOCAL_MODE_NOTICE: &str = "No service endpoint is configured, so mhc runs in local mode. Set MAHO_SERVICE_API_URL to an account service origin to use one.";

/// Where a resolved value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSource {
    /// The caller's own setting.
    Explicit,
    /// An environment variable.
    Env(&'static str),
}

impl EndpointSource {
    /// The setting's name as a user would look for it.
    pub fn setting(self) -> &'static str {
        match self {
            Self::Explicit => "service API origin",
            Self::Env(name) => name,
        }
    }
}

/// A checked service origin (`scheme://host[:port]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceOrigin {
    pub origin: String,
    pub source: EndpointSource,
}

impl ServiceOrigin {
    /// `<origin><path>` with exactly one slash between.
    pub fn join(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.origin.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// The lowercase host of the origin.
    pub fn host(&self) -> Option<String> {
        url::Url::parse(&self.origin)
            .ok()
            .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
    }
}

/// A configured origin the guard refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointError {
    pub source: EndpointSource,
    pub refusal: EndpointRefusal,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.source.setting(), self.refusal)
    }
}

impl std::error::Error for EndpointError {}

fn present(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The service API origin, `None` in local mode.
pub fn resolve_service_api(
    explicit: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<ServiceOrigin>, EndpointError> {
    let picked = present(explicit.map(str::to_owned))
        .map(|value| (EndpointSource::Explicit, value))
        .or_else(|| {
            present(lookup(SERVICE_API_URL_ENV))
                .map(|value| (EndpointSource::Env(SERVICE_API_URL_ENV), value))
        })
        .or_else(|| {
            present(lookup(OMO_SERVICE_API_URL_ENV))
                .map(|value| (EndpointSource::Env(OMO_SERVICE_API_URL_ENV), value))
        });
    let Some((source, value)) = picked else {
        return Ok(None);
    };
    let origin =
        check_service_origin(&value).map_err(|refusal| EndpointError { source, refusal })?;
    Ok(Some(ServiceOrigin { origin, source }))
}

/// [`resolve_service_api`] over this process's environment.
pub fn resolve_service_api_from_process(
    explicit: Option<&str>,
) -> Result<Option<ServiceOrigin>, EndpointError> {
    resolve_service_api(explicit, |key| std::env::var(key).ok())
}

/// The hosts that may receive a trace context: the configured service API's host, if any. A host
/// mhc never calls is never named here.
pub fn service_trace_hosts(lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    match resolve_service_api(None, lookup) {
        Ok(Some(origin)) => origin.host().into_iter().collect(),
        _ => Vec::new(),
    }
}
