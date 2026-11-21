use maho_core::dynamic_prompt::{PromptSurface, build::BuildDynamicSystemPromptOptions};
use maho_ext_prompt_preset::{
    gpt_6_2::{GPT62_SECTIONS, gpt62_rules},
    gpt_surface::{GPT_APP_UNRUN_CHECK_RULE, GPT_APP_UNVERIFIED_SLOT},
    presets::{ModelMetadata, resolve_preset},
    settings::PromptPresetName,
};

fn render(model_id: &str, surface: PromptSurface, selected_tools: &[&str]) -> String {
    let options = BuildDynamicSystemPromptOptions {
        custom_prompt: None,
        append_system_prompt: None,
        on_parts: None,
        cwd: "/tmp/project".into(),
        selected_tools: selected_tools
            .iter()
            .map(|tool| (*tool).to_owned())
            .collect(),
        tool_snippets: Default::default(),
        prompt_guidelines: vec!["Keep tool output short.".to_owned()],
        context_files: Vec::new(),
        skills: Vec::new(),
        tuning_section: None,
        core_prompt: None,
        workstation_dialect: None,
        surface,
    };
    resolve_preset(
        ModelMetadata {
            id: model_id,
            provider: "openai",
            name: None,
            prompt_preset: None,
        },
        PromptPresetName::Auto,
        options,
    )
    .expect("gpt-6.2 ids resolve to a preset")
    .prompt
}

#[test]
fn every_gpt_6_2_tier_renders_the_standalone_core() {
    for id in [
        "gpt-6.2-astra",
        "gpt-6.2-astra-fast",
        "gpt-6.2-sol",
        "gpt-6.2-sol-fast",
        "gpt-6.2-sol-ultrafast",
    ] {
        let resolved = resolve_preset(
            ModelMetadata {
                id,
                provider: "openai",
                name: None,
                prompt_preset: None,
            },
            PromptPresetName::Auto,
            BuildDynamicSystemPromptOptions {
                custom_prompt: None,
                append_system_prompt: None,
                on_parts: None,
                cwd: "/tmp/project".into(),
                selected_tools: Vec::new(),
                tool_snippets: Default::default(),
                prompt_guidelines: Vec::new(),
                context_files: Vec::new(),
                skills: Vec::new(),
                tuning_section: None,
                core_prompt: None,
                workstation_dialect: None,
                surface: PromptSurface::Terminal,
            },
        )
        .unwrap_or_else(|| panic!("{id} resolves"));
        assert_eq!(resolved.name, PromptPresetName::Gpt62, "{id}");
        assert_eq!(resolved.name.as_str(), "gpt-6.2");
    }
}

#[test]
fn the_core_renders_every_section_in_table_order() {
    let prompt = render("gpt-6.2-sol", PromptSurface::Terminal, &["read"]);
    let mut cursor = 0;
    for section in GPT62_SECTIONS {
        let heading = format!("## {section}\n\n");
        let at = prompt[cursor..]
            .find(&heading)
            .unwrap_or_else(|| panic!("section {section} follows the previous one"));
        cursor += at + heading.len();
    }
    assert!(prompt.contains("a coding agent working in the user's workspace"));
}

#[test]
fn each_rule_renders_once_under_its_own_section() {
    let prompt = render(
        "gpt-6.2-astra",
        PromptSurface::Terminal,
        &["eval", "monitor"],
    );
    for rule in gpt62_rules()
        .into_iter()
        .filter(|rule| rule.surfaces.is_empty() || rule.surfaces.contains(&PromptSurface::Terminal))
    {
        assert_eq!(
            prompt.matches(rule.directive.as_str()).count(),
            1,
            "rule {} renders exactly once",
            rule.id
        );
    }
}

#[test]
fn the_terminal_surface_keeps_one_routing_line_and_no_handoff_block() {
    let prompt = render("gpt-6.2-sol", PromptSurface::Terminal, &["read"]);
    assert!(prompt.contains("I read this as [the goal as you understand it]."));
    assert!(prompt.contains("and anything left unverified."));
    for absent in ["## Handoff", "Ask / For you / Now / Next", "## Stop Goal"] {
        assert!(
            !prompt.contains(absent),
            "terminal core carries no {absent}"
        );
    }
}

#[test]
fn the_app_and_chat_surfaces_swap_the_surface_bound_rules() {
    let app = render("gpt-6.2-sol", PromptSurface::App, &["read"]);
    assert!(!app.contains("I read this as"));
    assert!(app.contains(&format!("and {GPT_APP_UNVERIFIED_SLOT}.")));
    assert!(app.contains(GPT_APP_UNRUN_CHECK_RULE));
    assert!(app.contains("Report proactively while you work."));

    let chat = render("gpt-6.2-sol", PromptSurface::Chat, &["read"]);
    assert!(!chat.contains("I read this as"));
    assert!(chat.contains(maho_core::dynamic_prompt::handoff::CHAT_REPLY_RULE));
    assert!(!chat.contains("Report proactively while you work."));
    assert!(chat.contains(GPT_APP_UNRUN_CHECK_RULE));
}

#[test]
fn background_runs_needs_the_eval_or_monitor_tool() {
    let marker = "Start a long run (a build, a test suite, a deploy) in the background";
    assert!(render("gpt-6.2-sol", PromptSurface::Terminal, &["eval"]).contains(marker));
    assert!(render("gpt-6.2-sol", PromptSurface::Terminal, &["monitor"]).contains(marker));
    assert!(!render("gpt-6.2-sol", PromptSurface::Terminal, &["read", "bash"]).contains(marker));
}

#[test]
fn only_the_tool_guidelines_of_the_tool_section_reach_the_core() {
    let prompt = render("gpt-6.2-sol", PromptSurface::Terminal, &["read"]);
    assert!(prompt.contains("## Tool Guidelines"));
    assert!(prompt.contains("Keep tool output short."));
    assert!(!prompt.contains("## Available Tools"));
}

#[test]
fn the_limits_and_code_sections_render_as_lists() {
    let prompt = render("gpt-6.2-sol", PromptSurface::Terminal, &["read"]);
    for rule in gpt62_rules()
        .into_iter()
        .filter(|rule| rule.section == "Code" || rule.section == "Limits")
    {
        assert!(
            prompt.contains(&format!("- {}", rule.directive)),
            "{} renders as a list item",
            rule.id
        );
    }
    let intent = gpt62_rules()
        .into_iter()
        .find(|rule| rule.id == "read-the-request")
        .expect("rule");
    assert!(!prompt.contains(&format!("- {}", intent.directive)));
}

#[test]
fn the_core_uses_no_capitalized_emphasis_and_keeps_never_to_the_limits() {
    for rule in gpt62_rules() {
        assert!(
            !rule.directive.contains("**"),
            "{} carries bold emphasis",
            rule.id
        );
        if rule.directive.contains("Never ") && rule.id != "refusal-is-final" {
            assert_eq!(rule.section, "Limits", "{} states an absolute", rule.id);
        }
    }
}
