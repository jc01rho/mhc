//! Which session controls a thread's endpoint accepts.
//!
//! Upstream counterpart: `thread/endpoint-controls.ts`. A host takes them all. A terminal control endpoint takes what
//! its `get_protocol_info` lists in `commands`; a terminal from before senpi's session controls lists none and takes
//! only the read-mostly set.

use std::collections::BTreeSet;

use serde_json::Value;

use super::types::GatewayEndpointKind;

/// Every control, in the order a `thread_list` row lists them.
pub const ALL_CONTROLS: [&str; 6] = [
    "send",
    "read",
    "rename",
    "set_model",
    "set_reasoning",
    "interrupt",
];

/// senpi `session-control-commands.ts` before the session controls: a terminal that lists no `commands`.
pub const LEGACY_TUI_COMMANDS: [&str; 8] = [
    "get_protocol_info",
    "list_sessions",
    "get_state",
    "get_messages",
    "set_session_name",
    "wake",
    "subscribe",
    "extension_ui_response",
];

/// The commands each control sends; a control is accepted when every one of them is.
pub fn control_commands(control: &str) -> &'static [&'static str] {
    match control {
        "send" => &["wake"],
        "read" => &["get_messages"],
        "rename" => &["set_session_name"],
        "set_model" => &["get_available_models", "set_model"],
        "set_reasoning" => &["get_available_thinking_levels", "set_thinking_level"],
        "interrupt" => &["interrupt"],
        _ => &[],
    }
}

/// The commands a terminal's `get_protocol_info` lists, or the legacy set when it lists none.
pub fn tui_commands_from(protocol_info: Option<&Value>) -> BTreeSet<String> {
    match protocol_info
        .and_then(|info| info.get("commands"))
        .and_then(Value::as_array)
    {
        Some(commands) => commands
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        None => LEGACY_TUI_COMMANDS
            .iter()
            .map(|command| (*command).to_owned())
            .collect(),
    }
}

/// The controls an endpoint of `kind` accepts.
pub fn accepted_controls(
    kind: GatewayEndpointKind,
    tui_commands: Option<&BTreeSet<String>>,
) -> Vec<String> {
    if kind != GatewayEndpointKind::Tui {
        return ALL_CONTROLS
            .iter()
            .map(|control| (*control).to_owned())
            .collect();
    }
    let legacy;
    let accepted = match tui_commands {
        Some(commands) => commands,
        None => {
            legacy = tui_commands_from(None);
            &legacy
        }
    };
    ALL_CONTROLS
        .iter()
        .filter(|control| {
            control_commands(control)
                .iter()
                .all(|command| accepted.contains(*command))
        })
        .map(|control| (*control).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_accepts_every_control() {
        assert_eq!(
            accepted_controls(GatewayEndpointKind::RpcHost, None),
            ALL_CONTROLS.map(str::to_owned).to_vec()
        );
    }

    #[test]
    fn a_legacy_terminal_accepts_only_send_read_and_rename() {
        assert_eq!(
            accepted_controls(GatewayEndpointKind::Tui, None),
            vec!["send", "read", "rename"]
        );
    }

    #[test]
    fn a_terminal_with_session_controls_accepts_what_it_lists() {
        let info = serde_json::json!({ "commands": ["wake", "get_messages", "interrupt", "get_available_models", "set_model"] });
        let commands = tui_commands_from(Some(&info));
        assert_eq!(
            accepted_controls(GatewayEndpointKind::Tui, Some(&commands)),
            vec!["send", "read", "set_model", "interrupt"]
        );
    }
}
