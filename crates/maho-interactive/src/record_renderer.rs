//! Port of senpi `modes/interactive/record-renderer.ts`: adapts canonical history and streaming RPC
//! records to the existing TUI message components. Messages stay JSON (`serde_json::Value`), the
//! shape the endpoint sends and `AssistantMessageComponent::update_content` reads.
use crate::{
    components::{
        assistant_message::AssistantMessageComponent,
        markdown_transform::{MarkdownComponent, shared_markdown_theme},
        user_message::UserMessageComponent,
    },
    theme::theme::{Theme, ThemeColor},
};
use maho_tui::{
    components::{
        markdown::{Markdown, MarkdownOptions},
        text::Text,
    },
    tui::{Component, Container},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

struct Row {
    key: String,
    message: Value,
    component: Rc<RefCell<dyn Component>>,
    assistant: Option<Rc<RefCell<AssistantMessageComponent>>>,
}

struct Active {
    message: Value,
    component: Rc<RefCell<AssistantMessageComponent>>,
}

pub struct RecordRenderer {
    rows: Vec<Row>,
    finalized: HashSet<String>,
    history_keys: HashMap<String, String>,
    active: Option<Active>,
    theme: Theme,
    container: Container,
}

fn set_part(content: &mut Vec<Value>, index: usize, part: Value) {
    if content.len() <= index {
        content.resize(index + 1, Value::Null);
    }
    if let Some(slot) = content.get_mut(index) {
        *slot = part;
    }
}

fn append_to_part(
    content: &mut [Value],
    index: usize,
    kind: &str,
    field: &str,
    delta: &str,
) -> bool {
    let Some(part) = content.get_mut(index) else {
        return false;
    };
    if part["type"] != kind {
        return false;
    }
    let current = part[field].as_str().unwrap_or_default().to_owned();
    part[field] = Value::String(format!("{current}{delta}"));
    true
}

fn message_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                if part["type"] == "text" {
                    part["text"].as_str().unwrap_or_default().to_owned()
                } else {
                    "[image]".to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

impl RecordRenderer {
    pub fn new(theme: &Theme) -> Self {
        Self {
            rows: Vec::new(),
            finalized: HashSet::new(),
            history_keys: HashMap::new(),
            active: None,
            theme: theme.clone(),
            container: Container::new(),
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn prepend(&mut self, entries: &[Value]) {
        let mut added = Vec::new();
        for entry in entries {
            if entry["type"] != "message" {
                continue;
            }
            let Some(key) = entry["id"].as_str() else {
                continue;
            };
            let message = &entry["message"];
            self.history_keys
                .insert(Self::message_key(message), key.to_owned());
            self.finalized.insert(key.to_owned());
            if self.rows.iter().any(|row| row.key == key) {
                continue;
            }
            added.push(self.new_row(key.to_owned(), message.clone()));
        }
        added.append(&mut self.rows);
        self.rows = added;
        self.rebuild();
    }

    pub fn accept(&mut self, record: &Value) {
        let entry_id = record["entryId"].as_str();
        match record["type"].as_str() {
            Some("message_start") => {
                let message = &record["message"];
                if self.finalized.contains(&self.key(message, entry_id)) {
                    return;
                }
                self.put(message, entry_id);
            }
            Some("message_update") => {
                if record["message"].is_object() {
                    self.put(&record["message"], entry_id);
                    return;
                }
                self.apply_delta(record);
            }
            Some("message_end") => {
                let message = &record["message"];
                self.put(message, entry_id);
                let key = self.key(message, entry_id);
                self.finalized.insert(key);
                self.active = None;
            }
            _ => {}
        }
    }

    fn apply_delta(&mut self, record: &Value) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        let event = &record["assistantMessageEvent"];
        let Some(index) = event["contentIndex"]
            .as_u64()
            .and_then(|index| usize::try_from(index).ok())
        else {
            return;
        };
        if !active.message["content"].is_array() {
            active.message["content"] = json!([]);
        }
        let Some(content) = active.message["content"].as_array_mut() else {
            return;
        };
        let delta = event["delta"].as_str().unwrap_or_default();
        match event["type"].as_str() {
            Some("text_start") => set_part(content, index, json!({"type": "text", "text": ""})),
            Some("thinking_start") => {
                set_part(content, index, json!({"type": "thinking", "thinking": ""}));
            }
            Some("text_delta") => {
                let appended = append_to_part(content, index, "text", "text", delta);
                if !appended {
                    set_part(content, index, json!({"type": "text", "text": delta}));
                }
            }
            Some("thinking_delta") => {
                let appended = append_to_part(content, index, "thinking", "thinking", delta);
                if !appended {
                    set_part(
                        content,
                        index,
                        json!({"type": "thinking", "thinking": delta}),
                    );
                }
            }
            Some("toolcall_start") => set_part(
                content,
                index,
                json!({"type": "toolCall", "id": event["id"], "name": event["toolName"], "arguments": {}}),
            ),
            Some("toolcall_end") => set_part(content, index, event["toolCall"].clone()),
            _ => {}
        }
        if let Some(usage) = record.get("usage") {
            active.message["usage"] = usage.clone();
        }
        active
            .component
            .borrow_mut()
            .update_content(&active.message, None);
    }

    fn message_key(message: &Value) -> String {
        if message["role"] == "toolResult" {
            format!(
                "tool:{}",
                message["toolCallId"].as_str().unwrap_or_default()
            )
        } else {
            format!(
                "{}:{}",
                message["role"].as_str().unwrap_or_default(),
                message["timestamp"]
            )
        }
    }

    fn key(&self, message: &Value, entry_id: Option<&str>) -> String {
        entry_id
            .map(str::to_owned)
            .or_else(|| self.history_keys.get(&Self::message_key(message)).cloned())
            .unwrap_or_else(|| Self::message_key(message))
    }

    fn assistant_component(
        &self,
        message: Option<Value>,
    ) -> Rc<RefCell<AssistantMessageComponent>> {
        Rc::new(RefCell::new(AssistantMessageComponent::new(
            message,
            false,
            shared_markdown_theme(&self.theme),
            "Thinking…",
            0,
            Vec::new(),
            self.theme.clone(),
        )))
    }

    fn new_row(&self, key: String, message: Value) -> Row {
        if message["role"] == "assistant" {
            let assistant = self.assistant_component(Some(message.clone()));
            return Row {
                key,
                message,
                component: assistant.clone(),
                assistant: Some(assistant),
            };
        }
        Row {
            key,
            component: self.component(&message),
            message,
            assistant: None,
        }
    }

    fn put(&mut self, message: &Value, entry_id: Option<&str>) {
        let key = self.key(message, entry_id);
        let message_key = Self::message_key(message);
        let existing = self.rows.iter().position(|row| row.key == key).or_else(|| {
            self.rows.iter().position(|row| {
                !self.finalized.contains(&row.key) && Self::message_key(&row.message) == message_key
            })
        });
        if message["role"] == "assistant" {
            let component = existing
                .and_then(|index| self.rows.get(index))
                .and_then(|row| row.assistant.clone())
                .unwrap_or_else(|| self.assistant_component(None));
            let snapshot = message.clone();
            component.borrow_mut().update_content(&snapshot, None);
            self.active = Some(Active {
                message: snapshot.clone(),
                component: component.clone(),
            });
            let row = Row {
                key,
                message: snapshot,
                component: component.clone(),
                assistant: Some(component),
            };
            match existing.and_then(|index| self.rows.get_mut(index)) {
                Some(slot) => *slot = row,
                None => self.rows.push(row),
            }
        } else {
            let row = Row {
                key,
                message: message.clone(),
                component: self.component(message),
                assistant: None,
            };
            match existing.and_then(|index| self.rows.get_mut(index)) {
                Some(slot) => *slot = row,
                None => self.rows.push(row),
            }
        }
        self.rebuild();
    }

    fn component(&self, message: &Value) -> Rc<RefCell<dyn Component>> {
        let markdown_theme = shared_markdown_theme(&self.theme);
        match message["role"].as_str() {
            Some("assistant") => self.assistant_component(Some(message.clone())),
            Some("user") => Rc::new(RefCell::new(UserMessageComponent::new(
                message_text(&message["content"]),
                self.theme.clone(),
                markdown_theme,
                0,
                Vec::new(),
            ))),
            Some("toolResult") => {
                let mut result = Container::new();
                result.add_child(Rc::new(RefCell::new(Text::with_padding(
                    self.theme.fg(
                        ThemeColor::Muted,
                        message["toolName"].as_str().unwrap_or_default(),
                    ),
                    1,
                    0,
                ))));
                result.add_child(Rc::new(RefCell::new(MarkdownComponent(Markdown::new(
                    &message_text(&message["content"]),
                    1,
                    0,
                    markdown_theme,
                    None,
                    MarkdownOptions::default(),
                )))));
                Rc::new(RefCell::new(result))
            }
            other => {
                let text = match &message["content"] {
                    Value::String(text) => text.clone(),
                    _ => other.unwrap_or_default().to_owned(),
                };
                Rc::new(RefCell::new(Text::with_padding(text, 1, 0)))
            }
        }
    }

    fn rebuild(&mut self) {
        self.container.detach_all();
        for row in &self.rows {
            self.container.add_child(row.component.clone());
        }
    }
}

impl Component for RecordRenderer {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    fn invalidate(&mut self) {
        self.container.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorMode;

    fn renderer() -> RecordRenderer {
        RecordRenderer::new(&Theme::builtin("dark", ColorMode::Truecolor).unwrap())
    }

    fn history() -> Vec<Value> {
        vec![json!({
            "type": "message", "id": "history", "parentId": null, "timestamp": "2026-01-01",
            "message": {"role": "user", "content": "Previous question", "timestamp": 1},
        })]
    }

    fn assistant(text: &str) -> Value {
        json!({
            "role": "assistant", "timestamp": 2,
            "content": [{"type": "text", "text": text}],
        })
    }

    fn rendered(renderer: &mut RecordRenderer) -> String {
        renderer.render(80).join("\n")
    }

    #[test]
    fn history_prepends_in_order_and_is_not_duplicated() {
        let mut renderer = renderer();
        renderer.prepend(&history());
        renderer.prepend(&history());
        assert_eq!(renderer.row_count(), 1);
        assert!(rendered(&mut renderer).contains("Previous"));
    }

    #[test]
    fn streaming_deltas_build_one_assistant_row_until_the_message_ends() {
        let mut renderer = renderer();
        renderer.accept(&json!({"type": "message_start", "message": {"role": "assistant", "timestamp": 2, "content": []}}));
        renderer.accept(&json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_start", "contentIndex": 0},
        }));
        renderer.accept(&json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "streamed"},
        }));
        assert_eq!(renderer.row_count(), 1);
        assert!(rendered(&mut renderer).contains("streamed"));
        renderer.accept(&json!({"type": "message_end", "message": assistant("final")}));
        assert_eq!(renderer.row_count(), 1);
        let text = rendered(&mut renderer);
        assert!(text.contains("final"));
        assert!(!text.contains("streamed"));
    }

    #[test]
    fn a_finalized_message_is_not_started_again_and_history_keys_win() {
        let mut renderer = renderer();
        renderer.prepend(&history());
        renderer.accept(&json!({
            "type": "message_start",
            "message": {"role": "user", "content": "Previous question", "timestamp": 1},
        }));
        assert_eq!(renderer.row_count(), 1);
        renderer.accept(&json!({
            "type": "message_end", "entryId": "e1", "message": assistant("one"),
        }));
        renderer.accept(&json!({
            "type": "message_start", "entryId": "e1", "message": assistant("one"),
        }));
        assert_eq!(renderer.row_count(), 2);
    }

    #[test]
    fn a_cumulative_message_in_an_update_replaces_the_row() {
        let mut renderer = renderer();
        renderer.accept(&json!({"type": "message_start", "message": assistant("a")}));
        renderer.accept(&json!({
            "type": "message_update",
            "message": assistant("authoritative"),
            "assistantMessageEvent": {"type": "text_delta", "contentIndex": 0, "delta": "ignored"},
        }));
        assert_eq!(renderer.row_count(), 1);
        assert!(rendered(&mut renderer).contains("authoritative"));
    }

    #[test]
    fn tool_results_render_their_tool_name_and_text() {
        let mut renderer = renderer();
        renderer.accept(&json!({
            "type": "message_end",
            "message": {
                "role": "toolResult", "toolCallId": "c1", "toolName": "readtool",
                "content": [{"type": "text", "text": "filebody"}], "timestamp": 3,
            },
        }));
        let text = rendered(&mut renderer);
        assert!(text.contains("readtool"));
        assert!(text.contains("filebody"));
    }
}
