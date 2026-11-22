use pretty_assertions::assert_eq;
use serde_json::json;

use super::prompts;
use super::tests::{contains, expect_resolved, models_of, resolve, spec_triple};
use super::*;

fn definition(name: &str) -> &'static BuiltinCategoryDefinition {
    builtin_category(name).unwrap_or_else(|| panic!("missing builtin category {name}"))
}

const ASTRA_IDS: [&str; 5] = [
    "gpt-6-astra",
    "openai/gpt-6-astra",
    "chatgpt-subscription/gpt-6-astra-fast",
    "vercel/openai/gpt-6-astra",
    "GPT-6-Astra",
];
const GPT_5_IDS: [&str; 4] = [
    "openai/gpt-5.6-sol",
    "chatgpt-subscription/gpt-5.6-terra",
    "openai/gpt-5.5",
    "gpt-5-5",
];
const OTHER_IDS: [Option<&str>; 4] = [
    Some("anthropic/claude-opus-5-5"),
    Some("kimi-coding/k3"),
    Some("zai-coding-plan/glm-5.3"),
    None,
];

fn gpt_5_and_other_ids() -> Vec<Option<&'static str>> {
    GPT_5_IDS
        .iter()
        .map(|id| Some(*id))
        .chain(OTHER_IDS)
        .collect()
}

// ---- category/openai-categories.test.ts ----

#[test]
fn gpt6_ids_match_with_and_without_provider_prefixes() {
    for id in ASTRA_IDS {
        assert!(is_gpt6_model(id), "{id}");
    }
    for id in GPT_5_IDS
        .iter()
        .copied()
        .chain(OTHER_IDS.into_iter().flatten())
    {
        assert!(!is_gpt6_model(id), "{id}");
    }
}

#[test]
fn ultrabrain_uses_the_astra_append_only_for_gpt6() {
    let generic = definition("ultrabrain").prompt_append;
    let resolve_append = definition("ultrabrain")
        .resolve_prompt_append
        .expect("ultrabrain resolver");
    for id in ASTRA_IDS {
        assert_eq!(
            resolve_ultrabrain_category_prompt_append(Some(id)),
            prompts::ULTRABRAIN_PROMPT_APPEND_GPT_6_ASTRA,
            "{id}"
        );
        assert_eq!(
            resolve_append(Some(id)),
            prompts::ULTRABRAIN_PROMPT_APPEND_GPT_6_ASTRA
        );
    }
    for id in gpt_5_and_other_ids() {
        assert_eq!(
            resolve_ultrabrain_category_prompt_append(id),
            generic,
            "{id:?}"
        );
        assert_eq!(resolve_append(id), generic);
    }
}

#[test]
fn each_deep_lane_uses_its_own_gpt_append_for_any_gpt_model() {
    for id in ASTRA_IDS.iter().chain(GPT_5_IDS.iter()).copied() {
        assert_eq!(
            resolve_deep_low_category_prompt_append(Some(id)),
            prompts::DEEP_LOW_PROMPT_APPEND_GPT,
            "{id}"
        );
        assert_eq!(
            resolve_deep_high_category_prompt_append(Some(id)),
            prompts::DEEP_HIGH_PROMPT_APPEND_GPT,
            "{id}"
        );
    }
}

#[test]
fn the_deep_lanes_use_the_generic_append_for_other_vendors() {
    for id in OTHER_IDS {
        assert_eq!(
            resolve_deep_low_category_prompt_append(id),
            prompts::DEEP_LOW_PROMPT_APPEND,
            "{id:?}"
        );
        assert_eq!(
            resolve_deep_high_category_prompt_append(id),
            prompts::DEEP_HIGH_PROMPT_APPEND,
            "{id:?}"
        );
    }
    assert_eq!(
        definition("deep-low").prompt_append,
        prompts::DEEP_LOW_PROMPT_APPEND
    );
    assert_eq!(
        definition("deep-high").prompt_append,
        prompts::DEEP_HIGH_PROMPT_APPEND
    );
    for (name, resolver) in [
        (
            "deep-low",
            resolve_deep_low_category_prompt_append as fn(Option<&str>) -> &'static str,
        ),
        ("deep-high", resolve_deep_high_category_prompt_append),
    ] {
        let wired = definition(name).resolve_prompt_append.expect("resolver");
        for id in ASTRA_IDS.iter().map(|id| Some(*id)).chain(OTHER_IDS) {
            assert_eq!(wired(id), resolver(id), "{name} {id:?}");
        }
    }
}

