//! One `claude auth *` invocation for a CLI-store account (senpi `claude-cli-process.ts`, senpi#4437).
//!
//! The native engine has no process-group signalling (the workspace forbids `unsafe` and the crate carries no signal
//! dependency), so a cancelled or timed-out child is killed outright instead of by SIGTERM followed by SIGKILL.
use crate::claude_cli_environment::{ClaudeCliEnvironmentOptions, build_claude_cli_environment};
use maho_ai::utils::abort::AbortSignal;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

/// One `claude auth *` invocation for a CLI-store account.
#[derive(Clone, Debug)]
pub struct ClaudeCliRun {
    /// The executable the turn path resolves, so sign-in and turns use the same binary.
    pub executable: PathBuf,
    pub config_dir: PathBuf,
    /// The environment the spawn derives its own from; the process environment when `None`.
    pub parent_env: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCliExit {
    /// `None` when a signal ended the child.
    pub exit_code: Option<i32>,
    pub stdout: String,
}

pub const DEFAULT_SHORT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Spawns `claude <args>` inside the account dir with piped stdio. Dropping the child kills it.
pub fn spawn_claude_cli(
    run: &ClaudeCliRun,
    args: &[&str],
    browser: Option<&Path>,
) -> std::io::Result<tokio::process::Child> {
    let parent = run
        .parent_env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect());
    let environment = build_claude_cli_environment(
        &parent,
        &ClaudeCliEnvironmentOptions {
            config_dir: &run.config_dir,
            browser,
        },
    );
    tokio::process::Command::new(&run.executable)
        .args(args)
        .current_dir(&run.config_dir)
        .env_clear()
        .envs(&environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

/// Runs a short command (`auth status`, `auth logout`) to exit, killing it at the deadline or when `signal` aborts
/// (both answer `None`). Never logs its output.
pub async fn run_claude_cli_to_exit(
    run: &ClaudeCliRun,
    args: &[&str],
    timeout: Duration,
    signal: Option<&AbortSignal>,
) -> Option<ClaudeCliExit> {
    if signal.is_some_and(AbortSignal::aborted) {
        return None;
    }
    let mut child = spawn_claude_cli(run, args, None).ok()?;
    drop(child.stdin.take());
    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let work = async move {
        let mut collected = Vec::new();
        let mut discarded = Vec::new();
        let (read, _) = tokio::join!(
            stdout.read_to_end(&mut collected),
            stderr.read_to_end(&mut discarded)
        );
        read.ok()?;
        let status = child.wait().await.ok()?;
        Some(ClaudeCliExit {
            exit_code: status.code(),
            stdout: String::from_utf8_lossy(&collected).into_owned(),
        })
    };
    match signal {
        Some(signal) => tokio::select! {
            outcome = tokio::time::timeout(timeout, work) => outcome.ok().flatten(),
            () = signal.cancelled() => None,
        },
        None => tokio::time::timeout(timeout, work).await.ok().flatten(),
    }
}
