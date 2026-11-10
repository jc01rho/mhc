//! Port of omo-cpa `src/types.ts`. Shared shapes; no secret ever enters these types.

/// omo provider name and its CPA server root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    pub name: String,
    pub root: String,
}

/// The resolved CPA endpoint plus key-presence flags. Never carries a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CpaConfig {
    pub root: String,
    pub providers: Vec<ProviderConfig>,
    /// Where the config was read from (for the report).
    pub source: String,
    /// True when an inference key was found.
    pub has_api_key: bool,
    /// True when a management key was supplied.
    pub has_management_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    Ok,
    Degraded,
    Down,
    RateLimited,
    Unknown,
}

impl HealthState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
            Self::RateLimited => "rate_limited",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HealthSnapshot {
    pub state: HealthState,
    /// Human-readable reason. Never a fabricated number.
    pub detail: String,
    pub consecutive_failures: u32,
    pub last_status: Option<u16>,
    pub retry_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub remaining_percent: Option<f64>,
    pub resets_at: Option<i64>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageModelQuota {
    pub id: String,
    pub windows: Vec<UsageWindow>,
    pub observed_at: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageAccountStatus {
    Ok,
    Error,
    Off,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageAccount {
    pub provider: String,
    pub label: String,
    /// `off` = the credential is disabled server-side, not an error.
    pub status: UsageAccountStatus,
    pub detail: Option<String>,
    pub meta: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub models: Vec<UsageModelQuota>,
    /// Oldest snapshot instant behind the rows this account renders.
    pub observed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UsageResult {
    Unsupported { reason: String },
    Supported { accounts: Vec<UsageAccount> },
}
