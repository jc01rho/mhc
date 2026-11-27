//! `/remote` in the terminal UI (omo#13741, #14234): `/remote <verb> ...` runs the `mhc remote` verb and
//! shows its answer; a bare `/remote` walks from a device to a session to what to do with it. A close
//! refused because the session is running a turn offers "Close anyway". Every line, title and option it
//! shows is display-escaped (omo `989f0a8a06`); the argv a verb gets keeps the raw values.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    Extension, ExtensionApi, ExtensionContext, ExtensionUiDialogOptions, NotificationType,
};
use maho_omo_gateway::thread::remote::ports::escape_for_display;
use serde_json::Value;

use crate::context::RemoteContext;
use crate::verbs::{TURN_ACTIVE, run_remote};

pub struct RemoteCommandComponent {
    pub version: String,
    /// The device service's state dir; `None` is this process's (`device_service_state_dir`).
    pub state_dir: Option<PathBuf>,
}

/// The verb's argv from the command text: for `send` everything after the session is the message.
pub fn command_argv(args: &str) -> Vec<String> {
    let mut words = args.split_whitespace();
    let Some(verb) = words.next() else {
        return Vec::new();
    };
    if verb == "send" {
        let rest = args.trim_start()[verb.len()..].trim_start();
        let Some((target, text)) = rest.split_once(char::is_whitespace) else {
            return vec![verb.to_owned(), rest.to_owned()];
        };
        return vec![verb.to_owned(), target.to_owned(), text.trim().to_owned()];
    }
    std::iter::once(verb.to_owned())
        .chain(words.map(str::to_owned))
        .collect()
}

type Captured = Arc<Mutex<Vec<(bool, String)>>>;

fn context_for(
    ctx: &ExtensionContext,
    version: &str,
    state_dir: Option<&Path>,
    captured: &Captured,
) -> RemoteContext {
    let mut remote = RemoteContext::for_process(ctx.agent_dir.clone(), version);
    if let Some(state_dir) = state_dir {
        remote.state_dir = state_dir.to_path_buf();
    }
    let (out, err) = (Arc::clone(captured), Arc::clone(captured));
    remote.output = Arc::new(move |line: &str| {
        out.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((false, line.to_owned()));
    });
    remote.error = Arc::new(move |line: &str| {
        err.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((true, line.to_owned()));
    });
    remote.cold_wake = None;
    remote.mhc_bin = None;
    remote
}

fn drain(captured: &Captured) -> Vec<(bool, String)> {
    std::mem::take(&mut *captured.lock().unwrap_or_else(PoisonError::into_inner))
}

