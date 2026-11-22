//! `claude auth login --claudeai` for one account dir (senpi `claude-cli-auth.ts`, senpi#4437 and senpi#5357).
//!
//! The login runs on its own task: the host hears about the CLI's browser URL and its copy-code URL through
//! callbacks, hands a pasted code to [`ClaudeCliLogin::submit_code`], and awaits [`ClaudeCliLogin::result`].
use crate::{
    claude_cli_browser_capture::{BrowserCaptureMode, ClaudeCliBrowserCapture},
    claude_cli_process::{ClaudeCliRun, spawn_claude_cli},
};
use maho_ai::utils::abort::AbortSignal;
use regex::Regex;
use std::{
    borrow::Cow,
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
    time::Instant,
};

/// The method select's two answers: the one-click browser flow, or a URL plus a pasted code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginMethod {
    Browser,
    CopyCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginFailure {
    OrgNotAllowed,
    ProviderNotAllowed,
    OauthFailed,
    UnexpectedExit,
    SpawnFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaudeCliLoginResult {
    Success,
    Failed(ClaudeCliLoginFailure),
    Cancelled,
    Timeout,
}

pub type UrlCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// Resets on every pasted code; matches the desktop's login completion budget.
pub const LOGIN_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long the pipes may stay open after the CLI exited before the login settles anyway.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

pub struct ClaudeCliLoginOptions {
    pub run: ClaudeCliRun,
    pub method: ClaudeCliLoginMethod,
    pub signal: Option<AbortSignal>,
    pub idle_timeout: Duration,
    /// The localhost-redirect URL the CLI hands `$BROWSER` (browser method only).
    pub on_automatic_url: Option<UrlCallback>,
    /// The copy-code URL the CLI prints for the paste fallback.
    pub on_manual_url: Option<UrlCallback>,
}

impl ClaudeCliLoginOptions {
    pub fn new(run: ClaudeCliRun, method: ClaudeCliLoginMethod) -> Self {
        Self {
            run,
            method,
            signal: None,
            idle_timeout: LOGIN_IDLE_TIMEOUT,
            on_automatic_url: None,
            on_manual_url: None,
        }
    }
}

pub struct ClaudeCliLogin {
    codes: mpsc::UnboundedSender<String>,
    result: tokio::task::JoinHandle<ClaudeCliLoginResult>,
}

impl ClaudeCliLogin {
    /// Writes a pasted code to the CLI's stdin (never argv). `false` when the text holds no code or the CLI is gone.
    pub fn submit_code(&self, input: &str) -> bool {
        match normalize_pasted_code(input) {
            Some(line) => self.codes.send(line).is_ok(),
            None => false,
        }
    }

    pub async fn result(self) -> ClaudeCliLoginResult {
        self.result.await.unwrap_or(ClaudeCliLoginResult::Failed(
            ClaudeCliLoginFailure::UnexpectedExit,
        ))
    }
}

/// OSC sequences (an OSC 8 hyperlink is `ESC ] 8 ; ; <uri> ST <text> ESC ] 8 ; ; ST`, ST being BEL or `ESC \`) and CSI
/// sequences (colors), removed explicitly so the visible text is left: Claude's copy-code URL joins its scopes with
/// `+`, which a generic control-character stripper can leave half in place.
static TERMINAL_SEQUENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-?]*[ -/]*[@-~]")
        .expect("terminal sequence pattern")
});

static MANUAL_URL_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"If the browser didn't open, visit: (https://\S+)\r?\n")
        .expect("manual url pattern")
});

static SUCCESS_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^(?:Paste code here if prompted > )?Login successful\.\r?$")
        .expect("login success pattern")
});

/// The copy-code URL is shown to the user to open, so only Claude's own sign-in hosts are passed on.
const MANUAL_URL_HOSTS: [&str; 4] = [
    "claude.ai",
    "claude.com",
    "platform.claude.com",
    "console.anthropic.com",
];

fn visible_text(output: &str) -> Cow<'_, str> {
    TERMINAL_SEQUENCE.replace_all(output, "")
}

