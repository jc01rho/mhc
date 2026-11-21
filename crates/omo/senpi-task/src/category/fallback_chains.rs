use std::sync::LazyLock;

use crate::delegate_adapter::DelegateFallbackEntry;

const ANTHROPIC: &[&str] = &[
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
    "vercel",
];
const KIMI: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
const OPENAI: &[&str] = &[
    "openai",
    "quotio-openai",
    "github-copilot",
    "opencode",
    "vercel",
];

// GPT-6.x rungs: only the OpenAI lanes serve GPT-6.1 Sol and GPT-6.2 (not Copilot or OpenCode Zen).
const GPT_NATIVE: &[&str] = &["openai", "quotio-openai", "vercel"];
const GPT_ZEN: &[&str] = &["openai", "opencode", "vercel"];

fn rung(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    crate::delegate_adapter::fallback_entry(providers, model, variant)
}

/// Mirrors model-core `category-model-requirements.ts`; senpi's kimi rungs carry both the
/// `kimi-coding` registry id and the `kimi-for-coding` models.dev id. Kimi rungs name the latest
/// Kimi (`LATEST_MODEL_BY_FAMILY.kimi`, kimi-k3.1-pro since omo v5.1.52).
pub static CATEGORY_FALLBACK_CHAINS: LazyLock<Vec<(&'static str, Vec<DelegateFallbackEntry>)>> =
    LazyLock::new(|| {
        vec![
            (
                "visual-engineering",
                vec![
                    rung(ANTHROPIC, "claude-opus-5", Some("max")),
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                    rung(
                        &["zai-coding-plan", "opencode-go", "vercel"],
                        "glm-5.2",
                        Some("max"),
                    ),
                    rung(OPENAI, "gpt-5.6-sol", Some("medium")),
                ],
            ),
            (
                "architect",
                vec![rung(ANTHROPIC, "claude-opus-5-5", Some("xhigh"))],
            ),
            (
                "ultrabrain",
                vec![
                    // Each GPT-6.2 rung keeps GPT-6 Astra right behind it, so a registry without 6.2
                    // resolves what it did before; Copilot does not serve 6.2 and keeps GPT-6 Astra.
                    rung(GPT_NATIVE, "gpt-6.2-astra", Some("max")),
                    rung(GPT_NATIVE, "gpt-6-astra", Some("max")),
                    rung(&["github-copilot"], "gpt-6-astra", Some("max")),
                    rung(GPT_ZEN, "gpt-6.2-astra", Some("max")),
                    rung(GPT_ZEN, "gpt-6-astra", Some("max")),
                    rung(GPT_NATIVE, "gpt-5.6-sol", Some("max")),
                    rung(&["github-copilot"], "gpt-5.6-sol", Some("max")),
                    rung(GPT_ZEN, "gpt-5.6-sol", Some("max")),
                ],
            ),
            (
                "deep",
                vec![
                    // GPT-6.2 Sol leads at the same medium effort on the OpenAI lanes, then GPT-6.1 Sol
                    // for a registry without 6.2; GPT-5.6 Sol stays behind them for every other lane.
                    rung(GPT_NATIVE, "gpt-6.2-sol", Some("medium")),
                    rung(GPT_NATIVE, "gpt-6.1-sol", Some("medium")),
                    rung(OPENAI, "gpt-5.6-sol", Some("medium")),
                ],
            ),
            (
                "artistry",
                vec![
                    rung(ANTHROPIC, "claude-opus-5-5", Some("max")),
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                ],
            ),
            (
                "quick",
                vec![
                    rung(
                        &["kimi-coding", "kimi-for-coding"],
                        "kimi-for-coding-highspeed",
                        None,
                    ),
                    rung(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")),
                    rung(&["deepseek"], "deepseek-v4-flash", Some("off")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "bailian-coding-plan",
                            "vercel",
                        ],
                        "qwen3.6-flash",
                        Some("low"),
                    ),
                    rung(&["opencode-go", "vercel"], "minimax-m3", Some("max")),
                    rung(&["opencode-go", "vercel"], "minimax-m2.7", Some("max")),
                    rung(&["xai"], "grok-4.20-0309-non-reasoning", None),
                    rung(
                        &["anthropic", "anthropic-api", "github-copilot", "vercel"],
                        "claude-haiku-4-5",
                        Some("off"),
                    ),
                ],
            ),
            (
                "unspecified-low",
                vec![
                    rung(
                        &["xai", "github-copilot", "opencode", "vercel"],
                        "grok-4.6",
                        Some("xhigh"),
                    ),
                    rung(GPT_NATIVE, "gpt-6.2-sol", Some("medium")),
                    rung(GPT_NATIVE, "gpt-6.1-sol", Some("medium")),
                    rung(OPENAI, "gpt-5.6-terra", Some("high")),
                    rung(ANTHROPIC, "claude-sonnet-5", Some("low")),
                    rung(
                        &[
                            "qwen-token-plan",
                            "alibaba-token-plan",
                            "qwen-token-plan-cn",
                            "alibaba-token-plan-cn",
                        ],
                        "qwen3.8-max-preview",
                        Some("max"),
                    ),
                    rung(
                        &["deepseek", "opencode-go", "vercel"],
                        "deepseek-v4-pro",
                        Some("max"),
                    ),
                    rung(
                        &["xiaomi", "opencode-go", "vercel"],
                        "mimo-v2.5-pro",
                        Some("max"),
                    ),
                ],
            ),
            (
                "unspecified-high",
                vec![
                    rung(KIMI, "kimi-k3.1-pro", Some("max")),
                    rung(ANTHROPIC, "claude-opus-5", Some("xhigh")),
                    rung(OPENAI, "gpt-5.6-sol", Some("high")),
                ],
            ),
            (
                "writing",
                vec![
                    rung(KIMI, "kimi-k3.1-pro", Some("low")),
                    rung(ANTHROPIC, "claude-opus-5", Some("low")),
                    rung(
                        &["google", "github-copilot", "opencode", "vercel"],
                        "gemini-3.1-pro",
                        None,
                    ),
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
