//! Port of `local-api/framing.ts`: LF-delimited UTF-8 JSON, one object per line, at most
//! [`LOCAL_LINE_MAX`] bytes per line (the LF not counted).

use serde_json::Value;

use super::errors::LocalApiError;

pub const LOCAL_LINE_MAX: usize = 1_048_576;
const LF: u8 = b'\n';

pub fn encode_line(value: &Value) -> Result<Vec<u8>, LocalApiError> {
    let mut body = serde_json::to_vec(value)
        .map_err(|error| LocalApiError::protocol_violation(error.to_string()))?;
    if body.len() > LOCAL_LINE_MAX {
        return Err(LocalApiError::protocol_violation(format!(
            "a {}-byte line exceeds LOCAL_LINE_MAX",
            body.len()
        )));
    }
    body.push(LF);
    Ok(body)
}

pub fn parse_line(line: &[u8]) -> Result<Value, LocalApiError> {
    let text = std::str::from_utf8(line)
        .map_err(|_| LocalApiError::protocol_violation("a line is not UTF-8"))?;
    serde_json::from_str(text).map_err(|_| LocalApiError::protocol_violation("a line is not JSON"))
}

/// One step of [`LineDecoder::push`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedLine {
    Line(Vec<u8>),
    /// A line crossed [`LOCAL_LINE_MAX`]; it is dropped up to its LF. Carries the bytes seen so far.
    Oversized(usize),
}

/// Splits a byte stream into lines. A line longer than [`LOCAL_LINE_MAX`] is reported once when it
/// crosses the limit and dropped up to its LF; decoding resynchronizes on the next line.
#[derive(Debug, Default)]
pub struct LineDecoder {
    buffer: Vec<u8>,
    discarding: bool,
}

impl LineDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<DecodedLine> {
        let mut out = Vec::new();
        let mut start = 0;
        while start < chunk.len() {
            let lf = chunk[start..]
                .iter()
                .position(|byte| *byte == LF)
                .map(|offset| start + offset);
            self.take(&chunk[start..lf.unwrap_or(chunk.len())], &mut out);
            let Some(lf) = lf else {
                return out;
            };
            self.finish(&mut out);
            start = lf + 1;
        }
        out
    }

    fn take(&mut self, piece: &[u8], out: &mut Vec<DecodedLine>) {
        if self.discarding || piece.is_empty() {
            return;
        }
        if self.buffer.len() + piece.len() > LOCAL_LINE_MAX {
            self.discarding = true;
            out.push(DecodedLine::Oversized(self.buffer.len() + piece.len()));
            self.buffer = Vec::new();
            return;
        }
        self.buffer.extend_from_slice(piece);
    }

    fn finish(&mut self, out: &mut Vec<DecodedLine>) {
        if self.discarding {
            self.discarding = false;
            return;
        }
        out.push(DecodedLine::Line(std::mem::take(&mut self.buffer)));
    }
}
