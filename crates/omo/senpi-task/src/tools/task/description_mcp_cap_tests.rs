//! `tools/task/description-mcp-cap.test.ts` (#14291, #14292)

use std::collections::BTreeMap;

use serde_json::json;

use crate::agents::AgentDefinition;
use crate::tools::task::categories::list_task_categories;
use crate::tools::task::description::{DescriptionInput, build_task_tool_description};

// Claude Code (the Claude subscription path) keeps only the first 4096 chars of an MCP tool
// description and appends "... [truncated]" (#14291).
const CLAUDE_CODE_MCP_DESCRIPTION_CAP: usize = 4096;

fn agent(name: &str, description: &str) -> AgentDefinition {
    let mut definition = AgentDefinition::named(name);
    definition.description = description.to_string().into();
    definition
}

#[test]
fn given_many_agents_and_a_long_custom_category_when_built_then_only_the_category_catalogue_lies_past_the_cap()
 {
    let config = json!({
        "categories": { "release-crew": { "description": "Ships the release train. ".repeat(80) } },
        "agents": {},
    });
    let categories = list_task_categories(&config);
    let mut agents: BTreeMap<String, AgentDefinition> = (0..40)
        .map(|index| {
            let name = format!("agent-{index}");
            (name.clone(), agent(&name, "Plain agent"))
        })
        .collect();
    agents.insert(
        "plan-consultant".to_string(),
        agent("plan-consultant", "Pre-planning consultant"),
    );
    agents.insert(
        "plan-reviewer".to_string(),
        agent("plan-reviewer", "Plan reviewer"),
    );

    let description = build_task_tool_description(&DescriptionInput {
        omo_config: &config,
        agents: &agents,
    });

    assert!(description.chars().count() > CLAUDE_CODE_MCP_DESCRIPTION_CAP);
    let first_category = categories.first().expect("at least one category");
    let catalogue_marker = format!("\n  - {}", first_category.name);
    let catalogue_start = description
        .find(&catalogue_marker)
        .expect("category catalogue marker");
    assert!(catalogue_start > 0);
    assert!(
        description[..catalogue_start].chars().count() <= CLAUDE_CODE_MCP_DESCRIPTION_CAP,
        "rules and agent list exceeded MCP cap: {}",
        description[..catalogue_start].chars().count()
    );
    let tail: Vec<&str> = description[catalogue_start + 1..].lines().collect();
    let unexpected: Vec<&str> = tail
        .iter()
        .copied()
        .filter(|line| !line.starts_with("  - ") && !line.starts_with("    "))
        .collect();
    assert!(
        unexpected.is_empty(),
        "non-catalogue lines after catalogue start: {unexpected:?}"
    );
    let category_lines = tail.iter().filter(|line| line.starts_with("  - ")).count();
    assert_eq!(category_lines, categories.len());
}
