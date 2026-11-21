//! Port of senpi packages/ai/src/utils/openai-prompt-cache-lane.ts (senpi#6292, closes #6290).
//!
//! Which OpenAI prompt-cache contract a Responses-lane request runs under
//! (<https://developers.openai.com/api/docs/guides/prompt-caching>, "Cache lifetime").

use crate::model::Model;
use crate::types::CacheRetention;
use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

/// `OpenAIPromptCacheLane`.
///
/// - `ChatgptSubscription`: signed-in ChatGPT usage (Codex backend or Sign in with ChatGPT). OpenAI keeps
///   this cache with no expiry (senpi#6290); no API doc publishes a lifetime, so it carries no TTL.
/// - `ApiLong`: GPT-5.6+ on a request that sends `prompt_cache_options.ttl: "12h"` (12-hour minimum).
/// - `ApiExtended`: GPT-5.6+ at the default `"30m"` minimum lifetime.
/// - `ApiShort`: earlier models, and gateways that proxy GPT-5.6+ ids without OpenAI's contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAIPromptCacheLane {
    ChatgptSubscription,
    ApiLong,
    ApiExtended,
    ApiShort,
}

/// GPT-5.6 and later (`gpt-5.6-*`, `gpt-5.10`, `gpt-6-*`, `gpt-7`, ...).
static OPENAI_EXTENDED_CACHE_MODEL_ID: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"^gpt-(?:5\.(?:[6-9]|\d{2,})|(?:[6-9]|\d{2,})(?:\.\d+)?)(?:-|$)")
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|error| panic!("{error}"))
});

/// The Codex backend serves only signed-in ChatGPT usage, whatever provider id a models.json entry gives it.
const CHATGPT_CODEX_BACKEND_HOST: &str = "chatgpt.com";
/// Sign in with ChatGPT routes plan usage to the API host, so only the `siwc` compat flag tells it from an API key.
const SIWC_HOST: &str = "api.openai.com";

fn hostname_of(base_url: &str) -> Option<String> {
    url::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_lowercase))
}

/// A boolean key of the model's raw compat object (`model.compat?.<key>`).
fn compat_flag(model: &Model, key: &str) -> Option<bool> {
    model
        .compat
        .as_ref()
        .and_then(|compat| compat.0.get(key))
        .and_then(serde_json::Value::as_bool)
}

fn is_chat_gpt_subscription(model: &Model) -> bool {
    let host = hostname_of(&model.base_url);
    if model.api == "openai-codex-responses" {
        return host.as_deref() == Some(CHATGPT_CODEX_BACKEND_HOST);
    }
    model.api == "openai-responses"
        && host.as_deref() == Some(SIWC_HOST)
        && compat_flag(model, "siwc") == Some(true)
}

/// The OpenAI-operated Responses lanes serving a GPT-5.6+ model, where OpenAI documents a cache lifetime of
/// at least 30 minutes. Gateways that merely proxy the same model ids keep the short lifetime: their routing
/// does not carry that contract.
fn has_extended_prompt_cache(model: &Model) -> bool {
    if model.api == "openai-responses" {
        if compat_flag(model, "supportsExplicitPromptCacheMode") == Some(true) {
            return true;
        }
        if model.provider != "openai"
            && hostname_of(&model.base_url).as_deref() != Some("api.openai.com")
        {
            return false;
        }
    }
    OPENAI_EXTENDED_CACHE_MODEL_ID.is_match(&model.id)
}

/// Mirrors the `openai-responses` adapter's `getPromptCacheOptions()`: `"12h"` is sent only on `long`
/// retention, to a model that accepts `prompt_cache_options` outside Sign in with ChatGPT, unless compat opts
/// out of long retention. Azure and Codex-protocol requests carry no `prompt_cache_options`.
fn sends_twelve_hour_ttl(model: &Model, retention: CacheRetention) -> bool {
    retention == CacheRetention::Long
        && model.api == "openai-responses"
        && compat_flag(model, "siwc") != Some(true)
        && compat_flag(model, "supportsExplicitPromptCacheMode") == Some(true)
        && compat_flag(model, "supportsLongCacheRetention") != Some(false)
}

