//! `engine.auth.check`'s two answers (security design 4.3), shaped as the engine exports them: the person's and the
//! script's full status, and a session's answer that carries only closed codes and the provider (the owner's option
//! (c)).

use serde::{Deserialize, Serialize};

use crate::schema::{Text, True};

/// The closed set a connection test's failure is told by; `unrecognized` for a stored reason outside it.
pub const AUTH_ACCOUNT_CHECK_CODES: [&str; 13] = [
    "rate_limit",
    "overloaded",
    "auth_error",
    "login_unreadable",
    "billing",
    "org_not_allowed",
    "entitlement",
    "long_context_unavailable",
    "other",
    "timeout",
    "cancelled",
    "no_result",
    "unrecognized",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckCode {
    RateLimit,
    Overloaded,
    AuthError,
    LoginUnreadable,
    Billing,
    OrgNotAllowed,
    Entitlement,
    LongContextUnavailable,
    Other,
    Timeout,
    Cancelled,
    NoResult,
    Unrecognized,
}

/// A session's account check: `ok` or one of the closed codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCheck {
    Ok,
    RateLimit,
    Overloaded,
    AuthError,
    LoginUnreadable,
    Billing,
    OrgNotAllowed,
    Entitlement,
    LongContextUnavailable,
    Other,
    Timeout,
    Cancelled,
    NoResult,
    Unrecognized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthState {
    Ready,
    NotReady,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotReadyReason {
    ProviderNotFound,
    CredentialsNotConfigured,
    CredentialNotAvailable,
    InvalidState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    ApiKey,
    Oauth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Ok,
    Failed,
}

/// What the provider returned, kept apart from the engine's own words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderText {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The connection test split in two: the engine's words from a closed code, and what the provider returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountCheck {
    pub model: Text,
    pub outcome: CheckOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<CheckCode>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_text: Option<ProviderText>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    Login,
    Import,
    Env,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    AuthError,
    RateLimit,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountRoute {
    Siwc,
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountStore {
    #[serde(rename = "claude-cli")]
    ClaudeCli,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignInState {
    SignedIn,
    NeedsSignIn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockedModel {
    pub model: Text,
    pub until: u64,
}

/// The engine's AuthAccount: the stored account summary with its connection test split.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthAccount {
    pub name: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<Text>,
    pub source: AccountSource,
    pub blocked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<BlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_until: Option<u64>,
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_models: Option<Vec<BlockedModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<AccountRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_usage_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_out: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store: Option<AccountStore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_in_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<AccountCheck>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_in: Option<SignInState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<True>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_url: Option<Text>,
}

/// A person's or script's answer: the check's status and every account, never the credential.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthStatus {
    pub status: AuthState,
    pub provider: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotReadyReason>,
    #[serde(rename = "authType", default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<AuthType>,
    pub accounts: Vec<AuthAccount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAccount {
    pub signed_in: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<SessionCheck>,
}

/// A session's answer: the provider, status codes from closed sets, and for each account only whether it is signed
/// in and its last test as a closed code. No other string, and no text a provider returned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAuthStatus {
    pub status: AuthState,
    pub provider: Text,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<NotReadyReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<AuthType>,
    pub accounts: Vec<SessionAccount>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreeFieldOrigin {
    Caller,
    Configuration,
    Provider,
    Process,
    Engine,
}

/// A free string of the person's answer, with where its text comes from; `may_embed` marks text that can carry a
/// credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeField {
    pub field: &'static str,
    pub origin: FreeFieldOrigin,
    pub may_embed: bool,
}

const fn embeds(field: &'static str, origin: FreeFieldOrigin) -> FreeField {
    FreeField {
        field,
        origin,
        may_embed: true,
    }
}

/// Every free string of the person's answer.
pub const AUTH_CHECK_FREE_FIELDS: [FreeField; 12] = [
    embeds("provider", FreeFieldOrigin::Caller),
    embeds("accounts[].name", FreeFieldOrigin::Configuration),
    embeds("accounts[].displayName", FreeFieldOrigin::Configuration),
    embeds("accounts[].email", FreeFieldOrigin::Provider),
    embeds("accounts[].subscriptionType", FreeFieldOrigin::Provider),
    embeds("accounts[].orgName", FreeFieldOrigin::Provider),
    embeds("accounts[].usageUrl", FreeFieldOrigin::Configuration),
    embeds(
        "accounts[].blockedModels[].model",
        FreeFieldOrigin::Configuration,
    ),
    embeds("accounts[].check.model", FreeFieldOrigin::Configuration),
    FreeField {
        field: "accounts[].check.message",
        origin: FreeFieldOrigin::Engine,
        may_embed: false,
    },
    embeds(
        "accounts[].check.provider_text.reason",
        FreeFieldOrigin::Provider,
    ),
    embeds(
        "accounts[].check.provider_text.message",
        FreeFieldOrigin::Provider,
    ),
];
