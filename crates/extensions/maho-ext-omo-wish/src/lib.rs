//! Native port of omo-wish `extensions/wish.js`: a turn whose prompt mentions `ulw`/`ultrawork`
//! arms the ultrawork directive once per session even when the prompt did not arrive as input
//! (an RPC prompt, a delivery). Extension-sourced input never arms the builtin engine, so the
//! prompt is classified as an external (RPC) prompt, which is what it is to the user. The directive and arming table are the shared `maho-ultrawork`
//! ones, so a session the builtin engine already armed is left alone.

use maho_ext_api::{
    BeforeAgentStartEventResult, CustomMessage, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionEvent, ExtensionHandlerOptions, InputSource,
};
use maho_ultrawork::{Engine, InputRequest, has_visible_keyword};
use std::sync::Arc;

/// The directive for `prompt` in `session`, arming the session; `None` when nothing applies.
pub fn wish_directive(engine: &Engine, session: &str, prompt: &str, preview: bool) -> Option<CustomMessage> {
    if preview || session.is_empty() || !has_visible_keyword(prompt) {
        return None;
    }
    if maho_ultrawork::snapshot(session).armed {
        return None;
    }
    engine.on_input(
        InputRequest { session, text: prompt, source: InputSource::Rpc, streaming: false },
        false,
    );
    engine.on_turn_start(session, false)
}

pub struct OmoWish;

impl Extension for OmoWish {
    fn register(&self, api: &mut ExtensionApi) {
        let engine = Engine::default();
        api.on_with_options(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, ctx| {
                let engine = engine.clone();
                Box::pin(async move {
                    let ExtensionEvent::BeforeAgentStart(start) = event else {
                        return Ok(EventResult::None);
                    };
                    let session = ctx.session_manager.session_id().to_owned();
                    Ok(match wish_directive(&engine, &session, &start.prompt, start.preview) {
                        Some(message) => EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                            message: Some(message),
                            ..Default::default()
                        }),
                        None => EventResult::None,
                    })
                })
            }),
            ExtensionHandlerOptions { preview_safe: true, mutates_messages: None },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_arms_once_and_skips_previews() {
        let engine = Engine::default();
        let session = format!("wish-test-{}", std::process::id());
        assert!(wish_directive(&engine, &session, "ulw fix it", true).is_none());
        assert!(wish_directive(&engine, &session, "just fix it", false).is_none());
        let message = wish_directive(&engine, &session, "ulw fix it", false).expect("directive");
        assert_eq!(message.custom_type, maho_ultrawork::DIRECTIVE_MESSAGE_TYPE);
        assert!(!message.display);
        assert!(maho_ultrawork::snapshot(&session).armed);
        assert!(wish_directive(&engine, &session, "ulw again", false).is_none());
        assert!(wish_directive(&engine, "", "ulw", false).is_none());
    }
}
