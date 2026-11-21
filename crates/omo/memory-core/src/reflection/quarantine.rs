//! Port of `reflection/quarantine.ts`: the terminal sentinel of a run whose evidence cannot be
//! settled any other way. Evidence first, sentinel second; nothing already in the run dir is moved,
//! rewritten or deleted.

use std::path::Path;

use serde::Serialize;

use super::reservation::write_json_atomic;

pub const QUARANTINE_FILENAME: &str = "quarantined.json";
pub const RESERVATION_EVIDENCE_FILENAME: &str = "reservation.quarantined.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    LedgerUnreadable,
    PrelaunchMissingAfterDeadline,
    TerminalClaimUnrecoverable,
    StrandedNoLedger,
}

impl QuarantineReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LedgerUnreadable => "ledger_unreadable",
            Self::PrelaunchMissingAfterDeadline => "prelaunch_missing_after_deadline",
            Self::TerminalClaimUnrecoverable => "terminal_claim_unrecoverable",
            Self::StrandedNoLedger => "stranded_no_ledger",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuarantineRecord {
    pub version: u32,
    pub run_id: String,
    /// `"reflection"` or `"dream"`.
    pub kind: String,
    pub trigger: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// `ledger.startedAt` when the ledger parses, else the reservation's `reservedAt`: never a fresh clock.
    pub generation: String,
    pub reason: QuarantineReason,
    pub quarantined_at: String,
    pub evidence: Vec<String>,
}

/// Evidence first, sentinel second; nothing already in the run dir is moved, rewritten or deleted.
/// `reservation` is the reservation's own JSON, kept byte-faithful as evidence.
pub fn write_quarantine(
    run_dir: &Path,
    record: &QuarantineRecord,
    reservation: Option<&serde_json::Value>,
) -> std::io::Result<()> {
    if let Some(reservation) = reservation {
        write_json_atomic(&run_dir.join(RESERVATION_EVIDENCE_FILENAME), reservation)?;
    }
    write_json_atomic(&run_dir.join(QUARANTINE_FILENAME), record)
}

#[cfg(test)]
#[path = "quarantine_tests.rs"]
mod tests;
