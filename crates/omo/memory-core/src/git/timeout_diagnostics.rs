//! Port of `git/timeout-diagnostics.ts` (omo #10821, #12815): opt-in evidence for a git that
//! outlives its timeout. On Windows CI one `git add` stalls past its timeout and its repository
//! stays locked long after the kill, and no run has reproduced it on demand. When
//! `OMO_GIT_TIMEOUT_DIAGNOSTICS_DIR` names a directory, the timeout records the live process table
//! (with the stalled git's thread wait states on Windows) before git is signalled. Unset, nothing
//! runs.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const GIT_TIMEOUT_DIAGNOSTICS_ENV: &str = "OMO_GIT_TIMEOUT_DIAGNOSTICS_DIR";

/// The snapshot blocks the timed-out caller for at most this long before git is signalled. That
/// cost exists only in diagnostics mode and only on an actual timeout, where the evidence matters.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const RECORDED_ARGV_LIMIT: usize = 12;

fn snapshot_command(pid: u32) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        let script = [
            "Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,CreationDate,CommandLine | Format-Table -AutoSize -Wrap | Out-String -Width 400".to_owned(),
            format!(
                "@({pid}) + @(Get-CimInstance Win32_Process -Filter \"ParentProcessId={pid}\" | ForEach-Object {{ $_.ProcessId }}) | ForEach-Object {{ \"threads of $_\"; (Get-Process -Id $_ -ErrorAction SilentlyContinue).Threads | Group-Object ThreadState,WaitReason -NoElement | Format-Table -AutoSize | Out-String }}"
            ),
        ]
        .join("; ");
        (
            "powershell",
            vec!["-NoProfile".to_owned(), "-Command".to_owned(), script],
        )
    } else {
        (
            "ps",
            vec![
                "-axo".to_owned(),
                "pid,ppid,stat,wchan,etime,command".to_owned(),
            ],
        )
    }
}

/// The table goes to a temp file, never a pipe: a capped pipe capture overflows on a host running
/// thousands of processes, which lost the whole table (omo #12801). A file has no such cap.
pub fn process_snapshot(pid: u32) -> String {
    let output = std::env::temp_dir().join(format!(
        "omo-git-timeout-snapshot-{}-{}.txt",
        std::process::id(),
        crate::support::random::random_uuid()
    ));
    let file = match create_private(&output) {
        Ok(file) => file,
        Err(error) => return format!("process snapshot failed: {error}"),
    };
    let captured = run_snapshot(pid, file).and_then(|()| std::fs::read_to_string(&output));
    let _ = std::fs::remove_file(&output);
    captured.unwrap_or_else(|error| format!("process snapshot failed: {error}"))
}

fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn run_snapshot(pid: u32, file: File) -> io::Result<()> {
    let (program, args) = snapshot_command(pid);
    let errors = file.try_clone()?;
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(file))
        .stderr(Stdio::from(errors));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{program} did not finish within {}ms",
                    SNAPSHOT_TIMEOUT.as_millis()
                ),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Records the process table for a timed-out git when `directory` (the diagnostics env value) is
/// set. A write failure is reported on stderr and never fails the caller.
pub fn record_git_timeout(
    directory: Option<&str>,
    pid: Option<u32>,
    argv: &[String],
    cwd: &Path,
    timeout_ms: u64,
) {
    record_git_timeout_with(directory, pid, argv, cwd, timeout_ms, &process_snapshot);
}

pub fn record_git_timeout_with(
    directory: Option<&str>,
    pid: Option<u32>,
    argv: &[String],
    cwd: &Path,
    timeout_ms: u64,
    snapshot: &dyn Fn(u32) -> String,
) {
    let (Some(directory), Some(pid)) = (directory.filter(|value| !value.is_empty()), pid) else {
        return;
    };
    if let Err(error) = write_timeout_record(directory, pid, argv, cwd, timeout_ms, snapshot) {
        eprintln!("git timeout diagnostics could not be written to {directory}: {error}");
    }
}

fn write_timeout_record(
    directory: &str,
    pid: u32,
    argv: &[String],
    cwd: &Path,
    timeout_ms: u64,
    snapshot: &dyn Fn(u32) -> String,
) -> io::Result<()> {
    std::fs::create_dir_all(directory)?;
    let now = crate::support::time::now_millis();
    let record = serde_json::json!({
        "at": crate::support::time::format_rfc3339_millis(now),
        "pid": pid,
        "cwd": cwd.to_string_lossy(),
        "timeoutMs": timeout_ms,
        "argv": argv.iter().take(RECORDED_ARGV_LIMIT).collect::<Vec<_>>(),
        "argCount": argv.len(),
        "processes": snapshot(pid),
    });
    let text = serde_json::to_string_pretty(&record).map_err(io::Error::other)?;
    std::fs::write(
        Path::new(directory).join(format!("git-timeout-{pid}-{now}.json")),
        format!("{text}\n"),
    )
}

#[cfg(test)]
#[path = "timeout_diagnostics_tests.rs"]
mod tests;
