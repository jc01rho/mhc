use super::*;

fn records_in(directory: &Path) -> Vec<serde_json::Value> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .map(|entry| {
            let raw = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            serde_json::from_str(&raw).unwrap()
        })
        .collect()
}

#[test]
fn nothing_is_recorded_while_the_diagnostics_directory_is_unset() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("diagnostics");

    record_git_timeout_with(
        None,
        Some(41),
        &["status".to_owned()],
        root.path(),
        30_000,
        &|_| panic!("no snapshot without a directory"),
    );
    record_git_timeout_with(
        Some(""),
        Some(41),
        &["status".to_owned()],
        root.path(),
        30_000,
        &|_| panic!("no snapshot for an empty directory"),
    );

    assert!(!directory.exists());
}

#[test]
fn a_timeout_records_the_process_table_and_bounded_argv() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("diagnostics");
    let argv: Vec<String> = (0..20).map(|index| format!("arg-{index}")).collect();

    record_git_timeout_with(
        Some(&directory.to_string_lossy()),
        Some(41),
        &argv,
        root.path(),
        30_000,
        &|pid| format!("table for {pid}"),
    );

    let records = records_in(&directory);
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["pid"], 41);
    assert_eq!(record["timeoutMs"], 30_000);
    assert_eq!(record["argCount"], 20);
    assert_eq!(record["argv"].as_array().unwrap().len(), 12);
    assert_eq!(record["argv"][0], "arg-0");
    assert_eq!(record["processes"], "table for 41");
    assert_eq!(record["cwd"], root.path().to_string_lossy().into_owned());
}

#[cfg(unix)]
#[test]
fn the_real_snapshot_lists_the_process_table_and_leaves_no_temp_file() {
    let pid = std::process::id();
    let prefix = format!("omo-git-timeout-snapshot-{pid}-");

    let table = process_snapshot(pid);

    assert!(table.contains("PID"), "{table}");
    assert!(table.contains(&pid.to_string()), "{table}");
    let leftovers = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .count();
    assert_eq!(leftovers, 0);
}
