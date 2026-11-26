//! `runners/rpc-process.ts`: spawn a senpi RPC child (never through a shell) with an isolated
//! session dir and return a steerable handle. Process destruction goes only through
//! `rpc/terminate.rs`.

use regex::Regex;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use serde_json::json;

use crate::runners::rpc::handle::{Clock, RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::model_admission::{
    RpcModelAdmission, RpcModelAdmissionOptions, create_rpc_model_admission,
};
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc::protocol_client::{
    MalformedLineHandler, RpcClientPort, RpcProtocolClient, RpcProtocolClientOptions,
};
use crate::runners::rpc::spawn::{RpcSpawnRuntime, build_rpc_spawn};
use crate::runners::types::{ChildExitOutcome, RpcRunnerSpec, RpcSpawnSpec};
use crate::runners::{RunnerFailure, RunnerFailureKind};

const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 10_000;

// #13165: the engine's cold start (spawn until it answers `get_protocol_info`) is bounded by its CPU
// progress, not by the chain deadline: a slow runner stretches the boot's wall time, not the CPU it
// needs. A boot that makes no CPU progress for the stall window, or burns past the CPU cap, is
// stuck; the outer bound holds even when the CPU cannot be read (another runtime, a refused handle).
const DEFAULT_STARTUP_STALL_MS: u64 = 10_000;
const DEFAULT_STARTUP_CPU_CAP_S: f64 = 120.0;
const DEFAULT_STARTUP_OUTER_BOUND_MS: u64 = 10 * 60_000;
const STARTUP_SAMPLE_MS: u64 = 250;
/// The bound on a `set_retry_fallback` exchange that keeps making CPU progress.
const CHAIN_EXCHANGE_OUTER_CAP_MS: u64 = 120_000;
const STARTUP_PROGRESS_EPSILON_S: f64 = 0.02;

pub type SpawnChild = Arc<dyn Fn(&RpcSpawnDescriptor) -> Arc<RpcChildProcess> + Send + Sync>;
pub type BuildSpawn = Arc<dyn Fn(&RpcRunnerSpec) -> RpcSpawnDescriptor + Send + Sync>;
/// Reads a process's own user+system CPU seconds, or answers `None` when the fact cannot be read.
pub type ReadCpuSeconds = Arc<dyn Fn(u32) -> Option<f64> + Send + Sync>;

#[derive(Clone, Default)]
pub struct RpcProcessRunnerOptions {
    pub fallback_chain_deadline_ms: Option<u64>,
    /// How long the engine may make no CPU progress while its first answer is pending (#13165).
    pub startup_stall_ms: Option<u64>,
    /// The CPU the engine may use starting up before it answers, in seconds (#13165).
    pub startup_cpu_cap_s: Option<f64>,
    /// The bound that holds even when the child's CPU cannot be read (#13165).
    pub startup_outer_bound_ms: Option<u64>,
    /// The injection seam for the CPU reader; defaults to `platform::native_process`.
    pub read_cpu_seconds: Option<ReadCpuSeconds>,
    pub exclude_models: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub spawn_child: Option<SpawnChild>,
    pub build_spawn: Option<BuildSpawn>,
    pub heartbeat_interval_ms: Option<u64>,
    pub on_malformed_line: Option<MalformedLineHandler>,
    pub now: Option<Clock>,
    pub model_admission: Option<RpcModelAdmission>,
    pub inherited_extensions: Vec<String>,
    /// `task.parent_model_fallback: false` (omo #12006, #12759): a child whose engine cannot hold its
    /// designated chain is never started. `false` keeps the manager-fallback behaviour.
    pub designated_chain_only: bool,
}

type StartRefusal = (
    RunnerFailureKind,
    String,
    Option<crate::manager::start_failure_cause::StartCause>,
);

pub struct RpcProcessRunner {
    fallback_chain_deadline_ms: u64,
    startup_bounds: StartupBounds,
    unsupported_noticed: std::sync::atomic::AtomicBool,
    exclude_models: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pending_teardowns: Mutex<Vec<JoinHandle<()>>>,
    spawn_child: SpawnChild,
    build_spawn: BuildSpawn,
    heartbeat_interval_ms: u64,
    on_malformed_line: Option<MalformedLineHandler>,
    now: Clock,
    model_admission: RpcModelAdmission,
    inherited_extensions: Vec<String>,
    designated_chain_only: bool,
}

impl RpcProcessRunner {
    pub fn new(options: RpcProcessRunnerOptions) -> Self {
        Self {
            fallback_chain_deadline_ms: options.fallback_chain_deadline_ms.unwrap_or(10_000),
            startup_bounds: StartupBounds {
                stall: std::time::Duration::from_millis(
                    options.startup_stall_ms.unwrap_or(DEFAULT_STARTUP_STALL_MS),
                ),
                cpu_cap_s: options
                    .startup_cpu_cap_s
                    .unwrap_or(DEFAULT_STARTUP_CPU_CAP_S),
                outer_bound: std::time::Duration::from_millis(
                    options
                        .startup_outer_bound_ms
                        .unwrap_or(DEFAULT_STARTUP_OUTER_BOUND_MS),
                ),
                read_cpu_seconds: options.read_cpu_seconds.unwrap_or_else(|| {
                    Arc::new(crate::platform::native_process::read_process_cpu_seconds)
                }),
            },
            unsupported_noticed: std::sync::atomic::AtomicBool::new(false),
            exclude_models: options.exclude_models,
            pending_teardowns: Mutex::new(Vec::new()),
            spawn_child: options.spawn_child.unwrap_or_else(|| {
                Arc::new(|descriptor| Arc::new(RpcChildProcess::spawn(descriptor)))
            }),
            build_spawn: options.build_spawn.unwrap_or_else(|| {
                let runtime = RpcSpawnRuntime::default();
                Arc::new(move |spec| build_rpc_spawn(spec, &runtime))
            }),
            heartbeat_interval_ms: options
                .heartbeat_interval_ms
                .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS),
            on_malformed_line: options.on_malformed_line,
            now: options
                .now
                .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis())),
            model_admission: options
                .model_admission
                .unwrap_or_else(|| create_rpc_model_admission(RpcModelAdmissionOptions::default())),
            inherited_extensions: options.inherited_extensions,
            designated_chain_only: options.designated_chain_only,
        }
    }

    pub fn settle_teardowns(&self) {
        let pending = std::mem::take(
            &mut *self
                .pending_teardowns
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for teardown in pending {
            if let Err(error) = teardown.join() {
                utils::logger::log(
                    "senpi-task rpc start cleanup panicked",
                    Some(&json!({"error":format!("{error:?}")})),
                );
            }
        }
    }

    /// Admit the model, spawn, then either drive the initial prompt or switch to the resume session.
    /// Failed non-resume starts discard their child in a tracked background teardown.
    pub fn start(&self, spec_input: &RpcRunnerSpec) -> Result<Arc<RpcChildHandle>, RunnerFailure> {
        let mut spec = spec_input.clone();
        if let Some(resolve) = &self.exclude_models
            && let Some(chain) = &spec.fallback_models
        {
            spec.fallback_models = Some(crate::manager::runner::without_excluded_selectors(
                chain,
                &resolve(),
            ));
        }
        if spec.extensions.is_none() && !self.inherited_extensions.is_empty() {
            spec.extensions = Some(self.inherited_extensions.clone());
        }
        (self.model_admission)(&spec)?;
        let descriptor = (self.build_spawn)(&spec);
        let child = (self.spawn_child)(&descriptor);
        let protocol = Arc::new(RpcProtocolClient::new(
            child,
            RpcProtocolClientOptions {
                on_malformed_line: self.on_malformed_line.clone(),
                auto_answer_ui: None,
            },
        ));
        let client: Arc<dyn RpcClientPort> = protocol.clone();
        let handle = RpcChildHandle::new(RpcChildHandleOptions {
            client: Arc::clone(&client),
            task_id: spec.task_id.clone(),
            heartbeat_interval_ms: self.heartbeat_interval_ms,
            now: Arc::clone(&self.now),
            input_store: spec.task_store_dir.as_ref().map(|dir| {
                crate::store::TaskRecordStore::new(&crate::store::StateDirConfig {
                    project_dir: std::path::PathBuf::from(&spec.cwd),
                    task_state_dir: Some(std::path::PathBuf::from(dir)),
                })
            }),
        });
        let chain_refusal = if self.designated_chain_only {
            spec.model
                .as_deref()
                .and_then(|model| self.hold_designated_chain(&protocol, &handle, &spec, model))
        } else {
            if let (Some(model), Some(chain)) = (&spec.model, &spec.fallback_models)
                && !chain.is_empty()
            {
                // #13165: only the engine's first answer is bounded by its CPU progress. Once it
                // answered, the chain deadline covers only the `set_retry_fallback` exchange.
                match with_startup_progress(
                    protocol.request(json!({"type":"get_protocol_info"})),
                    protocol.pid(),
                    &self.startup_bounds,
                ) {
                    Ok(Ok(response)) => {
                        let supported = response["success"] == true
                            && response["command"] == "get_protocol_info"
                            && response["data"]["capabilities"]
                                .as_array()
                                .is_some_and(|items| {
                                    items.iter().any(|item| item == "retry_fallback_command")
                                });
                        if !supported {
                            if !self
                                .unsupported_noticed
                                .swap(true, std::sync::atomic::Ordering::AcqRel)
                            {
                                utils::logger::log(
                                    "senpi-task engine lacks retry_fallback_command; retaining manager fallback",
                                    None,
                                );
                            }
                        } else {
                            let unavailable = match self.exchange_chain(
                                &protocol,
                                json!({"type":"set_retry_fallback","retryFallback":{"modelFallback":true,"fallbackChains":{(model):chain}}}),
                            ) {
                                Some(Ok(_)) => None,
                                Some(Err(error)) => Some(error.to_string()),
                                None => Some(self.chain_silence()),
                            };
                            if let Some(error) = unavailable {
                                utils::logger::log(
                                    "senpi-task child fallback configuration unavailable; retaining manager fallback",
                                    Some(&json!({"taskId":spec.task_id,"error":error})),
                                );
                            }
                        }
                    }
                    Ok(Err(error)) => utils::logger::log(
                        "senpi-task child fallback configuration unavailable; retaining manager fallback",
                        Some(&json!({"taskId":spec.task_id,"error":error.to_string()})),
                    ),
                    Err(startup) => utils::logger::log(
                        "senpi-task child fallback configuration unavailable; retaining manager fallback",
                        Some(&json!({"taskId":spec.task_id,"error":startup})),
                    ),
                }
            }
            None
        };
        let started = match chain_refusal {
            Some(refusal) => Err(refusal),
            None => match &spec.resume_session_path {
                None => handle
                    .start_initial_prompt(&spec.prompt)
                    .map(|()| None)
                    .map_err(|error| {
                        (
                            RunnerFailureKind::ChildPromptFailed,
                            error.to_string(),
                            crate::manager::start_failure_cause::classify_start_cause(&error),
                        )
                    }),
                Some(path) => client
                    .switch_session(path)
                    .map(|result| Some((path.clone(), result)))
                    .map_err(|error| {
                        (
                            RunnerFailureKind::SessionUnavailable,
                            error.to_string(),
                            crate::manager::start_failure_cause::classify_start_cause(&error),
                        )
                    }),
            },
        };
        let resume = match started {
            Ok(resume) => resume,
            Err((kind, message, cause)) => {
                let unresolved = match handle.exit_outcome() {
                    Some(ChildExitOutcome::Crashed { facts }) => {
                        child_unresolved_model_error(&facts.stderr_tail)
                    }
                    _ => None,
                };
                let task_id = spec.task_id.clone();
                let cleanup = move || {
                    if let Err(error) =
                        crate::runners::rpc::start_cleanup::discard_unstarted_rpc_handle(handle)
                    {
                        utils::logger::log(
                            "senpi-task rpc start cleanup failed",
                            Some(&json!({"taskId":task_id,"error":error.to_string()})),
                        );
                    }
                };
                if spec.resume_session_path.is_none() {
                    let mut pending = self
                        .pending_teardowns
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    // Retire completed workers without losing panic diagnostics.
                    let mut index = 0;
                    while index < pending.len() {
                        if pending[index].is_finished() {
                            if let Err(error) = pending.swap_remove(index).join() {
                                utils::logger::log(
                                    "senpi-task rpc start cleanup panicked",
                                    Some(&json!({"error":format!("{error:?}")})),
                                );
                            }
                        } else {
                            index += 1;
                        }
                    }
                    pending.push(std::thread::spawn(cleanup));
                } else {
                    cleanup();
                }
                return Err(match unresolved {
                    Some(detail) => RunnerFailure::new(
                        RunnerFailureKind::ModelUnavailable,
                        format!("the child could not resolve its model: {detail}"),
                    ),
                    None => RunnerFailure::new(kind, message).with_cause(cause),
                });
            }
        };
        handle.attach_start_facts(
            RpcSpawnSpec {
                cwd: spec.cwd.clone(),
                extensions: spec.extensions.clone(),
                member_env: spec.member_env.clone(),
            },
            resume,
        );
        Ok(handle)
    }

    /// `applyFallbackChain` under the designated-chain-only policy: the child's own chain (with
    /// tombstones) goes out as `set_retry_fallback` before its first prompt or session switch, and a
    /// chain the engine does not hold fails the start, so no model outside the chain ever runs.
    ///
    /// The wait for the engine's FIRST answer is bounded by its CPU progress, not by the chain
    /// deadline (#13165): `fallback_chain_deadline_ms` covers only `set_retry_fallback` once the
    /// engine has answered.
    fn hold_designated_chain(
        &self,
        protocol: &RpcProtocolClient,
        handle: &RpcChildHandle,
        spec: &RpcRunnerSpec,
        model: &str,
    ) -> Option<StartRefusal> {
        use crate::runners::designated_chain_error::DesignatedChainNotHeldReason as Reason;
        let fallbacks = spec.fallback_models.clone().unwrap_or_default();
        let profile = crate::runners::designated_fallback_profile::designated_fallback_profile(
            model, &fallbacks,
        );
        let probe = match with_startup_progress(
            protocol.request(json!({"type":"get_protocol_info"})),
            protocol.pid(),
            &self.startup_bounds,
        ) {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Some(unanswered_refusal(
                    Reason::Refused,
                    format!("refused: {error}"),
                    handle,
                    spec,
                    model,
                    &fallbacks,
                ));
            }
            Err(startup) => {
                return Some(unanswered_refusal(
                    Reason::Timeout,
                    startup,
                    handle,
                    spec,
                    model,
                    &fallbacks,
                ));
            }
        };
        let supported = probe["success"] == true
            && probe["command"] == "get_protocol_info"
            && probe["data"]["capabilities"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == "retry_fallback_command"));
        if !supported {
            return Some(unanswered_refusal(
                Reason::Unsupported,
                "this senpi engine cannot take a task child's model chain (no retry_fallback_command); upgrade it".to_string(),
                handle,
                spec,
                model,
                &fallbacks,
            ));
        }
        let (reason, detail) = match self.exchange_chain(
            protocol,
            json!({"type":"set_retry_fallback","retryFallback":profile}),
        ) {
            Some(Ok(response)) if response["success"] == true => return None,
            Some(Ok(response)) => (Reason::Refused, format!("refused: {}", response["error"])),
            Some(Err(error)) => (Reason::Refused, format!("refused: {error}")),
            None => (Reason::Timeout, self.chain_silence()),
        };
        Some(unanswered_refusal(
            reason, detail, handle, spec, model, &fallbacks,
        ))
    }

    /// Sends a `set_retry_fallback` command and waits for its answer. `fallback_chain_deadline_ms`
    /// is how long the engine may stay silent without its CPU advancing; a busy engine on a slow box
    /// keeps the wait going up to [`CHAIN_EXCHANGE_OUTER_CAP_MS`]. `None` means the engine stalled.
    fn exchange_chain(
        &self,
        protocol: &RpcProtocolClient,
        command: serde_json::Value,
    ) -> Option<Result<serde_json::Value, crate::runners::rpc::protocol_client::RpcClientError>>
    {
        let pending = match protocol.request(command) {
            Ok(pending) => pending,
            Err(error) => return Some(Err(error)),
        };
        let pid = protocol.pid();
        let window = std::time::Duration::from_millis(self.fallback_chain_deadline_ms);
        let cap = window.max(std::time::Duration::from_millis(
            CHAIN_EXCHANGE_OUTER_CAP_MS,
        ));
        let sample = std::time::Duration::from_millis(STARTUP_SAMPLE_MS);
        let started = std::time::Instant::now();
        let mut last_cpu = -1.0_f64;
        let mut last_advance = started;
        loop {
            let waited = started.elapsed();
            let silent = last_advance.elapsed();
            if waited >= cap || silent >= window {
                return None;
            }
            let step = sample.min(cap - waited).min(window - silent);
            if let Some(answer) = pending.try_wait_timeout(step) {
                return Some(answer);
            }
            if let Some(pid) = pid
                && let Some(cpu) = (self.startup_bounds.read_cpu_seconds)(pid)
                && cpu >= last_cpu + STARTUP_PROGRESS_EPSILON_S
            {
                last_cpu = cpu;
                last_advance = std::time::Instant::now();
            }
        }
    }

    fn chain_silence(&self) -> String {
        format!(
            "no answer within {} ms without CPU progress",
            self.fallback_chain_deadline_ms
        )
    }
}

