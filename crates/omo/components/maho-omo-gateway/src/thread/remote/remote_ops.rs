// allow: SIZE_OK — remote status round and followRemote stream/transcript state machine (omo thread/remote/remote-ops.ts)
//! The status round every cross-device resolution starts with, and `follow_remote` for following one send's turn on
//! another device's session (omo `thread/remote/remote-ops.ts`; #13267, #13272, #13481, #13512 / `25dc4d8220`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::channel::oneshot;
use serde_json::{Map, Value, json};

use crate::thread::adapter::BoxFuture;
use crate::thread::address::refusals::{ThreadRefusal, refusal};
use crate::thread::address::resolve_typed_device;
use crate::thread::errors::ThreadToolFailure;

use super::outcome::relay_outcome_failure;
use super::ports::{
    RemoteCommandOutcome, RemoteSessionRow, RemoteThreadSurface, RemoteWatchHandlers,
    RemoteWatchOptions, RemoteWatchOutcome,
};

const READ_TAIL: usize = 256;

/// The engine payload of a `session.command` response (`response.data`, or `response.receipt` on agentd, else `{}`).
pub fn reply_data(response: &Value) -> Map<String, Value> {
    if let Some(data) = response.get("data").and_then(Value::as_object) {
        return data.clone();
    }
    if let Some(receipt) = response.get("receipt").and_then(Value::as_object) {
        if let Some(inner) = receipt.get("data").and_then(Value::as_object) {
            return inner.clone();
        }
        return receipt.clone();
    }
    Map::new()
}

/// What a connected relay's status round names: this device's account id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub here: Option<String>,
}

/// A5: the relay is paid only; on any other plan the remote path answers a refusal, never a silent failure or a queue.
pub async fn remote_ready(remote: &RemoteThreadSurface) -> Result<Ready, ThreadRefusal> {
    let status = match remote.client.service_status().await {
        Ok(status) => status,
        Err(failure) => {
            let details = if failure.service_not_installed {
                json!({ "reason": failure.message })
            } else {
                json!({ "reason": failure.message, "state": "unreachable" })
            };
            return Err(refusal(
                "host_unavailable",
                "This device's device service is not reachable.",
                "Install or start the device service, then retry.",
                Some(details),
            ));
        }
    };
    if status.state == "plan_required" {
        return Err(refusal(
            "relay_requires_paid_plan",
            "Reaching sessions on other devices needs a paid plan; this account's plan has no relay.",
            "Upgrade the plan to use other devices, or work with threads on this device.",
            None,
        ));
    }
    if status.state != "connected" {
        let state = status.state;
        return Err(refusal(
            "host_unavailable",
            format!("Other devices are not reachable from this device right now ({state})."),
            "Sign in, turn remote sessions on, or wait for the device service to connect, then retry.",
            Some(json!({ "state": state })),
        ));
    }
    Ok(Ready {
        here: status.device_id,
    })
}

const NOT_SIGNED_IN: [&str; 2] = ["sign_in_required", "signed_out"];

/// The refusal for a locate's or command's refusal reason that names this device's own missing relay sign-in.
pub fn not_signed_in_failure(reason: &str, device: &str) -> Option<ThreadRefusal> {
    NOT_SIGNED_IN.contains(&reason).then(|| {
        refusal(
            "host_unavailable",
            format!(
                "This device is not attached to the relay ({reason}), so device {device} cannot be reached; nothing was sent."
            ),
            "Retry when this device is signed in: sign in to omo here, then retry.",
            Some(json!({ "device": device, "reason": reason })),
        )
    })
}

/// How the send a follow belongs to was answered (#13267).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteFollowLane {
    Started,
    Queued,
    Steered,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FollowRemoteOptions {
    pub text: Option<String>,
    pub client_message_id: Option<String>,
}

pub struct RemoteFollow {
    pub settled: BoxFuture<String>,
    stop: Arc<dyn Fn() + Send + Sync>,
    aim_fn: Arc<dyn Fn(RemoteFollowLane) + Send + Sync>,
}

impl RemoteFollow {
    pub fn stop(&self) {
        (self.stop)();
    }

    pub fn aim(&self, lane: RemoteFollowLane) {
        (self.aim_fn)(lane);
    }
}

