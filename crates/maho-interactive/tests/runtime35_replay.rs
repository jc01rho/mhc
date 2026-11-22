use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_interactive::{
    components::tool_execution::{
        ToolExecutionComponent, ToolExecutionOptions, ToolExecutionPresentation,
    },
    replay_assistant_tools::*,
    theme::{ColorMode, Theme},
};
use maho_tui::tui::Component;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Host {
    messages: Vec<AssistantMessage>,
    notice_owners: Vec<bool>,
    tools: Vec<Rc<RefCell<ToolExecutionComponent>>>,
    pending: Vec<String>,
}
impl ReplayToolHost for Host {
    fn expanded(&self) -> bool {
        true
    }
    fn add_message(&mut self, message: AssistantMessage, owns_turn_notice: bool) {
        self.messages.push(message);
        self.notice_owners.push(owns_turn_notice);
    }
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>) {
        self.tools.push(component);
    }
    fn create_tool(
        &mut self,
        name: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> ToolExecutionComponent {
        ToolExecutionComponent::new(
            name,
            id,
            serde_json::Value::Object(args.clone()),
            ToolExecutionOptions::default(),
            None,
            "/tmp",
            ToolExecutionPresentation::Classic,
            None,
            Theme::builtin("dark", ColorMode::Truecolor).expect("theme"),
        )
    }
    fn add_pending(&mut self, id: &str, _: Rc<RefCell<ToolExecutionComponent>>) {
        self.pending.push(id.into());
    }
}
fn message(reason: StopReason) -> AssistantMessage {
    serde_json::from_value(serde_json::json!({
        "content": [{ "type": "text", "text": "before" }, { "type": "toolCall", "id": "first", "name": "custom", "arguments": {} }, { "type": "text", "text": "between" }, { "type": "toolCall", "id": "second", "name": "custom", "arguments": {} }, { "type": "text", "text": "after" }],
        "api": "anthropic-messages", "provider": "test", "model": "test", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0 } }, "stopReason": reason, "timestamp": 0
    })).expect("message")
}

#[test]
fn replay_keeps_text_segments_in_original_positions_and_tracks_pending_tools() {
    let mut host = Host::default();
    replay_assistant_tools(&message(StopReason::ToolUse), &mut host);
    assert_eq!(
        host.messages
            .iter()
            .map(|message| message.content.len())
            .collect::<Vec<_>>(),
        [2, 1, 1]
    );
    assert_eq!(host.pending, ["first", "second"]);
    assert!(
        matches!(&host.messages[1].content[0], ContentBlock::Text(text) if text.text == "between")
    );
}

#[test]
fn aborted_replay_finishes_tools_as_error_instead_of_leaving_them_pending() {
    let mut host = Host::default();
    replay_assistant_tools(&message(StopReason::Aborted), &mut host);
    assert!(host.pending.is_empty());
    let lines = host.tools[0].borrow_mut().render(80).join("\n");
    assert!(lines.contains("Provider request failed"));
}

#[test]
fn message_without_tools_is_replayed_once_without_segmentation() {
    let mut host = Host::default();
    let mut message = message(StopReason::Stop);
    message.content = vec![ContentBlock::text("only text")];
    replay_assistant_tools(&message, &mut host);
    assert_eq!(host.messages, [message]);
    assert_eq!(host.notice_owners, [true]);
    assert!(host.tools.is_empty());
}

#[test]
fn error_replay_marks_unrun_tool_cards_with_not_run_instead_of_raw_error() {
    let mut host = Host::default();
    let mut msg = message(StopReason::Error);
    msg.error_message = Some("429 Raw quota error".into());
    replay_assistant_tools(&msg, &mut host);
    assert_eq!(host.notice_owners, [false, false, true]);
    let lines = host.tools[0].borrow_mut().render(80).join("\n");
    assert!(lines.contains(maho_interactive::aborted_error_label::UNRUN_AFTER_ERROR));
    assert!(!lines.contains("429 Raw quota error"));
}

#[test]
fn question_list_emits_cursor_marker_on_own_answer_row_when_focused() {
    use maho_interactive::components::ask_user_question_render::render_question_list;
    use maho_interactive::components::ask_user_question_state::{
        AskUserQuestionState, Question, QuestionOption, QuestionRequest,
    };
    use maho_tui::tui::CURSOR_MARKER;

    let theme = Theme::builtin("dark", ColorMode::Truecolor).expect("theme");
    let state = AskUserQuestionState::new(QuestionRequest {
        request_id: "r1".into(),
        questions: vec![Question {
            id: "q1".into(),
            header: "H".into(),
            question: "Pick?".into(),
            options: vec![QuestionOption {
                label: "A".into(),
                description: None,
            }],
            multi_select: false,
        }],
        wait_for_answer: true,
        timeout_ms: 0,
    });
    let focused_lines = render_question_list(&theme, &state, true);
    assert!(
        focused_lines
            .last()
            .is_some_and(|line| line.ends_with(CURSOR_MARKER))
    );
    let unfocused_lines = render_question_list(&theme, &state, false);
    assert!(
        unfocused_lines
            .iter()
            .all(|line| !line.contains(CURSOR_MARKER))
    );
}

#[test]
fn theme_markdown_emphasis_wires_optional_tokens() {
    use maho_interactive::components::markdown_transform::get_markdown_theme;
    use maho_interactive::theme::{MarkdownEmphasisColor, theme_json::validate_theme_json};

    let dark = Theme::builtin("dark", ColorMode::Truecolor).expect("dark");
    assert!(!dark.has_markdown_emphasis(MarkdownEmphasisColor::MdBold));
    assert_eq!(
        dark.markdown_emphasis(MarkdownEmphasisColor::MdBold, "x"),
        "x"
    );

    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../src/theme/dark.json")).expect("json");
    raw["colors"]["mdBold"] = serde_json::Value::String("#ff8000".into());
    let custom = Theme::from_json(
        validate_theme_json("custom", raw).expect("valid"),
        ColorMode::Truecolor,
    )
    .expect("theme");
    assert!(custom.has_markdown_emphasis(MarkdownEmphasisColor::MdBold));
    assert_eq!(
        custom.markdown_emphasis(MarkdownEmphasisColor::MdBold, "x"),
        "\x1b[38;2;255;128;0mx\x1b[39m"
    );
    let md_theme = get_markdown_theme(&custom);
    assert!(md_theme.strong.is_some());
    assert!(md_theme.emphasis.is_none());
}