#[derive(Clone)]
struct StartupBounds {
    stall: std::time::Duration,
    cpu_cap_s: f64,
    outer_bound: std::time::Duration,
    read_cpu_seconds: ReadCpuSeconds,
}

fn with_startup_progress(
    request: Result<
        crate::runners::rpc::protocol_client::PendingResponse,
        crate::runners::rpc::protocol_client::RpcClientError,
    >,
    pid: Option<u32>,
    bounds: &StartupBounds,
) -> Result<Result<serde_json::Value, crate::runners::rpc::protocol_client::RpcClientError>, String>
{
    let pending = match request {
        Ok(pending) => pending,
        Err(error) => return Ok(Err(error)),
    };
    let started = std::time::Instant::now();
    let sample = std::time::Duration::from_millis(STARTUP_SAMPLE_MS);
    let mut last_cpu = -1.0_f64;
    let mut last_advance = started;
    loop {
        let elapsed = started.elapsed();
        if elapsed >= bounds.outer_bound {
            return Err(format!(
                "no answer within the {} ms startup outer bound",
                bounds.outer_bound.as_millis()
            ));
        }
        let remaining_outer = bounds.outer_bound.saturating_sub(elapsed);
        let step = if pid.is_some() {
            sample.min(remaining_outer)
        } else {
            remaining_outer
        };
        if let Some(answer) = pending.try_wait_timeout(step) {
            return Ok(answer);
        }
        if started.elapsed() >= bounds.outer_bound {
            return Err(format!(
                "no answer within the {} ms startup outer bound",
                bounds.outer_bound.as_millis()
            ));
        }
        if let Some(pid) = pid
            && let Some(cpu) = (bounds.read_cpu_seconds)(pid)
        {
            if cpu > bounds.cpu_cap_s {
                return Err(format!(
                    "the engine used {cpu:.1} s of CPU starting up without answering, over the {} s cap",
                    bounds.cpu_cap_s
                ));
            }
            if cpu >= last_cpu + STARTUP_PROGRESS_EPSILON_S {
                last_cpu = cpu;
                last_advance = std::time::Instant::now();
            } else if last_advance.elapsed() >= bounds.stall {
                return Err(format!(
                    "the engine made no CPU progress for {} s before answering",
                    last_advance.elapsed().as_secs_f64().round() as u64
                ));
            }
        }
    }
}

