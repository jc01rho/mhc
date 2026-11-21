//! Port of `reflection/run-ledger.ts` (omo #12517): the run ledger a reflection supervisor writes
//! (`runs/<runId>/ledger.json`). The strict parser lives here so reconciliation and the CLI reaper
//! tell a valid ledger from an unreadable one the same way.

use serde_json::Value;
#[derive(Clone, Debug)]
pub struct ReservationRunLedger(Value);
impl ReservationRunLedger {
    pub fn value(&self) -> &Value {
        &self.0
    }
    pub fn run_id(&self) -> &str {
        self.0["runId"].as_str().expect("validated runId")
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ReservationLedgerError(pub &'static str);
impl std::fmt::Display for ReservationLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ReservationLedgerError {}
fn invalid(message: &'static str) -> ReservationLedgerError {
    ReservationLedgerError(message)
}
fn one_of(value: Option<&Value>, allowed: &[&str]) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| allowed.contains(&value))
}
fn positive_integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|value| value.is_finite() && value.fract() == 0.0 && value > 0.0)
}
pub fn parse_reservation_run_ledger(
    value: Value,
) -> Result<ReservationRunLedger, ReservationLedgerError> {
    let Some(record) = value.as_object() else {
        return Err(invalid("Invalid reservation run ledger"));
    };
    if record.get("version").and_then(Value::as_f64) != Some(1.0)
        || !record.get("runId").is_some_and(Value::is_string)
    {
        return Err(invalid("Invalid reservation run ledger identity"));
    }
    if !one_of(record.get("kind"), &["reflection", "dream"]) {
        return Err(invalid("Invalid reservation run kind"));
    }
    if !one_of(
        record.get("trigger"),
        &["step-count", "compaction", "manual", "dream"],
    ) {
        return Err(invalid("Invalid reservation run trigger"));
    }
    let dream = record["kind"] == "dream";
    if dream != (record["trigger"] == "dream") {
        return Err(invalid("Reservation run kind and trigger disagree"));
    }
    if dream
        && !one_of(
            record.get("origin"),
            &["manual", "idle", "shutdown", "pressure"],
        )
    {
        return Err(invalid("Dream run origin is required"));
    }
    if [
        "startedAt",
        "worktreeDir",
        "worktreeBranch",
        "baseSha",
        "gitFilePath",
        "gitFileSnapshot",
        "commonConfigPath",
    ]
    .iter()
    .any(|key| !record.get(*key).is_some_and(Value::is_string))
    {
        return Err(invalid("Invalid reservation run paths"));
    }
    if ["hardDeadlineAt", "terminationGraceMs", "deadlineAt"]
        .iter()
        .any(|key| {
            !record
                .get(*key)
                .and_then(Value::as_f64)
                .is_some_and(f64::is_finite)
        })
    {
        return Err(invalid("Invalid reservation run deadline"));
    }
    if record["deadlineAt"].as_f64()
        != Some(
            record["hardDeadlineAt"]
                .as_f64()
                .expect("validated deadline")
                + record["terminationGraceMs"]
                    .as_f64()
                    .expect("validated grace"),
        )
    {
        return Err(invalid("Reservation run deadline mismatch"));
    }
    if !one_of(record.get("mergePolicy"), &["auto", "integration"]) {
        return Err(invalid("Invalid reservation merge policy"));
    }
    if record
        .get("targetDoc")
        .is_some_and(|target| !dream || !target.is_string())
    {
        return Err(invalid("Invalid dream target document"));
    }
    for (key, error) in [
        ("systemTokenBudget", "Invalid dream systemTokenBudget"),
        ("systemTokenTarget", "Invalid dream systemTokenTarget"),
    ] {
        if record
            .get(key)
            .is_some_and(|value| !dream || !positive_integer(value))
        {
            return Err(invalid(error));
        }
    }
    if record.contains_key("systemTokenBudget") != record.contains_key("systemTokenTarget") {
        return Err(invalid("Dream token budget metadata is incomplete"));
    }
    if ["pid", "childPid"].iter().any(|key| {
        record
            .get(*key)
            .is_some_and(|value| !positive_integer(value))
    }) || ["processStart", "childProcessStart"].iter().any(|key| {
        record
            .get(*key)
            .is_some_and(|value| !value.is_null() && !value.is_string())
    }) {
        return Err(invalid("Invalid reservation process identity"));
    }
    if record
        .get("attempt")
        .is_some_and(|value| !positive_integer(value))
    {
        return Err(invalid("Invalid reservation run attempt"));
    }
    for (key, error) in [
        ("model", "Invalid reservation run model"),
        ("thinking", "Invalid reservation run thinking"),
        ("category", "Invalid reservation run category"),
    ] {
        if record.get(key).is_some_and(|value| !value.is_string()) {
            return Err(invalid(error));
        }
    }
    if record.get("conversationIds").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    }) {
        return Err(invalid("Invalid reservation run conversations"));
    }
    if record
        .get("launching")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("Invalid reservation run launching state"));
    }
    if record
        .get("queued")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("Invalid reservation run queued state"));
    }
    if record
        .get("cleanupIncomplete")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err(invalid("Invalid reservation run cleanup state"));
    }
    if !record
        .get("commonConfigSnapshot")
        .is_some_and(|value| value.is_null() || value.is_string())
    {
        return Err(invalid("Invalid common config snapshot"));
    }
    if record.contains_key("finalizePhase")
        && !one_of(
            record.get("finalizePhase"),
            &["validated", "integrated", "settled"],
        )
    {
        return Err(invalid("Invalid finalization phase"));
    }
    for (key, error) in [
        ("validatedTipSha", "Invalid validatedTipSha"),
        ("integrationSha", "Invalid integrationSha"),
        ("finalizeReason", "Invalid finalizeReason"),
        ("finalizeDetail", "Invalid finalizeDetail"),
        ("finalizedAt", "Invalid finalizedAt"),
    ] {
        if record.get(key).is_some_and(|value| !value.is_string()) {
            return Err(invalid(error));
        }
    }
    if record.get("validatedChangedPaths").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    }) {
        return Err(invalid("Invalid validated changed paths"));
    }
    if record.contains_key("finalizeOutcome")
        && !one_of(
            record.get("finalizeOutcome"),
            &[
                "merged",
                "no_changes",
                "parent_dirty",
                "dirty_uncommitted",
                "merge_conflict",
                "admin_tamper",
                "timed_out",
                "failed",
            ],
        )
    {
        return Err(invalid("Invalid finalization outcome"));
    }
    Ok(ReservationRunLedger(value))
}

#[cfg(test)]
#[path = "run_ledger_tests.rs"]
mod tests;
