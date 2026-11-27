//! `platform/process-tree-cpu.test.ts` (#13953, #14820).

use std::collections::BTreeMap;

use super::*;

const ENGINE: u32 = 4100;
const ENGINE_CHILD: u32 = 4200;
const ENGINE_GRANDCHILD: u32 = 4300;
const RUNNER: u32 = 3000;
const RUNNER_WORKER: u32 = 3100;

struct Row {
    pid: u32,
    parent: u32,
    cpu: Option<f64>,
    start: Option<u64>,
}

fn table(rows: Vec<Row>) -> ProcessTable {
    let parents = rows.iter().map(|row| (row.pid, row.parent)).collect();
    let cpu: BTreeMap<u32, Option<f64>> = rows.iter().map(|row| (row.pid, row.cpu)).collect();
    let start: BTreeMap<u32, Option<u64>> = rows.iter().map(|row| (row.pid, row.start)).collect();
    ProcessTable {
        parents,
        cpu_seconds: Box::new(move |pid| cpu.get(&pid).copied().flatten()),
        start_time: Box::new(move |pid| start.get(&pid).copied().flatten()),
    }
}

fn row(pid: u32, parent: u32, cpu: f64, start: u64) -> Row {
    Row {
        pid,
        parent,
        cpu: Some(cpu),
        start: Some(start),
    }
}

// The startup CPU cap reads the engine child's tree. Windows keeps a process's creation-time parent pid
// after that parent exits and reuses pids, so a test runner started minutes earlier by a since-exited
// parent whose pid the engine now holds names the engine as its parent. Its lifetime CPU must not
// count, or a 3 s old engine trips the 120 s cap (#13953: 231.3 s reported for a child 1.5 s into its
// boot).
#[test]
fn a_long_lived_process_naming_the_engines_reused_pid_counts_only_when_born_after_its_parent() {
    let processes = table(vec![
        row(ENGINE, 1, 2.5, 1_000),
        row(ENGINE_CHILD, ENGINE, 0.4, 1_010),
        row(ENGINE_GRANDCHILD, ENGINE_CHILD, 0.1, 1_020),
        row(RUNNER, ENGINE, 215.0, 10),
        row(RUNNER_WORKER, RUNNER, 13.3, 20),
    ]);

    let total = sum_process_tree_cpu_seconds(ENGINE, &processes);

    assert_eq!(total, Some(3.0));
}

#[test]
fn a_child_whose_start_stamp_equals_its_parents_counts() {
    // darwin stamps are whole seconds, so a child spawned in its parent's first second carries the same
    // stamp; only a stamp strictly earlier than the parent's proves a stale parent pid.
    let processes = table(vec![
        row(ENGINE, 1, 0.2, 1_000),
        row(ENGINE_CHILD, ENGINE, 1.3, 1_000),
    ]);

    assert_eq!(sum_process_tree_cpu_seconds(ENGINE, &processes), Some(1.5));
}

#[test]
fn a_descendant_whose_start_time_cannot_be_read_still_counts() {
    // an unreadable stamp is not proof of a stale parent, and dropping it would undercount a booting
    // engine.
    let processes = table(vec![
        row(ENGINE, 1, 0.2, 1_000),
        Row {
            pid: ENGINE_CHILD,
            parent: ENGINE,
            cpu: Some(1.3),
            start: None,
        },
    ]);

    assert_eq!(sum_process_tree_cpu_seconds(ENGINE, &processes), Some(1.5));
}

#[test]
fn an_unreadable_root_answers_none() {
    let processes = table(vec![Row {
        pid: ENGINE,
        parent: 1,
        cpu: None,
        start: Some(1_000),
    }]);

    assert_eq!(sum_process_tree_cpu_seconds(ENGINE, &processes), None);
}
