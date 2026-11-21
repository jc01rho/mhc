//! Legacy HTTP+SSE client transport rules (MCP protocol 2024-11-05): the SDK's `SSEClientTransport`,
//! which oh-my-openagent routes an explicit `type: "sse"` skill MCP server to (#11591). A GET opens
//! the event stream; the server's `endpoint` event names the URL every JSON-RPC message is POSTed
//! to, and replies come back as `message` events on the stream. [`crate::transport_sdk::McpClient`]
//! owns the connection; this module holds the endpoint and event-stream rules it applies.
use crate::errors::McpError;

/// The POST endpoint as the client knows it.
#[derive(Clone, Debug)]
pub enum SseEndpoint {
    /// The GET stream is opening, or open without an `endpoint` event yet.
    Pending,
    /// The announced endpoint, resolved against the stream URL and on its origin.
    Ready(url::Url),
    /// The stream failed or ended; every later send fails with this error.
    Failed(McpError),
}

/// Upstream `new URL(event.data, this._url)` and its origin check: an endpoint on another origin
/// would receive every message and the configured headers, so it is refused.
pub fn resolve_sse_endpoint(stream: &url::Url, data: &str) -> Result<url::Url, String> {
    let endpoint = stream
        .join(data)
        .map_err(|error| format!("Invalid SSE endpoint: {error}"))?;
    if endpoint.origin() != stream.origin() {
        return Err(format!(
            "Endpoint origin does not match connection origin: {}",
            endpoint.origin().ascii_serialization()
        ));
    }
    Ok(endpoint)
}

/// One dispatched server-sent event: its type (`message` when unnamed) and its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// Incremental `text/event-stream` decoder: `data` lines join with LF, a blank line dispatches,
/// `event` names the type until that dispatch, comments (`:`) and other fields are skipped, and an
/// event with no data line is dropped.
pub struct SseDecoder {
    buffer: Vec<u8>,
    skip_lf: bool,
    first_line: bool,
    event: String,
    data: String,
}

impl Default for SseDecoder {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            skip_lf: false,
            first_line: true,
            event: String::new(),
            data: String::new(),
        }
    }
}

impl SseDecoder {
    /// Feeds one chunk and returns the events it completed, in order.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(line) = crate::transport_sdk::take_sse_line(
            &mut self.buffer,
            &mut self.skip_lf,
            &mut self.first_line,
        ) {
            if line.is_empty() {
                let event = std::mem::take(&mut self.event);
                let mut data = std::mem::take(&mut self.data);
                // Every data line ends with LF; the last one is not part of the event.
                if data.pop().is_some() {
                    events.push(SseEvent {
                        event: if event.is_empty() {
                            "message".to_owned()
                        } else {
                            event
                        },
                        data,
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = value.to_owned(),
                "data" => {
                    self.data.push_str(value);
                    self.data.push('\n');
                }
                _ => {}
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> url::Url {
        url::Url::parse("http://127.0.0.1:4000/sse").expect("stream url")
    }

    fn event(event: &str, data: &str) -> SseEvent {
        SseEvent {
            event: event.to_owned(),
            data: data.to_owned(),
        }
    }

    #[test]
    fn an_endpoint_resolves_against_the_stream_url() {
        for (data, expected) in [
            (
                "/messages?sessionId=a",
                "http://127.0.0.1:4000/messages?sessionId=a",
            ),
            ("messages", "http://127.0.0.1:4000/messages"),
            ("http://127.0.0.1:4000/rpc", "http://127.0.0.1:4000/rpc"),
        ] {
            assert_eq!(
                resolve_sse_endpoint(&stream(), data)
                    .expect("same-origin endpoint")
                    .as_str(),
                expected
            );
        }
    }

    #[test]
    fn an_endpoint_on_another_origin_is_refused() {
        for data in [
            "http://127.0.0.1:4001/messages",
            "https://127.0.0.1:4000/messages",
            "http://localhost:4000/messages",
            "//attacker.test/messages",
        ] {
            let error = resolve_sse_endpoint(&stream(), data).expect_err(data);
            assert!(
                error.starts_with("Endpoint origin does not match connection origin: "),
                "{data}: {error}"
            );
        }
    }

    #[test]
    fn events_dispatch_on_blank_lines_across_chunk_boundaries() {
        let mut decoder = SseDecoder::default();
        assert!(
            decoder
                .push(b"\xEF\xBB\xBFevent: endpoint\r\ndata: /messages")
                .is_empty()
        );
        assert_eq!(
            decoder.push(b"\r\n\r\n: keep-alive\n\ndata: {\"a\":\ndata: 1}\n\n"),
            vec![
                event("endpoint", "/messages"),
                event("message", "{\"a\":\n1}")
            ]
        );
    }

    #[test]
    fn the_event_type_lasts_one_dispatch_and_an_event_without_data_is_dropped() {
        let mut decoder = SseDecoder::default();
        assert_eq!(
            decoder.push(b"event: endpoint\n\nevent: ping\ndata\n\ndata: x\r\r"),
            vec![event("ping", ""), event("message", "x")]
        );
    }
}