fn unanswered_refusal(
    reason: crate::runners::designated_chain_error::DesignatedChainNotHeldReason,
    detail: String,
    handle: &RpcChildHandle,
    spec: &RpcRunnerSpec,
    model: &str,
    fallbacks: &[String],
) -> StartRefusal {
    let chain =
        crate::runners::designated_fallback_profile::designated_chain_text(model, fallbacks);
    if handle.exit_outcome().is_some() {
        return (
            RunnerFailureKind::ChildPromptFailed,
            format!(
                "task {}: its designated model chain ({chain}) was not applied because the child exited while it was being sent",
                spec.task_id
            ),
            None,
        );
    }
    let models: Vec<String> = std::iter::once(model.to_string())
        .chain(fallbacks.iter().cloned())
        .collect();
    (
        RunnerFailureKind::ChildPromptFailed,
        format!(
            "task {}: its designated model chain ({chain}) was not applied ({detail}), and a child never runs outside its designated models, so it was not started",
            spec.task_id
        ),
        Some(crate::runners::designated_chain_error::chain_not_held_cause(reason, &models)),
    )
}

static ANSI_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1B\[[0-?]*[ -/]*[@-~]").expect("ANSI escape regex"));
static UNRESOLVED_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Model "[^"\n]+" not found\. Use --list-models to see available models\.|Unknown provider "[^"\n]+"\. Use --list-models to see available providers/models\."#).expect("model error regex")
});
pub fn child_unresolved_model_error(stderr_tail: &str) -> Option<String> {
    let clean = ANSI_ESCAPE.replace_all(stderr_tail, "");
    UNRESOLVED_MODEL
        .find(&clean)
        .map(|found| found.as_str().to_owned())
}
#[cfg(test)]
mod model_error_tests {
    use super::*;
    #[test]
    fn recognizes_only_engine_model_diagnostics() {
        for diagnostic in [
            r#"Model "fixture/missing" not found. Use --list-models to see available models."#,
            r#"Unknown provider "missing". Use --list-models to see available providers/models."#,
        ] {
            assert_eq!(
                child_unresolved_model_error(&format!("noise\n\x1b[31m{diagnostic}\x1b[0m\n")),
                Some(diagnostic.into())
            );
        }
        assert_eq!(child_unresolved_model_error("ECONNRESET"), None);
        assert_eq!(child_unresolved_model_error("Model not found"), None);
    }
}

