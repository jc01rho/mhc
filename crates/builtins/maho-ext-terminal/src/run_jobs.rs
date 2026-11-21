//! Background sessions as `interrupt { background: "stop" }` sees them (senpi#6675).
//!
//! Port of senpi `terminal/extension-state.ts` `recordRunJob` and the run-job parts of `terminal/session-bundle.ts`
//! (`trackRunJob`, `stopForInterrupt`, `#interruptStops`). The tracking lives in the notifier: the exit of every
//! background session already passes through it, and the session shutdown resets it.

use crate::manager::TerminalManager;
use crate::notify::TerminalNotifier;
use maho_ext_api::{
    ExtensionContext, ExtensionFailure, ExtensionFuture, RunBackgroundJob, RunBackgroundJobKind,
    RunBackgroundJobRelease, RunBackgroundJobStop,
};
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn boxed(
    future: impl Future<Output = Result<(), ExtensionFailure>> + Send + 'static,
) -> ExtensionFuture<'static, ()> {
    Box::pin(future)
}

/// The run jobs the background sessions were recorded as, and the sessions an interrupt stopped.
#[derive(Default)]
pub struct RunJobs {
    releases: BTreeMap<String, RunBackgroundJobRelease>,
    interrupt_stops: BTreeMap<String, String>,
}

impl RunJobs {
    /// senpi `trackRunJob`: holds the release of the run job session `id` was recorded as.
    pub fn track(&mut self, id: &str, release: RunBackgroundJobRelease) {
        if let Some(previous) = self.releases.insert(id.to_owned(), release) {
            previous();
        }
    }

    /// An interrupt on `turn_id` is about to stop session `id`.
    pub fn note_interrupt_stop(&mut self, id: &str, turn_id: &str) {
        self.interrupt_stops
            .insert(id.to_owned(), turn_id.to_owned());
    }

    /// The stop of session `id` did not end it, so its exit is not an interrupt's.
    pub fn forget_interrupt_stop(&mut self, id: &str) {
        self.interrupt_stops.remove(id);
    }

    /// Session `id` ended: drops its run job and answers the turn whose interrupt stopped it, if one did.
    pub fn finish(&mut self, id: &str) -> Option<String> {
        if let Some(release) = self.releases.remove(id) {
            release();
        }
        self.interrupt_stops.remove(id)
    }

    /// senpi `teardown`: releases every run job.
    pub fn release_all(&mut self) {
        for release in std::mem::take(&mut self.releases).into_values() {
            release();
        }
        self.interrupt_stops.clear();
    }
}

/// senpi `recordRunJob`: records the background session `id` as a run job of the turn running now. A context that
/// is not bound yet, or no longer active, records nothing.
pub fn record_run_job(
    ctx: &Mutex<Option<ExtensionContext>>,
    notifier: &Mutex<TerminalNotifier>,
    id: &str,
    description: &str,
    stop: RunBackgroundJobStop,
) {
    let job = RunBackgroundJob {
        kind: RunBackgroundJobKind::Bash,
        id: id.to_owned(),
        description: Some(description.to_owned()),
        stop,
    };
    let registered = lock(ctx)
        .as_ref()
        .map(|ctx| ctx.register_run_background_job(job));
    if let Some(Ok(release)) = registered {
        lock(notifier).run_jobs.track(id, release);
    }
}

/// senpi `stopForInterrupt` for a session this process runs: tree-stops it (TERM, then KILL) with the turn noted, so
/// its exit notice reaches the next turn without waking the session.
pub fn in_process_stop(
    manager: Arc<Mutex<TerminalManager>>,
    notifier: Arc<Mutex<TerminalNotifier>>,
    id: String,
) -> RunBackgroundJobStop {
    Arc::new(move |turn_id: String| {
        let (manager, notifier, id) = (Arc::clone(&manager), Arc::clone(&notifier), id.clone());
        boxed(async move { stop_for_interrupt(&manager, &notifier, &id, &turn_id).await })
    })
}

/// senpi `stopForInterrupt` for a session that lives on the tool child: this stop does not reach it, and says so.
pub fn child_stop(id: &str) -> RunBackgroundJobStop {
    let message = format!("{id} runs on the session's tool child, which this stop does not reach");
    Arc::new(move |_turn_id: String| {
        let message = message.clone();
        boxed(async move { Err(ExtensionFailure::new(message)) })
    })
}

