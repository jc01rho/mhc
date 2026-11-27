//! Port of `remote-render.ts` and `bin/lib/display-escape.js`: the human forms of `mhc remote`;
//! every verb also prints its value as JSON with `--json`.

use serde_json::Value;

/// Unicode general categories Cc (`char::is_control`), Cf, Zl and Zp. With `keep_line_feeds`,
/// `U+000A` stays raw for multi-line output while `\r` and every other control/format/separator
/// character is escaped.
pub fn unsafe_for_display(character: char, keep_line_feeds: bool) -> bool {
    if keep_line_feeds && character == '\n' {
        return false;
    }
    character.is_control()
        || matches!(
            character as u32,
            0x00ad
                | 0x0600..=0x0605
                | 0x061c
                | 0x06dd
                | 0x070f
                | 0x0890..=0x0891
                | 0x08e2
                | 0x180e
                | 0x200b..=0x200f
                | 0x2028..=0x202e
                | 0x2060..=0x2064
                | 0x2066..=0x206f
                | 0xfeff
                | 0xfff9..=0xfffb
                | 0x110bd
                | 0x110cd
                | 0x13430..=0x1343f
                | 0x1bca0..=0x1bca3
                | 0x1d173..=0x1d17a
                | 0xe0001
                | 0xe0020..=0xe007f
        )
}

fn escape_display(text: &str, keep_line_feeds: bool) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if !unsafe_for_display(character, keep_line_feeds) {
            escaped.push(character);
            continue;
        }
        let code = character as u32;
        if code > 0xffff {
            escaped.push_str(&format!("\\u{{{code:x}}}"));
        } else {
            escaped.push_str(&format!("\\u{code:04x}"));
        }
    }
    escaped
}

pub fn escape_for_display(text: &str) -> String {
    escape_display(text, false)
}

pub fn escape_for_display_keep_lf(text: &str) -> String {
    escape_display(text, true)
}

pub fn state_label(state: &str) -> &str {
    match state {
        "not_configured" => "Not set up",
        "disabled" => "Off",
        "sign_in_required" => "Sign-in required",
        "plan_required" => "Plan required",
        "keychain_locked" => "Keychain locked",
        "connecting" => "Connecting",
        "activation_pending" => "Waiting for activation",
        "connected" => "Connected",
        "offline" => "Offline",
        "authorization_stale" => "Reconnecting",
        "service_degraded" => "Needs attention",
        "update_required" => "Update required",
        "signed_out" => "Signed out",
        "fail_closed" => "Needs repair",
        other => other,
    }
}

