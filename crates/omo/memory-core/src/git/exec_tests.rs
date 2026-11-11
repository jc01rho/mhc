use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions, GitExecResult, system_git_exec};

struct FailingMockExec {
    call_count: Arc<AtomicUsize>,
}

impl GitExec for FailingMockExec {
    fn run(&self, argv: &[String], _options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("git command not found: {}", argv.join(" ")),
        ))
    }
}

struct TimeoutMockExec;

impl GitExec for TimeoutMockExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "git {} timed out after {}ms",
                argv.join(" "),
                options.timeout_ms
            ),
        ))
    }
}

#[test]
fn given_missing_working_directory_when_run_executes_then_reports_exit_code_128() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let missing_path = temp_dir.path().join("missing-dir");
    let exec = system_git_exec();

    let options = GitExecOptions {
        cwd: missing_path,
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let result = exec
        .run(
            &[
                "rev-parse".to_string(),
                "--verify".to_string(),
                "HEAD".to_string(),
            ],
            &options,
        )
        .expect("run");

    assert_eq!(result.code, 128);
    assert!(result.stderr.contains("No such file or directory"));
}

#[test]
fn given_binary_not_found_when_run_executes_then_not_found_error_is_produced() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let call_count = Arc::new(AtomicUsize::new(0));
    let mock = FailingMockExec {
        call_count: Arc::clone(&call_count),
    };

    let options = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let err = mock.run(&["--version".to_string()], &options).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[test]
fn given_process_exceeding_timeout_when_run_executes_then_timeout_error_is_produced() {
    let mock = TimeoutMockExec;
    let options = GitExecOptions {
        cwd: PathBuf::from("."),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };

    let err = mock.run(&["--version".to_string()], &options).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    let git_err = GitError::Timeout {
        argv: vec!["--version".to_string()],
        timeout_ms: 5000,
    };
    assert!(git_err.to_string().contains("timed out after 5000ms"));
}

#[test]
fn given_input_via_stdin_when_hash_object_reads_it_then_git_produces_sha() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let exec = system_git_exec();

    let init_opts = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };
    let init_res = exec.run(&["init".to_string()], &init_opts).expect("init");
    assert_eq!(init_res.code, 0);

    let stdin_opts = GitExecOptions {
        cwd: temp_dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: Some(b"hello world\n".to_vec()),
    };

    let hash_res = exec
        .run(
            &["hash-object".to_string(), "--stdin".to_string()],
            &stdin_opts,
        )
        .expect("hash-object");

    assert_eq!(hash_res.code, 0);
    assert_eq!(
        hash_res.stdout.trim(),
        "3b18e512dba79e4c8300dd08aeb37f8e728b8dad"
    );
}

// --- Hermetic ports of the createNodeGitExec runtime tests.

use std::io::{Error as IoError, ErrorKind};
use std::sync::Mutex;

use super::exec::{GitExecRuntime, GitPlatform, create_git_exec};

type Responder = dyn Fn(&str, &GitExecOptions) -> std::io::Result<GitExecResult> + Send + Sync;

struct Recorder {
    attempts: Arc<Mutex<Vec<String>>>,
    stdins: Arc<Mutex<Vec<Option<Vec<u8>>>>>,
    exec: Arc<dyn GitExec>,
}

fn hermetic(platform: GitPlatform, respond: Arc<Responder>) -> Recorder {
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let stdins = Arc::new(Mutex::new(Vec::new()));
    let (a, s) = (Arc::clone(&attempts), Arc::clone(&stdins));
    let exec = create_git_exec(GitExecRuntime {
        platform: Some(platform),
        run_command: Some(Arc::new(
            move |exe: &str, _argv: &[String], options: &GitExecOptions| {
                a.lock().expect("attempts").push(exe.to_string());
                s.lock().expect("stdins").push(options.stdin.clone());
                respond(exe, options)
            },
        )),
    });
    Recorder {
        attempts,
        stdins,
        exec,
    }
}

