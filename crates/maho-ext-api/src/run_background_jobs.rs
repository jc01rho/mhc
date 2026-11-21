//! Port of senpi packages/coding-agent/src/core/run-background-jobs.ts (senpi#6675).
//!
//! The background jobs each turn started (a bash command run with `run_in_background`, or one that
//! auto-detached), keyed by the turn id (`turn_epoch`) that was running when the job became background.
//! `interrupt { background: "stop" }` stops the jobs of the turn it interrupted, so nothing that turn left
//! running wakes the session later. A job leaves the registry when it ends on its own (`release`) or once it is
//! stopped.
//!
//! The module lives in this crate rather than in `maho-core` because the extension context contract
//! (`ExtensionContext::register_run_background_job`) names `RunBackgroundJob`, and the core depends on this crate.

use crate::{ExtensionContext, ExtensionFailure, ExtensionFuture};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The kind of background work a job is; senpi has only `bash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RunBackgroundJobKind {
    Bash,
}

/// Ends one job without waking the session and fails with the reason it could not. It takes the id of the
/// interrupted turn.
pub type RunBackgroundJobStop = Arc<dyn Fn(String) -> ExtensionFuture<'static, ()> + Send + Sync>;

/// Drops a recorded job once it ended on its own.
pub type RunBackgroundJobRelease = Box<dyn FnOnce() + Send>;

/// A job the turn started (senpi `RunBackgroundJob`).
#[derive(Clone)]
pub struct RunBackgroundJob {
    pub kind: RunBackgroundJobKind,
    pub id: String,
    pub description: Option<String>,
    pub stop: RunBackgroundJobStop,
}

/// A job as `interrupt { background: "stop" }` names it (senpi `StoppedRunBackgroundJob`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoppedRunBackgroundJob {
    pub kind: RunBackgroundJobKind,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A job that could not be stopped, with the reason its own stop failed with (senpi `FailedRunBackgroundJob`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FailedRunBackgroundJob {
    #[serde(flatten)]
    pub job: StoppedRunBackgroundJob,
    pub reason: String,
}

/// What `interrupt { background: "stop" }` stopped, and each job it could not stop (senpi `RunBackgroundStopOutcome`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RunBackgroundStopOutcome {
    pub stopped: Vec<StoppedRunBackgroundJob>,
    pub failed: Vec<FailedRunBackgroundJob>,
}

#[derive(Default)]
struct Registered {
    next_id: u64,
    by_turn: BTreeMap<String, Vec<(u64, RunBackgroundJob)>>,
}

fn lock(registered: &Mutex<Registered>) -> MutexGuard<'_, Registered> {
    registered.lock().unwrap_or_else(PoisonError::into_inner)
}

fn drop_job(registered: &Mutex<Registered>, turn_id: &str, id: u64) {
    let mut registered = lock(registered);
    let Some(jobs) = registered.by_turn.get_mut(turn_id) else {
        return;
    };
    jobs.retain(|(job_id, _)| *job_id != id);
    if jobs.is_empty() {
        registered.by_turn.remove(turn_id);
    }
}

/// The background jobs each turn started (senpi `RunBackgroundJobs`).
#[derive(Clone, Default)]
pub struct RunBackgroundJobs {
    registered: Arc<Mutex<Registered>>,
}

impl RunBackgroundJobs {
    /// Records `job` under `turn_id`; the returned release drops it once it ended on its own.
    pub fn register(&self, turn_id: &str, job: RunBackgroundJob) -> RunBackgroundJobRelease {
        let id = {
            let mut registered = lock(&self.registered);
            registered.next_id += 1;
            let id = registered.next_id;
            registered
                .by_turn
                .entry(turn_id.to_owned())
                .or_default()
                .push((id, job));
            id
        };
        let registered = Arc::clone(&self.registered);
        let turn_id = turn_id.to_owned();
        Box::new(move || drop_job(&registered, &turn_id, id))
    }

    /// Stops the jobs `turn_id` started, in start order, and forgets them. A job whose stop fails is named with the
    /// reason and does not keep the others from stopping.
    pub async fn stop(&self, turn_id: &str) -> RunBackgroundStopOutcome {
        let jobs = lock(&self.registered)
            .by_turn
            .remove(turn_id)
            .unwrap_or_default();
        let mut outcome = RunBackgroundStopOutcome::default();
        for (_, job) in jobs {
            let named = StoppedRunBackgroundJob {
                kind: job.kind,
                id: job.id.clone(),
                description: job.description.clone(),
            };
            match (job.stop)(turn_id.to_owned()).await {
                Ok(()) => outcome.stopped.push(named),
                Err(failure) => outcome.failed.push(FailedRunBackgroundJob {
                    job: named,
                    reason: failure.message,
                }),
            }
        }
        outcome
    }
}

impl ExtensionContext {
    /// senpi `ctx.registerRunBackgroundJob`: records a background job under the turn running at that moment, so
    /// `interrupt { background: "stop" }` on that turn stops it. Outside a turn, and on a context no session backs,
    /// nothing is recorded. The returned release drops the job once it ended on its own.
    pub fn register_run_background_job(
        &self,
        job: RunBackgroundJob,
    ) -> Result<RunBackgroundJobRelease, ExtensionFailure> {
        match self.session_manager.extension_context_actions() {
            Some(actions) => {
                actions.assert_active()?;
                Ok(actions.register_run_background_job(job))
            }
            None => Ok(Box::new(|| {})),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    fn boxed(
        future: impl std::future::Future<Output = Result<(), ExtensionFailure>> + Send + 'static,
    ) -> ExtensionFuture<'static, ()> {
        Box::pin(future)
    }

    fn job(
        id: &str,
        log: &Arc<Mutex<Vec<String>>>,
        outcome: Result<(), &'static str>,
    ) -> RunBackgroundJob {
        let log = Arc::clone(log);
        let job_id = id.to_owned();
        RunBackgroundJob {
            kind: RunBackgroundJobKind::Bash,
            id: id.to_owned(),
            description: Some(format!("{id} description")),
            stop: Arc::new(move |turn_id: String| {
                let log = Arc::clone(&log);
                let job_id = job_id.clone();
                boxed(async move {
                    log.lock().unwrap().push(format!("{job_id}@{turn_id}"));
                    outcome.map_err(ExtensionFailure::new)
                })
            }),
        }
    }

    #[test]
    fn stops_only_the_named_turns_jobs_in_start_order() {
        let jobs = RunBackgroundJobs::default();
        let log = Arc::default();
        let _first = jobs.register("1", job("a", &log, Ok(())));
        let _other = jobs.register("2", job("other", &log, Ok(())));
        let _second = jobs.register("1", job("b", &log, Ok(())));

        let outcome = block_on(jobs.stop("1"));

        assert_eq!(*log.lock().unwrap(), ["a@1", "b@1"]);
        assert_eq!(
            outcome
                .stopped
                .iter()
                .map(|job| job.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            outcome.stopped[0].description.as_deref(),
            Some("a description")
        );
        assert!(outcome.failed.is_empty());
        assert_eq!(
            block_on(jobs.stop("1")),
            RunBackgroundStopOutcome::default()
        );
        assert_eq!(block_on(jobs.stop("2")).stopped.len(), 1);
    }

    #[test]
    fn a_job_that_cannot_stop_is_named_with_its_reason_and_the_rest_still_stop() {
        let jobs = RunBackgroundJobs::default();
        let log = Arc::default();
        let _stuck = jobs.register("7", job("a", &log, Err("still running after the stop")));
        let _fine = jobs.register("7", job("b", &log, Ok(())));

        let outcome = block_on(jobs.stop("7"));

        assert_eq!(*log.lock().unwrap(), ["a@7", "b@7"]);
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            json!({
                "stopped": [{"kind": "bash", "id": "b", "description": "b description"}],
                "failed": [{
                    "kind": "bash",
                    "id": "a",
                    "description": "a description",
                    "reason": "still running after the stop"
                }]
            })
        );
    }

    #[test]
    fn a_released_job_is_no_longer_stopped() {
        let jobs = RunBackgroundJobs::default();
        let log = Arc::default();
        let release = jobs.register("3", job("ended", &log, Ok(())));
        let _live = jobs.register("3", job("live", &log, Ok(())));

        release();
        let outcome = block_on(jobs.stop("3"));

        assert_eq!(*log.lock().unwrap(), ["live@3"]);
        assert_eq!(outcome.stopped.len(), 1);
    }

    #[test]
    fn a_job_without_a_description_is_named_without_the_field() {
        let named = StoppedRunBackgroundJob {
            kind: RunBackgroundJobKind::Bash,
            id: "bash_1".into(),
            description: None,
        };
        assert_eq!(
            serde_json::to_value(&named).unwrap(),
            json!({"kind": "bash", "id": "bash_1"})
        );
    }
}
