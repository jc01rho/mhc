use crate::{
    constants::{DIFF_VIEWER_ROW_CAP, GIT_TIMEOUT_MS},
    format::sanitize_terminal_text,
    types::{PanelColor, PanelFuture, PanelGitEntry, PanelGitStatus, PanelRow, PanelUi},
};
use std::sync::Arc;
#[derive(Clone, Debug)]
pub struct ExecOutput {
    pub stdout: String,
    pub code: i32,
}
pub trait PanelExec: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        args: Vec<String>,
        cwd: &'a str,
        timeout_ms: u64,
    ) -> PanelFuture<'a, Result<ExecOutput, String>>;
}
pub struct PanelCommandContext {
    pub mode: Option<String>,
    pub ui: Option<Arc<dyn PanelUi>>,
}
pub struct PanelCommandDeps<'a> {
    pub status: Option<&'a PanelGitStatus>,
    pub exec: Option<&'a dyn PanelExec>,
    pub mounted: bool,
}
fn file_option_label(file: &PanelGitEntry) -> String {
    let delta = if file.added.is_none() && file.removed.is_none() {
        String::new()
    } else {
        format!(
            "  +{}/-{}",
            file.added.unwrap_or(0),
            file.removed.unwrap_or(0)
        )
    };
    sanitize_terminal_text(&format!("{} {}{delta}", file.xy, file.path))
}
pub async fn run_diff_command(deps: PanelCommandDeps<'_>, ctx: &PanelCommandContext) {
    let Some(ui) = &ctx.ui else {
        return;
    };
    if !deps.mounted {
        ui.notify("The side panel is off in this session.", "info");
        return;
    }
    let Some(status) = deps.status.filter(|s| !s.files.is_empty()) else {
        ui.notify("No changes in the working copy.", "info");
        return;
    };
    let Some(exec) = deps.exec else {
        ui.notify(
            "This host exposes no exec, so git cannot be run.",
            "warning",
        );
        return;
    };
    let options: Vec<_> = status.files.iter().map(file_option_label).collect();
    let Some(choice) = ui.select("Show the diff of", options.clone()).await else {
        return;
    };
    let Some(file) = options
        .iter()
        .position(|o| o == &choice)
        .and_then(|i| status.files.get(i))
    else {
        return;
    };
    open_file_diff(ui.as_ref(), exec, status, file).await;
}
pub async fn open_file_diff(
    ui: &dyn PanelUi,
    exec: &dyn PanelExec,
    status: &PanelGitStatus,
    file: &PanelGitEntry,
) {
    let mut rows = read_git_diff(exec, &status.root, file).await;
    if rows.len() > DIFF_VIEWER_ROW_CAP {
        let omitted = rows.len() - DIFF_VIEWER_ROW_CAP;
        rows.truncate(DIFF_VIEWER_ROW_CAP);
        rows.push(PanelRow::colored(
            format!("... {omitted} more lines"),
            PanelColor::Dim,
        ));
    }
    ui.viewer(&format!("{}  (diff, read-only)", file.path), rows)
        .await;
}
/// Per-pathspec literal magic stands in for the global `--literal-pathspecs` flag, which git refuses
/// to combine with any other global pathspec switch: a shell that exports `GIT_GLOB_PATHSPECS=1` or
/// `GIT_ICASE_PATHSPECS=1` makes git exit 128 before any diff is produced (omo #11176), and the host's
/// `exec` carries no `env`, so an override could never reach git. Per-pathspec magic is not a global
/// switch, so it does not trip that conflict, while a `*` or `[` in a file name is still matched
/// literally. It cannot cancel a global `GIT_ICASE_PATHSPECS` (git ORs the global switch into each
/// pathspec), but the panel only queries paths exactly as `git status` reported them, so the reported
/// path's diff is still shown.
///
/// The one switch per-pathspec magic cannot coexist with is `GIT_LITERAL_PATHSPECS`: with it exported,
/// git reads `:(literal)` as part of the file name, so the diff and the untracked listing match nothing
/// and exit 0. The panel runs in the host process, so it sees the environment the spawned git inherits;
/// when that switch is on, literal matching already holds and the paths stay bare.
pub fn git_diff_args(file: &PanelGitEntry) -> Vec<String> {
    git_diff_args_with(file, literal_switch_exported())
}
fn literal_switch_exported() -> bool {
    literal_switch_is_on(std::env::var("GIT_LITERAL_PATHSPECS").ok().as_deref())
}
fn literal_switch_is_on(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !matches!(
            value.trim().to_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        )
    })
}
fn literal_pathspec(path: &str, literal_switch: bool) -> String {
    if literal_switch {
        path.to_owned()
    } else {
        format!(":(literal){path}")
    }
}
fn git_diff_args_with(file: &PanelGitEntry, literal_switch: bool) -> Vec<String> {
    if file.xy == "??" {
        // `--no-index` operands are plain filesystem paths, not pathspecs: no magic prefix.
        return ["diff", "--no-index", "--", "/dev/null", &file.path]
            .into_iter()
            .map(str::to_owned)
            .collect();
    }
    let mut args: Vec<_> = ["diff", "HEAD", "-M", "--"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if let Some(from) = &file.from {
        args.push(literal_pathspec(from, literal_switch));
    }
    args.push(literal_pathspec(&file.path, literal_switch));
    args
}
pub async fn read_git_diff(
    exec: &dyn PanelExec,
    root: &str,
    file: &PanelGitEntry,
) -> Vec<PanelRow> {
    read_git_diff_with(exec, root, file, literal_switch_exported()).await
}
async fn read_git_diff_with(
    exec: &dyn PanelExec,
    root: &str,
    file: &PanelGitEntry,
    literal_switch: bool,
) -> Vec<PanelRow> {
    if file.xy == "??" && file.path.ends_with('/') {
        let args = ["ls-files", "--others", "--exclude-standard", "-z", "--"]
            .into_iter()
            .map(str::to_owned)
            .chain([literal_pathspec(&file.path, literal_switch)])
            .collect();
        let Ok(listed) = exec.exec("git", args, root, GIT_TIMEOUT_MS).await else {
            return diff_error();
        };
        if listed.code != 0 {
            return diff_error();
        }
        let paths: Vec<_> = listed
            .stdout
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect();
        let results = futures::future::join_all(paths.iter().take(50).map(|path| {
            exec.exec(
                "git",
                git_diff_args_with(
                    &PanelGitEntry {
                        xy: "??".into(),
                        path: (*path).into(),
                        ..PanelGitEntry::default()
                    },
                    literal_switch,
                ),
                root,
                GIT_TIMEOUT_MS,
            )
        }))
        .await;
        let mut rows = Vec::new();
        for result in results {
            let Ok(result) = result else {
                return diff_error();
            };
            rows.extend(colorize_diff(&result.stdout));
        }
        if paths.len() > 50 {
            rows.push(PanelRow::colored(
                format!("... {} more files in {}", paths.len() - 50, file.path),
                PanelColor::Muted,
            ));
        }
        return nonempty_diff(rows);
    }
    let mut result = exec
        .exec(
            "git",
            git_diff_args_with(file, literal_switch),
            root,
            GIT_TIMEOUT_MS,
        )
        .await;
    if result
        .as_ref()
        .is_ok_and(|r| r.code == 128 && r.stdout.is_empty())
        && !file.xy.starts_with(' ')
    {
        let untracked = PanelGitEntry {
            xy: "??".into(),
            path: file.path.clone(),
            ..PanelGitEntry::default()
        };
        result = exec
            .exec(
                "git",
                git_diff_args_with(&untracked, literal_switch),
                root,
                GIT_TIMEOUT_MS,
            )
            .await;
    }
    let Ok(result) = result else {
        return diff_error();
    };
    if result.stdout.is_empty() && result.code != 0 && result.code != 1 {
        return vec![PanelRow::colored(
            format!("git diff failed (exit {})", result.code),
            PanelColor::Error,
        )];
    }
    nonempty_diff(colorize_diff(&result.stdout))
}
fn diff_error() -> Vec<PanelRow> {
    vec![PanelRow::colored(
        "git diff could not be run",
        PanelColor::Error,
    )]
}
fn nonempty_diff(rows: Vec<PanelRow>) -> Vec<PanelRow> {
    if rows.is_empty() {
        vec![PanelRow::colored("no textual change", PanelColor::Muted)]
    } else {
        rows
    }
}
pub fn colorize_diff(output: &str) -> Vec<PanelRow> {
    output
        .split('\n')
        .filter(|s| !s.is_empty())
        .map(|line| {
            let color = if line.starts_with("+++")
                || line.starts_with("---")
                || [
                    "diff ",
                    "index ",
                    "new file mode",
                    "deleted file mode",
                    "old mode",
                    "new mode",
                    "similarity index",
                    "dissimilarity index",
                    "rename from",
                    "rename to",
                    "copy from",
                    "copy to",
                    "Binary files",
                ]
                .iter()
                .any(|p| line.starts_with(p))
            {
                PanelColor::Muted
            } else if line.starts_with("@@") {
                PanelColor::Accent
            } else if line.starts_with('+') {
                PanelColor::Success
            } else if line.starts_with('-') {
                PanelColor::Error
            } else {
                PanelColor::Text
            };
            PanelRow::colored(line, color)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Stop, TerminalHandler, WidgetFactory};
    use std::sync::Mutex;
    #[derive(Default)]
    struct Ui {
        notices: Mutex<Vec<String>>,
        options: Mutex<Vec<String>>,
        rows: Mutex<Vec<PanelRow>>,
        choice: Mutex<Option<usize>>,
    }
    impl PanelUi for Ui {
        fn set_widget(&self, _: &str, _: Option<WidgetFactory>, _: bool) {}
        fn notify(&self, message: &str, _: &str) {
            self.notices.lock().expect("lock").push(message.into());
        }
        fn select<'a>(
            &'a self,
            _: &'a str,
            options: Vec<String>,
        ) -> PanelFuture<'a, Option<String>> {
            let choice = self
                .choice
                .lock()
                .expect("lock")
                .and_then(|i| options.get(i).cloned());
            *self.options.lock().expect("lock") = options;
            Box::pin(async move { choice })
        }
        fn viewer<'a>(&'a self, _: &'a str, rows: Vec<PanelRow>) -> PanelFuture<'a, ()> {
            *self.rows.lock().expect("lock") = rows;
            Box::pin(async {})
        }
        fn on_terminal_input(&self, _: TerminalHandler) -> Option<Stop> {
            None
        }
    }
    struct Exec(usize);
    impl PanelExec for Exec {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: Vec<String>,
            _: &'a str,
            _: u64,
        ) -> PanelFuture<'a, Result<ExecOutput, String>> {
            Box::pin(async move {
                Ok(ExecOutput {
                    stdout: (0..self.0)
                        .map(|i| format!("+line {i}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    code: 0,
                })
            })
        }
    }
    fn status() -> PanelGitStatus {
        PanelGitStatus {
            root: "/repo".into(),
            files: vec![
                PanelGitEntry {
                    xy: " M".into(),
                    path: "tracked.txt".into(),
                    added: Some(2),
                    removed: Some(1),
                    ..PanelGitEntry::default()
                },
                PanelGitEntry {
                    xy: "??".into(),
                    path: "brand-new.txt".into(),
                    ..PanelGitEntry::default()
                },
            ],
        }
    }
    #[test]
    fn select_then_diff_and_cap() {
        futures::executor::block_on(async {
            let ui = Arc::new(Ui::default());
            *ui.choice.lock().expect("lock") = Some(0);
            let ctx = PanelCommandContext {
                mode: Some("tui".into()),
                ui: Some(ui.clone()),
            };
            let status = status();
            let exec = Exec(DIFF_VIEWER_ROW_CAP + 3);
            run_diff_command(
                PanelCommandDeps {
                    status: Some(&status),
                    exec: Some(&exec),
                    mounted: true,
                },
                &ctx,
            )
            .await;
            assert_eq!(
                *ui.options.lock().expect("lock"),
                vec![" M tracked.txt  +2/-1", "?? brand-new.txt"]
            );
            let rows = ui.rows.lock().expect("lock");
            assert_eq!(rows.len(), DIFF_VIEWER_ROW_CAP + 1);
            assert_eq!(rows.last().expect("last").text, "... 3 more lines");
        });
    }
    #[test]
    fn guards_and_dismissal_do_not_open_viewer() {
        futures::executor::block_on(async {
            let ui = Arc::new(Ui::default());
            let ctx = PanelCommandContext {
                mode: None,
                ui: Some(ui.clone()),
            };
            let status = status();
            let exec = Exec(2);
            for deps in [
                PanelCommandDeps {
                    status: Some(&status),
                    exec: Some(&exec),
                    mounted: false,
                },
                PanelCommandDeps {
                    status: None,
                    exec: Some(&exec),
                    mounted: true,
                },
                PanelCommandDeps {
                    status: Some(&status),
                    exec: None,
                    mounted: true,
                },
                PanelCommandDeps {
                    status: Some(&status),
                    exec: Some(&exec),
                    mounted: true,
                },
            ] {
                run_diff_command(deps, &ctx).await;
            }
            assert_eq!(ui.notices.lock().expect("lock").len(), 3);
            assert!(ui.rows.lock().expect("lock").is_empty());
        });
    }
    #[test]
    fn labels_remove_control_bytes() {
        let file = PanelGitEntry {
            xy: "??".into(),
            path: "evil\x1b]8;;x\x07\u{202e}name.txt".into(),
            ..PanelGitEntry::default()
        };
        assert_eq!(file_option_label(&file), "?? evil]8;;xname.txt");
    }
    fn entry(xy: &str, path: &str, from: Option<&str>) -> PanelGitEntry {
        PanelGitEntry {
            xy: xy.into(),
            path: path.into(),
            from: from.map(str::to_owned),
            ..PanelGitEntry::default()
        }
    }
    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }
    fn reply(stdout: &str, code: i32) -> ExecOutput {
        ExecOutput {
            stdout: stdout.into(),
            code,
        }
    }
    struct Recorder {
        calls: Mutex<Vec<Vec<String>>>,
        replies: Mutex<std::collections::VecDeque<ExecOutput>>,
    }
    impl Recorder {
        fn new(replies: Vec<ExecOutput>) -> Self {
            Self {
                calls: Mutex::default(),
                replies: Mutex::new(replies.into()),
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().expect("lock").clone()
        }
    }
    impl PanelExec for Recorder {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            args: Vec<String>,
            _: &'a str,
            _: u64,
        ) -> PanelFuture<'a, Result<ExecOutput, String>> {
            self.calls.lock().expect("lock").push(args);
            let reply = self
                .replies
                .lock()
                .expect("lock")
                .pop_front()
                .unwrap_or_else(|| reply("", 0));
            Box::pin(async move { Ok(reply) })
        }
    }
    #[test]
    fn tracked_paths_carry_per_pathspec_literal_magic_and_never_the_global_flag() {
        assert_eq!(
            git_diff_args_with(&entry(" M", "tracked.txt", None), false),
            argv(&["diff", "HEAD", "-M", "--", ":(literal)tracked.txt"])
        );
        assert_eq!(
            git_diff_args_with(&entry("R ", "renamed-new.txt", Some("renamed.txt")), false),
            argv(&[
                "diff",
                "HEAD",
                "-M",
                "--",
                ":(literal)renamed.txt",
                ":(literal)renamed-new.txt"
            ])
        );
        assert_eq!(
            git_diff_args_with(&entry("??", "brand-new.txt", None), false),
            argv(&["diff", "--no-index", "--", "/dev/null", "brand-new.txt"])
        );
        let wildcard = git_diff_args_with(&entry(" M", "match*.txt", None), false);
        assert_eq!(
            wildcard.last().map(String::as_str),
            Some(":(literal)match*.txt")
        );
        assert!(!wildcard.iter().any(|arg| arg == "--literal-pathspecs"));
    }
    #[test]
    fn an_exported_literal_switch_keeps_the_paths_bare() {
        assert_eq!(
            git_diff_args_with(&entry(" M", "tracked.txt", None), true),
            argv(&["diff", "HEAD", "-M", "--", "tracked.txt"])
        );
        for (value, on) in [
            (None, false),
            (Some(""), false),
            (Some("0"), false),
            (Some(" False "), false),
            (Some("NO"), false),
            (Some("off"), false),
            (Some("1"), true),
            (Some("true"), true),
            (Some("yes"), true),
            (Some("On"), true),
        ] {
            assert_eq!(literal_switch_is_on(value), on, "{value:?}");
        }
    }
    #[test]
    fn a_staged_addition_before_the_first_commit_falls_back_to_the_null_diff() {
        futures::executor::block_on(async {
            let exec = Recorder::new(vec![reply("", 128), reply("+++ b/new.txt\n+hello", 1)]);
            let rows =
                read_git_diff_with(&exec, "/repo", &entry("A ", "new.txt", None), false).await;
            assert_eq!(
                exec.calls(),
                vec![
                    argv(&["diff", "HEAD", "-M", "--", ":(literal)new.txt"]),
                    argv(&["diff", "--no-index", "--", "/dev/null", "new.txt"]),
                ]
            );
            let text: Vec<_> = rows.iter().map(|row| row.text.as_str()).collect();
            assert_eq!(text, ["+++ b/new.txt", "+hello"]);
        });
    }
    #[test]
    fn an_untracked_directory_is_listed_with_literal_magic_and_each_file_diffed_plain() {
        futures::executor::block_on(async {
            let exec = Recorder::new(vec![
                reply("build/a.txt\0build/b.txt\0", 0),
                reply("+content", 1),
                reply("+content", 1),
            ]);
            read_git_diff_with(&exec, "/repo", &entry("??", "build/", None), false).await;
            assert_eq!(
                exec.calls(),
                vec![
                    argv(&[
                        "ls-files",
                        "--others",
                        "--exclude-standard",
                        "-z",
                        "--",
                        ":(literal)build/"
                    ]),
                    argv(&["diff", "--no-index", "--", "/dev/null", "build/a.txt"]),
                    argv(&["diff", "--no-index", "--", "/dev/null", "build/b.txt"]),
                ]
            );
        });
    }
    struct RealGit(Vec<(&'static str, &'static str)>);
    impl PanelExec for RealGit {
        fn exec<'a>(
            &'a self,
            command: &'a str,
            args: Vec<String>,
            cwd: &'a str,
            _: u64,
        ) -> PanelFuture<'a, Result<ExecOutput, String>> {
            Box::pin(async move {
                let output = std::process::Command::new(command)
                    .args(&args)
                    .current_dir(cwd)
                    .envs(self.0.iter().copied())
                    .output()
                    .map_err(|error| error.to_string())?;
                Ok(ExecOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    code: output.status.code().unwrap_or(1),
                })
            })
        }
    }
    #[test]
    fn real_git_returns_the_diff_while_a_competing_pathspec_switch_is_exported() {
        let root = std::env::temp_dir().join(format!(
            "omo_panel_diff_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("fixture dir");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("git runs");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("tracked.txt"), "a\n").expect("write");
        git(&["add", "tracked.txt"]);
        git(&[
            "-c",
            "user.name=panel",
            "-c",
            "user.email=panel@test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "init",
        ]);
        std::fs::write(root.join("tracked.txt"), "a\nb\n").expect("write");
        let cwd = root.to_str().expect("utf-8 path").to_owned();
        for variable in ["GIT_GLOB_PATHSPECS", "GIT_ICASE_PATHSPECS"] {
            let exec = RealGit(vec![(variable, "1")]);
            let rows = futures::executor::block_on(read_git_diff_with(
                &exec,
                &cwd,
                &entry(" M", "tracked.txt", None),
                false,
            ));
            assert!(
                rows.iter().any(|row| row.text == "+b"),
                "{variable}: {rows:?}"
            );
        }
        let _removed = std::fs::remove_dir_all(&root);
    }
}
