//! Port of `remote-usage.ts`: the full usage and each verb's own entry.

pub const USAGE_HEAD: &str = "usage: mhc remote <command> [--json]";

const USAGE_ENTRIES: [(&[&str], &str); 14] = [
    (
        &["status"],
        "  status                                   the device service's connection state",
    ),
    (
        &["devices"],
        "  devices                                  the devices on this account",
    ),
    (
        &["sessions"],
        "  sessions [--device D] [--project P] [--state running|idle|ended]\n                                           sessions across the account's devices",
    ),
    (
        &["enable", "disable"],
        "  enable | disable                         turn remote sessions on or off for this device",
    ),
    (
        &["control"],
        "  control [on|off]                         whether your other devices can view and command this\n                                           device's sessions (on unless you turn it off)",
    ),
    (
        &["wake"],
        "  wake [on|off|default]                    whether sessions on this device that are not running may be started\n                                           by a message from your other devices; on unless turned off",
    ),
    (
        &["bootstrap"],
        "  bootstrap                                set up this device: its key, relayd.json, and the always-on\n                                           mhc-relayd and session host units",
    ),
    (
        &["daemon"],
        "  daemon                                   run mhc-relayd, this device's open device service, in the foreground",
    ),
    (
        &["attach"],
        "  attach <device>/<session>                open a session in the terminal UI (not served by mhc-relayd yet)",
    ),
    (
        &["open"],
        "  open <device> [--project P] [--model provider/id]\n                                           start a session on another device (not served by mhc-relayd yet)",
    ),
    (
        &["send"],
        "  send <session> <text|-> [--steer|--follow-up] [--no-wait]\n                                           send a message; returns once the session took it.\n                                           Name the session <device>/<session>, or by its id alone.",
    ),
    (
        &["model"],
        "  model <session> <provider/id>            switch the model; also takes <device>/<session>",
    ),
    (
        &["interrupt"],
        "  interrupt <session>                      stop the running turn; also takes <device>/<session>",
    ),
    (
        &["close"],
        "  close <session> [--force]                end the session; --force also ends a running turn",
    ),
];

pub fn remote_usage() -> String {
    let mut lines = vec![USAGE_HEAD.to_owned()];
    lines.extend(USAGE_ENTRIES.iter().map(|(_, text)| (*text).to_owned()));
    lines.join("\n")
}

pub fn is_help_flag(arg: &str) -> bool {
    arg == "--help" || arg == "-h"
}

/// The head line and one verb's entry; the full usage for an unknown verb.
pub fn verb_usage(verb: &str) -> String {
    USAGE_ENTRIES
        .iter()
        .find(|(verbs, _)| verbs.contains(&verb))
        .map_or_else(remote_usage, |(_, text)| format!("{USAGE_HEAD}\n{text}"))
}

pub fn is_verb(verb: &str) -> bool {
    USAGE_ENTRIES.iter().any(|(verbs, _)| verbs.contains(&verb))
}
