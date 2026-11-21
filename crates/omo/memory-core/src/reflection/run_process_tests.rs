use super::*;

fn dead(_: u32) -> ProcessLiveness {
    ProcessLiveness::Dead
}

fn alive(_: u32) -> ProcessLiveness {
    ProcessLiveness::Alive
}

fn start_one(_: u32) -> Option<String> {
    Some("proc-start-epoch:1".to_string())
}

fn start_two(_: u32) -> Option<String> {
    Some("proc-start-epoch:2".to_string())
}

fn seams<'a>(
    liveness: &'a dyn Fn(u32) -> ProcessLiveness,
    start: &'a dyn Fn(u32) -> Option<String>,
) -> RecordedProcessSeams<'a> {
    RecordedProcessSeams {
        pid_liveness: Some(liveness),
        process_start: Some(start),
    }
}

fn classify_both(
    pid: Option<u64>,
    start: RecordedStart<'_>,
    seams: &RecordedProcessSeams<'_>,
) -> [RecordedProcessVerdict; 2] {
    [
        classify_recorded_process(pid, start, seams),
        classify_recorded_process_lenient(pid, start, seams),
    ]
}

#[test]
fn a_dead_pid_with_no_recorded_start_reads_dead_only_leniently() {
    let seams = seams(&dead, &start_one);

    assert_eq!(
        classify_both(Some(7), RecordedStart::Undefined, &seams),
        [
            RecordedProcessVerdict::Unknown,
            RecordedProcessVerdict::Dead
        ]
    );
}

#[test]
fn a_dead_pid_with_a_null_recorded_start_reads_dead_for_both_classifiers() {
    let seams = seams(&dead, &start_one);

    assert_eq!(
        classify_both(Some(7), RecordedStart::Null, &seams),
        [RecordedProcessVerdict::Dead, RecordedProcessVerdict::Dead]
    );
}

#[test]
fn a_live_pid_with_no_recorded_start_stays_unknown_for_both_classifiers() {
    let seams = seams(&alive, &start_one);

    assert_eq!(
        classify_both(Some(7), RecordedStart::Undefined, &seams),
        [
            RecordedProcessVerdict::Unknown,
            RecordedProcessVerdict::Unknown
        ]
    );
}

#[test]
fn no_recorded_pid_is_absent_for_both_classifiers() {
    let seams = seams(&dead, &start_one);

    assert_eq!(
        classify_both(None, RecordedStart::Undefined, &seams),
        [
            RecordedProcessVerdict::Absent,
            RecordedProcessVerdict::Absent
        ]
    );
}

#[test]
fn a_live_pid_whose_recorded_start_belongs_to_another_process_is_proven_reused() {
    let seams = seams(&alive, &start_two);

    assert_eq!(
        classify_both(
            Some(7),
            RecordedStart::Identity("proc-start-epoch:1"),
            &seams
        ),
        [RecordedProcessVerdict::Dead, RecordedProcessVerdict::Dead]
    );
}

#[test]
fn a_live_pid_with_the_recorded_start_is_alive() {
    let seams = seams(&alive, &start_one);

    assert_eq!(
        classify_both(
            Some(7),
            RecordedStart::Identity("proc-start-epoch:1"),
            &seams
        ),
        [RecordedProcessVerdict::Alive, RecordedProcessVerdict::Alive]
    );
}

#[test]
fn a_pid_the_os_cannot_name_is_never_declared_dead() {
    let seams = seams(&dead, &start_one);

    assert_eq!(
        classify_both(Some(u64::MAX), RecordedStart::Null, &seams),
        [
            RecordedProcessVerdict::Unknown,
            RecordedProcessVerdict::Unknown
        ]
    );
}