impl Recorder {
    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().expect("attempts").clone()
    }
}

fn success() -> GitExecResult {
    GitExecResult {
        code: 0,
        stdout: "git version test\n".to_string(),
        stderr: String::new(),
        termination: None,
    }
}

fn missing_git() -> IoError {
    IoError::new(ErrorKind::NotFound, "spawn failed: ENOENT")
}

fn env_options(pairs: &[(&str, &str)]) -> GitExecOptions {
    let mut env = BTreeMap::from([("PATH".to_string(), "/nonexistent".to_string())]);
    for (k, v) in pairs {
        env.insert((*k).to_string(), (*v).to_string());
    }
    GitExecOptions {
        cwd: PathBuf::from("."),
        timeout_ms: 5000,
        env,
        stdin: None,
    }
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

const NON_ENOENT: [(ErrorKind, &str); 3] = [
    (ErrorKind::PermissionDenied, "EACCES"),
    (ErrorKind::PermissionDenied, "EPERM"),
    (ErrorKind::Other, "EIO"),
];

#[test]
fn given_bare_runner_reports_missing_cwd_result_when_run_then_windows_fallbacks_are_not_attempted()
{
    let missing_cwd = GitExecResult {
        code: 128,
        stdout: String::new(),
        stderr: "fatal: cannot change directory".to_string(),
        termination: None,
    };
    let expected = missing_cwd.clone();
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(move |_, _| Ok(missing_cwd.clone())),
    );

    let result = rec
        .exec
        .run(
            &argv(&["status"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect("run");

    assert_eq!(result, expected);
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_cwd_exists_but_no_git_on_path_when_git_runs_then_not_found_error_fires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let options = GitExecOptions {
        cwd: dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::from([("PATH".to_string(), "/nonexistent".to_string())]),
        stdin: None,
    };

    let err = create_git_exec(GitExecRuntime {
        platform: Some(GitPlatform::Other),
        run_command: None,
    })
    .run(&argv(&["--version"]), &options)
    .expect_err("git must be missing");

    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn given_git_resolves_on_path_when_windows_roots_exist_then_bare_git_keeps_precedence() {
    let rec = hermetic(GitPlatform::Windows, Arc::new(|_, _| Ok(success())));

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_non_windows_platform_and_missing_git_when_git_runs_then_windows_roots_are_not_attempted() {
    let rec = hermetic(GitPlatform::Other, Arc::new(|_, _| Err(missing_git())));

    let err = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[("ProgramFiles", "C:\\Program Files")]),
        )
        .expect_err("missing");

    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert_eq!(rec.attempts(), vec!["git"]);
}

#[test]
fn given_bare_git_fails_with_non_enoent_cause_when_windows_roots_exist_then_error_propagates_without_fallback()
 {
    for (kind, code) in NON_ENOENT {
        let rec = hermetic(
            GitPlatform::Windows,
            Arc::new(move |_, _| Err(IoError::new(kind, code))),
        );

        let err = rec
            .exec
            .run(
                &argv(&["--version"]),
                &env_options(&[("ProgramFiles", "C:\\Program Files")]),
            )
            .expect_err(code);

        assert_eq!((err.kind(), err.to_string()), (kind, code.to_string()));
        assert_eq!(rec.attempts(), vec!["git"], "{code}");
    }
}

#[test]
fn given_unc_device_and_root_relative_roots_when_fallback_runs_then_only_drive_qualified_root_is_attempted()
 {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "\\\\server\\share"),
                ("ProgramW6432", "\\\\?\\C:\\Program Files"),
                ("ProgramFiles(x86)", "\\Program Files (x86)"),
                ("LOCALAPPDATA", "C:\\Users\\Example User\\AppData\\Local"),
            ]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(
        rec.attempts(),
        vec![
            "git".to_string(),
            "C:\\Users\\Example User\\AppData\\Local\\Programs\\Git\\cmd\\git.exe".to_string(),
        ]
    );
}

#[test]
fn given_distinct_standard_roots_when_every_executable_is_absent_then_candidates_attempted_in_deterministic_order()
 {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            Err(IoError::new(
                ErrorKind::NotFound,
                if exe == "git" { "original" } else { "fallback" },
            ))
        }),
    );

    let err = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "C:\\Program Files"),
                ("ProgramW6432", "D:\\Program Files"),
                ("ProgramFiles(x86)", "E:\\Program Files (x86)"),
                ("LOCALAPPDATA", "F:\\User Data"),
            ]),
        )
        .expect_err("all absent");

    assert_eq!(err.to_string(), "original");
    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files\\Git\\cmd\\git.exe",
            "E:\\Program Files (x86)\\Git\\cmd\\git.exe",
            "F:\\User Data\\Programs\\Git\\cmd\\git.exe",
        ]
    );
}

