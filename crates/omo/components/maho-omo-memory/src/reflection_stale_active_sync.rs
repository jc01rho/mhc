//! Recovery of a reflection reservation whose holder died after this session already reconciled
//! (`reflection-stale-active.ts`, #10377, #11976, omo #12517).
//!
//! Session-start reconciliation runs once per session, so a holder that dies later leaves
//! `runtime/reflection/active.lock` held and every later request only queues behind it. The shared
//! reaper (`memory_core::reflection::stale_lock_reaper`, also run by `mhc doctor --fix`) decides,
//! fail-closed, whether that holder is provably gone: a dead holder and the reapable stuck shapes
//! (launch interrupted, processes dead, unreadable ledger) are settled by reconciliation, a torn lock
//! is moved aside. Anything it cannot prove is reported with the next step, never silently kept.

use std::path::Path;

use memory_core::reflection::{
    StaleLockDecision, StaleLockProbe, StaleLockReaperAction, StaleLockReaperOptions,
    StaleLockReaperReport, run_stale_lock_reaper,
};

pub use memory_core::reflection::stale_holder_proof::{
    ReflectionLockProbe as ActiveHolderSeams, ReflectionLockVerdict,
};

/// The shared decision (`memory-core reflection/stale-holder.ts`); `mhc doctor` runs the same code.
pub fn classify_active_reflection_holder(
    reflection_dir: &Path,
    seams: Option<&ActiveHolderSeams<'_>>,
) -> ActiveHolderVerdict {
    let owned = ActiveHolderSeams::default();
    memory_core::reflection::stale_holder_proof::classify_reflection_lock_holder(
        reflection_dir,
        seams.unwrap_or(&owned),
    )
}

pub type ActiveHolderVerdict = ReflectionLockVerdict;

pub fn stale_reflection_lock_notice(_reflection_dir: &Path, run_id: &str, reason: &str) -> String {
    format!(
        "memory reflection is stuck on a stale lock: run {run_id} still holds it and {reason}, so new reflection requests only queue. Next step: run /doctor; if no reflection run is active, run `mhc doctor --fix`, which releases a provably dead holder and moves a torn lock aside."
    )
}

const NOT_RELEASED: &str = "recovering it did not release the lock";

pub struct StaleHolderRecoveryInput<'a> {
    pub reflection_dir: &'a Path,
    pub reconcile: &'a dyn Fn() -> Result<(), String>,
    pub seams: Option<&'a ActiveHolderSeams<'a>>,
    /// False once this session already tried to settle the run: report it instead of reconciling again.
    pub should_reconcile: Option<&'a dyn Fn(&str) -> bool>,
}

pub struct StaleHolderRecovery {
    pub verdict: ActiveHolderVerdict,
    pub notice: Option<String>,
    /// The run whose lock this pass released.
    pub recovered_run_id: Option<String>,
}

impl StaleHolderRecovery {
    fn unchanged(verdict: ActiveHolderVerdict) -> Self {
        Self {
            verdict,
            notice: None,
            recovered_run_id: None,
        }
    }

    fn reported(
        reflection_dir: &Path,
        verdict: ActiveHolderVerdict,
        run_id: &str,
        reason: &str,
    ) -> Self {
        Self {
            notice: Some(stale_reflection_lock_notice(reflection_dir, run_id, reason)),
            verdict,
            recovered_run_id: None,
        }
    }
}

fn reaper_probe<'a>(seams: Option<&ActiveHolderSeams<'a>>) -> StaleLockProbe<'a> {
    StaleLockProbe {
        base: seams.copied().unwrap_or_default(),
        ..Default::default()
    }
}

