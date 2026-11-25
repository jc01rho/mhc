//! Port of `local-api/errors.ts`: an owner's error response (code, reason, action) or a failure the
//! client raises itself.

use std::fmt;

use serde_json::Value;

/// Why a state entry was refused as `insecure_state_dir`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsecureStateCause {
    OwnerAdministrators,
    OwnerOther,
    Reparse,
    DaclNotExact,
    Unreadable,
    EntryOwner,
    TokenOwnerUnset,
}

impl InsecureStateCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OwnerAdministrators => "owner_administrators",
            Self::OwnerOther => "owner_other",
            Self::Reparse => "reparse",
            Self::DaclNotExact => "dacl_not_exact",
            Self::Unreadable => "unreadable",
            Self::EntryOwner => "entry_owner",
            Self::TokenOwnerUnset => "token_owner_unset",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalApiError {
    pub code: String,
    pub reason: String,
    pub action: Option<String>,
    pub limit_chars: Option<u64>,
    /// On a `session.command` that timed out: the id it was sent under, whose outcome may still arrive.
    pub command_id: Option<String>,
    pub insecure_cause: Option<InsecureStateCause>,
}

impl LocalApiError {
    pub fn new(code: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            reason: reason.into(),
            action: None,
            limit_chars: None,
            command_id: None,
            insecure_cause: None,
        }
    }

    pub fn protocol_violation(detail: impl Into<String>) -> Self {
        Self::new("protocol_violation", detail)
    }

    pub fn with_action(mut self, action: Option<String>) -> Self {
        self.action = action;
        self
    }

    pub fn with_insecure_cause(mut self, cause: Option<InsecureStateCause>) -> Self {
        self.insecure_cause = cause;
        self
    }

    pub fn is(&self, code: &str) -> bool {
        self.code == code
    }

    /// The error body of a `response` with `ok: false`; `None` when it breaks the contract.
    pub fn from_error_body(error: &Value) -> Option<Self> {
        let code = error.get("code")?.as_str()?;
        let reason = error.get("reason")?.as_str()?;
        let action = match error.get("action") {
            None | Some(Value::Null) => None,
            Some(Value::String(action)) => Some(action.clone()),
            Some(_) => return None,
        };
        let mut parsed = Self::new(code, reason).with_action(action);
        parsed.limit_chars = error.get("limit_chars").and_then(Value::as_u64);
        parsed.command_id = error
            .get("command_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(parsed)
    }

    /// The wire form of this error inside a `response`.
    pub fn to_error_body(&self) -> Value {
        let mut body = serde_json::json!({
            "code": self.code,
            "reason": self.reason,
            "action": self.action,
        });
        if let Some(limit) = self.limit_chars {
            body["limit_chars"] = limit.into();
        }
        body
    }
}

impl fmt::Display for LocalApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.reason)
    }
}

impl std::error::Error for LocalApiError {}
