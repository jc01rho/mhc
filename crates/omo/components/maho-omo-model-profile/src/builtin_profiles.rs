//! Port of omo-senpi `components/model-profile/builtin-profiles.ts` at omo `v5.1.47`: the named
//! model-chain table a human picks by lane.
//!
//! A model profile is a named, ordered model chain a human picks by lane ("Daily · Normal",
//! "Obsessive · Heavy") instead of by model id. It is not the `profiles` key in omo.json: that one is
//! a VSCode-style config-layer overlay activated by `OMO_PROFILE`.
//!
//! Every rung is `{ providers, model, variant? }` - the exact shape the category fallback chains
//! use - and NOT a single `provider/model` string, so a Copilot-only, Bedrock-only or gateway-only
//! user still resolves the model instead of reading "unavailable" while the model sits right there
//! in the registry. Provider spellings are copied from those chains: senpi-only `kimi-coding` plus
//! the leftover OpenCode `kimi-for-coding` alias those chains keep, and the engine GLM ids `zai` /
//! `zai-coding-cn` (not OpenCode's `zai-coding-plan`; #8824).
//!
//! The table is additive data: an `omo.json` `model_profiles.<name>` entry replaces the builtin of
//! the same name WHOLESALE (see [`crate::resolve`]), so a later change here can never silently
//! override a chain a user wrote.
//!
//! There is no alias, migration, or compatibility shim for retired ids (`capable`, `deep-work`,
//! `simple-work`). A stale `model_profile` value is `unknown`.
//!
//! Builtin GPT rungs rank providers exactly like the `deep-low` / `deep-high` category chains
//! (#8737): the ChatGPT subscription first, then the `openai` API/proxy lane, then Copilot and
//! OpenCode where they serve the model. A user overlay that names `openai/` is scoped to that
//! provider.
//!
//! Unset sessions start on Daily · Normal or the first resolving default lane.
//! Every builtin is a lane; the retired `recommended` id starts the same default.
//!
//! Each Obsessive lane carries a one-line caution a picker renders in bold, and both Obsessive lanes
//! head with GPT-6.2 Astra, keeping GPT-6 Astra (and, on Normal, the GPT-6.x Sol tiers) right behind.

use indexmap::IndexMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileFamily {
    Daily,
    Geeky,
}

impl ModelProfileFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Geeky => "geeky",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProfileTier {
    Normal,
    Heavy,
}

impl ModelProfileTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Heavy => "heavy",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinModelProfile {
    pub display_name: &'static str,
    pub description: &'static str,
    /// One-line warning a picker renders in bold; set only where a lane must warn the user off.
    pub caution: Option<&'static str>,
    /// Picker axes: every builtin is a lane.
    pub family: ModelProfileFamily,
    pub tier: ModelProfileTier,
    pub models: &'static [BuiltinRung],
}

/// A builtin rung. `providers` and `model` are `&'static str` so the table is `const`-friendly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinRung {
    pub providers: &'static [&'static str],
    pub model: &'static str,
    pub variant: Option<&'static str>,
}

/// Fresh-session default when `model_profile` is unset. Not written back to config.
pub const DEFAULT_MODEL_PROFILE_ORDER: [&str; 4] =
    ["daily-normal", "daily-heavy", "geeky-normal", "geeky-heavy"];
pub const DEFAULT_MODEL_PROFILE_ID: &str = DEFAULT_MODEL_PROFILE_ORDER[0];

/// Saved builtin ids that start the unset default unless the user defines that name.
pub const RETIRED_MODEL_PROFILE_IDS: &[&str] = &["recommended"];

const CLAUDE_PROVIDERS: &[&str] = &[
    "anthropic-subscription",
    "anthropic",
    "anthropic-api",
    "github-copilot",
    "opencode",
];
const KIMI_PROVIDERS: &[&str] = &[
    "kimi-coding",
    "kimi-for-coding",
    "moonshotai",
    "opencode-go",
];
// Engine Z.AI ids. `omo setup` imports OpenCode's `zai-coding-plan` key as `zai` (#8799).
const GLM_PROVIDERS: &[&str] = &["zai", "zai-coding-cn", "opencode-go"];
const GPT_PROVIDERS: &[&str] = &[
    "chatgpt-subscription",
    "openai",
    "github-copilot",
    "opencode",
];
// Copilot does not serve GPT-6.2, so the GPT-6.2 Astra rung lists the other GPT providers and GPT-6 Astra
// stays right behind it on all four (also covering a registry without 6.2). GPT-6.x Sol and its Fast tier
// are served only on the two OpenAI lanes here, and Obsessive · Normal ends on GPT-5.6 Sol, which keeps
// the profile on every GPT provider.
const GPT_6_2_ASTRA_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai", "opencode"];
const GPT_SOL_PROVIDERS: &[&str] = &["chatgpt-subscription", "openai"];

/// The one-line warning both Obsessive lanes carry: they are for one hard task, not everyday work.
const OBSESSIVE_CAUTION: &str = "NOT RECOMMENDED FOR WRITING OR EVERYDAY WORK.";

