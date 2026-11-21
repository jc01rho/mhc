use super::*;
use tempfile::TempDir;

fn memory_root(identities: usize, repo_less: &[usize]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for index in 0..identities {
        let identity = root.path().join("agents").join(format!("agent-{index:02}"));
        std::fs::create_dir_all(identity.join("runtime")).unwrap();
        if !repo_less.contains(&index) {
            std::fs::create_dir_all(identity.join("repo")).unwrap();
        }
    }
    std::fs::create_dir_all(root.path().join("agents").join("no-runtime")).unwrap();
    root
}

fn no_lines(_identity: &HostSweepIdentity) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

fn swept(result: &HostSweepResult) -> (&[String], bool) {
    match result {
        HostSweepResult::Swept {
            visited, completed, ..
        } => (visited.as_slice(), *completed),
        HostSweepResult::Busy => panic!("expected a sweep, got busy"),
    }
}

#[test]
fn two_bounded_passes_visit_every_identity_in_sorted_order_and_record_completion() {
    let root = memory_root(30, &[3, 17, 28]);
    let mut seen: Vec<HostSweepIdentity> = Vec::new();
    let first_clock = || 1_000_u64;
    let second_clock = || 2_000_u64;

    let first = sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(
            |identity: &HostSweepIdentity| -> Result<Vec<String>, String> {
                seen.push(identity.clone());
                Ok(Vec::new())
            },
        )],
        max_identities: Some(25),
        now: Some(&first_clock),
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();
    let cursor = read_host_sweep_state(root.path());
    let second = sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(
            |identity: &HostSweepIdentity| -> Result<Vec<String>, String> {
                seen.push(identity.clone());
                Ok(Vec::new())
            },
        )],
        max_identities: Some(25),
        now: Some(&second_clock),
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    assert!(!swept(&first).1);
    assert_eq!(cursor.cursor.as_deref(), Some("agent-24"));
    let (visited, completed) = swept(&second);
    assert!(completed);
    assert_eq!(
        visited,
        ["agent-25", "agent-26", "agent-27", "agent-28", "agent-29"]
    );
    let names: Vec<String> = seen.iter().map(|identity| identity.name.clone()).collect();
    let expected: Vec<String> = (0..30).map(|index| format!("agent-{index:02}")).collect();
    assert_eq!(names, expected);
    assert_eq!(
        read_host_sweep_state(root.path()),
        HostSweepState {
            cursor: None,
            completed_at: Some(format_rfc3339_millis(2_000)),
        }
    );
    let repo_less: Vec<&str> = seen
        .iter()
        .filter(|identity| !identity.has_repo)
        .map(|identity| identity.name.as_str())
        .collect();
    assert_eq!(repo_less, ["agent-03", "agent-17", "agent-28"]);
    assert!(!seen.iter().any(|identity| identity.name == "no-runtime"));
}

#[test]
fn an_unbounded_pass_records_completion_and_clears_the_cursor() {
    let root = memory_root(4, &[]);
    let at = crate::support::time::parse_rfc3339("2026-11-05T06:00:00.000Z").unwrap();
    let clock = move || u64::try_from(at).unwrap();

    let result = sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(no_lines)],
        now: Some(&clock),
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    assert!(swept(&result).1);
    assert_eq!(
        read_host_sweep_state(root.path()),
        HostSweepState {
            cursor: None,
            completed_at: Some("2026-11-05T06:00:00.000Z".to_string()),
        }
    );
}

#[test]
fn a_second_driver_skips_without_waiting_while_the_lock_is_held() {
    let root = memory_root(2, &[]);
    let record = create_lock_record("runtime-sweep", CreateLockRecordOptions::default()).unwrap();
    let mut result = None;

    with_lock(
        &root.path().join(HOST_SWEEP_LOCK_FILE),
        &record,
        &AcquireLockOptions::default(),
        || -> Result<(), String> {
            result = Some(sweep_host_identities(HostSweepOptions {
                steps: vec![Box::new(no_lines)],
                ..HostSweepOptions::new(root.path())
            }));
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(result, Some(Ok(HostSweepResult::Busy)));
}

#[test]
fn an_unbounded_driver_finishes_an_interrupted_round_before_wrapping() {
    let root = memory_root(4, &[]);
    sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(no_lines)],
        max_identities: Some(2),
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    let result = sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(no_lines)],
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    let (visited, completed) = swept(&result);
    assert!(completed);
    assert_eq!(visited, ["agent-02", "agent-03", "agent-00", "agent-01"]);
}

#[test]
fn a_dry_run_writes_no_cursor() {
    let root = memory_root(3, &[]);

    sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(no_lines)],
        max_identities: Some(1),
        dry_run: true,
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    assert_eq!(
        read_host_sweep_state(root.path()),
        HostSweepState::default()
    );
}

#[test]
fn step_lines_are_collected_in_visit_order() {
    let root = memory_root(2, &[]);

    let result = sweep_host_identities(HostSweepOptions {
        steps: vec![Box::new(
            |identity: &HostSweepIdentity| -> Result<Vec<String>, String> {
                Ok(vec![format!("visited {}", identity.name)])
            },
        )],
        ..HostSweepOptions::new(root.path())
    })
    .unwrap();

    match result {
        HostSweepResult::Swept { lines, .. } => {
            assert_eq!(lines, ["visited agent-00", "visited agent-01"]);
        }
        HostSweepResult::Busy => panic!("expected a sweep"),
    }
}