/// What a verb printed, one escaped line per line it wrote (line feeds between them kept).
fn shown_lines(lines: &[(bool, String)]) -> String {
    lines
        .iter()
        .map(|(_, line)| escape_for_display(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One device row as the picker shows it.
fn device_option(row: &Value) -> String {
    escape_for_display(&format!(
        "{} ({})",
        row["label"].as_str().unwrap_or_default(),
        row["device"].as_str().unwrap_or_default()
    ))
}

/// The listed sessions of `device` as `(shown, target)`: the option is escaped, the target a verb gets stays raw.
fn session_options(device: &str, listing: &Value) -> Vec<(String, String)> {
    listing["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["session_id"].as_str())
                .map(|id| {
                    let target = format!("{device}/{id}");
                    (escape_for_display(&target), target)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Runs one verb and shows what it printed. True when it was a close refused because the session is
/// running a turn.
async fn run_and_show(
    ctx: &ExtensionContext,
    remote: &RemoteContext,
    captured: &Captured,
    argv: &[String],
) -> bool {
    let code = run_remote(remote, argv).await;
    let lines = drain(captured);
    let text = shown_lines(&lines);
    if !text.is_empty() {
        let kind = if code == 0 {
            NotificationType::Info
        } else {
            NotificationType::Error
        };
        ctx.ui.notify(&text, kind);
    }
    code != 0
        && argv.first().is_some_and(|verb| verb == "close")
        && lines
            .iter()
            .any(|(error, line)| *error && line.contains(TURN_ACTIVE))
}

/// One verb's `--json` value, run silently.
async fn json_of(remote: &RemoteContext, captured: &Captured, argv: &[&str]) -> Option<Value> {
    let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
    let code = run_remote(remote, &argv).await;
    let lines = drain(captured);
    if code != 0 {
        return None;
    }
    lines
        .iter()
        .find(|(error, _)| !error)
        .and_then(|(_, line)| serde_json::from_str(line).ok())
}

async fn pick(ctx: &ExtensionContext, remote: &RemoteContext, captured: &Captured) {
    let Some(devices) = json_of(remote, captured, &["devices", "--json"]).await else {
        ctx.ui
            .notify(crate::verbs::NOT_RUNNING, NotificationType::Error);
        return;
    };
    let rows = devices.as_array().cloned().unwrap_or_default();
    let labels: Vec<String> = rows.iter().map(device_option).collect();
    let Some(chosen) = ctx
        .ui
        .select(
            "Remote device",
            &labels,
            ExtensionUiDialogOptions::default(),
        )
        .await
    else {
        return;
    };
    let Some(device) = labels
        .iter()
        .position(|label| *label == chosen)
        .and_then(|index| rows[index]["device"].as_str())
        .map(str::to_owned)
    else {
        return;
    };
    let Some(listing) = json_of(
        remote,
        captured,
        &["sessions", "--device", device.as_str(), "--json"],
    )
    .await
    else {
        ctx.ui.notify(
            "Could not list that device's sessions.",
            NotificationType::Error,
        );
        return;
    };
    let sessions = session_options(&device, &listing);
    if sessions.is_empty() {
        ctx.ui
            .notify("No sessions on that device.", NotificationType::Info);
        return;
    }
    let shown: Vec<String> = sessions.iter().map(|(option, _)| option.clone()).collect();
    let Some(chosen) = ctx
        .ui
        .select("Session", &shown, ExtensionUiDialogOptions::default())
        .await
    else {
        return;
    };
    let Some((title, target)) = sessions.into_iter().find(|(option, _)| *option == chosen) else {
        return;
    };
    let actions = vec![
        "Send a message".to_owned(),
        "Interrupt".to_owned(),
        "Switch model".to_owned(),
        "Close".to_owned(),
    ];
    let Some(action) = ctx
        .ui
        .select(&title, &actions, ExtensionUiDialogOptions::default())
        .await
    else {
        return;
    };
    let argv: Vec<String> = match action.as_str() {
        "Send a message" => {
            let Some(text) = ctx
                .ui
                .input("Message", None, ExtensionUiDialogOptions::default())
                .await
                .filter(|text| !text.trim().is_empty())
            else {
                return;
            };
            vec!["send".into(), target.clone(), "--".into(), text]
        }
        "Interrupt" => vec!["interrupt".into(), target.clone()],
        "Switch model" => {
            let Some(model) = ctx
                .ui
                .input(
                    "Model (provider/id)",
                    None,
                    ExtensionUiDialogOptions::default(),
                )
                .await
                .filter(|model| model.contains('/'))
            else {
                return;
            };
            vec!["model".into(), target.clone(), model]
        }
        _ => vec!["close".into(), target.clone()],
    };
    let turn_active = run_and_show(ctx, remote, captured, &argv).await;
    if turn_active {
        let anyway = ctx
            .ui
            .confirm(
                "Close anyway?",
                &format!("{title} is running a turn. Close it anyway, ending that turn?"),
                ExtensionUiDialogOptions::default(),
            )
            .await;
        if anyway {
            run_and_show(
                ctx,
                remote,
                captured,
                &["close".into(), target, "--force".into()],
            )
            .await;
        }
    }
}

impl Extension for RemoteCommandComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let version = self.version.clone();
        let state_dir = self.state_dir.clone();
        api.register_command(
            "remote",
            Some("Devices and sessions on your other devices".into()),
            Some("[status|devices|sessions|send|model|interrupt|close|control|wake] ...".into()),
            Arc::new(move |args: &str, ctx: &ExtensionContext| {
                let version = version.clone();
                let state_dir = state_dir.clone();
                let argv = command_argv(args);
                Box::pin(async move {
                    let captured: Captured = Arc::default();
                    let remote = context_for(ctx, &version, state_dir.as_deref(), &captured);
                    if argv
                        .first()
                        .is_some_and(|verb| verb == "daemon" || verb == "bootstrap")
                    {
                        ctx.ui.notify(
                            "Run `mhc remote daemon` or `mhc remote bootstrap` in a terminal.",
                            NotificationType::Info,
                        );
                    } else if argv.is_empty() {
                        pick(ctx, &remote, &captured).await;
                    } else {
                        run_and_show(ctx, &remote, &captured, &argv).await;
                    }
                    Ok(())
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // omo 989f0a8a06 / 944c86d6a4 (command-display-escape.test.ts): what /remote shows carries no control, format or
    // separator character, ordinary text appears unchanged, and the target a verb is run with keeps the raw value.
    #[test]
    fn the_picker_shows_escaped_options_and_keeps_the_raw_target() {
        assert_eq!(
            device_option(&json!({ "label": "a\u{1b}[31m", "device": "d\u{202e}1" })),
            "a\\u001b[31m (d\\u202e1)"
        );
        assert_eq!(
            device_option(&json!({ "label": "Studio 한글", "device": "dev-1" })),
            "Studio 한글 (dev-1)"
        );
        assert_eq!(
            session_options(
                "dev\u{200b}",
                &json!({ "rows": [{ "session_id": "s\u{2028}1" }, { "title": "no id" }] })
            ),
            vec![(
                "dev\\u200b/s\\u20281".to_owned(),
                "dev\u{200b}/s\u{2028}1".to_owned()
            )]
        );
    }

    #[test]
    fn a_verbs_output_is_shown_line_by_line_escaped() {
        let lines = vec![
            (false, "ok \u{1b}]0;x\u{7}".to_owned()),
            (true, "line\rtwo".to_owned()),
            (false, "plain".to_owned()),
        ];
        assert_eq!(
            shown_lines(&lines),
            "ok \\u001b]0;x\\u0007\nline\\u000dtwo\nplain"
        );
    }
}
