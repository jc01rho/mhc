//! The launch options of a new thread.
//!
//! Upstream counterpart: `thread/control/create-launch.ts` (omo #13409, #13410): checked before anything is opened. The
//! launch fields name a new thread only; the model is an exact provider/id; the approval mode is the creator's own or
//! one that allows less, never more, so a session cannot start a thread that may do what it may not.

use serde_json::json;

use crate::thread::contracts_create_params::ThreadCreateInput;
use crate::thread::contracts_permission_presets::preset_within;
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};

/// The engine launch profile a create names: senpi's `open_session` fields, applied only when it creates the session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchProfile {
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateLaunch {
    pub permission_preset: Option<String>,
    pub profile: LaunchProfile,
}

fn refusal(
    code: &str,
    message: String,
    next_action: &str,
    details: Option<serde_json::Value>,
) -> Box<ThreadToolFailure> {
    Box::new(thread_tool_failure(code, message, next_action, details))
}

/// The new thread's launch options from a create call. `prompt` and `delivery` are not launch fields: a create that
/// opens an existing thread sends its prompt to it (#14335), so they are taken with `thread` too.
pub fn create_launch(
    value: &ThreadCreateInput,
    creator: Option<&str>,
) -> Result<CreateLaunch, Box<ThreadToolFailure>> {
    let named: Vec<&str> = [
        ("model", value.model.is_some()),
        ("reasoning", value.reasoning.is_some()),
        ("permission_preset", value.permission_preset.is_some()),
    ]
    .into_iter()
    .filter(|(_, present)| *present)
    .map(|(field, _)| field)
    .collect();
    if value.thread.is_some() && !named.is_empty() {
        return Err(refusal(
            "invalid_arguments",
            format!(
                "{} apply to a new thread; thread opens an existing one as it is.",
                named.join(", ")
            ),
            "Drop them to open the thread, or drop thread to create a new one with them.",
            Some(json!({ "fields": named })),
        ));
    }
    if value.delivery.is_some() && value.prompt.is_none() {
        return Err(refusal(
            "invalid_arguments",
            "delivery names how prompt is delivered; no prompt was given.".to_owned(),
            "Pass prompt, or drop delivery.",
            None,
        ));
    }
    let mut profile = LaunchProfile {
        thinking_level: value.reasoning.clone(),
        ..LaunchProfile::default()
    };
    if let Some(model) = &value.model {
        let slash = model.find('/');
        match slash {
            Some(index) if index > 0 && index < model.len() - 1 => {
                profile.provider = Some(model[..index].to_owned());
                profile.model_id = Some(model[index + 1..].to_owned());
            }
            _ => {
                return Err(refusal(
                    "invalid_arguments",
                    format!("model {model:?} is not a provider/id."),
                    "Pass the exact provider/id the device lists (thread_set_model, tool.thread_set_model() in an eval cell, shows them).",
                    Some(json!({ "model": model })),
                ));
            }
        }
    }
    if let Some(asked) = value.permission_preset.as_deref()
        && !preset_within(asked, creator)
    {
        let mut details = json!({ "reason": "permission_preset_wider", "requested": asked });
        if let (Some(creator), Some(object)) = (creator, details.as_object_mut()) {
            object.insert("creator".to_owned(), json!(creator));
        }
        return Err(refusal(
            "scope_denied",
            format!(
                "This session runs in {}; a thread it creates cannot run in {asked}, which allows more.",
                creator.unwrap_or("an unknown approval mode")
            ),
            "Pass this session's own mode or a narrower one (ask is always allowed), or leave permission_preset unset.",
            Some(details),
        ));
    }
    let permission_preset = value
        .permission_preset
        .clone()
        .or_else(|| creator.map(str::to_owned));
    Ok(CreateLaunch {
        permission_preset,
        profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(build: impl FnOnce(&mut ThreadCreateInput)) -> ThreadCreateInput {
        let mut value = ThreadCreateInput::default();
        build(&mut value);
        value
    }

    #[test]
    fn a_model_and_a_level_become_the_launch_profile() {
        let launch = create_launch(
            &input(|value| {
                value.model = Some("anthropic/claude-opus-5-5".into());
                value.reasoning = Some("high".into());
            }),
            None,
        )
        .expect("launch");
        assert_eq!(
            launch.profile,
            LaunchProfile {
                provider: Some("anthropic".into()),
                model_id: Some("claude-opus-5-5".into()),
                thinking_level: Some("high".into()),
            }
        );
        assert_eq!(launch.permission_preset, None);
    }

    #[test]
    fn a_model_that_is_not_provider_slash_id_is_refused() {
        for model in ["opus", "/opus", "anthropic/"] {
            let refused = create_launch(&input(|value| value.model = Some(model.into())), None)
                .expect_err("refused");
            assert_eq!(refused.code.as_str(), "invalid_arguments", "{model}");
            assert_eq!(refused.details.expect("details")["model"], model);
        }
    }

    #[test]
    fn launch_fields_apply_to_a_new_thread_only() {
        let refused = create_launch(
            &input(|value| {
                value.thread = Some("dur-on-b".into());
                value.model = Some("anthropic/opus".into());
                value.permission_preset = Some("ask".into());
            }),
            None,
        )
        .expect_err("refused");
        assert_eq!(refused.code.as_str(), "invalid_arguments");
        assert_eq!(
            refused.details.expect("details")["fields"],
            json!(["model", "permission_preset"])
        );
        assert!(
            create_launch(
                &input(|value| {
                    value.thread = Some("dur-on-b".into());
                    value.prompt = Some("carry on".into());
                    value.delivery = Some("follow_up".into());
                }),
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_delivery_needs_a_prompt() {
        let refused = create_launch(&input(|value| value.delivery = Some("steer".into())), None)
            .expect_err("refused");
        assert_eq!(refused.code.as_str(), "invalid_arguments");
        assert!(refused.message.contains("no prompt was given"));
    }

    #[test]
    fn the_approval_mode_is_the_creators_or_narrower_never_wider() {
        let wider = create_launch(
            &input(|value| value.permission_preset = Some("full-access".into())),
            Some("workspace"),
        )
        .expect_err("refused");
        assert_eq!(wider.code.as_str(), "scope_denied");
        assert_eq!(
            wider.details.expect("details"),
            json!({ "reason": "permission_preset_wider", "requested": "full-access", "creator": "workspace" })
        );

        let narrower = create_launch(
            &input(|value| value.permission_preset = Some("read-only".into())),
            Some("workspace"),
        )
        .expect("narrower");
        assert_eq!(narrower.permission_preset.as_deref(), Some("read-only"));

        let inherited = create_launch(&input(|_| {}), Some("workspace")).expect("inherited");
        assert_eq!(inherited.permission_preset.as_deref(), Some("workspace"));

        let unknown = create_launch(
            &input(|value| value.permission_preset = Some("read-only".into())),
            None,
        )
        .expect_err("unknown creator");
        assert!(unknown.message.contains("an unknown approval mode"));
        assert_eq!(
            unknown.details.expect("details"),
            json!({ "reason": "permission_preset_wider", "requested": "read-only" })
        );
        assert!(
            create_launch(
                &input(|value| value.permission_preset = Some("ask".into())),
                None
            )
            .is_ok()
        );
    }
}
