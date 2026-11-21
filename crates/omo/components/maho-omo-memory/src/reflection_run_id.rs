use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use memory_core::reflection::{
    REFLECTION_RUN_ID_PREFIX, RunIdHighWaterPaths, read_reflection_run_id_mark,
    record_reflection_run_id_mark, scan_reflection_run_high_water,
};

/// Mints reflection run ids one past the highest id still ON DISK. The durable
/// `reflection/run-id.json` mark is a fifth input and is raised on every mint, so runtime retention
/// can prune old traces without lowering the high water. The reservation store calls this factory
/// inside the `reflection-scheduler` lock, which is what makes the mark write race-free.
pub fn create_reflection_run_id_factory(
    reflection: PathBuf,
    sessions: PathBuf,
    worktrees: PathBuf,
) -> Arc<dyn Fn() -> String + Send + Sync> {
    let high_water = Arc::new(AtomicU64::new(0));
    Arc::new(move || {
        let scanned = scan_reflection_run_high_water(RunIdHighWaterPaths {
            reflection: &reflection,
            reflection_sessions: &sessions,
            worktrees: &worktrees,
        });
        let mark = read_reflection_run_id_mark(&reflection).unwrap_or(0);
        let next = scanned
            .max(mark)
            .max(high_water.load(Ordering::Relaxed))
            .saturating_add(1);
        let _ = high_water.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.max(next))
        });
        if let Err(error) = record_reflection_run_id_mark(&reflection, next) {
            eprintln!("memory reflection run id mark not recorded: {error}");
        }
        format!("{REFLECTION_RUN_ID_PREFIX}{next}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_advance_past_disk_and_reservation_state() {
        let root = tempfile::tempdir().unwrap();
        let reflection = root.path().join("reflection");
        let sessions = root.path().join("sessions");
        let worktrees = root.path().join("worktrees");
        for path in [&reflection.join("completions"), &sessions, &worktrees] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(reflection.join("completions/reflection-run-4.json"), "{}").unwrap();
        std::fs::write(
            reflection.join("active.lock"),
            r#"{"runId":"reflection-run-7"}"#,
        )
        .unwrap();
        std::fs::write(worktrees.join("1755000000000-reflection-run-9"), "").unwrap();
        let mint = create_reflection_run_id_factory(reflection, sessions, worktrees);
        assert_eq!(mint(), "reflection-run-10");
        assert_eq!(mint(), "reflection-run-11");
    }

    #[test]
    fn the_durable_mark_keeps_ids_rising_after_every_trace_is_pruned() {
        let root = tempfile::tempdir().unwrap();
        let reflection = root.path().join("reflection");
        let sessions = root.path().join("sessions");
        let worktrees = root.path().join("worktrees");
        std::fs::create_dir_all(reflection.join("completions")).unwrap();
        std::fs::write(reflection.join("completions/reflection-run-4.json"), "{}").unwrap();
        let mint = create_reflection_run_id_factory(
            reflection.clone(),
            sessions.clone(),
            worktrees.clone(),
        );
        assert_eq!(mint(), "reflection-run-5");

        std::fs::remove_file(reflection.join("completions/reflection-run-4.json")).unwrap();
        let after_prune = create_reflection_run_id_factory(reflection, sessions, worktrees);

        assert_eq!(after_prune(), "reflection-run-6");
    }
}