/// Settles or reaps a provably gone holder; reports any other non-live holder. The notice is the
/// stuck state that must be shown, and `recovered_run_id` the run whose lock was released.
pub fn recover_stale_reflection_holder(input: StaleHolderRecoveryInput<'_>) -> StaleHolderRecovery {
    // No lock or a live holder is the reaper's "nothing to decide".
    let current = classify_active_reflection_holder(input.reflection_dir, input.seams);
    if matches!(
        current,
        ActiveHolderVerdict::None | ActiveHolderVerdict::Live { .. }
    ) {
        return StaleHolderRecovery::unchanged(current);
    }
    let runtime_dir = input
        .reflection_dir
        .parent()
        .unwrap_or(input.reflection_dir);
    let mut options = StaleLockReaperOptions::new(runtime_dir);
    options.probe = reaper_probe(input.seams);
    options.settle = Some(input.reconcile);
    options.should_settle = input.should_reconcile;
    let report = match run_stale_lock_reaper(&options) {
        Ok(report) => report,
        Err(error) => {
            // The settle failed, so the lock is still held: report, never retry here.
            let after = classify_active_reflection_holder(input.reflection_dir, input.seams);
            let run_id = after
                .run_id()
                .or(current.run_id())
                .unwrap_or("unknown")
                .to_string();
            return StaleHolderRecovery::reported(
                input.reflection_dir,
                after,
                &run_id,
                &format!("recovering it failed: {error}"),
            );
        }
    };
    let StaleLockReaperReport {
        before,
        decision,
        action,
        after,
        ..
    } = report;
    let run_id = decision.run_id().unwrap_or("unknown").to_string();
    match (&decision, action) {
        (StaleLockDecision::None, _) => StaleHolderRecovery::unchanged(before),
        (_, StaleLockReaperAction::SettleDeclined) => {
            StaleHolderRecovery::reported(input.reflection_dir, before, &run_id, NOT_RELEASED)
        }
        (_, StaleLockReaperAction::Settled | StaleLockReaperAction::Reaped) => match &after {
            ActiveHolderVerdict::Stuck {
                run_id: stuck_run,
                reason,
            } => {
                let (stuck_run, reason) = (stuck_run.clone(), reason.clone());
                StaleHolderRecovery::reported(input.reflection_dir, after, &stuck_run, &reason)
            }
            ActiveHolderVerdict::Dead { run_id: dead_run } if *dead_run == run_id => {
                StaleHolderRecovery::reported(input.reflection_dir, after, &run_id, NOT_RELEASED)
            }
            _ => StaleHolderRecovery {
                verdict: before,
                notice: None,
                recovered_run_id: Some(run_id),
            },
        },
        (StaleLockDecision::Stuck { reason, .. }, _) => {
            StaleHolderRecovery::reported(input.reflection_dir, before, &run_id, reason)
        }
        _ => StaleHolderRecovery::reported(input.reflection_dir, before, &run_id, NOT_RELEASED),
    }
}

