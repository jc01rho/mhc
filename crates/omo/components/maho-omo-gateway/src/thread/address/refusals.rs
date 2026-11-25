//! Wording of `maho_thread_address_core` refusals and candidate addresses for the thread tools.
//!
//! Upstream counterpart: `refusals.ts` (#14305, #14280, #14270, #14487, #14386).

use maho_thread_address_core::{
    AddressRefusal, NotAskedReason, RefusalDetails, SilentReason, device_label, format_address,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::thread::contracts_results::ThreadStatus;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

/// A refusal as the thread tools answer it: a taxonomy code, the message, the recovery step and the details.
pub type ThreadRefusal = ThreadToolFailure;

/// The core's candidate shape on top of the ladder's: `address` (`<device id>/<thread>`) resolves as written and
/// `device_name` names the device. With no device id known the candidate carries no `address`: its `thread` is the
/// address then, and one field names a session (#14044).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressCandidate {
    pub thread: String,
    pub name: String,
    pub cwd: String,
    pub preview: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
    pub state: ThreadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unnamed: Option<bool>,
}

pub fn as_refusal(failure: ThreadToolFailure) -> ThreadRefusal {
    failure
}

pub fn as_failure(refused: ThreadRefusal) -> ThreadToolFailure {
    refused
}

const LIST_ALL: &str =
    "call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell)";
const RETRY_LIST: &str = "Retry the call; call thread_list (tool.thread_list() in an eval cell) to see which devices answered.";

