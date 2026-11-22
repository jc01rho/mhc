use maho_ai::types::ImageContent;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachStartupModel {
    pub provider: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AttachPrompt {
    pub text: String,
    pub images: Vec<ImageContent>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachStartup {
    pub model: Option<AttachStartupModel>,
    pub thinking: Option<String>,
    pub prompts: Vec<AttachPrompt>,
}

pub fn apply_attach_startup(
    mut request: impl FnMut(Value) -> Result<Value, String>,
    session_id: &str,
    startup: &AttachStartup,
) -> Result<(), String> {
    if let Some(model) = &startup.model {
        request(json!({
            "type": "set_model",
            "sessionId": session_id,
            "provider": model.provider,
            "modelId": model.model_id,
            "scope": "session",
        }))?;
    }
    if let Some(thinking) = &startup.thinking {
        request(json!({
            "type": "set_thinking_level",
            "sessionId": session_id,
            "level": thinking,
        }))?;
    }
    for prompt in &startup.prompts {
        let mut command = json!({
            "type": "prompt",
            "sessionId": session_id,
            "message": prompt.text,
            "streamingBehavior": "followUp",
            "clientMessageId": Uuid::new_v4().to_string(),
        });
        if !prompt.images.is_empty() {
            command["images"] =
                serde_json::to_value(&prompt.images).map_err(|error| error.to_string())?;
        }
        request(command)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_session_scoped_model_thinking_and_prompts_in_order_and_stops_on_refusal() {
        let mut seen = Vec::<Value>::new();
        let startup = AttachStartup {
            model: Some(AttachStartupModel {
                provider: "anthropic".into(),
                model_id: "claude-sonnet-4".into(),
            }),
            thinking: Some("high".into()),
            prompts: vec![
                AttachPrompt {
                    text: "first line".into(),
                    images: Vec::new(),
                },
                AttachPrompt {
                    text: "second line".into(),
                    images: Vec::new(),
                },
            ],
        };
        apply_attach_startup(
            |command| {
                seen.push(command);
                Ok(json!({"success": true}))
            },
            "rpc-1",
            &startup,
        )
        .expect("applied");
        assert_eq!(seen.len(), 4);
        assert_eq!(
            seen[0],
            json!({
                "type": "set_model",
                "sessionId": "rpc-1",
                "provider": "anthropic",
                "modelId": "claude-sonnet-4",
                "scope": "session",
            })
        );
        assert_eq!(
            seen[1],
            json!({
                "type": "set_thinking_level",
                "sessionId": "rpc-1",
                "level": "high",
            })
        );
        assert_eq!(seen[2]["type"], "prompt");
        assert_eq!(seen[2]["message"], "first line");
        assert_eq!(seen[2]["streamingBehavior"], "followUp");
        assert!(seen[2]["clientMessageId"].is_string());
        assert_eq!(seen[3]["message"], "second line");

        let mut refused_calls = 0usize;
        let err = apply_attach_startup(
            |_command| {
                refused_calls += 1;
                Err("model_not_found".into())
            },
            "rpc-1",
            &startup,
        )
        .expect_err("refused");
        assert_eq!(err, "model_not_found");
        assert_eq!(refused_calls, 1);
    }
}
