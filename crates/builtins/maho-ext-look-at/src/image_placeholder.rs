use maho_ai::api::transform_messages::{ImageDowngradeOptions, downgrade_unsupported_images};
use maho_ai::model::Model;
use maho_ai::types::{ContentBlock, Message, UserContent};
use maho_ext_api::AgentMessage;

fn message_carries_image(message: &Message) -> bool {
    match message {
        Message::User(user) => match &user.content {
            UserContent::Blocks(blocks) => blocks
                .iter()
                .any(|block| matches!(block, ContentBlock::Image(_))),
            UserContent::Text(_) => false,
        },
        Message::ToolResult(result) => result
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_))),
        _ => false,
    }
}

pub fn try_replace_images_naming_tool(
    messages: &[AgentMessage],
    model: &Model,
    tool_name: &str,
) -> Option<Vec<AgentMessage>> {
    let options = ImageDowngradeOptions {
        image_description_tool: Some(tool_name.to_owned()),
    };
    let mut replaced = false;
    let result: Vec<AgentMessage> = messages
        .iter()
        .map(|message| {
            let AgentMessage::Llm(llm_message) = message else {
                return message.clone();
            };
            if !message_carries_image(llm_message) {
                return message.clone();
            }
            replaced = true;
            downgrade_unsupported_images(std::slice::from_ref(llm_message), model, &options)
                .into_iter()
                .next()
                .map_or_else(|| message.clone(), AgentMessage::Llm)
        })
        .collect();
    replaced.then_some(result)
}

pub fn replace_images_naming_tool(
    messages: &[AgentMessage],
    model: &Model,
    tool_name: &str,
) -> Vec<AgentMessage> {
    try_replace_images_naming_tool(messages, model, tool_name).unwrap_or_else(|| messages.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text_only_model() -> Model {
        serde_json::from_value(json!({
            "id": "main",
            "name": "main",
            "api": "faux",
            "provider": "faux",
            "baseUrl": "http://faux.invalid",
            "reasoning": false,
            "input": ["text"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128_000,
            "maxTokens": 4096,
        }))
        .expect("text-only model")
    }

    #[test]
    fn keeps_the_session_entry_identity_of_a_message_whose_image_it_replaces() {
        let mut session = maho_core::session_manager::SessionManager::in_memory(".", None, None);
        let entry = session.append_message(json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "what is this" },
                { "type": "image", "data": "AAAA", "mimeType": "image/png" },
            ],
            "timestamp": 1,
        }));
        let entry_id = entry["id"].as_str().expect("entry id").to_owned();
        let context = session.current_context();
        let messages: Vec<AgentMessage> = context
            .messages
            .into_iter()
            .map(|value| maho_core::agent_session::session_message_from_value(value).expect("msg"))
            .collect();

        let replaced = replace_images_naming_tool(&messages, &text_only_model(), "look_at");

        assert_ne!(replaced[0], messages[0]);
        assert_eq!(context.entry_ids, vec![entry_id]);
    }
}
