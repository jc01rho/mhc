//! `senpi/model-decorators.ts`: strip trailing `:` thinking-level and service-tier decorators from
//! a model reference, right to left and at most two.

pub const SENPI_THINKING_LEVELS: &[&str] =
    &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

pub const SENPI_SERVICE_TIERS: &[&str] = &["auto", "flex", "priority", "ultrafast"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDecorators {
    pub base: String,
    pub thinking_level: Option<String>,
    pub service_tier: Option<String>,
}

/// Strip trailing `:<thinking-level>` and `:<service-tier>` decorators from a model reference,
/// right to left and at most two — the same suffix grammar senpi's `parseModelPattern` consumes
/// after its full-id match. A suffix that matches neither vocabulary stays glued to the id, so a
/// genuine colon id (`provider/model:exacto`) is never mangled.
pub fn split_model_decorators(reference: &str, thinking_only: bool) -> ModelDecorators {
    let mut base = reference.to_string();
    let mut thinking_level: Option<String> = None;
    let mut service_tier: Option<String> = None;
    for _ in 0..2 {
        let Some(colon) = base.rfind(':') else {
            break;
        };
        if colon == 0 {
            break;
        }
        let suffix = &base[colon + 1..];
        if thinking_level.is_none() && SENPI_THINKING_LEVELS.contains(&suffix) {
            thinking_level = Some(suffix.to_string());
            base.truncate(colon);
            continue;
        }
        if !thinking_only && service_tier.is_none() && SENPI_SERVICE_TIERS.contains(&suffix) {
            service_tier = Some(suffix.to_string());
            base.truncate(colon);
            continue;
        }
        break;
    }
    ModelDecorators {
        base,
        thinking_level,
        service_tier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_level_and_service_tier_suffixes_split_right_to_left() {
        assert_eq!(
            split_model_decorators("openai/gpt-5:high:priority", false),
            ModelDecorators {
                base: "openai/gpt-5".to_string(),
                thinking_level: Some("high".to_string()),
                service_tier: Some("priority".to_string()),
            }
        );
        assert_eq!(
            split_model_decorators("openai/gpt-5:flex:xhigh", false),
            ModelDecorators {
                base: "openai/gpt-5".to_string(),
                thinking_level: Some("xhigh".to_string()),
                service_tier: Some("flex".to_string()),
            }
        );
    }

    #[test]
    fn a_genuine_colon_id_stays_glued() {
        assert_eq!(
            split_model_decorators("openrouter/model:exacto", false),
            ModelDecorators {
                base: "openrouter/model:exacto".to_string(),
                thinking_level: None,
                service_tier: None,
            }
        );
        assert_eq!(
            split_model_decorators("openrouter/model:exacto:high", false),
            ModelDecorators {
                base: "openrouter/model:exacto".to_string(),
                thinking_level: Some("high".to_string()),
                service_tier: None,
            }
        );
    }

    #[test]
    fn thinking_only_leaves_a_service_tier_on_the_id() {
        assert_eq!(
            split_model_decorators("openai/gpt-5:priority", true),
            ModelDecorators {
                base: "openai/gpt-5:priority".to_string(),
                thinking_level: None,
                service_tier: None,
            }
        );
        assert_eq!(
            split_model_decorators("openai/gpt-5:priority:high", true),
            ModelDecorators {
                base: "openai/gpt-5:priority".to_string(),
                thinking_level: Some("high".to_string()),
                service_tier: None,
            }
        );
    }

    #[test]
    fn at_most_two_decorators_are_stripped() {
        assert_eq!(
            split_model_decorators("openai/gpt-5:low:high:priority", false),
            ModelDecorators {
                base: "openai/gpt-5:low".to_string(),
                thinking_level: Some("high".to_string()),
                service_tier: Some("priority".to_string()),
            }
        );
    }

    #[test]
    fn a_bare_or_trailing_colon_is_not_a_decorator() {
        assert_eq!(
            split_model_decorators(":high", false),
            ModelDecorators {
                base: ":high".to_string(),
                thinking_level: None,
                service_tier: None,
            }
        );
        assert_eq!(
            split_model_decorators("openai/gpt-5:", false),
            ModelDecorators {
                base: "openai/gpt-5:".to_string(),
                thinking_level: None,
                service_tier: None,
            }
        );
    }

    #[test]
    fn vocabularies_are_upstreams() {
        assert_eq!(
            SENPI_THINKING_LEVELS,
            &["off", "minimal", "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            SENPI_SERVICE_TIERS,
            &["auto", "flex", "priority", "ultrafast"]
        );
    }
}
