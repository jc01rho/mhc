//! `tools/task/description-mcp-cap.test.ts` (#14291)

use std::collections::BTreeMap;

use serde_json::json;

use crate::agents::AgentDefinition;
use crate::tools::task::description::{DescriptionInput, build_task_tool_description};

// Claude Code (the Claude subscription path) keeps only the first 4096 chars of an MCP tool
// description and appends "... [truncated]" (#14291).
const CLAUDE_CODE_MCP_DESCRIPTION_CAP: usize = 4096;

const RULE_ANCHORS: [&str; 7] = [
    "Choose exactly one input form",
    "Each spawn MUST provide EITHER category OR subagent_type",
    "model is an explicit override for subagent_type spawns ONLY",
    "NEVER combine model with category",
    "task_send continues an existing child; task always spawns.",
    "Prompts MUST be in English.",
    "plan-reviewer is one-shot",
];

fn agent(name: &str, description: &str) -> AgentDefinition {
    let mut definition = AgentDefinition::named(name);
    definition.description = description.to_string().into();
    definition
}

#[test]
fn given_builtin_categories_plus_a_long_custom_one_when_built_then_every_rule_ends_inside_the_mcp_cap()
 {
    let config = json!({
        "categories": { "release-crew": { "description": "Ships the release train. ".repeat(80) } },
        "agents": {},
    });
    let agents: BTreeMap<String, AgentDefinition> = [
        ("explore", "Codebase search"),
        ("librarian", "Docs research"),
        ("plan-consultant", "Pre-planning consultant"),
        ("plan-reviewer", "Plan reviewer"),
    ]
    .into_iter()
    .map(|(name, description)| (name.to_string(), agent(name, description)))
    .collect();

    let description = build_task_tool_description(&DescriptionInput {
        omo_config: &config,
        agents: &agents,
    });

    assert!(description.chars().count() > CLAUDE_CODE_MCP_DESCRIPTION_CAP);
    let kept: String = description
        .chars()
        .take(CLAUDE_CODE_MCP_DESCRIPTION_CAP)
        .collect();
    let cut: Vec<&str> = RULE_ANCHORS
        .into_iter()
        .filter(|anchor| !kept.contains(anchor))
        .collect();
    assert!(
        cut.is_empty(),
        "rules past the MCP description cap: {cut:?}"
    );
    assert!(kept.contains("Available agents: explore, librarian"));
}
