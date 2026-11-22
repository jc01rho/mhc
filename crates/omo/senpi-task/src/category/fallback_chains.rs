use std::sync::LazyLock;

use crate::delegate_adapter::DelegateFallbackEntry;

// Mirrors omo v5.1.79 `senpi-task/src/category/fallback-chains.ts` (itself mirrored from model-core
// `category-model-requirements.ts`). senpi-only differences model-core does not carry:
//   - kimi rungs carry both the `kimi-coding` registry id and the `kimi-for-coding` models.dev id;
//   - every rung names a thinking level senpi's catalog accepts for that model (#9378);
//   - glm rungs use the engine ids `zai` and `zai-coding-cn` (#8799, #8824);
//   - every claude-* rung is headed by `anthropic-subscription` and every GPT rung by
//     `chatgpt-subscription` then `openai`: rung provider order is the ranking, so a subscription
//     outranks a metered lane (#8051, #8300) while an `openai`-only machine still reaches the rung (#8734).
// Models are spelled as omo's `LATEST_MODEL_BY_FAMILY` names them at v5.1.79.
const CLAUDE: &[&str] = &[
    "anthropic-subscription",
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
];
const KIMI: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
const GPT_SUBSCRIPTION: &[&str] = &["chatgpt-subscription", "openai"];
const GPT_SUBSCRIPTION_ZEN: &[&str] = &["chatgpt-subscription", "openai", "opencode"];
const GPT_EVERY_LANE: &[&str] = &[
    "chatgpt-subscription",
    "openai",
    "github-copilot",
    "opencode",
];
const ARGON: &[&str] = &["google", "google-vertex", "opencode"];

