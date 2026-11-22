//! Which public-socket connections count as host activity (senpi `host-observe-request.ts`).
//! A read marks itself with `observe: true` so the supervisor does not count it as an attachment.
pub const OBSERVE_REQUEST_FIELD: &str = "observe";
pub const WATCH_HOST_OCCUPANCY: &str = "watch_host_occupancy";
pub const HOST_OCCUPANCY_RECORD: &str = "host_occupancy";
pub const ORIGIN_HOST_INSTANCE_FIELD: &str = "origin_host_instance";
const HOLD_HOST_FIELD: &str = "hold_host";
const OBSERVING_COMMANDS: &[&str] = &[
    "get_protocol_info",
    "get_snapshot",
    "list_sessions",
    "subscribe_sessions",
    "unsubscribe_sessions",
    "get_effect_status",
    "get_pending_inputs",
    "get_subscription_limits",
    // Side-effect-free reads of a session's state and history (senpi#4267).
    "get_state",
    "get_entries",
    WATCH_HOST_OCCUPANCY,
];
pub fn is_observing_request(line: &str) -> bool {
    let Ok(serde_json::Value::Object(record)) = serde_json::from_str::<serde_json::Value>(line)
    else {
        return false;
    };
    let kind = record.get("type").and_then(serde_json::Value::as_str);
    // `warm` opens nothing and `authenticate` only proves the endpoint: neither attaches.
    if kind == Some("warm") || kind == Some("park_session") || kind == Some("authenticate") {
        return true;
    }
    record.get(OBSERVE_REQUEST_FIELD) == Some(&serde_json::Value::Bool(true))
        && kind.is_some_and(|kind| OBSERVING_COMMANDS.contains(&kind))
}
/// Whether an observing request line asks to hold (`Some(true)`) or release (`Some(false)`) the host (senpi#5626).
pub fn observing_hold_change(line: &str) -> Option<bool> {
    if !is_observing_request(line) {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .as_object()?
        .get(HOLD_HOST_FIELD)?
        .as_bool()
}
/// Whether a request line is the supervisor observer's `watch_host_occupancy` (oh-my-openagent#14272).
pub fn is_host_occupancy_watch(line: &str) -> bool {
    if !line.contains(WATCH_HOST_OCCUPANCY) {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
        .as_deref()
        == Some(WATCH_HOST_OCCUPANCY)
}
/// The `holders` of a `host_occupancy` record line; `None` for every other line (oh-my-openagent#14272).
pub fn host_occupancy_holders(line: &str) -> Option<usize> {
    if !line.contains(HOST_OCCUPANCY_RECORD) {
        return None;
    }
    let record = serde_json::from_str::<serde_json::Value>(line).ok()?;
    let object = record.as_object()?;
    if object.get("type")?.as_str()? != HOST_OCCUPANCY_RECORD {
        return None;
    }
    usize::try_from(object.get("holders")?.as_u64()?).ok()
}
/// The host generation this process runs in (`SENPI_RPC_HOST_INSTANCE_ID`, trimmed), or `None` outside any host.
pub fn origin_host_instance(env: &std::collections::HashMap<String, String>) -> Option<String> {
    env.get(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
        .map(|instance| instance.trim())
        .filter(|instance| !instance.is_empty())
        .map(str::to_owned)
}
/// `origin_host_instance` of this process's environment (senpi `originHostInstance()` with its `process.env`
/// default): what a socket client stamps on its requests so the host can tell its own children's traffic.
pub fn process_origin_host_instance() -> Option<String> {
    std::env::var(crate::protocol_identity::HOST_INSTANCE_ID_ENV)
        .ok()
        .map(|instance| instance.trim().to_owned())
        .filter(|instance| !instance.is_empty())
}
/// The generation a request line says its client runs in; `None` when unmarked or not JSON.
pub fn request_origin_host_instance(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    value
        .as_object()?
        .get(ORIGIN_HOST_INSTANCE_FIELD)?
        .as_str()
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
}
#[cfg(test)]
mod tests {
    use super::*;
    // senpi host-observe-request.test.ts (senpi#4267).
    #[test]
    fn get_state_observe_a_marked_read_is_an_observing_read() {
        for kind in ["get_state", "get_entries"] {
            assert!(is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1","observe":true}).to_string()
            ));
        }
    }
    #[test]
    fn an_unmarked_read_is_not_an_observing_read() {
        for kind in ["get_state", "get_entries"] {
            assert!(!is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1"}).to_string()
            ));
            assert!(!is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1","observe":"true"}).to_string()
            ));
        }
    }
    #[test]
    fn still_ignores_observe_on_a_command_that_is_not_a_read() {
        assert!(!is_observing_request(
            &serde_json::json!({"type":"prompt","sessionId":"s1","message":"x","observe":true})
                .to_string()
        ));
    }
    #[test]
    fn origin_host_instance_reads_the_trimmed_generation() {
        let env = |value: &str| {
            std::collections::HashMap::from([(
                crate::protocol_identity::HOST_INSTANCE_ID_ENV.to_owned(),
                value.to_owned(),
            )])
        };
        assert_eq!(origin_host_instance(&env(" g1 ")).as_deref(), Some("g1"));
        assert_eq!(origin_host_instance(&env("  ")), None);
        assert_eq!(origin_host_instance(&Default::default()), None);
    }
    #[test]
    fn only_marked_reads_observe_and_warm_always_does() {
        assert!(is_observing_request(
            r#"{"type":"subscribe_sessions","observe":true}"#
        ));
        assert!(!is_observing_request(r#"{"type":"subscribe_sessions"}"#));
        assert!(!is_observing_request(r#"{"type":"prompt","observe":true}"#));
        assert!(is_observing_request(r#"{"type":"warm"}"#));
        assert!(is_observing_request(r#"{"type":"authenticate"}"#));
        for kind in ["get_state", "get_entries", "get_snapshot"] {
            assert!(is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1","observe":true}).to_string()
            ));
            assert!(!is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1"}).to_string()
            ));
            assert!(!is_observing_request(
                &serde_json::json!({"type":kind,"sessionId":"s1","observe":"true"}).to_string()
            ));
        }
        assert!(!is_observing_request("nope"));
        assert_eq!(
            request_origin_host_instance(r#"{"origin_host_instance":"x"}"#).as_deref(),
            Some("x")
        );
        assert_eq!(
            request_origin_host_instance(r#"{"origin_host_instance":""}"#),
            None
        );
    }
}
