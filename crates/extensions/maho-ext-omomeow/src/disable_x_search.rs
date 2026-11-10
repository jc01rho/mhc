//! Native port of the owner's `~/.omo/agent/extensions/disable-x-search.js` ("User policy: do not
//! expose or execute x_search"): the tool is dropped from the active set at session start and before
//! each agent turn, and a call that still names it is blocked. The CLI maps that file here by name.

use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ToolCallEventResult,
};
use std::sync::Arc;

pub const TOOL: &str = "x_search";
pub const REASON: &str = "x_search is disabled by user policy.";

/// The active set without x_search, or `None` when it is already absent (nothing to change).
pub fn without_x_search(active: Vec<String>) -> Option<Vec<String>> {
    active
        .iter()
        .any(|name| name == TOOL)
        .then(|| active.into_iter().filter(|name| name != TOOL).collect())
}

pub struct DisableXSearch;

impl Extension for DisableXSearch {
    fn register(&self, api: &mut ExtensionApi) {
        let sender = Arc::new(ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        for kind in [EventKind::SessionStart, EventKind::BeforeAgentStart] {
            let sender = sender.clone();
            api.on(
                kind,
                Arc::new(move |_, _| {
                    let sender = sender.clone();
                    Box::pin(async move {
                        let actions = sender.runtime.session_actions()?;
                        if let Some(active) = without_x_search(actions.get_active_tools()?) {
                            actions.set_active_tools(active)?;
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        api.on(
            EventKind::ToolCall,
            Arc::new(|event, _| {
                Box::pin(async move {
                    if let ExtensionEvent::ToolCall(call) = event
                        && call.tool_name == TOOL
                    {
                        return Ok(EventResult::ToolCall(ToolCallEventResult {
                            block: Some(true),
                            reason: Some(REASON.into()),
                            terminate: None,
                        }));
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x_search_is_removed_and_the_rest_keeps_its_order() {
        let active = vec!["read".to_owned(), "x_search".to_owned(), "bash".to_owned()];
        assert_eq!(
            without_x_search(active),
            Some(vec!["read".to_owned(), "bash".to_owned()])
        );
    }

    #[test]
    fn a_set_without_x_search_is_left_alone() {
        assert_eq!(without_x_search(vec!["read".to_owned()]), None);
    }
}
