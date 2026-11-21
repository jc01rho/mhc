//! `runners/designated-fallback-profile.ts` + `runners/retry-fallback-profile.ts`: the fallback
//! policy that holds a child to exactly its designated models (omo #12006 / #11781, #12759). Used only
//! under the designated-chain-only policy (`task.parent_model_fallback: false`).

use serde_json::{Map, Value, json};

/// The designated models in try order, as a refusal names them: `a -> b -> c`.
pub fn designated_chain_text(model: &str, fallback_models: &[String]) -> String {
    std::iter::once(model)
        .chain(fallback_models.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// The child's own chain keyed by its model, and an empty chain (senpi's tombstone) for every other
/// designated model's base, so neither senpi's shipped default chains nor the user's
/// `retry.fallbackChains` can extend the walk past the last rung. A one-model child gets fallback off.
pub fn designated_fallback_profile(model: &str, fallback_models: &[String]) -> Value {
    let mut chains = Map::new();
    for selector in std::iter::once(model).chain(fallback_models.iter().map(String::as_str)) {
        let base = crate::runners::rpc::model_admission::base_model(selector);
        if base != model {
            chains.insert(base.to_string(), json!([]));
        }
    }
    chains.insert(model.to_string(), json!(fallback_models));
    json!({ "modelFallback": !fallback_models.is_empty(), "fallbackChains": chains })
}

#[cfg(test)]
mod tests {
    use super::*;

    // rpc-process-fallback-chain.test.ts: the chain and its tombstones.
    #[test]
    fn a_chained_child_tombstones_every_other_designated_model() {
        let fallbacks = vec![
            "openai/gpt-5.6-sol".to_string(),
            "kimi-coding/kimi-k3:high".to_string(),
        ];
        assert_eq!(
            designated_fallback_profile("anthropic/claude-opus-5-5", &fallbacks),
            json!({
                "modelFallback": true,
                "fallbackChains": {
                    "anthropic/claude-opus-5-5": ["openai/gpt-5.6-sol", "kimi-coding/kimi-k3:high"],
                    "openai/gpt-5.6-sol": [],
                    "kimi-coding/kimi-k3": [],
                },
            })
        );
        assert_eq!(
            designated_chain_text("anthropic/claude-opus-5-5", &fallbacks),
            "anthropic/claude-opus-5-5 -> openai/gpt-5.6-sol -> kimi-coding/kimi-k3:high"
        );
    }

    #[test]
    fn a_one_model_child_gets_fallback_off() {
        assert_eq!(
            designated_fallback_profile("anthropic/claude-opus-5-5", &[]),
            json!({
                "modelFallback": false,
                "fallbackChains": { "anthropic/claude-opus-5-5": [] },
            })
        );
    }
}