#[cfg(all(test, unix))]
mod startup_tests {
    use super::*;
    #[test]
    fn crashed_child_model_diagnostics_are_typed_and_teardowns_settle() {
        for (stderr, expected) in [
            (
                r#"Model "fixture/missing" not found. Use --list-models to see available models."#,
                RunnerFailureKind::ModelUnavailable,
            ),
            (
                r#"Unknown provider "fixture". Use --list-models to see available providers/models."#,
                RunnerFailureKind::ModelUnavailable,
            ),
            ("connection reset", RunnerFailureKind::ChildPromptFailed),
        ] {
            let script = format!("read -r command; printf '%s\\n' '{}' >&2; exit 1", stderr);
            let runner = RpcProcessRunner::new(RpcProcessRunnerOptions {
                model_admission: Some(Arc::new(|_| Ok(()))),
                build_spawn: Some(Arc::new(move |_| RpcSpawnDescriptor {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), script.clone()],
                    cwd: "/tmp".into(),
                    env: std::env::vars().collect(),
                })),
                ..Default::default()
            });
            let failure = runner
                .start(&RpcRunnerSpec {
                    task_id: "model-start".into(),
                    model: Some("fixture/missing".into()),
                    prompt: "hello".into(),
                    ..Default::default()
                })
                .err()
                .expect("startup rejection");
            assert_eq!(failure.kind, expected);
            runner.settle_teardowns();
            assert!(runner.pending_teardowns.lock().expect("pending").is_empty());
        }
    }
    #[test]
    fn settle_teardowns_waits_for_registered_completion_event() {
        let runner = RpcProcessRunner::new(Default::default());
        let (release, wait) = std::sync::mpsc::channel();
        let (done, completion) = std::sync::mpsc::channel();
        runner
            .pending_teardowns
            .lock()
            .expect("pending")
            .push(std::thread::spawn(move || {
                wait.recv_timeout(std::time::Duration::from_secs(2))
                    .expect("release cleanup");
                done.send(()).expect("completion");
            }));
        release.send(()).expect("release");
        runner.settle_teardowns();
        completion
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("settled");
        assert!(runner.pending_teardowns.lock().expect("pending").is_empty());
    }
}