#[test]
fn given_duplicate_standard_roots_when_fallback_runs_then_equivalent_candidates_attempted_once() {
    let rec = hermetic(GitPlatform::Windows, Arc::new(|_, _| Err(missing_git())));

    let _ = rec.exec.run(
        &argv(&["--version"]),
        &env_options(&[
            ("ProgramFiles", "C:\\Program Files"),
            ("ProgramW6432", "c:\\PROGRAM FILES"),
            ("ProgramFiles(x86)", "D:\\Program Files (x86)"),
        ]),
    );

    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files (x86)\\Git\\cmd\\git.exe",
        ]
    );
}

#[test]
fn given_first_fallback_yields_enoent_when_fallback_runs_then_next_candidate_is_attempted() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" || exe.starts_with("C:") {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );

    let result = rec
        .exec
        .run(
            &argv(&["--version"]),
            &env_options(&[
                ("ProgramFiles", "C:\\Program Files"),
                ("ProgramW6432", "D:\\Program Files"),
            ]),
        )
        .expect("run");

    assert_eq!(result, success());
    assert_eq!(
        rec.attempts(),
        vec![
            "git",
            "C:\\Program Files\\Git\\cmd\\git.exe",
            "D:\\Program Files\\Git\\cmd\\git.exe",
        ]
    );
}

fn two_roots() -> GitExecOptions {
    env_options(&[
        ("ProgramFiles", "C:\\Program Files"),
        ("ProgramW6432", "D:\\Program Files"),
    ])
}

#[test]
fn given_fallback_fails_with_non_enoent_error_when_fallback_runs_then_error_propagates_without_later_candidates()
 {
    // Covers both TS loops: GitNotFoundError caused by EACCES/EPERM/EIO and a raw spawn EACCES/EPERM/EIO.
    for (kind, code) in NON_ENOENT {
        let rec = hermetic(
            GitPlatform::Windows,
            Arc::new(move |exe, _| {
                if exe == "git" {
                    Err(missing_git())
                } else {
                    Err(IoError::new(kind, code))
                }
            }),
        );

        let err = rec
            .exec
            .run(&argv(&["--version"]), &two_roots())
            .expect_err(code);

        assert_eq!(err.to_string(), code);
        assert_eq!(
            rec.attempts(),
            vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"],
            "{code}"
        );
    }
}

#[test]
fn given_fallback_timeout_when_fallback_runs_then_timeout_propagates_without_later_candidates() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Err(IoError::new(ErrorKind::TimedOut, "timeout"))
            }
        }),
    );

    let err = rec
        .exec
        .run(&argv(&["--version"]), &two_roots())
        .expect_err("timeout");

    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert_eq!(
        rec.attempts(),
        vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"]
    );
}

