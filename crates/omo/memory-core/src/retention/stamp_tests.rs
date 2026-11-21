use super::*;

use crate::retention::budget::{RetentionCursor, RetentionStore};
use crate::retention::cap::RuntimeCapCursor;
use crate::retention::test_support::{NOW, exists, identity, read_json};

fn cursor(store: &str, last_name: Option<&str>) -> RetentionCursor {
    RetentionCursor {
        store: store.to_string(),
        last_name: last_name.map(str::to_string),
    }
}

#[test]
fn a_store_cursor_round_trips_through_the_runtime_stamp_file() {
    let id = identity();
    let runtime = &id.paths.runtime;
    std::fs::create_dir_all(runtime).expect("runtime dir");
    let points = RetentionResumePoints {
        cursor: Some(cursor("journals", Some("s-05"))),
        ..RetentionResumePoints::default()
    };

    write_retention_resume_points(runtime, &points).expect("write resume points");

    assert_eq!(read_retention_resume_points(runtime), points);
    assert!(retention_resume_pending(runtime));
    let stored = read_json(&retention_cursor_path(runtime));
    assert_eq!(stored["version"], 1);
    assert_eq!(stored["cursor"]["store"], "journals");
    assert_eq!(stored["cursor"]["lastName"], "s-05");
}

#[test]
fn a_cursor_without_a_last_name_round_trips_as_a_store_only_resume_point() {
    let id = identity();
    let runtime = &id.paths.runtime;
    std::fs::create_dir_all(runtime).expect("runtime dir");
    let points = RetentionResumePoints {
        cursor: Some(cursor("runs", None)),
        ..RetentionResumePoints::default()
    };

    write_retention_resume_points(runtime, &points).expect("write resume points");

    let read = read_retention_resume_points(runtime);
    assert_eq!(read.cursor, points.cursor);
    assert!(read.cap.is_none());
    assert!(!read.cap_stalled);
    assert!(retention_resume_pending(runtime));
}

#[test]
fn clearing_the_cursor_removes_the_file_and_leaves_nothing_pending() {
    let id = identity();
    let runtime = &id.paths.runtime;
    std::fs::create_dir_all(runtime).expect("runtime dir");
    let points = RetentionResumePoints {
        cursor: Some(cursor("journals", Some("s-05"))),
        ..RetentionResumePoints::default()
    };
    write_retention_resume_points(runtime, &points).expect("write resume points");

    clear_retention_cursor(runtime).expect("clear cursor");

    assert!(!exists(&retention_cursor_path(runtime)));
    assert_eq!(
        read_retention_resume_points(runtime),
        RetentionResumePoints::default()
    );
    assert!(!retention_resume_pending(runtime));
    clear_retention_cursor(runtime).expect("clearing twice is not an error");
}

#[test]
fn an_unreadable_stamp_file_reads_as_no_resume_point() {
    let id = identity();
    let runtime = &id.paths.runtime;
    std::fs::create_dir_all(runtime).expect("runtime dir");
    std::fs::write(retention_cursor_path(runtime), "{not json").expect("broken stamp");

    assert_eq!(
        read_retention_resume_points(runtime),
        RetentionResumePoints::default()
    );
    assert!(!retention_resume_pending(runtime));
}

#[test]
fn a_cap_resume_point_round_trips_and_a_stalled_one_is_not_pending() {
    let id = identity();
    let runtime = &id.paths.runtime;
    std::fs::create_dir_all(runtime).expect("runtime dir");
    let cap = RuntimeCapCursor {
        tier: "sidecars".to_string(),
        clock: NOW,
        last_name: "abc".to_string(),
    };
    let stalled = RetentionResumePoints {
        cursor: None,
        cap: Some(cap.clone()),
        cap_stalled: true,
    };

    write_retention_resume_points(runtime, &stalled).expect("write stalled cap");

    assert_eq!(read_retention_resume_points(runtime), stalled);
    assert!(!retention_resume_pending(runtime));
    assert_eq!(
        read_json(&retention_cursor_path(runtime))["capStalled"],
        true
    );

    let pending = RetentionResumePoints {
        cap_stalled: false,
        ..stalled
    };
    write_retention_resume_points(runtime, &pending).expect("write pending cap");
    assert_eq!(read_retention_resume_points(runtime), pending);
    assert!(retention_resume_pending(runtime));
}

#[test]
fn the_stamp_config_key_is_the_one_the_git_config_writer_uses() {
    assert_eq!(RETENTION_STAMP_CONFIG_KEY, "omo.retentionAt");
    let id = identity();
    assert_eq!(
        retention_cursor_path(&id.paths.runtime),
        id.paths.runtime.join("retention.json")
    );
    assert_eq!(RetentionStore::Runs.as_str(), "runs");
}
