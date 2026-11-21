use std::path::Path;

use super::*;

fn charged(path: &Path) -> u64 {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.blocks() > 0 || metadata.len() == 0 {
            return metadata.blocks() * 512;
        }
    }
    metadata.len()
}

fn write_bytes(path: &Path, length: usize, byte: u8) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![byte; length]).unwrap();
}

#[cfg(unix)]
#[test]
fn a_symlink_to_a_large_external_dir_counts_as_itself_and_is_never_followed() {
    let base = tempfile::tempdir().unwrap();
    let external = base.path().join("external");
    write_bytes(&external.join("big.bin"), 4 * 1024 * 1024, 1);
    let runtime = base.path().join("runtime");
    let runs = runtime.join("reflection").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    let link = runs.join("linked");
    std::os::unix::fs::symlink(&external, &link).unwrap();

    let size = measure_runtime_size(&runtime, None).unwrap();

    let expected: u64 = [&runtime, &runtime.join("reflection"), &runs, &link]
        .iter()
        .map(|path| charged(path))
        .sum();
    assert_eq!(size.bytes, expected);
    assert!(size.bytes < 4 * 1024 * 1024);
}

#[test]
fn a_regular_file_is_charged_by_allocated_blocks_the_way_du_is() {
    let base = tempfile::tempdir().unwrap();
    let runtime = base.path().join("runtime");
    let file = runtime
        .join("transcripts")
        .join("s1")
        .join("transcript.jsonl");
    write_bytes(&file, 10_001, 7);

    let size = measure_runtime_size(&runtime, None).unwrap();

    let s1 = runtime.join("transcripts").join("s1");
    let directories: u64 = [&runtime, &runtime.join("transcripts"), &s1]
        .iter()
        .map(|path| charged(path))
        .sum();
    assert_eq!(size.bytes, directories + charged(&file));
    assert!(charged(&file) >= 10_001);
}

#[test]
fn each_store_is_reported_largest_first_and_they_sum_to_the_total() {
    let base = tempfile::tempdir().unwrap();
    let runtime = base.path().join("runtime");
    write_bytes(
        &runtime
            .join("reflection")
            .join("runs")
            .join("reflection-run-1")
            .join("child-1.log"),
        300_000,
        1,
    );
    write_bytes(
        &runtime
            .join("recall")
            .join("sidecars")
            .join("abc")
            .join("wakes.ndjson"),
        120_000,
        1,
    );
    write_bytes(
        &runtime
            .join("transcripts")
            .join("s1")
            .join("transcript.jsonl"),
        40_000,
        1,
    );
    std::fs::write(runtime.join("receipts.jsonl"), "{}\n").unwrap();

    let size = measure_runtime_size(&runtime, None).unwrap();

    let leading: Vec<&str> = size
        .stores
        .iter()
        .take(3)
        .map(|store| store.store.as_str())
        .collect();
    assert_eq!(
        leading,
        ["reflection/runs", "recall/sidecars", "transcripts"]
    );
    let split_roots =
        charged(&runtime) + charged(&runtime.join("reflection")) + charged(&runtime.join("recall"));
    let stores: u64 = size.stores.iter().map(|store| store.bytes).sum();
    assert_eq!(split_roots + stores, size.bytes);
    assert_eq!(
        size.stores
            .iter()
            .find(|store| store.store == "receipts.jsonl")
            .map(|store| store.bytes),
        Some(charged(&runtime.join("receipts.jsonl")))
    );
}

#[test]
fn a_fired_deadline_aborts_the_walk_instead_of_finishing() {
    let base = tempfile::tempdir().unwrap();
    let runtime = base.path().join("runtime");
    write_bytes(
        &runtime
            .join("transcripts")
            .join("s1")
            .join("transcript.jsonl"),
        3,
        b'x',
    );
    let fired = || true;

    let result = measure_runtime_size(&runtime, Some(&fired));

    assert!(matches!(result, Err(RetentionError::Aborted)), "{result:?}");
}

#[test]
fn a_missing_runtime_directory_measures_zero() {
    let base = tempfile::tempdir().unwrap();

    let size = measure_runtime_size(&base.path().join("missing"), None).unwrap();

    assert_eq!(size, RuntimeSize::default());
}
