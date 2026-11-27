//! The agentd wire frames (omo-nyang `src/relay/protocol.ts`, `src/relay/wire.ts`).
//!
//! One JSON object per line over plain TCP: the client opens with `hello`, the daemon answers `welcome`, and every
//! request after that is a `cmd` answered by a `res` under the same id. Events arrive as `evt` frames on a named
//! stream, acknowledged cumulatively by an `ack` cursor, so a reconnect resumes from the last seq this side kept.
//! Liveness is the protocol's own `ping`/`pong`; there is no transport clock to keep.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// The longest frame line either side accepts; a longer one is a protocol violation, never a frame.
pub const MAX_LINE: usize = 64 * 1024 * 1024;

/// The protocol version this client speaks (`RELAY_PROTOCOL`).
pub const RELAY_PROTOCOL: i64 = 1;

/// A frame that cannot be read: an over-long line, text that is not JSON, or JSON of the wrong shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    LineTooLong,
    NotJson(String),
    Shape(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LineTooLong => write!(formatter, "a frame line is longer than {MAX_LINE} bytes"),
            Self::NotJson(reason) => write!(formatter, "a frame line is not JSON: {reason}"),
            Self::Shape(reason) => write!(formatter, "a frame has the wrong shape: {reason}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// `welcome`: the daemon's node name, the last seq each stream holds, and the last seq already pruned per stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WelcomeFrame {
    pub v: i64,
    pub node: String,
    pub streams: BTreeMap<String, u64>,
    pub floors: BTreeMap<String, u64>,
}

/// `res`: the answer to one `cmd`, under the id the command carried.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultFrame {
    pub id: String,
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

/// `evt`: one event on a stream. `to` marks a coalesced frame carrying `seq..=to` merged into one event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventFrame {
    pub stream: String,
    pub seq: u64,
    pub to: Option<u64>,
    pub event: Value,
}

impl EventFrame {
    /// The last seq this frame covers: `to` when coalesced, else its own.
    pub fn last_seq(&self) -> u64 {
        self.to.unwrap_or(self.seq).max(self.seq)
    }
}

/// A frame the daemon sends.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerFrame {
    Welcome(WelcomeFrame),
    Result(ResultFrame),
    Event(EventFrame),
    /// A stream created after this connection's `hello`: its first event is `from`.
    Stream {
        stream: String,
        from: u64,
    },
    Ping {
        n: i64,
        at: i64,
    },
    Pong {
        n: i64,
        at: i64,
    },
    /// The daemon refused the connection or a frame; `reason` is its own text.
    Refused {
        reason: String,
    },
}

fn cursor_map(value: Option<&Value>) -> BTreeMap<String, u64> {
    let mut cursors = BTreeMap::new();
    let Some(Value::Object(entries)) = value else {
        return cursors;
    };
    for (stream, seq) in entries {
        if let Some(seq) = seq.as_u64() {
            cursors.insert(stream.clone(), seq);
        }
    }
    cursors
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, FrameError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| FrameError::Shape(format!("{key} is missing")))
}

/// One server frame from its parsed JSON.
pub fn parse_server_frame(value: &Value) -> Result<ServerFrame, FrameError> {
    let kind = text(value, "t")?;
    match kind {
        "welcome" => Ok(ServerFrame::Welcome(WelcomeFrame {
            v: value.get("v").and_then(Value::as_i64).unwrap_or_default(),
            node: text(value, "node").unwrap_or_default().to_owned(),
            streams: cursor_map(value.get("streams")),
            floors: cursor_map(value.get("floors")),
        })),
        "res" => Ok(ServerFrame::Result(ResultFrame {
            id: text(value, "id")?.to_owned(),
            ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
            data: value.get("data").cloned(),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })),
        "evt" => Ok(ServerFrame::Event(EventFrame {
            stream: text(value, "s")?.to_owned(),
            seq: value
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or_else(|| FrameError::Shape("evt.seq is missing".to_owned()))?,
            to: value.get("to").and_then(Value::as_u64),
            event: value.get("e").cloned().unwrap_or(Value::Null),
        })),
        "stream" => Ok(ServerFrame::Stream {
            stream: text(value, "s")?.to_owned(),
            from: value
                .get("from")
                .and_then(Value::as_u64)
                .ok_or_else(|| FrameError::Shape("stream.from is missing".to_owned()))?,
        }),
        "ping" => Ok(ServerFrame::Ping {
            n: value.get("n").and_then(Value::as_i64).unwrap_or_default(),
            at: value.get("at").and_then(Value::as_i64).unwrap_or_default(),
        }),
        "pong" => Ok(ServerFrame::Pong {
            n: value.get("n").and_then(Value::as_i64).unwrap_or_default(),
            at: value.get("at").and_then(Value::as_i64).unwrap_or_default(),
        }),
        "refused" => Ok(ServerFrame::Refused {
            reason: text(value, "reason").unwrap_or_default().to_owned(),
        }),
        other => Err(FrameError::Shape(format!("unknown frame type {other}"))),
    }
}

