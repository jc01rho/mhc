use maho_core::dynamic_prompt::{
    PromptSurface,
    build::BuildDynamicSystemPromptOptions,
    workstation::{BuildWorkstationSectionOptions, WorkstationDialect, build_workstation_section},
};
use maho_ext_prompt_preset::{
    deepseek_v4::DEEPSEEK_V4_RULES,
    deepseek_v4_1_flash_rules::{DEEPSEEK_V41_FLASH_SECTIONS, deepseek_v41_flash_rules},
    presets::{ModelMetadata, resolve_preset},
    settings::PromptPresetName,
};
use std::collections::HashMap;

#[test]
fn preset_rules_route_distinct_reasoning_policies() {
    for preset in [
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
    ] {
        let ids: Vec<_> = DEEPSEEK_V4_RULES
            .iter()
            .filter(|rule| rule.presets.contains(&preset))
            .map(|rule| rule.id)
            .collect();
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"injected-directive-authority"));
        assert_eq!(
            ids.contains(&"settled-reading"),
            preset != "deepseek-v4-pro"
        );
        assert_eq!(ids.contains(&"reasoning-aim"), preset == "deepseek-v4-pro");
    }
}

fn render_v41(selected_tools: &[&str], surface: PromptSurface) -> String {
    resolve_preset(
        ModelMetadata {
            id: "deepseek-flash",
            provider: "deepseek",
            name: None,
            prompt_preset: None,
        },
        PromptPresetName::DeepseekV41Flash,
        BuildDynamicSystemPromptOptions {
            cwd: "/workspace".into(),
            selected_tools: selected_tools.iter().map(|t| (*t).to_owned()).collect(),
            tool_snippets: HashMap::from([("read".to_owned(), "READ_SNIPPET".to_owned())]),
            prompt_guidelines: vec!["GUIDELINE_SENTINEL".to_owned()],
            surface,
            ..Default::default()
        },
    )
    .expect("deepseek-v4-1-flash resolves")
    .prompt
}

fn sections_of(prompt: &str) -> Vec<(String, String)> {
    prompt
        .split("\n## ")
        .skip(1)
        .filter_map(|part| {
            let (head, body) = part.split_once('\n')?;
            Some((head.trim().to_owned(), body.to_owned()))
        })
        .collect()
}

#[test]
fn v41_flash_renders_each_rule_of_each_surface_once_under_its_own_section() {
    let rules = deepseek_v41_flash_rules();
    let session_tools = [
        "read",
        "edit",
        "bash",
        "create_goal",
        "todo",
        "task",
        "eval",
        "monitor",
    ];
    for surface in [
        PromptSurface::Terminal,
        PromptSurface::App,
        PromptSurface::Chat,
    ] {
        let prompt = render_v41(&session_tools, surface);
        let sections = sections_of(&prompt);
        let section_map: HashMap<_, _> = sections.iter().cloned().collect();
        for rule in &rules {
            let expected =
                usize::from(rule.surfaces.is_empty() || rule.surfaces.contains(&surface));
            assert_eq!(
                prompt.matches(rule.directive.as_str()).count(),
                expected,
                "{} on {surface:?}",
                rule.id
            );
            assert_eq!(
                section_map
                    .get(rule.section)
                    .map_or(0, |body| body.matches(rule.directive.as_str()).count()),
                expected,
                "{} in {} on {surface:?}",
                rule.id,
                rule.section
            );
        }
        let headings: Vec<&str> = sections.iter().map(|(h, _)| h.as_str()).collect();
        let mut expected_headings: Vec<&str> = DEEPSEEK_V41_FLASH_SECTIONS.to_vec();
        expected_headings.push("Tool Guidelines");
        assert_eq!(headings, expected_headings);
    }
}

#[test]
fn v41_flash_renders_a_tool_bound_rule_for_any_one_of_its_tools_and_drops_it_without_them() {
    let rules = deepseek_v41_flash_rules();
    let tool_bound: Vec<_> = rules
        .into_iter()
        .filter(|rule| !rule.requires_tool.is_empty())
        .collect();
    assert!(!tool_bound.is_empty());
    let unbound = render_v41(&["read", "edit", "bash"], PromptSurface::Terminal);
    for rule in &tool_bound {
        assert_eq!(
            unbound.matches(rule.directive.as_str()).count(),
            0,
            "{} without tools",
            rule.id
        );
        let surface = rule
            .surfaces
            .first()
            .copied()
            .unwrap_or(PromptSurface::Terminal);
        for tool in rule.requires_tool {
            assert_eq!(
                render_v41(&["read", tool], surface)
                    .matches(rule.directive.as_str())
                    .count(),
                1,
                "{} with {tool}",
                rule.id
            );
        }
    }
}

#[test]
fn v41_flash_drops_a_section_whose_rules_are_all_unmounted() {
    let sections: HashMap<_, _> = sections_of(&render_v41(
        &["read", "edit", "bash"],
        PromptSurface::Terminal,
    ))
    .into_iter()
    .collect();
    assert!(!sections.contains_key("Goal and plan"));
}

#[test]
fn v41_flash_keeps_the_guidelines_and_renders_no_tool_list() {
    let sections: HashMap<_, _> = sections_of(&render_v41(
        &["read", "edit", "bash"],
        PromptSurface::Terminal,
    ))
    .into_iter()
    .collect();
    assert!(sections["Tool Guidelines"].contains("GUIDELINE_SENTINEL"));
    assert!(!sections.contains_key("Available Tools"));
    for body in sections.values() {
        assert!(!body.contains("READ_SNIPPET"));
    }
}

#[test]
fn v41_flash_shares_no_rule_text_with_the_v4_presets() {
    let prompt = render_v41(
        &[
            "read",
            "edit",
            "bash",
            "create_goal",
            "todo",
            "task",
            "eval",
            "monitor",
        ],
        PromptSurface::Terminal,
    );
    for rule in DEEPSEEK_V4_RULES {
        assert!(!prompt.contains(rule.directive), "{}", rule.id);
    }
}

#[test]
fn v41_flash_spends_no_emphasis_outside_the_limits_and_the_refusal_rule() {
    for rule in deepseek_v41_flash_rules() {
        assert!(!rule.directive.contains("**"), "{}", rule.id);
        let invariant = rule.section == "Limits" || rule.id == "refusal-is-final";
        assert_eq!(rule.directive.contains("Never "), invariant, "{}", rule.id);
    }
}

#[test]
fn v41_flash_uses_the_codex_workstation_dialect() {
    let workstation = build_workstation_section(&BuildWorkstationSectionOptions {
        selected_tools: vec!["bash".to_owned()],
        dialect: Some(WorkstationDialect::Codex),
        facts: None,
    });
    assert!(render_v41(&["bash"], PromptSurface::Terminal).contains(&workstation));
}