fn labels(ids: &[String], names: &[String]) -> String {
    ids.iter()
        .enumerate()
        .map(|(index, id)| {
            let name = names.get(index).map_or("", String::as_str);
            device_label(id, name)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The core's plain refusal, worded for the thread tools; `thread` is the address part that missed, when one did.
pub fn tool_refusal(
    core: &AddressRefusal,
    thread: Option<&str>,
    answered: Option<&[String]>,
) -> ThreadRefusal {
    let details_value = serde_json::to_value(&core.details).ok();
    let typed = core.typed.as_deref().unwrap_or_default();
    match (&core.details, core.reason) {
        (RefusalDetails::Device { .. }, "no_device") => thread_tool_failure(
            "not_found",
            format!("No device named {typed} is signed in to this account."),
            format!("Pass a device name or id from candidates, or {LIST_ALL}."),
            details_value,
        ),
        (RefusalDetails::Device { candidates, .. }, "devices_share_name") => {
            let online_prefix = if candidates.iter().all(|row| row.online) {
                "online "
            } else {
                ""
            };
            thread_tool_failure(
                "ambiguous_target",
                format!(
                    "{} {online_prefix}devices are named {typed}; none was picked.",
                    candidates.len()
                ),
                "Pass the device id from candidates.",
                details_value,
            )
        }
        (RefusalDetails::Device { candidates, .. }, _) => thread_tool_failure(
            "ambiguous_target",
            format!(
                "{} devices have an id starting {typed}; none was picked.",
                candidates.len()
            ),
            "Pass more of the device id, or its name.",
            details_value,
        ),
        (RefusalDetails::Offline { .. }, _) => thread_tool_failure(
            "device_offline",
            format!("Device {typed} is offline."),
            "Retry when that device is online; nothing was queued.",
            details_value,
        ),
        (
            RefusalDetails::Silent {
                reason: SilentReason::ListingIncomplete,
                devices,
                device_names,
                ..
            },
            _,
        ) => thread_tool_failure(
            "host_unavailable",
            format!(
                "Nothing matched on the devices that answered; {} did not answer this listing round.",
                labels(devices, device_names)
            ),
            RETRY_LIST,
            details_value,
        ),
        (
            RefusalDetails::Silent {
                reason: SilentReason::DeviceUnanswered,
                devices,
                device_names,
                device_reasons,
            },
            _,
        ) => {
            let mut slow: Vec<String> = Vec::new();
            let mut unanswered: Vec<String> = Vec::new();
            for (index, id) in devices.iter().enumerate() {
                let label = device_label(id, device_names.get(index).map_or("", String::as_str));
                if device_reasons.get(id).map(String::as_str) == Some("lookup_timeout") {
                    slow.push(label);
                } else {
                    unanswered.push(label);
                }
            }
            let mut outcomes: Vec<String> = Vec::new();
            if !slow.is_empty() {
                let verb = if slow.len() == 1 { "was" } else { "were" };
                outcomes.push(format!(
                    "{} {verb} too slow for the lookup",
                    slow.join(", ")
                ));
            }
            if !unanswered.is_empty() {
                outcomes.push(format!("{} did not answer", unanswered.join(", ")));
            }
            let retry = if slow.is_empty() {
                let who = if unanswered.len() == 1 {
                    "that device answers"
                } else {
                    "those devices answer"
                };
                format!("Retry when {who}.")
            } else {
                "Retry the lookup; a slow device may answer in time on another attempt.".to_owned()
            };
            // omo#14270 / #14487: a miss on the device that really keeps the file reads as "wait for the silent one"
            // without this.
            let next = match answered {
                Some(list) if !list.is_empty() => format!(
                    "{retry} If the session should be on a device that answered, its file is not where that device looks for sessions."
                ),
                _ => retry,
            };
            let answered_clause = match answered {
                Some(list) if !list.is_empty() => format!(" ({})", list.join(", ")),
                _ => String::new(),
            };
            let what = match thread {
                Some(thread) => format!("session {thread}"),
                None => "it".to_owned(),
            };
            let mut details_map = match details_value {
                Some(Value::Object(map)) => map,
                _ => Map::new(),
            };
            if let Some(thread) = thread {
                details_map.insert("thread".to_owned(), Value::String(thread.to_owned()));
            }
            if let Some(answered) = answered {
                details_map.insert("answered".to_owned(), json!(answered));
            }
            thread_tool_failure(
                "host_unavailable",
                format!(
                    "No device that answered{answered_clause} holds {what}; {}.",
                    outcomes.join(", and ")
                ),
                next,
                Some(Value::Object(details_map)),
            )
        }
        (
            RefusalDetails::NotAsked {
                reason: NotAskedReason::Remote,
                thread,
                device,
                device_name,
            },
            _,
        ) => thread_tool_failure(
            "host_unavailable",
            format!(
                "This device has no connection to the account's other devices, so {} could not be asked for {thread}.",
                device_label(device, device_name)
            ),
            "Retry from a session whose device service is set up, or send to another thread as a normal send.",
            details_value,
        ),
        (
            RefusalDetails::NotAsked {
                reason,
                thread,
                device,
                device_name,
            },
            _,
        ) => {
            let why = if *reason == NotAskedReason::Wake {
                "this device's service cannot carry a wake, so it does not probe"
            } else {
                "this device's service cannot make the single probe that would tell"
            };
            thread_tool_failure(
                "host_unavailable",
                format!(
                    "{} does not list {thread}, and {why}, so whether it still holds that session is unknown.",
                    device_label(device, device_name)
                ),
                "Retry when the device service is reachable, or send to another thread as a normal send.",
                details_value,
            )
        }
        (
            RefusalDetails::Thread {
                thread,
                device,
                device_name,
                ..
            },
            _,
        ) => thread_tool_failure(
            "not_found",
            format!(
                "No thread {thread} on {}.",
                device_label(device, device_name)
            ),
            format!("Pass a thread that device holds, or {LIST_ALL}."),
            details_value,
        ),
    }
}

/// The ladder's refusal with the core's candidate shape and `details.kind` (design section 5). A candidate's device is
/// the id: this device's own candidates name it once the account told this device its id (`here`), and otherwise keep
/// `here`.
pub fn with_addresses<F>(
    mut error: ThreadToolFailure,
    here: Option<&str>,
    name_of: F,
) -> ThreadRefusal
where
    F: Fn(&str) -> Option<String>,
{
    let Some(Value::Object(mut map)) = error.details.take() else {
        if error.code.as_str() == "not_found" || error.code.as_str() == "ambiguous_target" {
            error.details = Some(json!({ "kind": "thread" }));
        }
        return error;
    };
    let Some(Value::Array(raw_candidates)) = map.remove("candidates") else {
        if error.code.as_str() == "not_found" || error.code.as_str() == "ambiguous_target" {
            map.insert("kind".to_owned(), Value::String("thread".to_owned()));
        }
        error.details = Some(Value::Object(map));
        return error;
    };
    let candidates: Vec<Value> = raw_candidates
        .into_iter()
        .map(|candidate| {
            let Value::Object(mut item) = candidate else {
                return candidate;
            };
            let resolved_device = item.get("device").and_then(Value::as_str).map(|device| {
                if device == "here" {
                    here.unwrap_or("here").to_owned()
                } else {
                    device.to_owned()
                }
            });
            if let Some(device) = resolved_device {
                let thread = item
                    .get("thread")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let prefix = format!("{device}/");
                let address = if thread.starts_with(&prefix) {
                    thread
                } else {
                    format_address(&device, &thread)
                };
                item.insert("device".to_owned(), Value::String(device.clone()));
                item.insert("address".to_owned(), Value::String(address));
                if let Some(name) = name_of(&device) {
                    item.insert("device_name".to_owned(), Value::String(name));
                }
            }
            Value::Object(item)
        })
        .collect();
    map.insert("kind".to_owned(), Value::String("thread".to_owned()));
    map.insert("candidates".to_owned(), Value::Array(candidates));
    error.details = Some(Value::Object(map));
    error
}
