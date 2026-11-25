//! The device service's answer to "is this device admitted with the grant now" (omo#13037 design 5.6).
//!
//! Upstream counterpart: `thread/gateway/intake-admission.ts`. No answer is `unknown`, and `unknown` and `refused` both
//! fail closed: a row from another device opens nothing and is admitted to no turn without `admitted`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntakeAdmission {
    Admitted,
    Refused,
    Unknown,
}

impl IntakeAdmission {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Refused => "refused",
            Self::Unknown => "unknown",
        }
    }
}