/// `classifyOpenAIPromptCacheLane`.
pub fn classify_openai_prompt_cache_lane(
    model: &Model,
    retention: CacheRetention,
) -> OpenAIPromptCacheLane {
    if is_chat_gpt_subscription(model) {
        return OpenAIPromptCacheLane::ChatgptSubscription;
    }
    if sends_twelve_hour_ttl(model, retention) {
        return OpenAIPromptCacheLane::ApiLong;
    }
    if has_extended_prompt_cache(model) {
        OpenAIPromptCacheLane::ApiExtended
    } else {
        OpenAIPromptCacheLane::ApiShort
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ModelCompat;
    use serde_json::json;

    fn model(
        api: &str,
        provider: &str,
        base_url: &str,
        id: &str,
        compat: serde_json::Value,
    ) -> Model {
        Model {
            prompt_cache: None,
            supports_assistant_prefill: None,
            id: id.into(),
            name: id.into(),
            api: api.into(),
            provider: provider.into(),
            base_url: base_url.into(),
            reasoning: true,
            default_thinking_level: None,
            thinking_level_map: None,
            input: vec![crate::types::InputModality::Text],
            cost: crate::types::ModelCost::default(),
            context_window: 400_000,
            max_tokens: 128_000,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: compat.as_object().cloned().map(ModelCompat),
            input_limits: None,
            deprecated: None,
        }
    }

    fn openai(id: &str, compat: serde_json::Value) -> Model {
        model(
            "openai-responses",
            "openai",
            "https://api.openai.com/v1",
            id,
            compat,
        )
    }

    #[test]
    fn the_codex_backend_host_is_the_chatgpt_subscription_whatever_the_provider_id() {
        for provider in ["chatgpt-subscription", "my-codex-proxy-name"] {
            let codex = model(
                "openai-codex-responses",
                provider,
                "https://chatgpt.com/backend-api",
                "gpt-6.2-sol",
                json!({}),
            );
            assert_eq!(
                classify_openai_prompt_cache_lane(&codex, CacheRetention::Long),
                OpenAIPromptCacheLane::ChatgptSubscription
            );
        }
        let elsewhere = model(
            "openai-codex-responses",
            "chatgpt-subscription",
            "https://gateway.example.com/backend-api",
            "gpt-6.2-sol",
            json!({}),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&elsewhere, CacheRetention::Short),
            OpenAIPromptCacheLane::ApiExtended
        );
    }

    #[test]
    fn sign_in_with_chatgpt_on_the_api_host_is_told_apart_by_the_siwc_flag() {
        let siwc = openai(
            "gpt-6.2-sol",
            json!({ "siwc": true, "supportsExplicitPromptCacheMode": true }),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&siwc, CacheRetention::Long),
            OpenAIPromptCacheLane::ChatgptSubscription
        );
        let api_key = openai(
            "gpt-6.2-sol",
            json!({ "supportsExplicitPromptCacheMode": true }),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&api_key, CacheRetention::Long),
            OpenAIPromptCacheLane::ApiLong
        );
    }

    #[test]
    fn long_retention_is_api_long_only_where_the_twelve_hour_ttl_is_sent() {
        let sol = openai(
            "gpt-6.2-sol",
            json!({ "supportsExplicitPromptCacheMode": true }),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&sol, CacheRetention::Long),
            OpenAIPromptCacheLane::ApiLong
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&sol, CacheRetention::Short),
            OpenAIPromptCacheLane::ApiExtended
        );
        let opted_out = openai(
            "gpt-6.2-sol",
            json!({ "supportsExplicitPromptCacheMode": true, "supportsLongCacheRetention": false }),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&opted_out, CacheRetention::Long),
            OpenAIPromptCacheLane::ApiExtended
        );
        let azure = model(
            "azure-openai-responses",
            "azure-openai-responses",
            "https://example.openai.azure.com/openai/v1",
            "gpt-6.2-sol",
            json!({ "supportsExplicitPromptCacheMode": true }),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&azure, CacheRetention::Long),
            OpenAIPromptCacheLane::ApiExtended
        );
    }

    #[test]
    fn earlier_models_and_proxied_gpt_ids_keep_the_short_lifetime() {
        assert_eq!(
            classify_openai_prompt_cache_lane(&openai("gpt-5.5", json!({})), CacheRetention::Short),
            OpenAIPromptCacheLane::ApiShort
        );
        let proxied = model(
            "openai-responses",
            "some-gateway",
            "https://gateway.example.com/v1",
            "gpt-6.2-sol",
            json!({}),
        );
        assert_eq!(
            classify_openai_prompt_cache_lane(&proxied, CacheRetention::Short),
            OpenAIPromptCacheLane::ApiShort
        );
        for id in [
            "gpt-5.6-sol",
            "gpt-5.10",
            "gpt-6-astra",
            "gpt-6.2-astra",
            "gpt-7",
        ] {
            assert_eq!(
                classify_openai_prompt_cache_lane(&openai(id, json!({})), CacheRetention::Short),
                OpenAIPromptCacheLane::ApiExtended,
                "{id}"
            );
        }
    }
}