/// Session start: the reaper moves a torn `active.lock` aside first (reconciliation cannot read past
/// it) and counts the settle the caller then performs, so every session reconciles exactly once.
pub fn reap_before_session_reconcile(
    runtime_dir: &Path,
    seams: Option<&ActiveHolderSeams<'_>>,
) -> Result<StaleLockReaperReport, String> {
    let settle_after_reap = || -> Result<(), String> { Ok(()) };
    let mut options = StaleLockReaperOptions::new(runtime_dir);
    options.probe = reaper_probe(seams);
    options.settle = Some(&settle_after_reap);
    options.settle_always = true;
    run_stale_lock_reaper(&options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::locks::ProcessLiveness;
    use std::path::PathBuf;

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const OUTCOME: &str = r#"{"version":1,"runId":"run-1","attempt":1,"finishedAt":"2026-01-01T00:00:00.000Z","childExit":{"code":0,"signal":null},"timedOut":false}"#;

    struct Fixture {
        _root: tempfile::TempDir,
        reflection: PathBuf,
    }

    fn fixture(launcher_pid: u32) -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let reflection = root.path().join("reflection");
        write(
            &reflection,
            "active.lock",
            &serde_json::json!({
                "runId":"run-1","launcherPid":launcher_pid,
                "launcherHostname":memory_core::support::host::hostname(),
            })
            .to_string(),
        );
        Fixture {
            _root: root,
            reflection,
        }
    }

    fn seams<'a>(liveness: &'a dyn Fn(u32) -> ProcessLiveness) -> ActiveHolderSeams<'a> {
        ActiveHolderSeams {
            pid_liveness: Some(liveness),
            ..Default::default()
        }
    }

    #[test]
    fn a_provably_dead_holder_is_settled_through_reconciliation() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let reconciles = std::cell::Cell::new(0);
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| {
                reconciles.set(reconciles.get() + 1);
                // Reconciliation is the only code that settles a run: it removes the lock.
                std::fs::remove_file(fixture.reflection.join("active.lock"))
                    .map_err(|error| error.to_string())
            },
            seams: Some(&seams),
            should_reconcile: None,
        });
        assert_eq!(reconciles.get(), 1);
        assert_eq!(
            recovery.verdict,
            ActiveHolderVerdict::Dead {
                run_id: "run-1".into()
            }
        );
        assert!(recovery.notice.is_none());
        assert_eq!(recovery.recovered_run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn a_live_or_unknown_holder_is_never_reconciled() {
        for liveness in [ProcessLiveness::Alive, ProcessLiveness::Unknown] {
            let fixture = fixture(4242);
            write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
            let check = move |_: u32| liveness;
            let seams = seams(&check);
            let reconciles = std::cell::Cell::new(0);
            let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
                reflection_dir: &fixture.reflection,
                reconcile: &|| {
                    reconciles.set(reconciles.get() + 1);
                    Ok(())
                },
                seams: Some(&seams),
                should_reconcile: None,
            });
            assert_eq!(reconciles.get(), 0);
            assert!(matches!(
                recovery.verdict,
                ActiveHolderVerdict::Live { .. } | ActiveHolderVerdict::Stuck { .. }
            ));
        }
    }

    #[test]
    fn a_dead_holder_that_stays_held_is_reported_once_with_the_next_step() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let attempts = std::cell::Cell::new(0);
        let should = |_: &str| {
            attempts.set(attempts.get() + 1);
            false
        };
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| panic!("a run this session already tried is not reconciled again"),
            seams: Some(&seams),
            should_reconcile: Some(&should),
        });
        assert_eq!(attempts.get(), 1);
        let notice = recovery.notice.expect("a stuck holder is reported");
        assert!(notice.contains("run run-1 still holds it"));
        assert!(notice.contains("recovering it did not release the lock"));
        assert!(notice.contains("Next step: run /doctor"));
        assert!(notice.contains("mhc doctor --fix"));
    }

    #[test]
    fn an_unreadable_lock_is_reported_as_stuck_and_never_reconciled() {
        let fixture = fixture(4242);
        let denied = |_: &Path| Err("EACCES".to_string());
        let seams = ActiveHolderSeams {
            read_text: Some(&denied),
            ..Default::default()
        };
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| panic!("an unreadable lock is not provably dead"),
            seams: Some(&seams),
            should_reconcile: None,
        });
        assert!(matches!(
            recovery.verdict,
            ActiveHolderVerdict::Stuck { .. }
        ));
        assert!(recovery.notice.is_some());
    }

    #[test]
    fn a_recent_torn_lock_is_reported_and_never_moved_or_reconciled() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "active.lock", "{\"runId\":");
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| panic!("a torn lock is not settled through reconciliation"),
            seams: None,
            should_reconcile: None,
        });
        let notice = recovery.notice.expect("a torn lock is reported");
        assert!(notice.contains("active.lock is not valid JSON and it is recent"));
        assert!(recovery.recovered_run_id.is_none());
        assert!(fixture.reflection.join("active.lock").exists());
    }

    #[test]
    fn a_launch_that_never_started_is_settled_and_its_run_is_reported_as_recovered() {
        let fixture = fixture(4242);
        write(
            &fixture.reflection,
            "active.lock",
            &serde_json::json!({
                "runId": "run-1",
                "request": {"trigger": "manual", "conversationIds": [], "snapshots": []},
                "reservedAt": "2020-01-01T00:00:00.000Z",
                "launcherPid": 4242,
                "launcherHostname": memory_core::support::host::hostname(),
            })
            .to_string(),
        );
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let reconciles = std::cell::Cell::new(0);
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| {
                reconciles.set(reconciles.get() + 1);
                std::fs::remove_file(fixture.reflection.join("active.lock"))
                    .map_err(|error| error.to_string())
            },
            seams: Some(&seams),
            should_reconcile: None,
        });
        assert_eq!(reconciles.get(), 1);
        assert!(recovery.notice.is_none());
        assert_eq!(recovery.recovered_run_id.as_deref(), Some("run-1"));
    }

    #[test]
    fn a_reconcile_that_fails_leaves_the_holder_reported_with_the_cause() {
        let fixture = fixture(4242);
        write(&fixture.reflection, "runs/run-1/outcome.json", OUTCOME);
        let dead = |_: u32| ProcessLiveness::Dead;
        let seams = seams(&dead);
        let recovery = recover_stale_reflection_holder(StaleHolderRecoveryInput {
            reflection_dir: &fixture.reflection,
            reconcile: &|| Err("worktree is busy".to_string()),
            seams: Some(&seams),
            should_reconcile: None,
        });
        let notice = recovery.notice.expect("a failed recovery is reported");
        assert!(notice.contains("recovering it failed: worktree is busy"));
        assert!(recovery.recovered_run_id.is_none());
    }

    #[test]
    fn session_start_without_a_lock_counts_one_settle_and_changes_nothing() {
        let fixture = fixture(4242);
        std::fs::remove_file(fixture.reflection.join("active.lock")).unwrap();
        let runtime = fixture.reflection.parent().unwrap();
        let before: Vec<_> = std::fs::read_dir(runtime)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let report = reap_before_session_reconcile(runtime, None).expect("reaper runs");
        assert_eq!(report.decision, StaleLockDecision::None);
        assert_eq!(report.action, StaleLockReaperAction::None);
        let after: Vec<_> = std::fs::read_dir(runtime)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(before, after);
    }
}