fn rung(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

pub static CATEGORY_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        vec![
            (
                "visual-engineering",
                vec![
                    // Argon at its lowest accepted thinking level leads (#10187): the catalog maps off/minimal to
                    // null for this model, so `low` is the floor. A pinned engine whose catalog has no Argon row
                    // leaves the rung unresolvable, so the chain behaves as before until adoption.
                    rung(ARGON, "gemini-4-argon", Some("low")),
                    rung(CLAUDE, "claude-fable-5-2", Some("max")),
                    rung(CLAUDE, "claude-fable-5-1", Some("max")),
                    rung(CLAUDE, "claude-opus-5-5", Some("max")),
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                ],
            ),
            (
                "architect",
                vec![
                    rung(CLAUDE, "claude-fable-5-2", Some("xhigh")),
                    rung(CLAUDE, "claude-fable-5-1", Some("xhigh")),
                ],
            ),
            (
                "ultrabrain",
                vec![
                    // Each GPT-6.2 rung keeps GPT-6 Astra right behind it, so a registry without 6.2 resolves what it
                    // did before; Copilot does not serve 6.2 and keeps its GPT-6 Astra rung.
                    rung(GPT_SUBSCRIPTION, "gpt-6.2-astra", Some("max")),
                    rung(GPT_SUBSCRIPTION, "gpt-6-astra", Some("max")),
                    rung(&["github-copilot"], "gpt-6-astra", Some("max")),
                    rung(GPT_SUBSCRIPTION_ZEN, "gpt-6.2-astra", Some("max")),
                    rung(GPT_SUBSCRIPTION_ZEN, "gpt-6-astra", Some("max")),
                    rung(GPT_SUBSCRIPTION, "gpt-5.6-sol", Some("max")),
                    rung(&["github-copilot"], "gpt-5.6-sol", Some("max")),
                    rung(GPT_SUBSCRIPTION_ZEN, "gpt-5.6-sol", Some("max")),
                ],
            ),
            (
                "deep-low",
                vec![
                    // GPT-6.2 Sol leads at medium effort (plain, then the Fast tier), then GPT-6.1 Sol for a registry
                    // without 6.2; GPT-5.6 Sol stays behind them so Copilot, OpenCode Zen and a registry without
                    // either still resolve the lane.
                    rung(GPT_SUBSCRIPTION, "gpt-6.2-sol", Some("medium")),
                    rung(GPT_SUBSCRIPTION, "gpt-6.2-sol-fast", Some("medium")),
                    rung(GPT_SUBSCRIPTION, "gpt-6.1-sol", Some("medium")),
                    rung(GPT_SUBSCRIPTION, "gpt-6.1-sol-fast", Some("medium")),
                    rung(GPT_EVERY_LANE, "gpt-5.6-sol", Some("medium")),
                    rung(GPT_SUBSCRIPTION, "gpt-5.6-sol-fast", Some("medium")),
                ],
            ),
            (
                "deep-high",
                vec![
                    rung(GPT_SUBSCRIPTION_ZEN, "gpt-6.2-astra", Some("high")),
                    rung(GPT_EVERY_LANE, "gpt-6-astra", Some("high")),
                ],
            ),
            (
                "artistry",
                vec![
                    rung(CLAUDE, "claude-fable-5-2", Some("max")),
                    rung(CLAUDE, "claude-fable-5-1", Some("max")),
                    rung(CLAUDE, "claude-opus-5-5", Some("max")),
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                ],
            ),
            (
                "quick",
                vec![
                    rung(GPT_SUBSCRIPTION, "gpt-6-luna-fast", Some("low")),
                    rung(
                        &[
                            "anthropic-subscription",
                            "anthropic",
                            "anthropic-api",
                            "github-copilot",
                        ],
                        "claude-haiku-5-5",
                        Some("medium"),
                    ),
                    rung(&["deepseek"], "deepseek-flash", Some("off")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "bailian-coding-plan",
                        ],
                        "qwen3.6-flash",
                        Some("low"),
                    ),
                    rung(&["opencode-go"], "minimax-m3", None),
                    rung(&["opencode-go"], "minimax-m2.7", None),
                    rung(&["xai"], "grok-4.20-0309-non-reasoning", None),
                    // Trailing, so every provider set that resolved quick before resolves the same model: these
                    // rungs only answer a machine logged in to Z.ai or Xiaomi alone. Neither flash model can turn
                    // thinking off, so both run at the lowest effort they accept.
                    rung(&["zai", "zai-coding-cn"], "glm-5.3-flash", Some("low")),
                    rung(&["xiaomi"], "mimo-v2.6-flash", Some("low")),
                ],
            ),
            (
                "unspecified-low",
                vec![
                    rung(CLAUDE, "claude-sonnet-5-5", Some("medium")),
                    rung(&["xiaomi", "opencode-go"], "mimo-v2.6-pro", Some("high")),
                    rung(
                        &["xai", "github-copilot", "opencode-go"],
                        "grok-4.7",
                        Some("xhigh"),
                    ),
                    // GLM 5.3 sits ahead of the GPT rungs so a Claude + Z.ai user does not fall from Sonnet to GPT.
                    rung(
                        &["zai", "zai-coding-cn", "opencode-go"],
                        "glm-5.3",
                        Some("high"),
                    ),
                    // GPT-6.2 Sol is the current GPT default, served only by the OpenAI lanes; GPT-6.1 Sol and then
                    // GPT-5.6 Sol stay behind it so Copilot, OpenCode Zen and an older registry still resolve it.
                    rung(GPT_SUBSCRIPTION, "gpt-6.2-sol", Some("medium")),
                    rung(GPT_SUBSCRIPTION, "gpt-6.1-sol", Some("medium")),
                    rung(GPT_EVERY_LANE, "gpt-5.6-sol", Some("medium")),
                    rung(CLAUDE, "claude-sonnet-5", Some("low")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "qwen-token-plan-cn",
                            "alibaba-token-plan-cn",
                        ],
                        "qwen3.8-max-preview",
                        Some("high"),
                    ),
                    rung(&["deepseek", "opencode-go"], "deepseek-v4-pro", Some("max")),
                    rung(&["xiaomi", "opencode-go"], "mimo-v2.5-pro", Some("high")),
                ],
            ),
            (
                "unspecified-high",
                vec![
                    rung(CLAUDE, "claude-opus-5-5", Some("medium")),
                    rung(
                        &["zai", "zai-coding-cn", "opencode-go"],
                        "glm-5.3",
                        Some("max"),
                    ),
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                ],
            ),
            (
                "writing",
                vec![
                    rung(ARGON, "gemini-4-argon", Some("low")),
                    rung(CLAUDE, "claude-opus-5-5", Some("low")),
                    rung(CLAUDE, "claude-opus-4-6", Some("max")),
                ],
            ),
        ]
    });

pub fn category_fallback_chain(category_name: &str) -> Option<&'static [DelegateFallbackEntry]> {
    CATEGORY_FALLBACK_CHAINS
        .iter()
        .find(|(name, _)| *name == category_name)
        .map(|(_, chain)| chain.as_slice())
}