#[test]
fn given_fallback_nonzero_result_when_fallback_runs_then_result_returns_without_later_candidates() {
    let nonzero = GitExecResult {
        code: 128,
        stdout: String::new(),
        stderr: "fatal: command failed".to_string(),
        termination: None,
    };
    let expected = nonzero.clone();
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(move |exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(nonzero.clone())
            }
        }),
    );

    let result = rec.exec.run(&argv(&["status"]), &two_roots()).expect("run");

    assert_eq!(result, expected);
    assert_eq!(
        rec.attempts(),
        vec!["git", "C:\\Program Files\\Git\\cmd\\git.exe"]
    );
}

#[test]
fn given_stdin_when_real_git_runs_then_bytes_are_piped_like_a_file_hash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exec = system_git_exec();
    let content = b"stdin-content\n".to_vec();
    let path = dir.path().join("content.txt");
    std::fs::write(&path, &content).expect("write");
    let base = GitExecOptions {
        cwd: dir.path().to_path_buf(),
        timeout_ms: 5000,
        env: BTreeMap::new(),
        stdin: None,
    };
    let expected = exec
        .run(
            &argv(&["hash-object", "--", &path.to_string_lossy()]),
            &base,
        )
        .expect("hash file");

    let result = exec
        .run(
            &argv(&["hash-object", "--stdin"]),
            &GitExecOptions {
                stdin: Some(content),
                ..base
            },
        )
        .expect("hash stdin");

    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, expected.stdout);
}

#[test]
fn given_stdin_and_windows_fallback_when_bare_git_absent_then_stdin_reaches_fallback_runner() {
    let rec = hermetic(
        GitPlatform::Windows,
        Arc::new(|exe, _| {
            if exe == "git" {
                Err(missing_git())
            } else {
                Ok(success())
            }
        }),
    );
    let options = GitExecOptions {
        stdin: Some(b"fallback-input".to_vec()),
        ..env_options(&[("ProgramFiles", "C:\\Program Files")])
    };

    rec.exec
        .run(&argv(&["hash-object", "--stdin"]), &options)
        .expect("run");

    let input = Some(b"fallback-input".to_vec());
    assert_eq!(
        *rec.stdins.lock().expect("stdins"),
        vec![input.clone(), input]
    );
}

#[test]
fn early_stdin_close_does_not_mask_git_failure() {
    let root = tempfile::tempdir().unwrap();
    let exec = system_git_exec();
    // Unknown commands make Git for Windows scan PATH for suggestions, which can exceed a few
    // seconds on a loaded CI runner; use the module's default bound so only a real hang fails.
    let options = GitExecOptions {
        cwd: root.path().to_path_buf(),
        timeout_ms: 30_000,
        stdin: Some(vec![b'x'; 4 * 1024 * 1024]),
        ..Default::default()
    };
    let result = exec.run(&["not-a-git-command".into()], &options).unwrap();
    assert_ne!(result.code, 0);
    assert!(!result.stderr.contains("stdin:"));
}

