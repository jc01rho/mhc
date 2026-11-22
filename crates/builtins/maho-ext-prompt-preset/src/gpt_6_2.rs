//! Port of `prompt-preset/gpt-6.2.ts` (senpi#6979, second edition senpi#7101 / senpi#7110).

pub use crate::gpt_6_2_rules::{GPT62_SECTIONS, Gpt62Rule, gpt62_rules};
use maho_core::dynamic_prompt::{
    PromptSurface,
    build::{
        BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, build_dynamic_system_prompt,
    },
    workstation::WorkstationDialect,
};

const LIST_SECTIONS: [&str; 2] = ["Code", "Limits"];

fn active_gpt62_rules(tool_names: &[String], surface: PromptSurface) -> Vec<Gpt62Rule> {
    gpt62_rules()
        .into_iter()
        .filter(|rule| {
            let has_tool = rule.requires_tool.is_empty()
                || rule
                    .requires_tool
                    .iter()
                    .any(|name| tool_names.iter().any(|tool| tool == name));
            has_tool && (rule.surfaces.is_empty() || rule.surfaces.contains(&surface))
        })
        .collect()
}

fn build_gpt62_core(context: &DynamicPromptCoreContext) -> String {
    let tool_names: Vec<String> = context.tools.iter().map(|tool| tool.name.clone()).collect();
    let rules = active_gpt62_rules(&tool_names, context.surface);
    let mut sections: Vec<String> = GPT62_SECTIONS
        .iter()
        .filter_map(|section| {
            let directives: Vec<&str> = rules
                .iter()
                .filter(|rule| rule.section == *section)
                .map(|rule| rule.directive.as_str())
                .collect();
            if directives.is_empty() {
                return None;
            }
            let body = if LIST_SECTIONS.contains(section) {
                directives
                    .iter()
                    .map(|directive| format!("- {directive}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                directives.join("\n\n")
            };
            Some(format!("## {section}\n\n{body}"))
        })
        .collect();
    if let Some(guidelines_at) = context.tool_section.find("## Tool Guidelines") {
        sections.push(context.tool_section[guidelines_at..].to_owned());
    }
    format!(
        "You are {}, a coding agent working in the user's workspace. You carry the user's goal to a finished, verified result: they hand you a problem and come back to find it solved.\n\n{}",
        maho_core::config::app_name(),
        sections.join("\n\n")
    )
}

/// `buildGpt62Prompt`.
pub fn build_gpt62_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.core_prompt = Some(&build_gpt62_core);
    options.workstation_dialect = Some(WorkstationDialect::Codex);
    build_dynamic_system_prompt(&options)
}