pub fn render_table(header: &[&str], shown_rows: &[Vec<String>]) -> Vec<String> {
    let rows: Vec<Vec<String>> = shown_rows
        .iter()
        .map(|row| row.iter().map(|cell| escape_for_display(cell)).collect())
        .collect();
    let widths: Vec<usize> = header
        .iter()
        .enumerate()
        .map(|(column, title)| {
            rows.iter()
                .map(|row| row.get(column).map_or(0, |cell| cell.chars().count()))
                .chain(std::iter::once(title.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: Vec<&str>| {
        cells
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let width = widths.get(column).copied().unwrap_or(0);
                format!(
                    "{cell}{}",
                    " ".repeat(width.saturating_sub(cell.chars().count()))
                )
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut lines = vec![line(header.to_vec())];
    lines.extend(
        rows.iter()
            .map(|row| line(row.iter().map(String::as_str).collect())),
    );
    lines
}

pub fn ago(ms: Option<i64>, now: i64) -> String {
    let Some(ms) = ms else {
        return "never".into();
    };
    let seconds = ((now - ms).max(0) as f64 / 1000.0).round();
    if seconds < 60.0 {
        return "just now".into();
    }
    let minutes = (seconds / 60.0).round();
    if minutes < 60.0 {
        return format!("{minutes}m ago");
    }
    let hours = (minutes / 60.0).round();
    if hours < 48.0 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", (hours / 24.0).round())
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A refusal's next action may name the eval-only `thread_list` tool; a person at a terminal runs the
/// CLI verb instead (omo#14305 review M4).
pub fn person_next_action(text: &str) -> String {
    if text.contains("thread_list") {
        "Run mhc remote sessions to see the account's sessions.".to_owned()
    } else {
        text.to_owned()
    }
}

/// A typed refusal as the lines a person reads on stderr: the message, the next action, then any candidates.
pub fn refusal_lines(message: &str, next_action: &str, candidates: &[String]) -> Vec<String> {
    let mut lines = vec![
        escape_for_display(&format!("mhc remote: {message}")),
        escape_for_display(&person_next_action(next_action)),
    ];
    lines.extend(candidates.iter().map(|line| escape_for_display(line)));
    lines
}

/// `mhc remote open --prompt` whose session opened but whose first prompt was not delivered.
pub fn undelivered_prompt_lines(label: &str, message: &str, next_action: &str) -> Vec<String> {
    vec![
        escape_for_display(&format!(
            "mhc remote: opened {label}, but its first prompt was not delivered: {message}"
        )),
        escape_for_display(&format!(
            "{next_action} Send it again with mhc remote send {label} <text>."
        )),
    ]
}

pub fn session_ended_line(device: &str, end: &str) -> String {
    escape_for_display(&format!(
        "mhc remote: the session on {device} ended before its turn settled ({end})."
    ))
}

pub fn status_lines(
    status: &Value,
    service_version: &str,
    local: &[(String, String)],
) -> Vec<String> {
    let device = status
        .get("device")
        .filter(|device| !device.is_null())
        .map_or("not set up", |device| text(device, "name"));
    let mut rows: Vec<(String, String)> = vec![
        (
            "State".into(),
            state_label(text(status, "state")).to_owned(),
        ),
        (
            "Remote sessions".into(),
            if status.get("enabled").and_then(Value::as_bool) == Some(true) {
                "on".into()
            } else {
                "off".into()
            },
        ),
        ("This device".into(), device.to_owned()),
        ("Service".into(), service_version.to_owned()),
    ];
    if let Some(action) = status
        .get("detail")
        .and_then(|detail| detail.get("action"))
        .and_then(Value::as_str)
    {
        rows.push(("Action".into(), action.to_owned()));
    }
    rows.extend(local.iter().cloned());
    let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    rows.into_iter()
        .map(|(label, value)| escape_for_display(&format!("{label:width$}  {value}")))
        .collect()
}

pub fn device_lines(devices: &[Value], now: i64) -> Vec<String> {
    if devices.is_empty() {
        return vec!["No devices on this account yet.".into()];
    }
    let rows: Vec<Vec<String>> = devices
        .iter()
        .map(|device| {
            let this_device = device.get("this_device").and_then(Value::as_bool) == Some(true);
            vec![
                text(device, "device").to_owned(),
                text(device, "label").to_owned(),
                if this_device {
                    "this device".to_owned()
                } else {
                    text(device, "kind").to_owned()
                },
                text(device, "platform").to_owned(),
                if device.get("online").and_then(Value::as_bool) == Some(true) {
                    "online".to_owned()
                } else {
                    ago(device.get("last_seen_ms").and_then(Value::as_i64), now)
                },
            ]
        })
        .collect();
    render_table(&["DEVICE", "NAME", "KIND", "PLATFORM", "SEEN"], &rows)
}

/// The name of `device` in `devices`, its id when it is not listed.
pub fn device_name<'a>(devices: &'a [Value], device: &'a str) -> &'a str {
    devices
        .iter()
        .find(|row| text(row, "device") == device)
        .map_or(device, |row| text(row, "label"))
}

pub fn session_lines(
    rows: &[Value],
    devices: &[Value],
    unanswered: &[String],
    now: i64,
) -> Vec<String> {
    let mut lines = if rows.is_empty() {
        vec!["No sessions found.".to_owned()]
    } else {
        let table: Vec<Vec<String>> = rows
            .iter()
            .map(|row| {
                let name = device_name(devices, text(row, "device"));
                let state = text(row, "state");
                vec![
                    format!("{name}/{}", text(row, "session_id")),
                    crate::device::row_display_name(row).name,
                    if text(row, "source") == "cache" {
                        format!("{state} (cached)")
                    } else {
                        state.to_owned()
                    },
                    text(row, "project").to_owned(),
                    name.to_owned(),
                    ago(row.get("updated_ms").and_then(Value::as_i64), now),
                ]
            })
            .collect();
        render_table(
            &["SESSION", "TITLE", "STATE", "PROJECT", "DEVICE", "UPDATED"],
            &table,
        )
    };
    if !unanswered.is_empty() {
        let names: Vec<&str> = unanswered
            .iter()
            .map(|device| device_name(devices, device))
            .collect();
        lines.push(String::new());
        lines.push(escape_for_display(&format!(
            "Not answered, rows may be missing or cached: {}",
            names.join(", ")
        )));
    }
    lines
}

pub fn wake_lines(state: &Value) -> Vec<String> {
    let enabled = state.get("enabled").and_then(Value::as_bool) == Some(true);
    let mut lines = vec![if enabled {
        "Waking is on: sessions on this device that are not running may be started by a message from your other devices.".to_owned()
    } else {
        "Waking is off: a message from another device waits until this session is opened here."
            .to_owned()
    }];
    lines.push(match state.get("stored") {
        None | Some(Value::Null) => {
            "Nothing is stored for this device, so it follows this build's default.".to_owned()
        }
        Some(Value::String(stored)) if stored == "unreadable" => {
            "The stored value could not be read; set it again to repair it.".to_owned()
        }
        Some(stored) => format!(
            "Stored on this device: {}.",
            if stored.as_bool() == Some(true) {
                "on"
            } else {
                "off"
            }
        ),
    });
    if state.get("overridden").and_then(Value::as_bool) == Some(true) {
        lines.push(
            "This run of the device service cannot wake sessions for other devices whatever the switch says."
                .to_owned(),
        );
    }
    lines
}

pub fn control_lines(state: &Value, now: i64) -> Vec<String> {
    let line = if state.get("attached").and_then(Value::as_bool) == Some(true) {
        "Remote control is on: your other devices can view and command sessions on this device."
    } else {
        "Remote control is off: your other devices cannot view or command sessions on this device."
    };
    let changed_at = text(state, "changedAt");
    match chrono::DateTime::parse_from_rfc3339(changed_at) {
        Ok(changed) if !changed_at.is_empty() => vec![
            line.to_owned(),
            format!("Changed {}.", ago(Some(changed.timestamp_millis()), now)),
        ],
        _ => vec![line.to_owned()],
    }
}