#[cfg(unix)]
#[test]
fn normal_exit_is_bounded_when_hook_descendant_holds_pipes() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let repo = super::repo::GitMemoryRepo::open(root.path(), "test").unwrap();
    repo.init(None).unwrap();
    let fifo = root.path().join("hold.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let hook = root.path().join(".git/hooks/post-commit");
    fs_write_hook(
        &hook,
        "#!/bin/sh\ncat < hold.fifo &\necho $! > holder.pid\necho hook-output >&2\nexit 0\n",
    );
    fn fs_write_hook(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let cwd = root.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        let result = system_git_exec().run(
            &[
                "commit".into(),
                "--allow-empty".into(),
                "-m".into(),
                "next".into(),
            ],
            &GitExecOptions {
                cwd,
                timeout_ms: 5_000,
                ..Default::default()
            },
        );
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(Duration::from_secs(15));
    // Cleanup even on a failed assertion: the FIFO reader never needs an arbitrary delay.
    if let Ok(pid) = std::fs::read_to_string(root.path().join("holder.pid")) {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", pid.trim()])
            .status();
    }
    let result = result
        .expect("git result must not await background hook EOF")
        .unwrap();
    assert_eq!(result.code, 0);
    assert!(result.stderr.contains("hook-output"));
    task.join().unwrap();
}

#[test]
fn given_windows_fallback_when_bare_git_is_absent_then_every_attempt_carries_the_fsmonitor_override_ahead_of_the_caller_argv()
 {
    let argvs = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&argvs);
    let exec = create_git_exec(GitExecRuntime {
        platform: Some(GitPlatform::Windows),
        run_command: Some(Arc::new(
            move |exe: &str, argv: &[String], _options: &GitExecOptions| {
                recorded.lock().expect("argvs").push(argv.to_vec());
                if exe == "git" {
                    Err(missing_git())
                } else {
                    Ok(success())
                }
            },
        )),
    });

    exec.run(
        &argv(&["-c", "user.name=fixture-writer", "status", "--porcelain"]),
        &env_options(&[("ProgramFiles", "C:\\Program Files")]),
    )
    .expect("run");

    let expected = argv(&[
        "-c",
        "core.fsmonitor=false",
        "-c",
        "user.name=fixture-writer",
        "status",
        "--porcelain",
    ]);
    assert_eq!(
        argvs.lock().expect("argvs").clone(),
        vec![expected.clone(), expected]
    );
}

/// Writes an executable script from a child `sh`: a write descriptor held by this multi-threaded test
/// binary can be inherited by another test's concurrent fork, and exec of the script then fails with
/// ETXTBSY ("Text file busy").
#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, content: &str) {
    let status = std::process::Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
            "sh",
            content,
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "write {}", path.display());
}

