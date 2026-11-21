use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn reason(usability: SandboxUsability) -> String {
    match usability {
        SandboxUsability::Unusable { reason } => reason,
        SandboxUsability::Usable => panic!("expected an unusable verdict"),
    }
}

#[test]
fn a_smoke_child_that_exited_zero_is_usable() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        exit_code: Some(0),
        ..BwrapSmokeResult::default()
    });

    assert_eq!(usability, SandboxUsability::Usable);
}

#[test]
fn the_apparmor_user_namespace_refusal_is_unusable_with_the_bwrap_stderr() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        exit_code: Some(1),
        stderr: "bwrap: setting up uid map: Permission denied\n".into(),
        ..BwrapSmokeResult::default()
    });

    assert!(reason(usability).contains("setting up uid map: Permission denied"));
}

#[test]
fn a_silent_nonzero_exit_still_names_the_exit_code() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        exit_code: Some(3),
        ..BwrapSmokeResult::default()
    });

    assert!(reason(usability).contains('3'));
}

#[test]
fn a_child_that_could_not_spawn_is_unusable_with_the_spawn_error() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        error_message: Some("spawn /usr/bin/bwrap ENOENT".into()),
        ..BwrapSmokeResult::default()
    });

    assert!(reason(usability).contains("spawn /usr/bin/bwrap ENOENT"));
}

#[test]
fn a_hung_child_is_unusable_and_the_reason_names_the_timeout() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        timed_out: true,
        ..BwrapSmokeResult::default()
    });

    assert!(reason(usability).contains("timed out"));
}

#[test]
fn the_timeout_wins_over_the_exit_classification() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        timed_out: true,
        stderr: "bwrap: partial\n".into(),
        ..BwrapSmokeResult::default()
    });

    let reason = reason(usability);
    assert!(reason.contains("timed out"));
    assert!(reason.contains("bwrap: partial"));
}

#[test]
fn the_reason_keeps_only_the_stderr_tail() {
    let usability = classify_bwrap_smoke(&BwrapSmokeResult {
        exit_code: Some(1),
        stderr: format!("{}{}", "x".repeat(500), "y".repeat(200)),
        ..BwrapSmokeResult::default()
    });

    assert_eq!(
        reason(usability),
        format!("smoke test exited 1: {}", "y".repeat(200))
    );
}

#[test]
fn a_definitive_verdict_is_memoized_per_executable() {
    let probe = BwrapProbe::new();
    let calls = AtomicUsize::new(0);
    let smoke = |_: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        BwrapSmokeResult {
            exit_code: Some(1),
            stderr: "bwrap: setting up uid map: Permission denied".into(),
            ..BwrapSmokeResult::default()
        }
    };

    let first = probe.probe("/usr/bin/bwrap", &smoke);
    let second = probe.probe("/usr/bin/bwrap", &smoke);

    assert_eq!(first, second);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_timed_out_verdict_is_returned_but_never_memoized() {
    let probe = BwrapProbe::new();
    let calls = AtomicUsize::new(0);
    let smoke = |_: &str| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            BwrapSmokeResult {
                timed_out: true,
                ..BwrapSmokeResult::default()
            }
        } else {
            BwrapSmokeResult {
                exit_code: Some(0),
                ..BwrapSmokeResult::default()
            }
        }
    };

    let first = probe.probe("/usr/bin/bwrap", &smoke);
    let second = probe.probe("/usr/bin/bwrap", &smoke);
    let third = probe.probe("/usr/bin/bwrap", &smoke);

    assert!(reason(first).contains("timed out"));
    assert_eq!(second, SandboxUsability::Usable);
    assert_eq!(third, SandboxUsability::Usable);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_missing_executable_cannot_run_the_smoke() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("bwrap");

    let result = run_bwrap_smoke(&missing.to_string_lossy());

    assert!(!result.timed_out);
    assert!(result.error_message.is_some());
    assert!(reason(classify_bwrap_smoke(&result)).contains("smoke test could not run"));
}