#[test]
fn only_the_deep_low_appends_carry_the_escalate_contract() {
    for append in [
        prompts::DEEP_LOW_PROMPT_APPEND,
        prompts::DEEP_LOW_PROMPT_APPEND_GPT,
    ] {
        assert!(append.contains("ESCALATE: deep-high"));
    }
    for append in [
        prompts::DEEP_HIGH_PROMPT_APPEND,
        prompts::DEEP_HIGH_PROMPT_APPEND_GPT,
    ] {
        assert!(!append.contains("ESCALATE"));
    }
}

#[test]
fn unspecified_high_uses_the_astra_append_only_for_gpt6() {
    let generic = definition("unspecified-high").prompt_append;
    let resolve_append = definition("unspecified-high")
        .resolve_prompt_append
        .expect("unspecified-high resolver");
    for id in ASTRA_IDS {
        assert_eq!(
            resolve_unspecified_high_category_prompt_append(Some(id)),
            prompts::UNSPECIFIED_HIGH_PROMPT_APPEND_GPT_6_ASTRA,
            "{id}"
        );
        assert_eq!(
            resolve_append(Some(id)),
            prompts::UNSPECIFIED_HIGH_PROMPT_APPEND_GPT_6_ASTRA
        );
    }
    for id in gpt_5_and_other_ids() {
        assert_eq!(
            resolve_unspecified_high_category_prompt_append(id),
            generic,
            "{id:?}"
        );
    }
}

#[test]
fn each_model_specific_append_is_a_distinct_block_named_after_its_category() {
    let appends = [
        ("ultrabrain", prompts::ULTRABRAIN_PROMPT_APPEND_GPT_6_ASTRA),
        ("deep-low", prompts::DEEP_LOW_PROMPT_APPEND_GPT),
        ("deep-high", prompts::DEEP_HIGH_PROMPT_APPEND_GPT),
        (
            "unspecified-high",
            prompts::UNSPECIFIED_HIGH_PROMPT_APPEND_GPT_6_ASTRA,
        ),
    ];
    for (name, append) in appends {
        assert!(
            append.starts_with(&format!("<Category_Context name=\"{name}\">")),
            "{name}"
        );
        assert!(append.ends_with("</Category_Context>"), "{name}");
        assert_ne!(append, definition(name).prompt_append, "{name}");
    }
    let distinct: std::collections::BTreeSet<&str> =
        appends.iter().map(|(_, append)| *append).collect();
    assert_eq!(distinct.len(), 4);
}

#[test]
fn the_gpt_lanes_default_to_astra_and_sol_on_the_subscription_lane() {
    let config = |name: &str| {
        let config = definition(name).config;
        (config.model, config.variant)
    };
    assert_eq!(
        config("ultrabrain"),
        ("chatgpt-subscription/gpt-6.2-astra", Some("max"))
    );
    assert_eq!(
        config("deep-high"),
        ("chatgpt-subscription/gpt-6.2-astra", Some("high"))
    );
    assert_eq!(
        config("deep-low"),
        ("chatgpt-subscription/gpt-6.2-sol", Some("medium"))
    );
    assert_eq!(
        config("unspecified-high"),
        ("anthropic/claude-opus-5-5", Some("medium"))
    );
}

#[test]
fn the_gpt_gates_open_on_their_own_models() {
    assert_eq!(
        definition("ultrabrain").requires_model.to_vec(),
        vec!["gpt-6.2-astra", "gpt-6-astra", "gpt-5.6-sol"]
    );
    assert_eq!(
        definition("deep-low").requires_model.to_vec(),
        vec![
            "gpt-6.2-sol",
            "gpt-6.2-sol-fast",
            "gpt-6.1-sol",
            "gpt-6.1-sol-fast",
            "gpt-5.6-sol-fast",
            "gpt-5.6-sol"
        ]
    );
    assert_eq!(
        definition("deep-high").requires_model.to_vec(),
        vec!["gpt-6.2-astra", "gpt-6-astra"]
    );
    assert!(definition("unspecified-high").requires_model.is_empty());
}

