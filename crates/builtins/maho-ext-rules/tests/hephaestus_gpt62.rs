//! omo #14186: GPT-6.2 stands alone, so no bundled Hephaestus variant is enabled on a model whose
//! senpi prompt preset is `gpt-6.2`; every other model keeps its variant.

use maho_ext_rules::rules::finder::{
    hephaestus_variant_file_for_model, is_gpt_6_2_model, is_hephaestus_candidate_enabled,
};

const VARIANTS: [&str; 3] = ["gpt-5.5.md", "gpt-5.6.md", "gpt-6.md"];

fn enabled_variants(model: Option<&str>) -> Vec<&'static str> {
    VARIANTS
        .into_iter()
        .filter(|file| {
            is_hephaestus_candidate_enabled(&format!("bundled-rules/hephaestus/{file}"), model)
        })
        .collect()
}

#[test]
fn recognizes_the_gpt_6_2_preset_ids_and_display_names() {
    for model in [
        "gpt-6.2-sol",
        "gpt-6.2-astra",
        "gpt-6.2-astra-fast",
        "gpt-6.2-sol-fast",
        "openai/gpt-6.2-sol",
        "chatgpt-subscription/gpt-6.2-astra",
        "openai-gpt-62-astra",
        "GPT-6.2 Astra",
        "GPT-6.2 Sol",
        "openai/gpt-6.2-astra:high",
    ] {
        assert!(is_gpt_6_2_model(model), "{model}");
    }
}

#[test]
fn leaves_other_gpt_6_tiers_and_lookalikes_out() {
    for model in [
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6-sol-fast",
        "gpt-6.2-luna",
        "gpt-6.2",
        "gpt-6.20-sol",
        "gpt-6.2-solar",
        "notgpt-6.2-sol",
        "gpt-5.6-sol",
        "claude-opus-5-5",
    ] {
        assert!(!is_gpt_6_2_model(model), "{model}");
    }
}

#[test]
fn a_gpt_6_2_model_enables_no_hephaestus_variant() {
    for model in [
        "openai/gpt-6.2-astra",
        "chatgpt-subscription/gpt-6.2-sol",
        "GPT-6.2 Sol",
    ] {
        assert_eq!(enabled_variants(Some(model)), Vec::<&str>::new(), "{model}");
    }
}

#[test]
fn every_other_model_keeps_exactly_its_own_variant() {
    for (model, variant) in [
        (Some("openai/gpt-6-astra"), "gpt-6.md"),
        (Some("chatgpt-subscription/gpt-6.1-sol"), "gpt-6.md"),
        (Some("gpt-5.6-sol"), "gpt-5.6.md"),
        (Some("anthropic/claude-opus-5-5"), "gpt-5.5.md"),
        (None, "gpt-5.5.md"),
    ] {
        assert_eq!(hephaestus_variant_file_for_model(model), variant);
        assert_eq!(enabled_variants(model), vec![variant], "{model:?}");
    }
}

#[test]
fn non_hephaestus_rules_stay_enabled_on_gpt_6_2() {
    assert!(is_hephaestus_candidate_enabled(
        "bundled-rules/other.md",
        Some("openai/gpt-6.2-astra")
    ));
    assert!(is_hephaestus_candidate_enabled(
        ".omo/rules/project.md",
        Some("openai/gpt-6.2-sol")
    ));
}