/// One frame line as a server frame.
pub fn parse_line(line: &str) -> Result<ServerFrame, FrameError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| FrameError::NotJson(error.to_string()))?;
    parse_server_frame(&value)
}

/// `hello`: this connection's node, the shared token, the seq each stream resumes from, and the streams it wants.
pub fn hello_frame(
    node: &str,
    token: &str,
    resume: &BTreeMap<String, u64>,
    subscribe: Option<&[String]>,
) -> Value {
    let mut frame = Map::new();
    frame.insert("t".to_owned(), json!("hello"));
    frame.insert("v".to_owned(), json!(RELAY_PROTOCOL));
    frame.insert("node".to_owned(), json!(node));
    frame.insert("token".to_owned(), json!(token));
    frame.insert("resume".to_owned(), json!(resume));
    if let Some(subscribe) = subscribe {
        frame.insert("subscribe".to_owned(), json!(subscribe));
    }
    Value::Object(frame)
}

/// `ack`: the last seq this side kept per stream.
pub fn ack_frame(cursor: &BTreeMap<String, u64>) -> Value {
    json!({ "t": "ack", "cursor": cursor })
}

/// `cmd`: one request, idempotent per `id` at the daemon, refused once `expiresAt` (epoch ms) has passed.
pub fn command_frame(id: &str, op: &str, args: &Value, expires_at: Option<i64>) -> Value {
    let mut frame = Map::new();
    frame.insert("t".to_owned(), json!("cmd"));
    frame.insert("id".to_owned(), json!(id));
    frame.insert("op".to_owned(), json!(op));
    frame.insert(
        "args".to_owned(),
        if args.is_object() {
            args.clone()
        } else {
            Value::Object(Map::new())
        },
    );
    if let Some(expires_at) = expires_at {
        frame.insert("expiresAt".to_owned(), json!(expires_at));
    }
    Value::Object(frame)
}

pub fn ping_frame(n: i64, at: i64) -> Value {
    json!({ "t": "ping", "n": n, "at": at })
}

pub fn pong_frame(n: i64, at: i64) -> Value {
    json!({ "t": "pong", "n": n, "at": at })
}

/// A byte stream cut into frame lines, holding an unfinished line until its newline arrives.
#[derive(Debug, Default)]
pub struct LineReader {
    buffer: Vec<u8>,
}

