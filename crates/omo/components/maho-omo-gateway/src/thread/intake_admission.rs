//! The device service's answer to "is this device admitted with the grant now" (omo#13037 design 5.6).
//!
//! Upstream counterpart: `thread/gateway/intake-admission.ts`. No answer is `unknown`, and `unknown` and `refused` both
//! fail closed: a row from another device opens nothing and is admitted to no turn without `admitted`.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::delivery_ledger::ledger_device;
use super::types::DeliveryRow;

pub type AdmitIntake = Arc<dyn Fn(&str) -> IntakeAdmission + Send + Sync>;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntakeVerdict {
    Admit,
    Refuse,
    Skip,
}

/// One drain pass's admission check (5.6, A2.2): asks each device at most once per pass.
pub struct IntakeAdmissionPass {
    admit: Option<AdmitIntake>,
    answers: BTreeMap<String, IntakeAdmission>,
}

impl IntakeAdmissionPass {
    pub fn new(admit: Option<AdmitIntake>) -> Self {
        Self {
            admit,
            answers: BTreeMap::new(),
        }
    }

    pub fn check(&mut self, row: &DeliveryRow) -> IntakeVerdict {
        let Some(ledger_id) = row.ledger_id.as_deref() else {
            return IntakeVerdict::Admit;
        };
        let Some(device) = ledger_device(ledger_id) else {
            return IntakeVerdict::Skip;
        };
        let Some(admit) = &self.admit else {
            return IntakeVerdict::Skip;
        };
        let answer = if let Some(cached) = self.answers.get(&device) {
            *cached
        } else {
            let fetched = admit(device.as_str());
            self.answers.insert(device, fetched);
            fetched
        };
        match answer {
            IntakeAdmission::Admitted => IntakeVerdict::Admit,
            IntakeAdmission::Refused => IntakeVerdict::Refuse,
            IntakeAdmission::Unknown => IntakeVerdict::Skip,
        }
    }
}
