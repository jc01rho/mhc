//! `/remote` in the terminal UI (omo#13741, #14234): `/remote <verb> ...` runs the `mhc remote` verb and
//! shows its answer; a bare `/remote` walks from a device to a session to what to do with it. A close
//! refused because the session is running a turn offers "Close anyway".

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    Extension, ExtensionApi, ExtensionContext, ExtensionUiDialogOptions, NotificationType,
};
use serde_json::Value;

use crate::context::RemoteContext;
use crate::verbs::run_remote;

pub struct RemoteCommandComponent {
    pub version: String,
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

fn context_for(ctx: &ExtensionContext, version: &str, captured: &Captured) -> RemoteContext {
    let mut remote = RemoteContext::for_process(ctx.agent_dir.clone(), version);
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

/// Runs one verb and shows what it printed; the exit code.
async fn run_and_show(
    ctx: &ExtensionContext,
    remote: &RemoteContext,
    captured: &Captured,
    argv: &[String],
) -> i32 {
    let code = run_remote(remote, argv).await;
    let lines = drain(captured);
    let text = lines
        .iter()
        .map(|(_, line)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        let kind = if code == 0 {
            NotificationType::Info
        } else {
            NotificationType::Error
        };
        ctx.ui.notify(&text, kind);
    }
    code
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
    let labels: Vec<String> = rows
        .iter()
        .map(|row| {
            format!(
                "{} ({})",
                row["label"].as_str().unwrap_or_default(),
                row["device"].as_str().unwrap_or_default()
            )
        })
        .collect();
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
    let sessions: Vec<String> = listing["rows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    row["session_id"]
                        .as_str()
                        .map(|id| format!("{device}/{id}"))
                })
                .collect()
        })
        .unwrap_or_default();
    if sessions.is_empty() {
        ctx.ui
            .notify("No sessions on that device.", NotificationType::Info);
        return;
    }
    let Some(target) = ctx
        .ui
        .select("Session", &sessions, ExtensionUiDialogOptions::default())
        .await
    else {
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
        .select(&target, &actions, ExtensionUiDialogOptions::default())
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
    let code = run_and_show(ctx, remote, captured, &argv).await;
    if action == "Close" && code != 0 {
        let anyway = ctx
            .ui
            .confirm(
                "Close anyway?",
                &format!("{target} is running a turn. Close it anyway, ending that turn?"),
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
        api.register_command(
            "remote",
            Some("Devices and sessions on your other devices".into()),
            Some("[status|devices|sessions|send|model|interrupt|close|control|wake] ...".into()),
            Arc::new(move |args: &str, ctx: &ExtensionContext| {
                let version = version.clone();
                let argv = command_argv(args);
                Box::pin(async move {
                    let captured: Captured = Arc::default();
                    let remote = context_for(ctx, &version, &captured);
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
