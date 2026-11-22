//! The `BROWSER` helper of one login (senpi `claude-cli-browser-capture.ts`, senpi#4437 and senpi#4654).
//!
//! `Capture` appends the URL the CLI wants to open to a private sink, so the host can hand it to the client (the
//! desktop opens it on the user's machine); `Discard` swallows it for copy-code login. The helper runs nothing but
//! that one write. The sink is read whenever the CLI prints or exits: the native engine carries no file watcher.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserCaptureMode {
    Capture,
    Discard,
}

pub struct ClaudeCliBrowserCapture {
    dir: tempfile::TempDir,
    sink_path: PathBuf,
    helper_path: PathBuf,
    consumed: usize,
}

fn sh_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The helper script. Windows: delayed expansion keeps `&` and `?` in the URL literal and `echo(` prints even an empty
/// argument. The CLI starts the helper in its own cwd (the account dir) and does not wait for it; Windows cannot delete
/// a directory that a live process has as its cwd, so the helper leaves it before `setlocal`. The helper never holds
/// the account dir after the URL is seen (senpi#4654).
pub fn helper_script(mode: BrowserCaptureMode, sink_path: &str, windows: bool) -> String {
    match (windows, mode) {
        (true, BrowserCaptureMode::Discard) => "@exit /b 0\r\n".to_owned(),
        (true, BrowserCaptureMode::Capture) => format!(
            "@echo off\r\ncd /d \"%SystemRoot%\"\r\nsetlocal EnableDelayedExpansion\r\nset \"u=%~1\"\r\n>>\"{sink_path}\" echo(!u!\r\n"
        ),
        (false, BrowserCaptureMode::Discard) => "#!/bin/sh\nexit 0\n".to_owned(),
        (false, BrowserCaptureMode::Capture) => format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> {}\n",
            sh_single_quoted(sink_path)
        ),
    }
}

#[cfg(unix)]
fn write_helper(path: &Path, script: &str) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(path)?;
    file.write_all(script.as_bytes())?;
    file.flush()
}

#[cfg(not(unix))]
fn write_helper(path: &Path, script: &str) -> std::io::Result<()> {
    std::fs::write(path, script)
}

impl ClaudeCliBrowserCapture {
    pub fn create(mode: BrowserCaptureMode) -> std::io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("senpi-claude-browser-")
            .tempdir()?;
        let sink_path = dir.path().join("urls");
        let helper_path = dir.path().join(if cfg!(windows) {
            "browser.cmd"
        } else {
            "browser"
        });
        let script = helper_script(mode, &sink_path.to_string_lossy(), cfg!(windows));
        write_helper(&helper_path, &script)?;
        Ok(Self {
            dir,
            sink_path,
            helper_path,
            consumed: 0,
        })
    }

    pub fn helper_path(&self) -> &Path {
        &self.helper_path
    }

    /// Hands `on_url` every URL written since the last call: only complete lines count, blank lines never do.
    pub fn flush(&mut self, on_url: &mut dyn FnMut(&str)) {
        let Ok(text) = std::fs::read_to_string(&self.sink_path) else {
            return;
        };
        let Some(end) = text.rfind('\n') else {
            return;
        };
        let lines: Vec<&str> = text[..=end]
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        for line in lines.iter().skip(self.consumed) {
            on_url(line);
        }
        self.consumed = lines.len();
    }

    /// A browser helper can outlive the CLI even after its stdio closes. Its absolute sink path is revoked before
    /// deletion: late opens fail, already-open files unlink safely. Cleanup never prevents the login outcome.
    pub fn dispose(self) {
        let original = self.dir.path().to_path_buf();
        let mut revoked = original.clone().into_os_string();
        revoked.push("-disposed");
        let revoked = PathBuf::from(revoked);
        let target = if std::fs::rename(&original, &revoked).is_ok() {
            revoked
        } else {
            original
        };
        let _ = std::fs::remove_dir_all(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unix_helper_appends_its_first_argument_to_the_sink_and_quotes_the_path() {
        assert_eq!(
            helper_script(BrowserCaptureMode::Capture, "/tmp/it's here/urls", false),
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '/tmp/it'\\''s here/urls'\n"
        );
        assert_eq!(
            helper_script(BrowserCaptureMode::Discard, "/tmp/urls", false),
            "#!/bin/sh\nexit 0\n"
        );
    }

    #[test]
    fn the_windows_helper_leaves_the_account_dir_before_it_writes() {
        let script = helper_script(BrowserCaptureMode::Capture, r"C:\tmp\urls", true);
        assert!(script.starts_with(
            "@echo off\r\ncd /d \"%SystemRoot%\"\r\nsetlocal EnableDelayedExpansion\r\n"
        ));
        assert!(script.ends_with(">>\"C:\\tmp\\urls\" echo(!u!\r\n"));
        assert_eq!(
            helper_script(BrowserCaptureMode::Discard, r"C:\tmp\urls", true),
            "@exit /b 0\r\n"
        );
    }

    #[test]
    fn only_complete_non_blank_sink_lines_are_handed_over_once() {
        let mut capture =
            ClaudeCliBrowserCapture::create(BrowserCaptureMode::Capture).expect("capture");
        let mut seen: Vec<String> = Vec::new();
        capture.flush(&mut |url| seen.push(url.to_owned()));
        assert!(seen.is_empty());

        std::fs::write(
            &capture.sink_path,
            "https://one.example/a?x=1&y=2\n\n  \nhttps://two.ex",
        )
        .expect("sink");
        capture.flush(&mut |url| seen.push(url.to_owned()));
        assert_eq!(seen, ["https://one.example/a?x=1&y=2"]);

        std::fs::write(
            &capture.sink_path,
            "https://one.example/a?x=1&y=2\n\n  \nhttps://two.example/b\r\n",
        )
        .expect("sink");
        capture.flush(&mut |url| seen.push(url.to_owned()));
        capture.flush(&mut |url| seen.push(url.to_owned()));
        assert_eq!(
            seen,
            ["https://one.example/a?x=1&y=2", "https://two.example/b"]
        );
        capture.dispose();
    }

    #[test]
    fn disposing_removes_the_helper_and_its_sink() {
        let capture =
            ClaudeCliBrowserCapture::create(BrowserCaptureMode::Discard).expect("capture");
        let helper = capture.helper_path().to_path_buf();
        assert!(helper.exists());
        capture.dispose();
        assert!(!helper.exists());
        assert!(!helper.parent().is_some_and(Path::exists));
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_helper_runs_and_records_the_url_it_was_given() {
        let mut capture =
            ClaudeCliBrowserCapture::create(BrowserCaptureMode::Capture).expect("capture");
        let status = std::process::Command::new(capture.helper_path())
            .arg("https://claude.ai/oauth/authorize?code=true&state=s")
            .status()
            .expect("helper runs");
        assert!(status.success());
        let mut seen: Vec<String> = Vec::new();
        capture.flush(&mut |url| seen.push(url.to_owned()));
        assert_eq!(
            seen,
            ["https://claude.ai/oauth/authorize?code=true&state=s"]
        );
        capture.dispose();
    }
}
