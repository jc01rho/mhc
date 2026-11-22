//! Successor-side reopen of handover-parked sessions (senpi `host-handover-reopen.ts`, senpi#3282).
//!
//! A successor host reopens exactly the set its predecessor held resident and parked, restoring each with
//! the same durable id. An entry leaves the record only after a successful reopen (crash-safe, at most once).
//! The loop is written against [`HandoverParkedStore`]; the file-backed store over
//! `<daemonDir>/handover-parked/*.json` is `host_handover_parked::FileHandoverParkedStore`.
use serde_json::Value;
use std::{collections::HashMap, future::Future, pin::Pin, time::Duration};
/// The watch budget must outlast a heavily loaded drain.
pub const HANDOVER_PARKED_REOPEN_BUDGET_MS: u64 = 120_000;
pub const HANDOVER_PARKED_REOPEN_POLL_MS: u64 = 250;
pub const HANDOVER_PARKED_REOPEN_MAX_ATTEMPTS: u32 = 8;
pub const HANDOVER_PARKED_REOPEN_QUIET_STREAK: u32 = 8;
pub const HANDOVER_PARKED_PREDECESSOR_POLL_MS: u64 = 2_000;
/// `task_status` values after which a parked session is never reopened.
pub const TERMINAL_TASK_STATUSES: [&str; 6] = [
    "completed",
    "cancelled",
    "error",
    "failed",
    "lost",
    "interrupted",
];
pub use crate::host_handover_parked::HandoverParkedEntry;
/// A refusal from the opener; `session_path_in_use` means the predecessor still holds the path's claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReopenError {
    pub code: Option<String>,
    pub message: String,
}
impl std::fmt::Display for ReopenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// The per-entry parked-session record.
pub trait HandoverParkedStore: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Vec<HandoverParkedEntry>>;
    fn clear(&self, session_path: &str) -> BoxFuture<'_, ()>;
    fn drop_if_file_gone(&self, session_path: &str) -> BoxFuture<'_, ()>;
}
/// Opens one parked session on this host, refusing with a [`ReopenError`].
pub type ParkedSessionOpener<'a> =
    &'a (dyn Fn(HandoverParkedEntry) -> BoxFuture<'static, Result<(), ReopenError>> + Send + Sync);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InheritedHandover {
    pub pending: usize,
    pub predecessor_draining: bool,
}
/// The host the watch reopens into. `should_reopen` returns false once that host is draining for a
/// handoff: a predecessor's own watch must never consume a park it wrote for its successor.
pub struct ReopenHolder<'a> {
    pub host_instance: String,
    pub host_socket: Option<String>,
    pub should_reopen: Option<&'a (dyn Fn() -> bool + Send + Sync)>,
    pub paths: Option<crate::host_daemon_paths::HostDaemonPaths>,
    pub on_progress: Option<&'a (dyn Fn(Option<InheritedHandover>) + Send + Sync)>,
}
impl ReopenHolder<'_> {
    fn reopening_stopped(&self) -> bool {
        self.should_reopen.is_some_and(|should| !should())
    }
}
pub fn predecessor_alive(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    self_instance: Option<&str>,
) -> bool {
    let entries = match std::fs::read_dir(&paths.generations_dir) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let instance_id = entry.file_name().to_string_lossy().into_owned();
        if Some(instance_id.as_str()) == self_instance {
            continue;
        }
        let generation = crate::host_daemon_paths::generation_paths(paths, &instance_id);
        let raw = crate::host_daemon_state::read_file_or_undefined(&generation.pid_file)
            .ok()
            .flatten();
        if let Some(pid) = crate::host_daemon_state::parse_json(raw.as_deref())
            .and_then(|record| record.get("pid").and_then(Value::as_u64))
            .and_then(|pid| u32::try_from(pid).ok())
            && crate::host_reservations::process_is_live(pid)
        {
            return true;
        }
        if crate::host_stalled_evidence::host_child_alive(&generation, |line| {
            crate::host_supervisor_log::supervisor_log(&line)
        }) == Some(true)
        {
            return true;
        }
    }
    false
}
/// Parks a PREVIOUS generation left for this one: a park this host wrote itself is for its successor.
async fn inherited_parks(
    store: &dyn HandoverParkedStore,
    holder: Option<&ReopenHolder<'_>>,
) -> Vec<HandoverParkedEntry> {
    store
        .read()
        .await
        .into_iter()
        .filter(|entry| {
            entry.parked_by.is_none()
                || entry.parked_by.as_deref() != holder.map(|holder| holder.host_instance.as_str())
        })
        .collect()
}
/// The terminal `task_status` of an entry: its own overlay key wins, else the session context's.
pub fn terminal_task_status(entry: &HandoverParkedEntry) -> Option<&str> {
    let status = match entry
        .context_overlay
        .as_ref()
        .and_then(Value::as_object)
        .filter(|overlay| overlay.contains_key("task_status"))
    {
        Some(overlay) => overlay.get("task_status"),
        None => entry
            .session_context
            .as_ref()
            .and_then(|context| context.get("task_status")),
    }
    .and_then(Value::as_str)?;
    TERMINAL_TASK_STATUSES.contains(&status).then_some(status)
}
pub async fn reopen_handover_parked_sessions(
    store: &dyn HandoverParkedStore,
    open: ParkedSessionOpener<'_>,
    holder: Option<&ReopenHolder<'_>>,
) -> () {
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(HANDOVER_PARKED_REOPEN_BUDGET_MS);
    let mut attempts = HashMap::new();
    let mut held_polls = HashMap::new();
    let mut empty_streak = 0u32;
    let mut saw_any_park = false;
    let mut waited_on_predecessor = false;
    let mut final_pass = false;
    let report = |pending: usize| {
        if let Some(holder) = holder
            && let Some(on_progress) = holder.on_progress
        {
            let predecessor_draining = holder
                .paths
                .as_ref()
                .is_some_and(|paths| predecessor_alive(paths, Some(&holder.host_instance)));
            on_progress(
                (pending > 0 || predecessor_draining).then_some(InheritedHandover {
                    pending,
                    predecessor_draining,
                }),
            );
        }
    };
    let done = || {
        if let Some(holder) = holder
            && let Some(on_progress) = holder.on_progress
        {
            on_progress(None);
        }
    };
    loop {
        // A draining predecessor must not reopen into its own registry a session it just parked for its successor.
        if holder.is_some_and(ReopenHolder::reopening_stopped) {
            done();
            return;
        }
        let parked = inherited_parks(store, holder).await;
        report(parked.len());
        for entry in parked {
            saw_any_park = true;
            reopen_one(store, open, entry, &mut attempts, &mut held_polls, holder).await;
        }
        let remaining = inherited_parks(store, holder).await.len();
        report(remaining);
        if holder.is_some_and(ReopenHolder::reopening_stopped) {
            done();
            return;
        }
        empty_streak = if remaining == 0 { empty_streak + 1 } else { 0 };
        let settled = (saw_any_park && empty_streak >= HANDOVER_PARKED_REOPEN_QUIET_STREAK)
            || tokio::time::Instant::now() >= deadline;
        if !settled {
            tokio::time::sleep(Duration::from_millis(HANDOVER_PARKED_REOPEN_POLL_MS)).await;
            continue;
        }
        if holder
            .and_then(|holder| holder.paths.as_ref())
            .is_some_and(|paths| {
                predecessor_alive(paths, holder.map(|holder| holder.host_instance.as_str()))
            })
        {
            waited_on_predecessor = true;
            tokio::time::sleep(Duration::from_millis(HANDOVER_PARKED_PREDECESSOR_POLL_MS)).await;
            continue;
        }
        if waited_on_predecessor && !final_pass {
            final_pass = true;
            continue;
        }
        done();
        return;
    }
}
async fn reopen_one(
    store: &dyn HandoverParkedStore,
    open: ParkedSessionOpener<'_>,
    entry: HandoverParkedEntry,
    attempts: &mut HashMap<String, u32>,
    held_polls: &mut HashMap<String, u32>,
    holder: Option<&ReopenHolder<'_>>,
) {
    if let Some(status) = terminal_task_status(&entry) {
        store.clear(&entry.session_path).await;
        eprintln!(
            "senpi rpc successor dropped a terminal parked session ({status}): {}",
            entry.session_path
        );
        return;
    }
    store.drop_if_file_gone(&entry.session_path).await;
    if !std::path::Path::new(&entry.session_path).exists() {
        eprintln!(
            "senpi rpc successor dropped a parked session whose file is gone: {}",
            entry.session_path
        );
        return;
    }
    let path = entry.session_path.clone();
    match open(entry).await {
        Ok(()) => {
            // The host started draining while this reopen was in flight: keep the record so the successor still reopens it.
            if holder.is_some_and(ReopenHolder::reopening_stopped) {
                return;
            }
            store.clear(&path).await;
        }
        Err(cause) => {
            // `session_path_in_use` is the predecessor still holding the path's claim: the park is waiting, not failing,
            // so it is retried and NOT counted toward the attempt cap.
            if cause.code.as_deref() == Some("session_path_in_use") {
                let polls = held_polls.entry(path.clone()).or_insert(0);
                *polls += 1;
                if *polls == 1 || (*polls).is_multiple_of(40) {
                    eprintln!(
                        "senpi rpc successor reopen parked session {path} waiting on the holder: {cause}"
                    );
                }
                return;
            }
            let tried = attempts.entry(path.clone()).or_insert(0);
            *tried += 1;
            if *tried >= HANDOVER_PARKED_REOPEN_MAX_ATTEMPTS {
                eprintln!(
                    "senpi rpc successor gave up reopening parked session {path} after {tried} attempts: {cause}"
                );
                store.clear(&path).await;
                return;
            }
            eprintln!("senpi rpc successor reopen parked session {path} failed: {cause}");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    #[derive(Default)]
    struct MemoryStore {
        entries: Mutex<Vec<HandoverParkedEntry>>,
        cleared: Mutex<Vec<String>>,
    }
    impl HandoverParkedStore for MemoryStore {
        fn read(&self) -> BoxFuture<'_, Vec<HandoverParkedEntry>> {
            Box::pin(async {
                self.entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
        }
        fn clear(&self, session_path: &str) -> BoxFuture<'_, ()> {
            let path = session_path.to_owned();
            Box::pin(async move {
                self.entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|entry| entry.session_path != path);
                self.cleared
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(path);
            })
        }
        fn drop_if_file_gone(&self, _: &str) -> BoxFuture<'_, ()> {
            Box::pin(async {})
        }
    }
    fn entry(path: &str) -> HandoverParkedEntry {
        HandoverParkedEntry {
            session_path: path.into(),
            ..Default::default()
        }
    }
    #[test]
    fn overlay_task_status_wins_over_the_session_context() {
        let mut parked = entry("p");
        parked.session_context = Some(serde_json::json!({"task_status":"completed"}));
        assert_eq!(terminal_task_status(&parked), Some("completed"));
        parked.context_overlay = Some(serde_json::json!({"task_status":"running"}));
        assert_eq!(terminal_task_status(&parked), None);
        parked.context_overlay = Some(serde_json::json!({"task_status":"lost"}));
        assert_eq!(terminal_task_status(&parked), Some("lost"));
        parked.context_overlay = Some(serde_json::json!({"other":1}));
        assert_eq!(terminal_task_status(&parked), Some("completed"));
        parked.session_context = Some(serde_json::json!({"task_status":7}));
        assert_eq!(terminal_task_status(&parked), None);
    }
    #[tokio::test]
    async fn a_terminal_parked_session_is_cleared_without_opening() {
        let store = MemoryStore::default();
        let mut parked = entry("/nonexistent/terminal.jsonl");
        parked.session_context = Some(serde_json::json!({"task_status":"failed"}));
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(parked);
        let opened = Arc::new(AtomicUsize::new(0));
        let counter = opened.clone();
        let opener = move |_: HandoverParkedEntry| {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, None).await;
        assert_eq!(opened.load(Ordering::SeqCst), 0);
        assert_eq!(
            store
                .cleared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_slice(),
            ["/nonexistent/terminal.jsonl"]
        );
    }
    #[tokio::test]
    async fn a_host_that_stopped_reopening_returns_before_the_quiet_streak_and_keeps_the_record() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("s.jsonl");
        std::fs::write(&file, "").unwrap();
        let path = file.to_string_lossy().into_owned();
        let store = MemoryStore::default();
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry(&path));
        let draining = Arc::new(AtomicBool::new(false));
        let flag = draining.clone();
        let opener = move |_: HandoverParkedEntry| {
            flag.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        let should = Arc::new(move || !draining.load(Ordering::SeqCst));
        let should_ref = should.clone();
        let check = move || should_ref();
        let holder = ReopenHolder {
            host_instance: "me".into(),
            host_socket: None,
            should_reopen: Some(&check),
            paths: None,
            on_progress: None,
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, Some(&holder)).await;
        assert_eq!(
            store
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        assert!(
            store
                .cleared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
    #[tokio::test]
    async fn a_successful_reopen_clears_the_entry_and_the_quiet_streak_ends_the_watch() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("s.jsonl");
        std::fs::write(&file, "").unwrap();
        let path = file.to_string_lossy().into_owned();
        let store = MemoryStore::default();
        store
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry(&path));
        let opener = |_: HandoverParkedEntry| {
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, None).await;
        assert!(
            store
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
    #[tokio::test]
    async fn reports_pending_parks_and_draining_predecessor_while_taking_over_and_none_when_done() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let socket = agent_dir.join("host.sock").to_string_lossy().into_owned();
        let paths = crate::host_daemon_paths::create_host_daemon_paths(&socket, agent_dir);
        let write_gen = |instance_id: &str, pid: u32| {
            let gen_paths = crate::host_daemon_paths::generation_paths(&paths, instance_id);
            std::fs::create_dir_all(&gen_paths.dir).unwrap();
            std::fs::write(
                &gen_paths.pid_file,
                serde_json::json!({"pid": pid, "instance_id": instance_id}).to_string(),
            )
            .unwrap();
        };
        write_gen("predecessor", std::process::id());
        write_gen("successor", std::process::id());

        let first = agent_dir.join("first.jsonl");
        let second = agent_dir.join("second.jsonl");
        std::fs::write(&first, "").unwrap();
        std::fs::write(&second, "").unwrap();
        let first_path = first.to_string_lossy().into_owned();
        let second_path = second.to_string_lossy().into_owned();

        let store = MemoryStore::default();
        {
            let mut entries = store
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut p1 = entry(&first_path);
            p1.parked_by = Some("predecessor".into());
            let mut p2 = entry(&second_path);
            p2.parked_by = Some("predecessor".into());
            entries.push(p1);
            entries.push(p2);
        }

        let current_progress: Arc<Mutex<Option<InheritedHandover>>> = Arc::default();
        let seen: Arc<Mutex<Vec<Option<InheritedHandover>>>> = Arc::default();
        let progress_for_opener = current_progress.clone();
        let seen_for_opener = seen.clone();
        let paths_for_opener = paths.clone();
        let second_for_opener = second_path.clone();
        let opener = move |entry: HandoverParkedEntry| {
            let snap = *progress_for_opener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            seen_for_opener
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(snap);
            if entry.session_path == second_for_opener {
                let gen_paths =
                    crate::host_daemon_paths::generation_paths(&paths_for_opener, "predecessor");
                std::fs::write(
                    &gen_paths.pid_file,
                    serde_json::json!({"pid": 2_147_483_646_u32, "instance_id": "predecessor"})
                        .to_string(),
                )
                .unwrap();
            }
            Box::pin(async { Ok(()) }) as BoxFuture<'static, Result<(), ReopenError>>
        };
        let progress_for_cb = current_progress.clone();
        let on_progress = move |state: Option<InheritedHandover>| {
            *progress_for_cb
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
        };
        let holder = ReopenHolder {
            host_instance: "successor".into(),
            host_socket: Some(socket),
            should_reopen: None,
            paths: Some(paths),
            on_progress: Some(&on_progress),
        };
        tokio::time::pause();
        reopen_handover_parked_sessions(&store, &opener, Some(&holder)).await;
        seen.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(
                *current_progress
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            );
        let snapshots = seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(
            snapshots.first().copied().flatten(),
            Some(InheritedHandover {
                pending: 2,
                predecessor_draining: true,
            })
        );
        assert_eq!(snapshots.last().copied().flatten(), None);
    }
}
