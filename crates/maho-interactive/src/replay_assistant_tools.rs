use crate::{
    aborted_error_label::unrun_tool_call_text,
    components::{
        tool_execution::ToolExecutionComponent, tool_execution_types::ToolExecutionResult,
    },
};
use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_tools::definition::ToolContent;
use std::{cell::RefCell, rc::Rc};

pub trait ReplayToolHost {
    fn expanded(&self) -> bool;
    /// `owns_turn_notice`: this part is the one that shows the turn's error, abort or length notice
    /// (senpi#4958).
    fn add_message(&mut self, message: AssistantMessage, owns_turn_notice: bool);
    fn add_child(&mut self, component: Rc<RefCell<ToolExecutionComponent>>);
    fn create_tool(
        &mut self,
        name: &str,
        id: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> ToolExecutionComponent;
    fn add_pending(&mut self, id: &str, component: Rc<RefCell<ToolExecutionComponent>>);
}

pub fn replay_assistant_tools(message: &AssistantMessage, host: &mut impl ReplayToolHost) {
    let first = message
        .content
        .iter()
        .position(|block| matches!(block, ContentBlock::ToolCall(_)));
    // Every part keeps the turn's stopReason, so one part shows the turn-ending notice: the text run
    // that closes the message after a tool call, as in live mode, else the head row (or, for an abort,
    // the tool card).
    let mut last_run_start: Option<usize> = None;
    if first.is_some() && !matches!(message.content.last(), Some(ContentBlock::ToolCall(_))) {
        let mut start = message.content.len() - 1;
        while start > 0 && !matches!(message.content[start - 1], ContentBlock::ToolCall(_)) {
            start -= 1;
        }
        last_run_start = Some(start);
    }
    let mut head = message.clone();
    if let Some(index) = first {
        head.content.truncate(index + 1);
    }
    // An aborted turn that ends in a tool call shows its notice on the tool card, as in live mode.
    host.add_message(
        head,
        first.is_none() || (last_run_start.is_none() && message.stop_reason != StopReason::Aborted),
    );
    let Some(mut index) = first else {
        return;
    };
    while index < message.content.len() {
        if let ContentBlock::ToolCall(call) = &message.content[index] {
            index += 1;
            let component = Rc::new(RefCell::new(host.create_tool(
                &call.name,
                &call.id,
                &call.arguments,
            )));
            component.borrow_mut().set_expanded(host.expanded());
            host.add_child(component.clone());
            if matches!(message.stop_reason, StopReason::Aborted | StopReason::Error) {
                let rendered = serde_json::to_value(message).unwrap_or(serde_json::Value::Null);
                let rendered =
                    crate::aborted_error_label::aborted_message_for_rendering(&rendered, 0, None);
                component.borrow_mut().update_result(
                    ToolExecutionResult {
                        content: vec![ToolContent::text(unrun_tool_call_text(&rendered))],
                        details: None,
                        is_error: true,
                    },
                    false,
                );
            } else {
                host.add_pending(&call.id, component);
            }
        } else {
            let start = index;
            while index < message.content.len()
                && !matches!(message.content[index], ContentBlock::ToolCall(_))
            {
                index += 1;
            }
            let mut segment = message.clone();
            segment.content = message.content[start..index].to_vec();
            host.add_message(segment, Some(start) == last_run_start);
        }
    }
}