/// An https URL on one of Claude's sign-in hosts, with no userinfo and no explicit port.
pub fn is_claude_sign_in_url(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.contains(':') {
        return false;
    }
    MANUAL_URL_HOSTS.contains(&authority.to_ascii_lowercase().as_str())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (b'+', _) => {
                decoded.push(b' ');
                index += 1;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| percent_decode(name) == key)
        .map(|(_, value)| percent_decode(value))
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

/// Accepts `code#state`, a bare code, a query string, or the full callback URL; returns the line the CLI reads.
pub fn normalize_pasted_code(input: &str) -> Option<String> {
    let value = input.trim();
    if value.is_empty() || value.contains(['\r', '\n']) {
        return None;
    }
    let query = if is_http_url(value) {
        let without_fragment = value.split('#').next().unwrap_or_default();
        without_fragment
            .split_once('?')
            .map_or("", |(_, query)| query)
    } else if !value.contains('#') && value.contains("code=") {
        value
    } else {
        return Some(value.to_owned());
    };
    let code = query_value(query, "code").filter(|code| !code.is_empty())?;
    match query_value(query, "state").filter(|state| !state.is_empty()) {
        Some(state) => Some(format!("{code}#{state}")),
        None => Some(code),
    }
}

fn failure_reason(stderr: &str) -> ClaudeCliLoginFailure {
    if stderr.contains("cli_auth_login_org_not_allowed") {
        ClaudeCliLoginFailure::OrgNotAllowed
    } else if stderr.contains("cli_auth_login_provider_not_allowed") {
        ClaudeCliLoginFailure::ProviderNotAllowed
    } else if stderr.contains("OAuth login failed") {
        ClaudeCliLoginFailure::OauthFailed
    } else {
        ClaudeCliLoginFailure::UnexpectedExit
    }
}

#[cfg(unix)]
fn create_account_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_account_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

#[derive(Clone, Copy)]
enum Ending {
    Cancelled,
    Timeout,
}

async fn wait_for_abort(signal: &Option<AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

async fn drive_login(
    options: ClaudeCliLoginOptions,
    mut codes: mpsc::UnboundedReceiver<String>,
) -> ClaudeCliLoginResult {
    let spawn_failed = ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::SpawnFailed);
    let mode = match options.method {
        ClaudeCliLoginMethod::Browser => BrowserCaptureMode::Capture,
        ClaudeCliLoginMethod::CopyCode => BrowserCaptureMode::Discard,
    };
    let Ok(mut capture) = ClaudeCliBrowserCapture::create(mode) else {
        return spawn_failed;
    };
    if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
        capture.dispose();
        return ClaudeCliLoginResult::Cancelled;
    }
    if create_account_dir(&options.run.config_dir).is_err() {
        capture.dispose();
        return spawn_failed;
    }
    let spawned = spawn_claude_cli(
        &options.run,
        &["auth", "login", "--claudeai"],
        Some(capture.helper_path()),
    );
    let Ok(mut child) = spawned else {
        capture.dispose();
        return spawn_failed;
    };
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        capture.dispose();
        return spawn_failed;
    };

    let on_automatic = options.on_automatic_url.clone();
    let on_manual = options.on_manual_url.clone();
    let mut out_bytes: Vec<u8> = Vec::new();
    let mut err_bytes: Vec<u8> = Vec::new();
    let mut out_chunk = [0_u8; 4096];
    let mut err_chunk = [0_u8; 4096];
    let (mut stdout_open, mut stderr_open, mut codes_open) = (true, true, true);
    let mut manual_seen = false;
    let mut ending: Option<Ending> = None;
    let mut exit_status: Option<std::io::Result<std::process::ExitStatus>> = None;
    let idle = tokio::time::sleep(options.idle_timeout);
    tokio::pin!(idle);
    let drain = tokio::time::sleep(DRAIN_AFTER_EXIT);
    tokio::pin!(drain);

    loop {
        tokio::select! {
            biased;
            read = stdout.read(&mut out_chunk), if stdout_open => match read {
                Ok(count) if count > 0 => {
                    out_bytes.extend_from_slice(&out_chunk[..count]);
                    capture.flush(&mut |url: &str| {
                        if let Some(callback) = &on_automatic {
                            callback(url);
                        }
                    });
                    if !manual_seen {
                        let text = String::from_utf8_lossy(&out_bytes);
                        let visible = visible_text(&text);
                        if let Some(found) = MANUAL_URL_LINE
                            .captures(&visible)
                            .and_then(|captures| captures.get(1))
                        {
                            manual_seen = true;
                            if is_claude_sign_in_url(found.as_str())
                                && let Some(callback) = &on_manual
                            {
                                callback(found.as_str());
                            }
                        }
                    }
                }
                _ => stdout_open = false,
            },
            read = stderr.read(&mut err_chunk), if stderr_open => match read {
                Ok(count) if count > 0 => err_bytes.extend_from_slice(&err_chunk[..count]),
                _ => stderr_open = false,
            },
            code = codes.recv(), if codes_open && ending.is_none() && exit_status.is_none() => match code {
                Some(line) => {
                    let _ = stdin.write_all(format!("{line}\n").as_bytes()).await;
                    let _ = stdin.flush().await;
                    idle.as_mut().reset(Instant::now() + options.idle_timeout);
                }
                None => codes_open = false,
            },
            () = &mut idle, if ending.is_none() && exit_status.is_none() => {
                ending = Some(Ending::Timeout);
                let _ = child.start_kill();
            },
            () = wait_for_abort(&options.signal), if ending.is_none() && exit_status.is_none() => {
                ending = Some(Ending::Cancelled);
                let _ = child.start_kill();
            },
            status = child.wait(), if exit_status.is_none() => {
                exit_status = Some(status);
                drain.as_mut().reset(Instant::now() + DRAIN_AFTER_EXIT);
            },
            () = &mut drain, if exit_status.is_some() => break,
        }
        if exit_status.is_some() && !stdout_open && !stderr_open {
            break;
        }
    }

    capture.flush(&mut |url: &str| {
        if let Some(callback) = &on_automatic {
            callback(url);
        }
    });
    let exit_code = exit_status
        .and_then(Result::ok)
        .and_then(|status| status.code());
    let outcome = match ending {
        Some(Ending::Cancelled) => ClaudeCliLoginResult::Cancelled,
        Some(Ending::Timeout) => ClaudeCliLoginResult::Timeout,
        None => {
            let stdout_text = String::from_utf8_lossy(&out_bytes);
            if exit_code == Some(0) && SUCCESS_LINE.is_match(&visible_text(&stdout_text)) {
                ClaudeCliLoginResult::Success
            } else if exit_code == Some(0) {
                ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::UnexpectedExit)
            } else {
                ClaudeCliLoginResult::Failed(failure_reason(&String::from_utf8_lossy(&err_bytes)))
            }
        }
    };
    capture.dispose();
    outcome
}

