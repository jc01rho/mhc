//! Line I/O over a stream: the local API framing (`device_service_core::local_api::framing`) for both
//! the local socket and the peer link.

use std::collections::VecDeque;

use device_service_core::local_api::LocalApiError;
use device_service_core::local_api::framing::{DecodedLine, LineDecoder, encode_line, parse_line};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const READ_BUFFER: usize = 64 * 1024;

pub struct LineReader<R> {
    reader: R,
    decoder: LineDecoder,
    queue: VecDeque<Vec<u8>>,
    buffer: Vec<u8>,
    oversized: u64,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            decoder: LineDecoder::new(),
            queue: VecDeque::new(),
            buffer: vec![0u8; READ_BUFFER],
            oversized: 0,
        }
    }

    /// The next JSON line; `Ok(None)` at the end of the stream. An oversized line is dropped and the
    /// stream resynchronizes on the next one.
    pub async fn next_value(&mut self) -> Result<Option<Value>, LocalApiError> {
        loop {
            if let Some(line) = self.queue.pop_front() {
                return parse_line(&line).map(Some);
            }
            let read = self
                .reader
                .read(&mut self.buffer)
                .await
                .map_err(|error| LocalApiError::new("closed", error.to_string()))?;
            if read == 0 {
                return Ok(None);
            }
            for decoded in self.decoder.push(&self.buffer[..read]) {
                match decoded {
                    DecodedLine::Line(line) => self.queue.push_back(line),
                    DecodedLine::Oversized(_) => self.oversized += 1,
                }
            }
        }
    }

    pub fn oversized(&self) -> u64 {
        self.oversized
    }
}

pub async fn write_value<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
) -> Result<(), LocalApiError> {
    let line = encode_line(value)?;
    writer
        .write_all(&line)
        .await
        .map_err(|error| LocalApiError::new("closed", error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| LocalApiError::new("closed", error.to_string()))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// RFC 3339 UTC, as `changedAt` carries it.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
