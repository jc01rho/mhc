//! Port of `platform/process-tree-cpu.ts` (#14820, #13953): the CPU seconds of a process plus every
//! live descendant, so a launcher that idles while the engine it started works still shows progress.

use std::collections::{BTreeMap, BTreeSet};

use super::native_process::{read_parent_pids, read_process_cpu_seconds, read_process_start_time};

/// The process table one tree sum reads: who names whom, each process's own CPU, and a start stamp
/// that orders processes by creation.
pub struct ProcessTable {
    pub parents: BTreeMap<u32, u32>,
    pub cpu_seconds: Box<dyn Fn(u32) -> Option<f64>>,
    pub start_time: Box<dyn Fn(u32) -> Option<u64>>,
}

/// CPU seconds of the process rooted at `pid` plus every live descendant, or `None` when the root
/// cannot be read.
///
/// A process counts as a child of the parent it names only if it was not created before that parent.
/// Windows records the parent pid at creation and never updates it, and it reuses pids, so a
/// long-lived process whose original parent has exited can name a fresh pid as its parent; without
/// the check its whole lifetime of CPU lands in the new process's tree (#13953).
pub fn sum_process_tree_cpu_seconds(pid: u32, table: &ProcessTable) -> Option<f64> {
    let root = (table.cpu_seconds)(pid)?;
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (child, parent) in &table.parents {
        if child == parent {
            continue;
        }
        children.entry(*parent).or_default().push(*child);
    }
    let mut total = root;
    let mut seen = BTreeSet::from([pid]);
    let mut pending: Vec<(u32, u32)> = children
        .get(&pid)
        .map(|siblings| siblings.iter().map(|child| (*child, pid)).collect())
        .unwrap_or_default();
    while let Some((child, parent)) = pending.pop() {
        if seen.contains(&child) || born_before(child, parent, table) {
            continue;
        }
        seen.insert(child);
        total += (table.cpu_seconds)(child).unwrap_or(0.0);
        if let Some(grandchildren) = children.get(&child) {
            pending.extend(grandchildren.iter().map(|grandchild| (*grandchild, child)));
        }
    }
    Some(total)
}

/// Only a stamp strictly earlier than the parent's proves a stale parent pid. Equal stamps count:
/// darwin stamps are whole seconds, so a child spawned in its parent's first second carries the
/// parent's stamp. An unreadable stamp counts too: on Windows a process that cannot be opened has no
/// readable CPU either, so it adds nothing.
fn born_before(child: u32, parent: u32, table: &ProcessTable) -> bool {
    let child_start = (table.start_time)(child);
    let parent_start = (table.start_time)(parent);
    matches!((child_start, parent_start), (Some(child_start), Some(parent_start)) if child_start < parent_start)
}

/// The live tree CPU of `pid`. A launcher that spawns the real engine as its own child (an npm shim, a
/// wrapper script) stays CPU-flat while the engine works, so a progress bound on the launcher alone
/// would call a booting engine stalled (#14820).
pub fn read_process_tree_cpu_seconds(pid: u32) -> Option<f64> {
    let Some(parents) = read_parent_pids() else {
        return read_process_cpu_seconds(pid);
    };
    sum_process_tree_cpu_seconds(
        pid,
        &ProcessTable {
            parents,
            cpu_seconds: Box::new(read_process_cpu_seconds),
            start_time: Box::new(read_process_start_time),
        },
    )
}

#[cfg(test)]
#[path = "process_tree_cpu_tests.rs"]
mod tests;
