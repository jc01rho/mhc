use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_REPLAYED_HISTORY_IMAGES: usize = 8;

#[derive(Clone)]
enum Origin {
    User,
    Tool { name: String, id: String },
}

impl Origin {
    fn source(&self) -> String {
        match self {
            Self::User => "attached by the user".into(),
            Self::Tool { name, id } => format!("returned by the {name} tool, id={id}"),
        }
    }
    fn subject(&self) -> String {
        match self {
            Self::User => "[image attached by the user".into(),
            Self::Tool { name, .. } => format!("[image returned by the {name} tool"),
        }
    }
}

struct Image {
    message: usize,
    entry: usize,
    answered: bool,
    origin: Origin,
    block: Value,
    hash: Option<String>,
    data: Option<String>,
}

fn canonical_base64(data: &str) -> Option<(String, String)> {
    let compact: String = data.chars().filter(|c| !matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')).collect();
    if compact.is_empty()
        || compact.trim_end_matches('=').len() % 4 == 1
        || !compact
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/_-=".contains(&b))
        || compact.bytes().filter(|b| *b == b'=').count() > 2
    {
        return None;
    }
    let normalized = compact
        .trim_end_matches('=')
        .replace('-', "+")
        .replace('_', "/");
    let mut bytes = Vec::new();
    let mut accumulator = 0u32;
    let mut bits = 0;
    for byte in normalized.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((accumulator >> bits) as u8);
        }
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(TABLE[((n >> 18) & 63) as usize] as char);
        encoded.push(TABLE[((n >> 12) & 63) as usize] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    Some((digest, encoded))
}

fn sdk_name(name: &str, custom: &std::collections::BTreeMap<String, String>) -> String {
    custom
        .get(name)
        .cloned()
        .unwrap_or_else(|| crate::tools::map_pi_tool_name(name, custom))
}

/// What a cold-seed replay does with the images of a history (senpi `ReplayedHistoryImages`, senpi#5275).
#[derive(Default)]
pub struct ReplayedHistoryImages {
    /// Replacement content for each history index that holds an image; every other message is replayed unchanged.
    pub history: std::collections::BTreeMap<usize, Value>,
    /// The current turn's tool images, sent after the history and its cache breakpoint.
    pub current_turn: Vec<Value>,
}

/// Rewrites the image entries of a cold-seed history (senpi#2490): identical bytes are sent once and referenced in
/// text afterwards, the replay is capped, and undecodable image data is dropped with a note.
///
/// What a history entry becomes depends only on the history before it, never on whether a later assistant message
/// answered it, so a request rebuilt after the turn ends starts with the bytes the previous one cached (senpi#5275).
/// A tool image is therefore always a text note in the history; while its turn is in progress it is also sent in
/// `current_turn`, which comes after the cache breakpoint and simply disappears when the turn ends (senpi#2768: an
/// answered tool image is never re-sent). A user upload is replayed in place under the same label in every state.
pub fn replay_history_images(
    messages: &[Value],
    custom: &std::collections::BTreeMap<String, String>,
    in_progress_turn: bool,
) -> ReplayedHistoryImages {
    let answer_boundary = if in_progress_turn {
        messages
            .iter()
            .rposition(|m| m["role"] == "user")
            .unwrap_or(0)
    } else {
        messages
            .iter()
            .rposition(|m| m["role"] == "assistant")
            .unwrap_or(0)
    };
    let mut images = Vec::new();
    for (mi, message) in messages.iter().enumerate() {
        let origin = match message["role"].as_str() {
            Some("user") => Origin::User,
            Some("toolResult") => Origin::Tool {
                name: sdk_name(message["toolName"].as_str().unwrap_or("tool"), custom),
                id: message["toolCallId"].as_str().unwrap_or("").into(),
            },
            _ => continue,
        };
        let Some(content) = message["content"].as_array() else {
            continue;
        };
        for (ei, block) in content.iter().enumerate() {
            if block["type"] != "image"
                || !block["mimeType"]
                    .as_str()
                    .is_some_and(crate::content_blocks::is_sdk_image_media_type)
            {
                continue;
            }
            let data = block["data"].as_str().map(str::to_owned);
            let decoded = data.as_deref().and_then(canonical_base64);
            images.push(Image {
                message: mi,
                entry: ei,
                answered: mi < answer_boundary,
                origin: origin.clone(),
                block: block.clone(),
                hash: decoded.as_ref().map(|v| v.0.clone()),
                data: decoded.map(|v| v.1),
            });
        }
    }
    if images.is_empty() {
        return ReplayedHistoryImages::default();
    }
    let mut ranking = std::collections::BTreeMap::<String, (bool, usize)>::new();
    for (index, image) in images.iter().enumerate() {
        if image.answered && matches!(image.origin, Origin::Tool { .. }) {
            continue;
        }
        if let Some(hash) = &image.hash {
            let old = ranking.get(hash);
            ranking.insert(
                hash.clone(),
                (
                    old.is_some_and(|v| v.0) || matches!(image.origin, Origin::User),
                    index,
                ),
            );
        }
    }
    let mut ranked: Vec<_> = ranking
        .iter()
        .map(|(hash, (user, index))| (hash.clone(), *user, *index))
        .collect();
    ranked.sort_by_key(|(_, user, index)| (std::cmp::Reverse(*user), std::cmp::Reverse(*index)));
    let allowed: std::collections::BTreeSet<_> = ranked
        .into_iter()
        .take(MAX_REPLAYED_HISTORY_IMAGES)
        .map(|x| x.0)
        .collect();
    let mut shown_in_history = std::collections::BTreeMap::<String, Origin>::new();
    let mut shown_in_current_turn = std::collections::BTreeMap::<String, Origin>::new();
    let mut replacements =
        std::collections::BTreeMap::<usize, std::collections::BTreeMap<usize, Vec<Value>>>::new();
    let mut current_turn = Vec::new();
    for image in images {
        let Image {
            message,
            entry,
            answered,
            origin,
            block,
            hash,
            data,
        } = image;
        let replacement = match hash.as_deref() {
            None => vec![text_block(format!(
                "{}: omitted because its image data is missing or unreadable]",
                origin.subject()
            ))],
            Some(hash) => {
                if let Some(first) = shown_in_history.get(hash) {
                    vec![text_block(duplicate_text(&origin, first))]
                } else if let Origin::Tool { name, id } = &origin {
                    if !answered {
                        if let Some(first) = shown_in_current_turn.get(hash) {
                            current_turn.push(text_block(duplicate_text(&origin, first)));
                        } else if !allowed.contains(hash) {
                            current_turn.push(text_block(capped_text(&origin)));
                        } else {
                            shown_in_current_turn.insert(hash.to_owned(), origin.clone());
                            current_turn.push(text_block(format!("[image returned by the {name} tool, id={id}, to your own call in the current turn (tool output, not a user attachment)]")));
                            current_turn.push(canonical_block(block, data));
                        }
                    }
                    vec![text_block(format!(
                        "[image returned by the {name} tool, id={id} (tool output, not a user attachment): not shown in the history; an image from your current turn is attached after the history, any other was already seen, so run the tool again if you need it]"
                    ))]
                } else if !allowed.contains(hash) {
                    vec![text_block(capped_text(&origin))]
                } else {
                    shown_in_history.insert(hash.to_owned(), origin.clone());
                    vec![
                        text_block(
                            "[image the user attached to this message of the history (not part of any newer message)]"
                                .to_owned(),
                        ),
                        canonical_block(block, data),
                    ]
                }
            }
        };
        replacements
            .entry(message)
            .or_default()
            .insert(entry, replacement);
    }
    let history = replacements
        .into_iter()
        .map(|(mi, entries)| {
            let content = messages[mi]["content"].as_array().expect("image content");
            let rewritten: Vec<Value> = content
                .iter()
                .enumerate()
                .flat_map(|(ei, value)| {
                    entries
                        .get(&ei)
                        .cloned()
                        .unwrap_or_else(|| vec![value.clone()])
                })
                .collect();
            (mi, json!(rewritten))
        })
        .collect();
    ReplayedHistoryImages {
        history,
        current_turn,
    }
}

fn text_block(text: String) -> Value {
    json!({"type":"text","text":text})
}

fn duplicate_text(origin: &Origin, first: &Origin) -> String {
    format!(
        "{}: identical to an image already shown above ({}); not attached again]",
        origin.subject(),
        first.source()
    )
}

fn capped_text(origin: &Origin) -> String {
    format!(
        "{}: omitted from this replay, which re-sends at most {MAX_REPLAYED_HISTORY_IMAGES} earlier images; re-read the source if you need it]",
        origin.subject()
    )
}

fn canonical_block(mut block: Value, data: Option<String>) -> Value {
    if let Some(data) = data {
        block["data"] = json!(data);
    }
    block
}

#[cfg(test)]
mod tests {
    use super::*;
    fn image(data: &str) -> Value {
        json!({"type":"image","mimeType":"image/png","data":data})
    }
    #[test]
    fn replay_accepts_ecmascript_whitespace_but_rejects_non_whitespace() {
        let expected = canonical_base64("aW1hZ2U=").expect("valid image");
        for whitespace in [
            '\t', '\n', '\r', ' ', '\u{00a0}', '\u{1680}', '\u{2003}', '\u{2028}', '\u{2029}',
            '\u{202f}', '\u{205f}', '\u{3000}', '\u{feff}',
        ] {
            assert_eq!(
                canonical_base64(&format!("aW1{whitespace}hZ2U=")),
                Some(expected.clone())
            );
        }
        for character in ['\u{0085}', '\u{200b}', '='] {
            assert!(canonical_base64(&format!("aW1{character}hZ2U=")).is_none());
        }
    }
    fn read_result(id: &str, data: &str) -> Value {
        json!({"role":"toolResult","toolName":"read","toolCallId":id,"content":[image(data)]})
    }
    fn texts(content: &Value) -> Vec<&str> {
        content
            .as_array()
            .expect("content")
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect()
    }
    #[test]
    fn history_images_carry_origin_deduplicate_and_omit_answered_tool_images() {
        let messages = vec![
            json!({"role":"user","content":[image("aW1hZ2U=")]}),
            json!({"role":"assistant","content":[]}),
            json!({"role":"toolResult","toolName":"read","toolCallId":"c1","content":[image("aW1hZ2U=")]}),
            json!({"role":"user","content":[image("bmV3")]}),
            json!({"role":"assistant","content":[]}),
        ];
        let replay = replay_history_images(&messages, &Default::default(), false);
        assert!(
            replay.history[&0][0]["text"]
                .as_str()
                .unwrap()
                .contains("this message of the history")
        );
        assert!(
            replay.history[&2][0]["text"]
                .as_str()
                .unwrap()
                .contains("identical to an image already shown")
        );
        assert!(
            replay.history[&3]
                .as_array()
                .expect("replayed content")
                .iter()
                .any(|block| block["type"] == "image")
        );
        assert!(replay.current_turn.is_empty());
    }
    #[test]
    fn replay_caps_distinct_images_and_labels_tool_outputs() {
        let data = [
            "AQ==", "Ag==", "Aw==", "BA==", "BQ==", "Bg==", "Bw==", "CA==", "CQ==", "Cg==",
        ];
        let messages: Vec<_> = data
            .iter()
            .map(|value| json!({"role":"user","content":[image(value)]}))
            .collect();
        let replay = replay_history_images(&messages, &Default::default(), false);
        let attached = replay
            .history
            .values()
            .flat_map(|content| content.as_array().expect("content"))
            .filter(|block| block["type"] == "image")
            .count();
        assert_eq!(attached, MAX_REPLAYED_HISTORY_IMAGES);
        let tool = vec![
            read_result("x", "aW1hZ2U="),
            json!({"role":"user","content":"next"}),
        ];
        let replay = replay_history_images(&tool, &Default::default(), false);
        assert!(
            replay.history[&0][0]["text"]
                .as_str()
                .unwrap()
                .contains("tool output, not a user attachment")
        );
    }
    #[test]
    fn a_tool_image_is_a_history_note_in_every_state_and_rides_the_current_turn_only_while_it_is_unanswered()
     {
        let in_progress = vec![
            json!({"role":"user","content":"look"}),
            json!({"role":"assistant","content":[]}),
            read_result("shot-1", "AQ=="),
        ];
        let mid_turn = replay_history_images(&in_progress, &Default::default(), true);
        let mut closed = in_progress.clone();
        closed.push(json!({"role":"assistant","content":[]}));
        let after_turn = replay_history_images(&closed, &Default::default(), false);

        assert_eq!(mid_turn.history, after_turn.history);
        assert!(
            mid_turn.history[&2][0]["text"].as_str().unwrap().contains(
                "id=shot-1 (tool output, not a user attachment): not shown in the history"
            )
        );
        assert_eq!(
            texts(&json!(mid_turn.current_turn)),
            [
                "[image returned by the Read tool, id=shot-1, to your own call in the current turn (tool output, not a user attachment)]"
            ]
        );
        assert_eq!(mid_turn.current_turn[1]["type"], "image");
        assert!(after_turn.current_turn.is_empty());
    }
    #[test]
    fn a_user_upload_keeps_its_history_label_whether_or_not_the_turn_has_ended() {
        let upload = vec![
            json!({"role":"user","content":[{"type":"text","text":"mockup"},image("AQ==")]}),
            json!({"role":"assistant","content":[]}),
            read_result("shot-1", "Ag=="),
        ];
        let mid_turn = replay_history_images(&upload, &Default::default(), true);
        let mut closed = upload.clone();
        closed.push(json!({"role":"assistant","content":[]}));
        let after_turn = replay_history_images(&closed, &Default::default(), false);

        assert_eq!(mid_turn.history[&0], after_turn.history[&0]);
        assert_eq!(
            texts(&mid_turn.history[&0]),
            [
                "mockup",
                "[image the user attached to this message of the history (not part of any newer message)]"
            ]
        );
        assert_eq!(mid_turn.history[&0][2]["type"], "image");
    }
    #[test]
    fn current_turn_tool_images_are_deduplicated_and_capped_like_history_images() {
        let mut messages = vec![
            json!({"role":"user","content":"look"}),
            json!({"role":"assistant","content":[]}),
            read_result("first", "AQ=="),
            read_result("again", "AQ=="),
        ];
        let replay = replay_history_images(&messages, &Default::default(), true);
        assert_eq!(replay.current_turn.len(), 3);
        assert!(
            replay.current_turn[2]["text"]
                .as_str()
                .unwrap()
                .contains("identical to an image already shown above")
        );
        assert_eq!(replay.history.len(), 2);

        for (n, data) in [
            "Ag==", "Aw==", "BA==", "BQ==", "Bg==", "Bw==", "CA==", "CQ==",
        ]
        .into_iter()
        .enumerate()
        {
            messages.push(read_result(&format!("more-{n}"), data));
        }
        let replay = replay_history_images(&messages, &Default::default(), true);
        let attached = replay
            .current_turn
            .iter()
            .filter(|block| block["type"] == "image")
            .count();
        assert_eq!(attached, MAX_REPLAYED_HISTORY_IMAGES);
        assert!(replay.current_turn.iter().any(|block| {
            block["text"]
                .as_str()
                .is_some_and(|text| text.contains("omitted from this replay"))
        }));
    }
}