fn message_of(entry: &Value) -> Option<&Map<String, Value>> {
    if let Some(msg) = entry.get("message").and_then(Value::as_object) {
        return Some(msg);
    }
    let obj = entry.as_object()?;
    obj.contains_key("role").then_some(obj)
}

pub fn entry_text_of(entry: &Value) -> Option<String> {
    let message = message_of(entry)?;
    match message.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter_map(|part| {
                    let obj = part.as_object()?;
                    (obj.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| obj.get("text").and_then(Value::as_str))
                        .flatten()
                })
                .collect();
            (!texts.is_empty()).then(|| texts.join(""))
        }
        _ => None,
    }
}

/// Whether a transcript entry is a user message, in either stored shape.
pub fn is_user_entry(entry: &Value) -> bool {
    let message = entry
        .get("message")
        .and_then(Value::as_object)
        .or_else(|| entry.as_object());
    message.and_then(|m| m.get("role")).and_then(Value::as_str) == Some("user")
}

fn entry_client_message_id_of(entry: &Value) -> Option<String> {
    let message = entry
        .get("message")
        .and_then(Value::as_object)
        .or_else(|| entry.as_object())?;
    message
        .get("clientMessageId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn entry_key_of(entry: &Value) -> String {
    entry
        .get("id")
        .and_then(Value::as_str)
        .map_or_else(|| entry.to_string(), str::to_owned)
}

fn text_carrying_user_entries(entries: &[Value], text: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            is_user_entry(entry)
                && entry_text_of(entry)
                    .as_deref()
                    .is_some_and(|t| t.contains(text))
        })
        .map(|(idx, _)| idx)
        .collect()
}

/// The matches that can anchor a send's turn (#13272).
pub fn anchor_matches(
    entries: &[Value],
    text: &str,
    client_message_id: Option<&str>,
) -> Vec<usize> {
    if let Some(id) = client_message_id {
        for (idx, entry) in entries.iter().enumerate() {
            if is_user_entry(entry) && entry_client_message_id_of(entry).as_deref() == Some(id) {
                return vec![idx];
            }
        }
    }
    let mut exact = Vec::new();
    let mut contained = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        if !is_user_entry(entry) {
            continue;
        }
        let Some(entry_text) = entry_text_of(entry) else {
            continue;
        };
        if entry_text == text {
            exact.push(idx);
        } else if entry_text.contains(text) {
            contained.push(idx);
        }
    }
    if !exact.is_empty() { exact } else { contained }
}