/// Runs `<claude> auth login --claudeai` for one account dir. `--console` and `--sso` are never passed, so an
/// API-billing login cannot happen here. Output, URLs and pasted codes are never logged. Must be called inside a tokio
/// runtime: the login drives the child on its own task.
pub fn start_claude_cli_login(options: ClaudeCliLoginOptions) -> ClaudeCliLogin {
    let (codes, receiver) = mpsc::unbounded_channel();
    let result = tokio::spawn(drive_login(options, receiver));
    ClaudeCliLogin { codes, result }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_code_is_reduced_to_the_line_the_cli_reads() {
        for (input, expected) in [
            ("abc#def", Some("abc#def")),
            ("  abc  ", Some("abc")),
            (
                "https://platform.claude.com/oauth/code/callback?code=abc&state=def",
                Some("abc#def"),
            ),
            ("code=abc&state=def", Some("abc#def")),
            ("code=a%20b+c", Some("a b c")),
            (
                "https://platform.claude.com/oauth/code/callback?code=abc#frag",
                Some("abc"),
            ),
            (
                "https://platform.claude.com/oauth/code/callback?state=def",
                None,
            ),
            ("", None),
            ("   ", None),
            ("abc\ndef", None),
        ] {
            assert_eq!(
                normalize_pasted_code(input).as_deref(),
                expected,
                "{input:?}"
            );
        }
    }

    #[test]
    fn only_claudes_own_sign_in_hosts_are_passed_on() {
        for accepted in [
            "https://claude.ai/oauth/authorize?code=true",
            "https://claude.com/x",
            "https://platform.claude.com/oauth/code/callback",
            "https://console.anthropic.com/",
            "https://CLAUDE.AI/oauth",
        ] {
            assert!(is_claude_sign_in_url(accepted), "{accepted}");
        }
        for rejected in [
            "https://claude.ai.evil.example/oauth/authorize?code=true&scope=user%3Aprofile+user%3Ainference",
            "https://claude.ai@evil.example/oauth/authorize?code=true",
            "https://evil.example/oauth/authorize?redirect=https%3A%2F%2Fclaude.ai",
            "http://claude.ai/oauth/authorize?code=true",
            "https://claude.ai:8443/oauth/authorize?code=true",
            "https://",
            "claude.ai",
        ] {
            assert!(!is_claude_sign_in_url(rejected), "{rejected}");
        }
    }

    #[test]
    fn terminal_sequences_are_removed_and_the_plus_in_a_scope_survives() {
        let url = "https://claude.ai/oauth/authorize?scope=user%3Aprofile+user%3Ainference&state=s";
        let bel = format!("\u{1b}]8;;{url}\u{7}{url}\u{1b}]8;;\u{7}");
        let st = format!("\u{1b}]8;;{url}\u{1b}\\{url}\u{1b}]8;;\u{1b}\\");
        let colored = "\u{1b}[32mLogin successful.\u{1b}[0m\r\n";
        assert_eq!(visible_text(&bel), url);
        assert_eq!(visible_text(&st), url);
        assert_eq!(visible_text(colored), "Login successful.\r\n");
    }

    #[test]
    fn a_success_line_may_follow_the_paste_prompt_but_never_another_prefix() {
        for shown in [
            "Paste code here if prompted > Login successful.\n",
            "Paste code here if prompted > \nLogin successful.\n",
            "Login successful.\r\n",
        ] {
            assert!(SUCCESS_LINE.is_match(shown), "{shown:?}");
        }
        assert!(!SUCCESS_LINE.is_match("\nError > Login successful.\n"));
        assert!(!SUCCESS_LINE.is_match("Login successfully\n"));
    }

    #[test]
    fn the_failure_names_the_reason_stderr_gives() {
        assert_eq!(
            failure_reason("OAuth login failed: cli_auth_login_org_not_allowed"),
            ClaudeCliLoginFailure::OrgNotAllowed
        );
        assert_eq!(
            failure_reason("cli_auth_login_provider_not_allowed"),
            ClaudeCliLoginFailure::ProviderNotAllowed
        );
        assert_eq!(
            failure_reason("OAuth login failed: boom"),
            ClaudeCliLoginFailure::OauthFailed
        );
        assert_eq!(failure_reason(""), ClaudeCliLoginFailure::UnexpectedExit);
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use crate::claude_cli_test_support::Fixture;
    use maho_ai::utils::abort::AbortController;

    const MANUAL_URL: &str = "https://claude.ai/oauth/authorize?code=true&client_id=fixture&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&scope=user%3Aprofile+user%3Ainference&state=fixture-state";

    fn url_channel() -> (UrlCallback, mpsc::UnboundedReceiver<String>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let callback: UrlCallback = Arc::new(move |url: &str| {
            let _ = sender.send(url.to_owned());
        });
        (callback, receiver)
    }

    fn drained(receiver: &mut mpsc::UnboundedReceiver<String>) -> Vec<String> {
        let mut urls = Vec::new();
        while let Ok(url) = receiver.try_recv() {
            urls.push(url);
        }
        urls
    }

    async fn settled(login: ClaudeCliLogin) -> ClaudeCliLoginResult {
        tokio::time::timeout(Duration::from_secs(60), login.result())
            .await
            .expect("the login settles in time")
    }

    fn process_is_gone(pid: &str) -> bool {
        !std::process::Command::new("kill")
            .args(["-0", pid])
            .status()
            .expect("kill runs")
            .success()
    }

    #[tokio::test]
    async fn browser_login_captures_both_urls_for_every_output_shape() {
        for mode in [
            "login-auto",
            "login-auto-inline",
            "login-auto-terminal",
            "login-auto-bel",
        ] {
            let fixture = Fixture::new(mode, &[]);
            let (on_automatic, mut automatic) = url_channel();
            let (on_manual, mut manual) = url_channel();
            let mut options =
                ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
            options.on_automatic_url = Some(on_automatic);
            options.on_manual_url = Some(on_manual);

            let result = settled(start_claude_cli_login(options)).await;

            assert_eq!(result, ClaudeCliLoginResult::Success, "{mode}");
            let automatic = drained(&mut automatic);
            assert_eq!(automatic.len(), 1, "{mode}");
            assert!(
                automatic[0].contains("redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback"),
                "{mode}"
            );
            assert_eq!(drained(&mut manual), [MANUAL_URL], "{mode}");
        }
    }

    #[tokio::test]
    async fn a_copy_code_url_outside_claudes_sign_in_hosts_is_never_passed_on() {
        let fixture = Fixture::new(
            "login-auto-bel",
            &[(
                "FAKE_CLAUDE_MANUAL_URL",
                "https://claude.ai.evil.example/oauth/authorize?code=true",
            )],
        );
        let (on_manual, mut manual) = url_channel();
        let mut options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
        options.on_manual_url = Some(on_manual);

        let result = settled(start_claude_cli_login(options)).await;

        assert_eq!(result, ClaudeCliLoginResult::Success);
        assert!(drained(&mut manual).is_empty());
    }

    #[tokio::test]
    async fn the_paste_fallback_writes_the_code_only_to_stdin() {
        for mode in ["login-paste", "login-paste-terminal"] {
            let fixture = Fixture::new(mode, &[]);
            let (on_manual, mut manual) = url_channel();
            let mut options =
                ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
            options.on_manual_url = Some(on_manual);
            let login = start_claude_cli_login(options);

            manual.recv().await.expect("the copy-code url arrives");
            assert!(login.submit_code(
                "https://platform.claude.com/oauth/code/callback?code=abc123&state=st456"
            ));
            let result = settled(login).await;

            assert_eq!(result, ClaudeCliLoginResult::Success, "{mode}");
            let record = fixture.recorded();
            assert!(record.contains("argv=auth login --claudeai\n"), "{record}");
            assert!(
                !record.contains("argv=auth login --claudeai abc123"),
                "{record}"
            );
            assert!(
                record.contains(&format!(
                    "config_dir={}\n",
                    fixture.run.config_dir.display()
                )),
                "{record}"
            );
            assert!(record.contains("stdin=abc123#st456\n"), "{record}");
        }
    }

    #[tokio::test]
    async fn an_incomplete_or_failed_login_is_rejected() {
        for mode in [
            "login-exit0-silent",
            "login-exit1-success",
            "login-auto-wrong-prefix",
        ] {
            let fixture = Fixture::new(mode, &[]);
            let (on_manual, mut manual) = url_channel();
            let mut options =
                ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
            options.on_manual_url = Some(on_manual);
            let login = start_claude_cli_login(options);

            manual.recv().await.expect("the copy-code url arrives");
            login.submit_code("code#state");

            assert_eq!(
                settled(login).await,
                ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::UnexpectedExit),
                "{mode}"
            );
        }
    }

    #[tokio::test]
    async fn an_org_refusal_maps_to_a_typed_reason() {
        let fixture = Fixture::new("login-org-denied", &[]);
        let options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);

        assert_eq!(
            settled(start_claude_cli_login(options)).await,
            ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::OrgNotAllowed)
        );
    }

    #[tokio::test]
    async fn copy_code_login_discards_the_one_click_url() {
        let fixture = Fixture::new("login-auto", &[]);
        let (on_automatic, mut automatic) = url_channel();
        let mut options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::CopyCode);
        options.on_automatic_url = Some(on_automatic);

        assert_eq!(
            settled(start_claude_cli_login(options)).await,
            ClaudeCliLoginResult::Success
        );
        assert!(drained(&mut automatic).is_empty());
    }

    #[tokio::test]
    async fn cancelling_kills_the_child() {
        let fixture = Fixture::new("login-hang", &[]);
        let controller = AbortController::new();
        let (on_manual, mut manual) = url_channel();
        let abort_on_url: UrlCallback = {
            let controller = controller.clone();
            Arc::new(move |url: &str| {
                on_manual(url);
                controller.abort(None);
            })
        };
        let mut options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
        options.signal = Some(controller.signal());
        options.on_manual_url = Some(abort_on_url);

        let result = settled(start_claude_cli_login(options)).await;

        assert_eq!(result, ClaudeCliLoginResult::Cancelled);
        assert_eq!(drained(&mut manual).len(), 1);
        let pid = fixture.recorded_pid();
        assert!(!pid.is_empty(), "the fixture recorded its pid");
        assert!(
            process_is_gone(&pid),
            "pid {pid} outlived the cancelled login"
        );
    }

    #[tokio::test]
    async fn the_idle_timeout_kills_the_child() {
        let fixture = Fixture::new("login-hang", &[]);
        let mut options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);
        options.idle_timeout = Duration::from_millis(1500);

        let result = settled(start_claude_cli_login(options)).await;

        assert_eq!(result, ClaudeCliLoginResult::Timeout);
        let pid = fixture.recorded_pid();
        assert!(!pid.is_empty(), "the fixture recorded its pid");
        assert!(
            process_is_gone(&pid),
            "pid {pid} outlived the timed-out login"
        );
    }

    #[tokio::test]
    async fn a_login_that_cannot_start_reports_spawn_failed() {
        let mut fixture = Fixture::new("login-auto", &[]);
        fixture.run.executable = fixture.run.config_dir.join("missing-claude");
        let options =
            ClaudeCliLoginOptions::new(fixture.run.clone(), ClaudeCliLoginMethod::Browser);

        assert_eq!(
            settled(start_claude_cli_login(options)).await,
            ClaudeCliLoginResult::Failed(ClaudeCliLoginFailure::SpawnFailed)
        );
    }
}
