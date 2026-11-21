//! Port of `cold-wake/host.ts`: where and how a cold-wake unit is installed, and the unit writer.
//!
//! omo v5.1.79 (#14249, fixes #14247): every unit or plist is written 0644 under any umask. A file's
//! creation mode is ANDed with `~umask` (umask 077 left 0600, which a service manager may refuse), so
//! the mode is set explicitly before the file is moved into place. An unchanged file an older build
//! left 0600 is set back to 0644 without counting as a change (no reload, no restart); a file this
//! user cannot chmod (another owner's, a read-only filesystem) is left as it is.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const UNIT_MODE: u32 = 0o644;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePlatform {
    Linux,
    Darwin,
}

impl ServicePlatform {
    /// The platform this build runs on, when it has a user service manager mhc installs into.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::Darwin)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i32,
    /// stdout on success; on failure stderr (or stdout when stderr is empty).
    pub output: String,
}

/// Runs `systemctl` / `launchctl` / `plutil`; tests script it.
pub trait CommandRunner: Send + Sync {
    fn run(&self, argv: &[&str]) -> CommandOutput;
}

/// The real command runner.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, argv: &[&str]) -> CommandOutput {
        let Some((program, args)) = argv.split_first() else {
            return CommandOutput {
                exit_code: 127,
                output: "no command".into(),
            };
        };
        match std::process::Command::new(program).args(args).output() {
            Ok(output) => {
                let exit_code = output.status.code().unwrap_or(1);
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let output = if exit_code == 0 || stderr.trim().is_empty() {
                    stdout
                } else {
                    stderr
                };
                CommandOutput { exit_code, output }
            }
            Err(error) => CommandOutput {
                exit_code: 127,
                output: error.to_string(),
            },
        }
    }
}

/// This user's home and uid, the command runner and a retry pause.
#[derive(Clone)]
pub struct ColdWakeHost {
    pub platform: ServicePlatform,
    pub home: PathBuf,
    pub uid: u32,
    pub runner: Arc<dyn CommandRunner>,
    pub pause: Arc<dyn Fn(Duration) + Send + Sync>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColdWakeOutcome {
    /// Installed; `unit_changed` when its file was written (and the manager reloaded).
    Installed {
        unit_changed: bool,
    },
    Removed,
    Unsupported,
    Failed {
        step: String,
        detail: String,
    },
}

impl ColdWakeOutcome {
    pub fn failed(step: &str, output: &str) -> Self {
        Self::Failed {
            step: step.to_owned(),
            detail: output.trim().lines().next().unwrap_or_default().to_owned(),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

#[cfg(unix)]
fn set_unit_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(UNIT_MODE))
}

#[cfg(not(unix))]
fn set_unit_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn unit_mode_of(path: &Path) -> io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn unit_mode_of(_path: &Path) -> io::Result<u32> {
    Ok(UNIT_MODE)
}

/// The best-effort repair of an unchanged unit's mode: EPERM/EACCES/EROFS leave the file as it is;
/// any other error is raised.
fn repair_mode(path: &Path) -> io::Result<()> {
    if unit_mode_of(path)? == UNIT_MODE {
        return Ok(());
    }
    match set_unit_mode(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Writes `text` through a temp file and a rename, mode 0644; `Ok(false)` when the content was
/// already there (its mode repaired).
pub fn write_if_changed(path: &Path, text: &str) -> io::Result<bool> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == text => {
            repair_mode(path)?;
            return Ok(false);
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&temp, text)
        .and_then(|()| set_unit_mode(&temp))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _cleanup = std::fs::remove_file(&temp);
    }
    written.map(|()| true)
}

pub fn remove_if_present(path: &Path) -> io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