fn extract_entries(response: &Value) -> Option<Vec<Value>> {
    let raw = response
        .get("entries")
        .or_else(|| response.get("messages"))
        .or_else(|| {
            response
                .get("data")
                .and_then(|d| d.get("entries").or_else(|| d.get("messages")))
        })?;
    Some(
        raw.as_array()?
            .iter()
            .filter(|v| v.is_object())
            .cloned()
            .collect(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FollowMode {
    Buffer,
    Stream,
    Transcript,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptMatch {
    Id,
    Text,
}

struct LastDelta {
    index: Option<Value>,
    ends_line: bool,
}

struct FollowState {
    done: bool,
    settle_tx: Option<oneshot::Sender<String>>,
    mode: FollowMode,
    last: Option<LastDelta>,
    new_message: bool,
    checking: bool,
    pending_signal: bool,
    match_ordinal: Option<usize>,
    earlier_entries: Option<HashSet<String>>,
    last_stop_reason: Option<String>,
    streamed: String,
    buffer: Vec<Value>,
}

struct FollowRunner {
    remote: Arc<RemoteThreadSurface>,
    row: RemoteSessionRow,
    on_text: Arc<dyn Fn(&str) + Send + Sync>,
    options: FollowRemoteOptions,
    stored_before_send: Option<HashSet<String>>,
    state: Mutex<FollowState>,
}

fn lock_state(m: &Mutex<FollowState>) -> MutexGuard<'_, FollowState> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FollowRunner {
    fn end(&self, reason: &str) {
        let tx = {
            let mut st = lock_state(&self.state);
            if st.done {
                return;
            }
            st.done = true;
            st.settle_tx.take()
        };
        if let Some(tx) = tx {
            let _ = tx.send(reason.to_owned());
        }
    }

    fn emit_text(&self, text: &str) {
        (self.on_text)(text);
    }

    fn stream_record_locked(&self, st: &mut FollowState, record: &Value) -> Option<String> {
        let record_type = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if record_type == "agent_settled" {
            let end_reason =
                if record.get("reason").and_then(Value::as_str) == Some("session_closed") {
                    "session_closed"
                } else if st.last_stop_reason.as_deref() == Some("aborted") {
                    "interrupted"
                } else {
                    "settled"
                };
            return Some(end_reason.to_owned());
        }
        if record_type == "agent_start" {
            st.last_stop_reason = None;
        }
        if record_type == "message_end"
            && let Some(msg) = record.get("message").and_then(Value::as_object)
            && msg.get("role").and_then(Value::as_str) == Some("assistant")
        {
            st.last_stop_reason = msg
                .get("stopReason")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if matches!(record_type, "message_start" | "message_end") {
            st.new_message = true;
        }
        let Some(event) = record
            .get("assistantMessageEvent")
            .and_then(Value::as_object)
        else {
            return None;
        };
        if record_type != "message_update"
            || event.get("type").and_then(Value::as_str) != Some("text_delta")
        {
            return None;
        }
        let Some(delta) = event
            .get("delta")
            .and_then(Value::as_str)
            .filter(|d| !d.is_empty())
        else {
            return None;
        };
        let content_index = event.get("contentIndex").cloned();
        if let Some(ref last) = st.last
            && !last.ends_line
            && (st.new_message || content_index != last.index)
        {
            st.streamed.push('\n');
            self.emit_text("\n");
        }
        st.new_message = false;
        st.last = Some(LastDelta {
            index: content_index,
            ends_line: delta.ends_with('\n'),
        });
        st.streamed.push_str(delta);
        self.emit_text(delta);
        None
    }

    fn prompt_match_of(&self, record: &Value) -> Option<PromptMatch> {
        let text = self.options.text.as_deref()?;
        if record.get("type").and_then(Value::as_str) != Some("message_end")
            || !is_user_entry(record)
        {
            return None;
        }
        if let Some(stamped) = entry_client_message_id_of(record) {
            return (Some(stamped.as_str()) == self.options.client_message_id.as_deref())
                .then_some(PromptMatch::Id);
        }
        entry_text_of(record)
            .is_some_and(|t| t.contains(text))
            .then_some(PromptMatch::Text)
    }

    fn shows_stamping(record: &Value) -> bool {
        is_user_entry(record) && entry_client_message_id_of(record).is_some()
    }

    fn receive(self: &Arc<Self>, record: &Value) {
        if record.get("type").and_then(Value::as_str) == Some("agent_settled")
            && record.get("reason").and_then(Value::as_str) == Some("session_closed")
        {
            self.end("session_closed");
            return;
        }
        let mut st = lock_state(&self.state);
        if st.done {
            return;
        }
        match st.mode {
            FollowMode::Buffer => {
                st.buffer.push(record.clone());
            }
            FollowMode::Stream => {
                if let Some(end_reason) = self.stream_record_locked(&mut st, record) {
                    drop(st);
                    self.end(&end_reason);
                }
            }
            FollowMode::Transcript => {
                let record_type = record
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let signal = if record_type == "agent_settled" {
                    Some(true)
                } else if record_type == "agent_end"
                    || (record_type == "message_start"
                        && record
                            .get("message")
                            .and_then(Value::as_object)
                            .and_then(|m| m.get("role"))
                            .and_then(Value::as_str)
                            == Some("user"))
                {
                    Some(false)
                } else {
                    None
                };
                drop(st);
                if let Some(settle_signal) = signal {
                    self.schedule_check(settle_signal);
                }
            }
        }
    }

    fn schedule_check(self: &Arc<Self>, settle_signal: bool) {
        let me = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                me.check(settle_signal).await;
            });
        } else {
            futures::executor::block_on(me.check(settle_signal));
        }
    }

    fn schedule_probe(
        self: &Arc<Self>,
        settle_signal: bool,
        prompt_stored: bool,
        stored_by_text: bool,
    ) {
        let me = Arc::clone(self);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                me.probe(settle_signal, prompt_stored, stored_by_text).await;
            });
        } else {
            futures::executor::block_on(me.probe(settle_signal, prompt_stored, stored_by_text));
        }
    }

    async fn check(self: &Arc<Self>, mut settle_signal: bool) {
        let Some(text) = self.options.text.clone() else {
            return;
        };
        {
            let mut st = lock_state(&self.state);
            if st.done || st.mode != FollowMode::Transcript {
                return;
            }
            if st.checking {
                if settle_signal {
                    st.pending_signal = true;
                }
                return;
            }
            st.checking = true;
        }
        loop {
            let page = self
                .remote
                .sessions
                .read(
                    self.row.device.clone(),
                    self.row.session_id.clone(),
                    READ_TAIL,
                )
                .await;
            if let RemoteCommandOutcome::Ok { response, .. } = page
                && let Some(entries) = extract_entries(&response)
            {
                let end_outcome = {
                    let mut st = lock_state(&self.state);
                    if st.done || st.mode != FollowMode::Transcript {
                        None
                    } else {
                        let client_id = self.options.client_message_id.as_deref();
                        let own = client_id.and_then(|cid| {
                            entries.iter().position(|e| {
                                is_user_entry(e)
                                    && entry_client_message_id_of(e).as_deref() == Some(cid)
                            })
                        });
                        let anchor: Option<usize> = if let Some(own_idx) = own {
                            Some(own_idx)
                        } else {
                            let matches = anchor_matches(&entries, &text, None);
                            if client_id.is_none() {
                                if matches.is_empty() {
                                    None
                                } else {
                                    let ord = *st.match_ordinal.get_or_insert(matches.len());
                                    Some(matches[ord.min(matches.len()) - 1])
                                }
                            } else {
                                let carrying = text_carrying_user_entries(&entries, &text);
                                let seen = st.earlier_entries.get_or_insert_with(|| {
                                    carrying
                                        .iter()
                                        .map(|&idx| entry_key_of(&entries[idx]))
                                        .collect()
                                });
                                let unseen: Vec<usize> = carrying
                                    .into_iter()
                                    .filter(|&idx| !seen.contains(&entry_key_of(&entries[idx])))
                                    .collect();
                                unseen
                                    .iter()
                                    .copied()
                                    .find(|&idx| {
                                        entry_text_of(&entries[idx]).as_deref()
                                            == Some(text.as_str())
                                    })
                                    .or_else(|| unseen.first().copied())
                            }
                        };
                        anchor.and_then(|anchor_idx| {
                            let (answer, superseded, answer_at_tail, interrupted) =
                                collect_turn_answer(&entries, anchor_idx);
                            let end_reason = if interrupted {
                                "interrupted"
                            } else {
                                "settled"
                            };
                            if superseded {
                                Some((answer, end_reason))
                            } else if answer.is_empty() && !(settle_signal && interrupted) {
                                None
                            } else if settle_signal && (answer_at_tail || interrupted) {
                                Some((answer, end_reason))
                            } else {
                                None
                            }
                        })
                    }
                };
                if let Some((answer, reason)) = end_outcome {
                    for (idx, piece) in answer.iter().enumerate() {
                        if idx > 0 {
                            self.emit_text("\n");
                        }
                        self.emit_text(piece);
                    }
                    self.end(reason);
                }
            }
            let mut st = lock_state(&self.state);
            if !st.done && st.mode == FollowMode::Transcript && st.pending_signal {
                st.pending_signal = false;
                settle_signal = true;
                continue;
            }
            st.checking = false;
            st.pending_signal = false;
            break;
        }
    }

    async fn probe(
        self: &Arc<Self>,
        settle_signal: bool,
        prompt_stored: bool,
        stored_by_text: bool,
    ) {
        let Some(text) = self.options.text.clone() else {
            return;
        };
        if lock_state(&self.state).done {
            return;
        }
        let page = self
            .remote
            .sessions
            .read(
                self.row.device.clone(),
                self.row.session_id.clone(),
                READ_TAIL,
            )
            .await;
        let RemoteCommandOutcome::Ok { response, .. } = page else {
            return;
        };
        let Some(entries) = extract_entries(&response) else {
            return;
        };
        let st = lock_state(&self.state);
        if st.done {
            return;
        }
        let client_id = self.options.client_message_id.as_deref();
        let own = client_id.and_then(|cid| {
            entries.iter().position(|e| {
                is_user_entry(e) && entry_client_message_id_of(e).as_deref() == Some(cid)
            })
        });
        let anchor = if let Some(own_idx) = own {
            own_idx
        } else {
            if !prompt_stored {
                return;
            }
            if stored_by_text && entries.iter().any(Self::shows_stamping) {
                return;
            }
            if stored_by_text && self.stored_before_send.is_none() {
                return;
            }
            let before = if stored_by_text {
                self.stored_before_send.as_ref()
            } else {
                None
            };
            let matches: Vec<usize> = anchor_matches(&entries, &text, None)
                .into_iter()
                .filter(|&idx| before.is_none_or(|set| !set.contains(&entry_key_of(&entries[idx]))))
                .collect();
            let Some(&last) = matches.last() else {
                return;
            };
            last
        };
        let (answer, superseded, answer_at_tail, interrupted) =
            collect_turn_answer(&entries, anchor);
        if !(superseded || (settle_signal && (answer_at_tail || interrupted))) {
            return;
        }
        if answer.is_empty() && !interrupted {
            return;
        }
        let stored = answer.join("\n");
        let streamed = st.streamed.clone();
        drop(st);
        if streamed.is_empty() {
            for (idx, piece) in answer.iter().enumerate() {
                if idx > 0 {
                    self.emit_text("\n");
                }
                self.emit_text(piece);
            }
        } else if let Some(tail) = stored.strip_prefix(&streamed) {
            if !tail.is_empty() {
                self.emit_text(tail);
            }
        } else if !streamed.starts_with(&stored) {
            if !streamed.ends_with('\n') {
                self.emit_text("\n");
            }
            self.emit_text(&stored);
        }
        self.end(if interrupted {
            "interrupted"
        } else {
            "settled"
        });
    }

    fn aim(self: &Arc<Self>, lane: RemoteFollowLane) {
        let mut st = lock_state(&self.state);
        if st.done || st.mode != FollowMode::Buffer {
            return;
        }
        if lane == RemoteFollowLane::Queued {
            st.mode = FollowMode::Transcript;
            drop(st);
            self.schedule_check(false);
            return;
        }
        st.mode = FollowMode::Stream;
        let run_start = st
            .buffer
            .iter()
            .rposition(|r| r.get("type").and_then(Value::as_str) == Some("agent_start"));
        let run_settled = run_start.is_some_and(|start| {
            st.buffer[start + 1..]
                .iter()
                .any(|r| r.get("type").and_then(Value::as_str) == Some("agent_settled"))
        });
        if let Some(start) = run_start
            && !run_settled
        {
            for record in &st.buffer[start + 1..] {
                if record.get("type").and_then(Value::as_str) == Some("message_end")
                    && let Some(ended) = record.get("message").and_then(Value::as_object)
                    && ended.get("role").and_then(Value::as_str) == Some("assistant")
                {
                    st.last_stop_reason = ended
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
        }
        if lane == RemoteFollowLane::Started {
            let by_id = st
                .buffer
                .iter()
                .rposition(|r| self.prompt_match_of(r) == Some(PromptMatch::Id));
            let text_allowed = self.options.client_message_id.is_none()
                || !st.buffer.iter().any(Self::shows_stamping);
            let by_text = if by_id.is_some() || !text_allowed {
                None
            } else {
                st.buffer
                    .iter()
                    .rposition(|r| self.prompt_match_of(r) == Some(PromptMatch::Text))
            };
            let own_prompt = by_id.or(by_text);
            let settled_already = own_prompt.is_some_and(|idx| {
                st.buffer[idx + 1..]
                    .iter()
                    .any(|r| r.get("type").and_then(Value::as_str) == Some("agent_settled"))
            });
            // 25dc4d8220 (#13512): retain buffered output from the identified active send.
            // Note: JS findLastIndex returns -1 when absent, so byId > runStart holds when runStart is absent or < byId.
            if let Some(id_idx) = by_id
                && run_start.is_none_or(|start| id_idx > start)
                && !settled_already
            {
                let tail: Vec<Value> = st.buffer[id_idx + 1..].to_vec();
                let mut end_from_buffer = None;
                for record in &tail {
                    if record.get("type").and_then(Value::as_str) == Some("message_start")
                        && is_user_entry(record)
                    {
                        break;
                    }
                    if let Some(reason) = self.stream_record_locked(&mut st, record) {
                        end_from_buffer = Some(reason);
                        break;
                    }
                }
                if let Some(reason) = end_from_buffer {
                    drop(st);
                    self.end(&reason);
                    return;
                }
            }
            let stored_by_text = by_id.is_none() && self.options.client_message_id.is_some();
            drop(st);
            self.schedule_probe(settled_already, own_prompt.is_some(), stored_by_text);
        }
    }
}

fn collect_turn_answer(entries: &[Value], anchor: usize) -> (Vec<String>, bool, bool, bool) {
    let mut answer = Vec::new();
    let mut superseded = false;
    let mut answer_at_tail = false;
    let mut stop_reason: Option<String> = None;
    for entry in &entries[anchor + 1..] {
        let Some(message) = message_of(entry) else {
            continue;
        };
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role == "user" {
            superseded = true;
            break;
        }
        if role != "assistant" {
            continue;
        }
        stop_reason = message
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let Some(text) = entry_text_of(entry).filter(|t| !t.is_empty()) {
            answer.push(text);
            answer_at_tail = true;
        } else {
            answer_at_tail = false;
        }
    }
    let interrupted = stop_reason.as_deref() == Some("aborted");
    (answer, superseded, answer_at_tail, interrupted)
}

/// Follows the answer to one send on another device's session (`remote-ops.ts` `followRemote`).
pub async fn follow_remote(
    remote: Arc<RemoteThreadSurface>,
    row: RemoteSessionRow,
    on_text: Arc<dyn Fn(&str) + Send + Sync>,
    options: FollowRemoteOptions,
) -> Result<RemoteFollow, ThreadToolFailure> {
    resolve_typed_device(remote.client.as_ref(), &row.device, true).await?;
    let mut stored_before_send: Option<HashSet<String>> = None;
    if let (Some(text), Some(_)) = (&options.text, &options.client_message_id) {
        let page = remote
            .sessions
            .read(row.device.clone(), row.session_id.clone(), READ_TAIL)
            .await;
        if let RemoteCommandOutcome::Ok { response, .. } = page
            && let Some(entries) = extract_entries(&response)
        {
            let keys = text_carrying_user_entries(&entries, text)
                .into_iter()
                .map(|idx| entry_key_of(&entries[idx]))
                .collect();
            stored_before_send = Some(keys);
        }
    }
    let (settle_tx, settle_rx) = oneshot::channel::<String>();
    let runner = Arc::new(FollowRunner {
        remote: Arc::clone(&remote),
        row: row.clone(),
        on_text,
        options,
        stored_before_send,
        state: Mutex::new(FollowState {
            done: false,
            settle_tx: Some(settle_tx),
            mode: FollowMode::Buffer,
            last: None,
            new_message: false,
            checking: false,
            pending_signal: false,
            match_ordinal: None,
            earlier_entries: None,
            last_stop_reason: None,
            streamed: String::new(),
            buffer: Vec::new(),
        }),
    });
    let r_rec = Arc::clone(&runner);
    let r_lost = Arc::clone(&runner);
    let handlers = RemoteWatchHandlers {
        record: Arc::new(move |record| {
            r_rec.receive(record);
            Ok(())
        }),
        status: None,
        resynced: None,
        lost: Some(Arc::new(move |reason| {
            r_lost.end(reason);
            Ok(())
        })),
    };
    let watched = remote
        .sessions
        .watch(
            row.device.clone(),
            row.session_id.clone(),
            handlers,
            RemoteWatchOptions::default(),
        )
        .await;
    let watch = match watched {
        RemoteWatchOutcome::Ok { watch } => watch,
        RemoteWatchOutcome::Refused { reason } => {
            return Err(*relay_outcome_failure(
                &RemoteCommandOutcome::Refused {
                    reason,
                    detail: None,
                },
                &row.device,
            ));
        }
    };
    let ended = watch.ended;
    let r_end = Arc::clone(&runner);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let reason = ended.await;
            r_end.end(&reason);
        });
    }
    let r_aim = Arc::clone(&runner);
    Ok(RemoteFollow {
        settled: Box::pin(async move { settle_rx.await.unwrap_or_else(|_| "stopped".to_owned()) }),
        stop: watch.stop,
        aim_fn: Arc::new(move |lane| r_aim.aim(lane)),
    })
}

#[cfg(test)]
#[path = "remote_ops_tests.rs"]
mod tests;