async fn stop_for_interrupt(
    manager: &Arc<Mutex<TerminalManager>>,
    notifier: &Mutex<TerminalNotifier>,
    id: &str,
    turn_id: &str,
) -> Result<(), ExtensionFailure> {
    if lock(manager).get(id).is_none() {
        return Err(ExtensionFailure::new(format!(
            "{id} runs on the session's tool child, which this stop does not reach"
        )));
    }
    lock(notifier).run_jobs.note_interrupt_stop(id, turn_id);
    let stopped = crate::manager::stop_shared(manager, id).await;
    let exited = lock(manager)
        .get(id)
        .is_none_or(|runtime| runtime.exited().unwrap_or(false));
    match stopped {
        Ok(true) if exited => Ok(()),
        Ok(_) => {
            lock(notifier).run_jobs.forget_interrupt_stop(id);
            Err(ExtensionFailure::new(format!(
                "{id} was still running after the stop (TERM, then KILL)"
            )))
        }
        Err(error) => {
            lock(notifier).run_jobs.forget_interrupt_stop(id);
            Err(ExtensionFailure::new(error.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn counted_release(count: &Arc<AtomicUsize>) -> RunBackgroundJobRelease {
        let count = Arc::clone(count);
        Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })
    }

    #[test]
    fn finishing_a_session_releases_its_run_job_once_and_answers_the_interrupting_turn() {
        let released = Arc::new(AtomicUsize::new(0));
        let mut jobs = RunJobs::default();
        jobs.track("bash_1", counted_release(&released));
        jobs.note_interrupt_stop("bash_1", "7");
        assert_eq!(jobs.finish("bash_1"), Some("7".to_owned()));
        assert_eq!(released.load(Ordering::SeqCst), 1);
        assert_eq!(jobs.finish("bash_1"), None);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_session_that_ended_on_its_own_answers_no_turn() {
        let released = Arc::new(AtomicUsize::new(0));
        let mut jobs = RunJobs::default();
        jobs.track("bash_2", counted_release(&released));
        assert_eq!(jobs.finish("bash_2"), None);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_stop_is_forgotten_and_a_retracked_job_releases_the_previous_one() {
        let released = Arc::new(AtomicUsize::new(0));
        let mut jobs = RunJobs::default();
        jobs.note_interrupt_stop("bash_3", "7");
        jobs.forget_interrupt_stop("bash_3");
        assert_eq!(jobs.finish("bash_3"), None);
        jobs.track("bash_3", counted_release(&released));
        jobs.track("bash_3", counted_release(&released));
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn teardown_releases_every_run_job_and_forgets_the_stops() {
        let released = Arc::new(AtomicUsize::new(0));
        let mut jobs = RunJobs::default();
        jobs.track("bash_1", counted_release(&released));
        jobs.track("bash_2", counted_release(&released));
        jobs.note_interrupt_stop("bash_1", "7");
        jobs.release_all();
        assert_eq!(released.load(Ordering::SeqCst), 2);
        assert_eq!(jobs.finish("bash_1"), None);
    }

    #[tokio::test]
    async fn a_session_on_the_tool_child_fails_the_stop_with_a_typed_reason() {
        let stop = child_stop("bash_9");
        let failure = stop("7".to_owned()).await.unwrap_err();
        assert_eq!(
            failure.message,
            "bash_9 runs on the session's tool child, which this stop does not reach"
        );
    }

    #[tokio::test]
    async fn an_unknown_session_fails_the_stop_and_notes_no_turn() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let notifier = Arc::new(Mutex::new(TerminalNotifier::default()));
        let failure = stop_for_interrupt(&manager, &notifier, "bash_404", "7")
            .await
            .unwrap_err();
        assert!(
            failure.message.contains("tool child"),
            "{}",
            failure.message
        );
        assert_eq!(lock(&notifier).run_jobs.finish("bash_404"), None);
    }

    #[tokio::test]
    async fn an_interrupt_stop_ends_the_session_and_remembers_the_turn_until_its_exit_is_reported()
    {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let notifier = Arc::new(Mutex::new(TerminalNotifier::default()));
        let id = lock(&manager)
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap();

        let stop = in_process_stop(Arc::clone(&manager), Arc::clone(&notifier), id.clone());
        stop("7".to_owned()).await.unwrap();

        assert!(lock(&manager).get(&id).unwrap().exited().unwrap());
        assert_eq!(lock(&notifier).run_jobs.finish(&id), Some("7".to_owned()));
        assert_eq!(lock(&notifier).run_jobs.finish(&id), None);
        crate::manager::teardown_shared(&manager).await.unwrap();
    }
}
