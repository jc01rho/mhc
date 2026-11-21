//! Port of `test/install-sh-root-guard.test.ts` (omo#14344): the installer refuses a `sudo`/`doas`
//! escalation into root's home, and installs for a shell whose only user is root (proot-distro,
//! a container).
//!
//! These are real subprocess tests: `/bin/bash` reads the shipped `install.sh` bytes. Each test owns
//! a temp dir holding a `bin/` of stub commands (`id`, `uname`) and removes it on drop.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use get_worker::INSTALL_SH;

const PIPELINE_OVERRIDE: &str =
    "curl -fsSL https://get.omo.dev/install.sh | OMO_INSTALL_ALLOW_SUDO=1 bash";

struct Run {
    status: Option<i32>,
    stderr: String,
}

struct Host {
    root: tempfile::TempDir,
    bin: PathBuf,
}

fn function_source(name: &str) -> String {
    let opener = format!("\n{name}() {{");
    let start = INSTALL_SH
        .find(&opener)
        .unwrap_or_else(|| panic!("install.sh has no {name}()"));
    let end = start
        + INSTALL_SH[start..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("install.sh never closes {name}()"));
    INSTALL_SH[start + 1..end + 2].to_owned()
}

/// A temp `bin/` whose `id -u` answers `uid`, plus `stubs` (name, shell body) and symlinks to the
/// `real` commands, so a run sees only those commands on PATH.
fn host_bin(uid: u32, stubs: &[(&str, &str)], real: &[&str]) -> Host {
    let root = tempfile::tempdir().expect("temp dir");
    let bin = root.path().join("bin");
    fs::create_dir(&bin).expect("bin dir");
    let mut all = vec![("id", format!("[ \"$1\" = -u ] && echo {uid}"))];
    all.extend(stubs.iter().map(|(name, body)| (*name, (*body).to_owned())));
    for (name, body) in &all {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the stub");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod the stub");
    }
    for name in real {
        let source = ["/bin", "/usr/bin"]
            .iter()
            .map(|dir| Path::new(dir).join(name))
            .find(|candidate| candidate.exists())
            .unwrap_or_else(|| panic!("missing test command: {name}"));
        std::os::unix::fs::symlink(source, bin.join(name)).expect("symlink the tool");
    }
    Host { root, bin }
}

/// Runs only `refuse_sudo_install`, lifted out of the shipped script, as `uid` with `env`.
fn guard(uid: u32, env: &[(&str, &str)]) -> Run {
    let host = host_bin(uid, &[], &[]);
    let program = [
        "set -euo pipefail".to_owned(),
        "say() { printf \"%s\\n\" \"$*\" >&2; }".to_owned(),
        "fail() { say \"omo installer: $*\"; exit 1; }".to_owned(),
        function_source("refuse_sudo_install"),
        "refuse_sudo_install".to_owned(),
        "echo allowed >&2".to_owned(),
    ]
    .join("\n");
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(&program)
        .env_clear()
        .env("PATH", host.bin.as_os_str())
        .envs(env.iter().copied())
        .output()
        .expect("run bash");
    Run {
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Runs the whole installer as `uid`. `uname -s` names an unsupported OS, so `main` stops at
/// `detect_asset`, right after the guard, with no network.
fn run_installer(uid: u32, env: &[(&str, &str)]) -> Run {
    let host = host_bin(
        uid,
        &[("uname", "echo Plan9")],
        &["bash", "mktemp", "cat", "rm"],
    );
    let mut child = Command::new("/bin/bash")
        .arg("-s")
        .env_clear()
        .env("HOME", host.root.path().join("home"))
        .env("PATH", host.bin.as_os_str())
        .env("TMPDIR", host.root.path())
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bash");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(INSTALL_SH.as_bytes())
        .expect("write the installer to stdin");
    let output = child.wait_with_output().expect("wait for bash");
    Run {
        status: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn assert_allowed(run: &Run) {
    assert_eq!(run.status, Some(0), "{}", run.stderr);
    assert_eq!(run.stderr, "allowed\n");
}

#[test]
fn a_normal_user_installs() {
    assert_allowed(&guard(1000, &[]));
}

#[test]
fn a_root_only_shell_with_no_sudo_installs_without_an_override() {
    assert_allowed(&guard(0, &[]));
}

#[test]
fn root_that_sudo_started_from_root_itself_installs() {
    assert_allowed(&guard(0, &[("SUDO_USER", "root")]));
}

#[test]
fn sudo_from_a_normal_user_refuses_and_prints_the_pipeline_form_override() {
    let run = guard(0, &[("SUDO_USER", "alice")]);
    assert_eq!(run.status, Some(1));
    assert!(!run.stderr.contains("allowed"), "{}", run.stderr);
    assert!(run.stderr.contains("alice"), "{}", run.stderr);
    assert!(run.stderr.contains(PIPELINE_OVERRIDE), "{}", run.stderr);
}

#[test]
fn doas_from_a_normal_user_refuses_too() {
    let run = guard(0, &[("DOAS_USER", "alice")]);
    assert_eq!(run.status, Some(1));
    assert!(run.stderr.contains(PIPELINE_OVERRIDE), "{}", run.stderr);
}

#[test]
fn sudo_from_a_normal_user_installs_when_the_override_is_set() {
    assert_allowed(&guard(
        0,
        &[("SUDO_USER", "alice"), ("OMO_INSTALL_ALLOW_SUDO", "1")],
    ));
}

#[test]
fn main_passes_the_guard_in_a_root_only_shell() {
    let run = run_installer(0, &[]);
    assert_eq!(
        run.stderr,
        "omo installer: unsupported operating system Plan9\n"
    );
    assert_eq!(run.status, Some(1));
}

#[test]
fn main_refuses_sudo_from_a_normal_user_before_detecting_the_platform() {
    let run = run_installer(0, &[("SUDO_USER", "alice")]);
    assert_eq!(run.status, Some(1));
    assert!(run.stderr.contains(PIPELINE_OVERRIDE), "{}", run.stderr);
    assert!(
        !run.stderr.contains("unsupported operating system"),
        "{}",
        run.stderr
    );
}