#[test]
fn astra_resolves_ultrabrain_and_deep_high_with_their_appends_on_either_lane() {
    for provider in ["openai", "chatgpt-subscription"] {
        let models = models_of(&[(provider, "gpt-6-astra")]);
        for (category, variant, append) in [
            (
                "ultrabrain",
                "max",
                prompts::ULTRABRAIN_PROMPT_APPEND_GPT_6_ASTRA,
            ),
            ("deep-high", "high", prompts::DEEP_HIGH_PROMPT_APPEND_GPT),
        ] {
            let resolved = expect_resolved(resolve(category, json!({}), &models));
            assert_eq!(
                spec_triple(&resolved),
                (provider, "gpt-6-astra", Some(variant))
            );
            assert_eq!(
                resolved.spec.prompt_append.as_deref(),
                Some(append),
                "{category}"
            );
        }
    }
}

#[test]
fn deep_low_walks_its_sol_tiers_in_order() {
    for (pairs, winner, gpt_append) in [
        (
            &[
                ("chatgpt-subscription", "gpt-5.6-sol"),
                ("chatgpt-subscription", "gpt-6.1-sol"),
            ][..],
            ("chatgpt-subscription", "gpt-6.1-sol", Some("medium")),
            true,
        ),
        (
            &[("openai", "gpt-5.6-sol"), ("openai", "gpt-6.1-sol-fast")][..],
            ("openai", "gpt-6.1-sol-fast", Some("medium")),
            false,
        ),
        (
            &[
                ("chatgpt-subscription", "gpt-5.6-sol"),
                ("chatgpt-subscription", "gpt-5.6-sol-fast"),
            ][..],
            ("chatgpt-subscription", "gpt-5.6-sol", Some("medium")),
            true,
        ),
        (
            &[("github-copilot", "gpt-5.6-sol")][..],
            ("github-copilot", "gpt-5.6-sol", Some("medium")),
            false,
        ),
    ] {
        let resolved = expect_resolved(resolve("deep-low", json!({}), &models_of(pairs)));
        assert_eq!(spec_triple(&resolved), winner);
        if gpt_append {
            assert_eq!(
                resolved.spec.prompt_append.as_deref(),
                Some(prompts::DEEP_LOW_PROMPT_APPEND_GPT)
            );
        }
    }
}

#[test]
fn deep_low_has_no_gpt_6_sol_rung() {
    let result = resolve(
        "deep-low",
        json!({}),
        &models_of(&[("openai", "gpt-6-sol")]),
    );
    assert_eq!(result.kind(), "model_unavailable");
    assert!(!contains(result.available_categories(), "deep-low"));
}

#[test]
fn unspecified_high_carries_no_gpt_rung_and_runs_opus_5_5_at_medium() {
    for provider in ["openai", "chatgpt-subscription"] {
        let result = resolve(
            "unspecified-high",
            json!({}),
            &models_of(&[(provider, "gpt-6-astra")]),
        );
        assert_eq!(result.kind(), "model_unavailable", "{provider}");
    }
    let resolved = expect_resolved(resolve(
        "unspecified-high",
        json!({}),
        &models_of(&[("anthropic", "claude-opus-5-5")]),
    ));
    assert_eq!(
        spec_triple(&resolved),
        ("anthropic", "claude-opus-5-5", Some("medium"))
    );
    assert_eq!(
        resolved.spec.prompt_append.as_deref(),
        Some(definition("unspecified-high").prompt_append)
    );
}

#[test]
fn a_lane_never_borrows_the_other_lanes_model() {
    assert_eq!(
        resolve(
            "deep-high",
            json!({}),
            &models_of(&[("openai", "gpt-5.6-sol")])
        )
        .kind(),
        "model_unavailable"
    );
    assert_eq!(
        resolve(
            "deep-low",
            json!({}),
            &models_of(&[("openai", "gpt-6-astra")])
        )
        .kind(),
        "model_unavailable"
    );
}

#[test]
fn the_retired_deep_name_resolves_as_deep_low() {
    let resolved = expect_resolved(resolve(
        "deep",
        json!({}),
        &models_of(&[("openai", "gpt-5.6-sol")]),
    ));
    assert_eq!(resolved.category, "deep-low");
    assert_eq!(resolved.spec.model_id, "gpt-5.6-sol");
    assert_eq!(resolved.spec.variant.as_deref(), Some("medium"));
}

#[test]
fn a_deep_high_overlay_follows_the_lane_append() {
    let resolved = expect_resolved(resolve(
        "deep-high",
        json!({ "categories": { "deep-high": { "prompt_append": "team-overlay" } } }),
        &models_of(&[("openai", "gpt-6-astra")]),
    ));
    assert_eq!(
        resolved.spec.prompt_append,
        Some(format!(
            "{}\n\nteam-overlay",
            prompts::DEEP_HIGH_PROMPT_APPEND_GPT
        ))
    );
}
