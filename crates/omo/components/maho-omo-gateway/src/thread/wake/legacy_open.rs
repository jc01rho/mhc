//! The wake's open on this agent directory's legacy host (omo#13037 design 5.2 step 5).
//!
//! Upstream counterpart: `thread/wake/legacy-open.ts`. One plain frame `open_session { sessionPath, cwd,
//! retain_on_disconnect: true }`: it names no `attended` field, so the open adds no unattended hold, and no
//! `permissionPreset`, so the session runs under the host's own default (A3.3). The socket request (`requestFrame` with
//! the host's key root) has no Rust counterpart in this crate; a host binds it through `WakeOpen`.

use std::sync::Arc;

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOpenRequest {
    pub session_path: String,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeOpenReply {
    Opened {
        session_id: String,
        state: Value,
        attached: bool,
    },
    Refused {
        code: String,
        retry_after_ms: Option<i64>,
    },
    Unreachable {
        message: String,
    },
}

pub type WakeOpen = Arc<dyn Fn(&WakeOpenRequest) -> WakeOpenReply + Send + Sync>;

pub fn wake_open_frame(request: &WakeOpenRequest) -> Value {
    json!({
        "type": "open_session",
        "sessionPath": request.session_path,
        "cwd": request.cwd,
        "retain_on_disconnect": true,
    })
}

/// The engine's refusal code: `errorCode` when present, else the code prefix legacy replies put in `error`.
pub fn refusal_of(frame: &Value) -> WakeOpenReply {
    let code = match (
        frame.get("errorCode").and_then(Value::as_str),
        frame.get("error").and_then(Value::as_str),
    ) {
        (Some(code), _) => code.to_owned(),
        (None, Some(error)) => error.split(':').next().unwrap_or(error).trim().to_owned(),
        (None, None) => "unknown".to_owned(),
    };
    let retry_after_ms = frame
        .get("errorData")
        .filter(|data| data.is_object())
        .and_then(|data| data.get("retry_after_ms"))
        .and_then(|hint| hint.as_i64().or_else(|| hint.as_f64().map(|ms| ms as i64)));
    WakeOpenReply::Refused {
        code,
        retry_after_ms,
    }
}

pub fn classify_open_frame(frame: &Value) -> WakeOpenReply {
    let data = frame.get("data").filter(|data| data.is_object());
    let session_id = data
        .and_then(|data| data.get("sessionId"))
        .and_then(Value::as_str);
    match (
        frame.get("success") == Some(&Value::Bool(true)),
        data,
        session_id,
    ) {
        (true, Some(data), Some(session_id)) => WakeOpenReply::Opened {
            session_id: session_id.to_owned(),
            state: data.get("state").cloned().unwrap_or(Value::Null),
            attached: data.get("attached") == Some(&Value::Bool(true)),
        },
        _ => refusal_of(frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_an_open_reply_then_it_carries_the_session_state_and_attachment() {
        let reply = classify_open_frame(&json!({
            "success": true,
            "data": { "sessionId": "rpc-7", "attached": false, "state": { "lifecycle": { "state": "done" } } }
        }));
        assert_eq!(
            reply,
            WakeOpenReply::Opened {
                session_id: "rpc-7".into(),
                state: json!({ "lifecycle": { "state": "done" } }),
                attached: false,
            }
        );
    }

    #[test]
    fn given_a_refusal_then_its_code_and_retry_hint_are_read_from_either_field() {
        assert_eq!(
            classify_open_frame(&json!({
                "success": false, "error": "session_path_in_use", "errorCode": "session_path_in_use",
                "errorData": { "owner": { "pid": 4242 }, "retry_after_ms": 2000 }
            })),
            WakeOpenReply::Refused {
                code: "session_path_in_use".into(),
                retry_after_ms: Some(2000),
            }
        );
        assert_eq!(
            classify_open_frame(&json!({ "success": false, "error": "host_stopping: draining" })),
            WakeOpenReply::Refused {
                code: "host_stopping".into(),
                retry_after_ms: None,
            }
        );
        assert_eq!(
            classify_open_frame(&json!({ "success": true, "data": {} })),
            WakeOpenReply::Refused {
                code: "unknown".into(),
                retry_after_ms: None,
            }
        );
    }

    #[test]
    fn given_a_request_then_the_frame_names_only_the_path_the_cwd_and_the_retained_open() {
        let frame = wake_open_frame(&WakeOpenRequest {
            session_path: "/s/a.jsonl".into(),
            cwd: "/work".into(),
        });
        let mut keys: Vec<&String> = frame
            .as_object()
            .map(|object| object.keys().collect())
            .unwrap_or_default();
        keys.sort();
        assert_eq!(keys, ["cwd", "retain_on_disconnect", "sessionPath", "type"]);
        assert_eq!(frame["retain_on_disconnect"], json!(true));
    }
}