impl LineReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// The complete lines in `chunk` plus anything left from earlier; a line longer than [`MAX_LINE`] is refused.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = self.buffer.drain(..=index).collect();
            line.pop();
            if line.len() > MAX_LINE {
                self.buffer.clear();
                return Err(FrameError::LineTooLong);
            }
            let text =
                String::from_utf8(line).map_err(|error| FrameError::NotJson(error.to_string()))?;
            if !text.trim().is_empty() {
                lines.push(text);
            }
        }
        if self.buffer.len() > MAX_LINE {
            self.buffer.clear();
            return Err(FrameError::LineTooLong);
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_welcome_carries_its_node_streams_and_floors() {
        let frame = parse_line(
            r#"{"t":"welcome","v":1,"node":"mac-mini","streams":{"mhc-session:s1":7},"floors":{"mhc-session:s1":3}}"#,
        )
        .expect("welcome");
        let ServerFrame::Welcome(welcome) = frame else {
            panic!("expected a welcome");
        };
        assert_eq!(welcome.v, RELAY_PROTOCOL);
        assert_eq!(welcome.node, "mac-mini");
        assert_eq!(welcome.streams.get("mhc-session:s1"), Some(&7));
        assert_eq!(welcome.floors.get("mhc-session:s1"), Some(&3));
    }

    #[test]
    fn a_result_keeps_its_id_and_both_answers() {
        let ok =
            parse_line(r#"{"t":"res","id":"c1","ok":true,"data":{"found":true}}"#).expect("res");
        let ServerFrame::Result(ok) = ok else {
            panic!("expected a result");
        };
        assert_eq!(ok.id, "c1");
        assert!(ok.ok);
        assert_eq!(
            ok.data
                .as_ref()
                .and_then(|data| data.get("found"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(ok.error, None);

        let refused = parse_line(
            r#"{"t":"res","id":"c2","ok":false,"error":"unsupported:mhc.session.read"}"#,
        )
        .expect("res");
        let ServerFrame::Result(refused) = refused else {
            panic!("expected a result");
        };
        assert!(!refused.ok);
        assert_eq!(
            refused.error.as_deref(),
            Some("unsupported:mhc.session.read")
        );
    }

    #[test]
    fn a_coalesced_event_covers_every_seq_it_merges() {
        let frame = parse_line(
            r#"{"t":"evt","s":"mhc-session:s1","seq":4,"to":9,"e":{"type":"message_update"}}"#,
        )
        .expect("evt");
        let ServerFrame::Event(event) = frame else {
            panic!("expected an event");
        };
        assert_eq!(event.stream, "mhc-session:s1");
        assert_eq!(event.seq, 4);
        assert_eq!(event.last_seq(), 9);

        let single =
            parse_line(r#"{"t":"evt","s":"mhc-session:s1","seq":10,"e":null}"#).expect("evt");
        let ServerFrame::Event(single) = single else {
            panic!("expected an event");
        };
        assert_eq!(single.last_seq(), 10);
    }

    #[test]
    fn ping_refused_and_stream_frames_read_their_own_fields() {
        let ping = parse_line(r#"{"t":"ping","n":3,"at":1700000000000}"#).expect("ping");
        assert_eq!(
            ping,
            ServerFrame::Ping {
                n: 3,
                at: 1_700_000_000_000
            }
        );
        let refused = parse_line(r#"{"t":"refused","reason":"unauthorized"}"#).expect("refused");
        assert_eq!(
            refused,
            ServerFrame::Refused {
                reason: "unauthorized".to_owned()
            }
        );
        let stream = parse_line(r#"{"t":"stream","s":"mhc-session:s2","from":1}"#).expect("stream");
        assert_eq!(
            stream,
            ServerFrame::Stream {
                stream: "mhc-session:s2".to_owned(),
                from: 1
            }
        );
    }

    #[test]
    fn a_line_that_is_not_a_known_frame_is_refused_instead_of_guessed() {
        assert_eq!(
            parse_line(r#"{"t":"nope"}"#),
            Err(FrameError::Shape("unknown frame type nope".to_owned()))
        );
        assert!(matches!(
            parse_line("not json"),
            Err(FrameError::NotJson(_))
        ));
        assert!(matches!(
            parse_line(r#"{"t":"evt","s":"s"}"#),
            Err(FrameError::Shape(_))
        ));
    }

    #[test]
    fn a_command_carries_its_id_op_args_and_expiry() {
        let frame = command_frame(
            "c9",
            "mhc.sessions.list",
            &json!({"limit": 5}),
            Some(1_700_000_000_000),
        );
        assert_eq!(frame["t"], "cmd");
        assert_eq!(frame["id"], "c9");
        assert_eq!(frame["op"], "mhc.sessions.list");
        assert_eq!(frame["args"]["limit"], 5);
        assert_eq!(frame["expiresAt"], 1_700_000_000_000i64);
        let no_expiry = command_frame("c10", "mhc.session.read", &Value::Null, None);
        assert!(no_expiry.get("expiresAt").is_none());
        assert!(no_expiry["args"].is_object());
    }

    #[test]
    fn a_hello_names_the_node_token_resume_cursor_and_subscription() {
        let mut resume = BTreeMap::new();
        resume.insert("mhc-session:s1".to_owned(), 12);
        let frame = hello_frame(
            "linux5",
            "secret",
            &resume,
            Some(&["mhc-session:".to_owned()]),
        );
        assert_eq!(frame["t"], "hello");
        assert_eq!(frame["v"], RELAY_PROTOCOL);
        assert_eq!(frame["node"], "linux5");
        assert_eq!(frame["token"], "secret");
        assert_eq!(frame["resume"]["mhc-session:s1"], 12);
        assert_eq!(frame["subscribe"][0], "mhc-session:");
        assert!(
            hello_frame("n", "t", &BTreeMap::new(), None)
                .get("subscribe")
                .is_none()
        );
    }

    #[test]
    fn the_line_reader_holds_a_partial_line_and_splits_the_rest() {
        let mut reader = LineReader::new();
        assert_eq!(
            reader.push(b"{\"t\":\"ping\"").expect("partial"),
            Vec::<String>::new()
        );
        let lines = reader
            .push(b",\"n\":1,\"at\":2}\n{\"t\":\"pong\",\"n\":1,\"at\":2}\n")
            .expect("lines");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], r#"{"t":"ping","n":1,"at":2}"#);
        assert!(matches!(
            parse_line(&lines[1]),
            Ok(ServerFrame::Pong { .. })
        ));
    }

    #[test]
    fn a_blank_line_is_not_a_frame() {
        let mut reader = LineReader::new();
        assert!(reader.push(b"\n  \n").expect("blank").is_empty());
    }
}