/// The builtin fsmonitor daemon exists on macOS and Windows only, but `core.fsmonitor` naming a hook
/// runs that hook on every index read on any platform, so a hook that records its calls proves
/// whether a memory git command honoured the user's `core.fsmonitor` (omo #12097).
#[cfg(unix)]
#[test]
fn given_core_fsmonitor_in_the_global_config_when_the_clean_tree_status_runs_through_the_exec_then_fsmonitor_never_runs()
 {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let global_config = root.path().join("gitconfig");
    std::fs::write(&global_config, "").unwrap();
    let env = BTreeMap::from([
        (
            "GIT_CONFIG_GLOBAL".to_string(),
            global_config.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
    ]);
    let raw_git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .envs(&env)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    raw_git(&["init", "--quiet"]);
    std::fs::write(repo.join("a.md"), "a\n").unwrap();
    raw_git(&["add", "a.md"]);
    let calls = root.path().join("fsmonitor.calls");
    let hook = root.path().join("fsmonitor-hook");
    write_executable_script(
        &hook,
        &format!("#!/bin/sh\necho called >> '{}'\nexit 1\n", calls.display()),
    );
    std::fs::write(
        &global_config,
        format!("[core]\n\tfsmonitor = {}\n", hook.display()),
    )
    .unwrap();
    let clean_tree_status = [
        "-c",
        "core.quotePath=false",
        "status",
        "--porcelain",
        "--untracked-files=all",
    ];

    // Positive control: the same status without the exec boundary runs the hook.
    raw_git(&clean_tree_status);
    assert!(calls.exists(), "positive control: fsmonitor hook must run");
    std::fs::remove_file(&calls).unwrap();

    let exec = system_git_exec();
    let options = GitExecOptions {
        cwd: repo.clone(),
        timeout_ms: 30_000,
        env: env.clone(),
        stdin: None,
    };
    let status = exec.run(&argv(&clean_tree_status), &options).unwrap();
    let bytes = exec.run_bytes(&argv(&clean_tree_status), &options).unwrap();

    assert_eq!((status.code, bytes.code), (0, 0));
    assert!(
        !calls.exists(),
        "fsmonitor ran: {:?}",
        std::fs::read_to_string(&calls)
    );
}

// --- omo #11943: a git child that exits non-zero without writing anything used to surface as a
// bare "exit code 1", which cannot tell a real silent failure from a child killed by something else.

use super::exec::GitTermination;
use super::repo_arguments::command_error;

#[test]
fn given_a_git_command_that_fails_without_output_when_it_runs_then_the_result_records_how_the_child_ended_and_the_error_names_it()
 {
    let dir = tempfile::tempdir().unwrap();
    let options = GitExecOptions {
        cwd: dir.path().to_path_buf(),
        timeout_ms: 30_000,
        ..Default::default()
    };
    let exec = system_git_exec();
    assert_eq!(exec.run(&argv(&["init", "-q"]), &options).unwrap().code, 0);
    let args = argv(&["rev-parse", "--verify", "--quiet", "refs/heads/missing"]);

    let result = exec.run(&args, &options).unwrap();

    assert_eq!(result.code, 1);
    assert_eq!(format!("{}{}", result.stdout, result.stderr), "");
    let termination = result.termination.clone().expect("termination recorded");
    assert_eq!(termination.signal, None);
    assert!(
        command_error(&args, &result)
            .to_string()
            .contains("with no output (no signal, after "),
        "{}",
        command_error(&args, &result)
    );
}

#[cfg(unix)]
#[test]
fn given_a_git_child_killed_by_a_signal_when_it_runs_then_the_result_names_the_signal() {
    let bin = tempfile::tempdir().unwrap();
    let fake_git = bin.path().join("git");
    write_executable_script(&fake_git, "#!/bin/sh\nkill -KILL $$\n");
    let options = GitExecOptions {
        cwd: bin.path().to_path_buf(),
        timeout_ms: 30_000,
        env: BTreeMap::from([(
            "PATH".to_string(),
            bin.path().to_string_lossy().into_owned(),
        )]),
        stdin: None,
    };
    let args = argv(&["add", "-A"]);

    let result = system_git_exec().run(&args, &options).unwrap();

    assert_eq!(
        result
            .termination
            .as_ref()
            .and_then(|t| t.signal.as_deref()),
        Some("SIGKILL")
    );
    assert!(
        command_error(&args, &result)
            .to_string()
            .starts_with("git add -A failed: exit code 1 with no output (signal SIGKILL, after "),
        "{}",
        command_error(&args, &result)
    );
}

#[test]
fn given_a_child_ended_by_an_external_signal_when_the_error_is_built_then_the_signal_is_in_the_message()
 {
    let error = GitError::Command {
        argv: argv(&["add", "-A"]),
        code: 1,
        stdout: String::new(),
        stderr: String::new(),
        termination: Some(GitTermination {
            signal: Some("SIGKILL".to_string()),
            elapsed_ms: 34_226,
        }),
    };

    assert_eq!(
        error.to_string(),
        "git add -A failed: exit code 1 with no output (signal SIGKILL, after 34226ms)"
    );
}

#[cfg(unix)]
#[test]
fn given_a_signal_without_a_mapped_name_when_the_error_is_built_then_the_signal_is_named_by_its_number()
 {
    assert_eq!(super::exec::signal_name(libc::SIGKILL), "SIGKILL");
    let error = GitError::Command {
        argv: argv(&["add"]),
        code: 1,
        stdout: String::new(),
        stderr: String::new(),
        termination: Some(GitTermination {
            signal: Some(super::exec::signal_name(64)),
            elapsed_ms: 5,
        }),
    };

    assert_eq!(
        error.to_string(),
        "git add failed: exit code 1 with no output (signal 64, after 5ms)"
    );
}

#[test]
fn given_git_wrote_stderr_when_the_error_is_built_then_stderr_stays_the_detail() {
    let error = GitError::Command {
        argv: argv(&["add"]),
        code: 128,
        stdout: String::new(),
        stderr: "fatal: bad\n".to_string(),
        termination: Some(GitTermination {
            signal: None,
            elapsed_ms: 3,
        }),
    };

    assert_eq!(error.to_string(), "git add failed: fatal: bad");
}