const DAILY_NORMAL_MODELS: &[BuiltinRung] = &[
    BuiltinRung {
        providers: CLAUDE_PROVIDERS,
        model: "claude-opus-5-5",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: KIMI_PROVIDERS,
        model: "kimi-k3.1-pro",
        variant: Some("max"),
    },
    BuiltinRung {
        providers: GLM_PROVIDERS,
        model: "glm-5.3",
        variant: Some("max"),
    },
];
// Daily · Heavy carries no Claude Fable rung (owner policy): it runs Claude Opus 5.5 at the xhigh
// effort the Fable rung used.
const DAILY_HEAVY_MODELS: &[BuiltinRung] = &[BuiltinRung {
    providers: CLAUDE_PROVIDERS,
    model: "claude-opus-5-5",
    variant: Some("xhigh"),
}];
const GEEKY_NORMAL_MODELS: &[BuiltinRung] = &[
    BuiltinRung {
        providers: GPT_6_2_ASTRA_PROVIDERS,
        model: "gpt-6.2-astra",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_PROVIDERS,
        model: "gpt-6-astra",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_SOL_PROVIDERS,
        model: "gpt-6.2-sol-fast",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_SOL_PROVIDERS,
        model: "gpt-6.2-sol",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_SOL_PROVIDERS,
        model: "gpt-6.1-sol-fast",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_SOL_PROVIDERS,
        model: "gpt-6.1-sol",
        variant: Some("medium"),
    },
    BuiltinRung {
        providers: GPT_PROVIDERS,
        model: "gpt-5.6-sol",
        variant: Some("medium"),
    },
];
const GEEKY_HEAVY_MODELS: &[BuiltinRung] = &[
    BuiltinRung {
        providers: GPT_6_2_ASTRA_PROVIDERS,
        model: "gpt-6.2-astra",
        variant: Some("high"),
    },
    BuiltinRung {
        providers: GPT_PROVIDERS,
        model: "gpt-6-astra",
        variant: Some("high"),
    },
];

// Key order is the order a picker renders. `deep` is deliberately NOT an id: builtin delegation
// categories already carry that name, and the two axes never compete (a profile picks the MAIN
// session model; categories keep their own chains).
//
// Every Claude rung is headed by `anthropic-subscription`, senpi's Claude subscription lane,
// exactly like the category chains (#8051): rung provider order IS the ranking.
pub fn builtin_model_profiles() -> IndexMap<&'static str, BuiltinModelProfile> {
    let mut profiles = IndexMap::new();
    profiles.insert(
        "daily-normal",
        BuiltinModelProfile {
            display_name: "Daily · Normal",
            description: "Gets any task done without fuss.",
            caution: None,
            family: ModelProfileFamily::Daily,
            tier: ModelProfileTier::Normal,
            models: DAILY_NORMAL_MODELS,
        },
    );
    profiles.insert(
        "daily-heavy",
        BuiltinModelProfile {
            display_name: "Daily · Heavy",
            description: "Gets any task done, after thinking it over from more sides.",
            caution: None,
            family: ModelProfileFamily::Daily,
            tier: ModelProfileTier::Heavy,
            models: DAILY_HEAVY_MODELS,
        },
    );
    profiles.insert(
        "geeky-normal",
        BuiltinModelProfile {
            display_name: "Obsessive · Normal",
            description: "Obsessive focus on one hard task.",
            caution: Some(OBSESSIVE_CAUTION),
            family: ModelProfileFamily::Geeky,
            tier: ModelProfileTier::Normal,
            models: GEEKY_NORMAL_MODELS,
        },
    );
    profiles.insert(
        "geeky-heavy",
        BuiltinModelProfile {
            display_name: "Obsessive · Heavy",
            description: "Obsessive focus on one hard task, checked from every side.",
            caution: Some(OBSESSIVE_CAUTION),
            family: ModelProfileFamily::Geeky,
            tier: ModelProfileTier::Heavy,
            models: GEEKY_HEAVY_MODELS,
        },
    );
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_a_lane_in_default_order() {
        let profiles = builtin_model_profiles();
        assert_eq!(
            profiles.keys().copied().collect::<Vec<_>>(),
            DEFAULT_MODEL_PROFILE_ORDER
        );
        assert_eq!(DEFAULT_MODEL_PROFILE_ID, "daily-normal");
        assert_eq!(RETIRED_MODEL_PROFILE_IDS, &["recommended"]);
        assert!(!profiles.contains_key("recommended"));
        let axes: Vec<_> = profiles
            .values()
            .map(|profile| (profile.family.as_str(), profile.tier.as_str()))
            .collect();
        assert_eq!(
            axes,
            [
                ("daily", "normal"),
                ("daily", "heavy"),
                ("geeky", "normal"),
                ("geeky", "heavy")
            ]
        );
    }

    #[test]
    fn no_builtin_lane_routes_to_a_claude_fable_model() {
        for (id, profile) in builtin_model_profiles() {
            assert!(
                profile
                    .models
                    .iter()
                    .all(|rung| !rung.model.contains("fable")),
                "{id}"
            );
        }
        let heavy = &builtin_model_profiles()["daily-heavy"];
        assert_eq!(heavy.models[0].model, "claude-opus-5-5");
        assert_eq!(heavy.models[0].variant, Some("xhigh"));
    }
}
