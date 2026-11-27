use crate::file_operations::{format_tool_list, resolve_file_mutation_routing};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionToolingDialect {
    Claude,
    Kimi,
}

pub struct ExecutionToolingRule {
    pub id: &'static str,
    pub concern: &'static str,
    pub claude: &'static str,
    pub kimi: &'static str,
}

pub const EXECUTION_TOOLING_RULES: &[ExecutionToolingRule] = &[
    ExecutionToolingRule {
        id: "eval-routing-decision",
        concern: "code-cell-routing",
        claude: "Sort a multi-call step before you write it: independent reads, searches, symbol lookups, and probes go into ONE `eval` cell via `parallel(thunks)` - an extra read-only call in that wave is nearly free, a stale assumption costs the turn - while edits[[ ({editTools}, not cell code)]], side-effecting commands, deploys, and approvals run one at a time, each observed before the next.",
        kimi: "Sort a multi-call step before you write it: put independent reads, searches, symbol lookups, and probes into one `eval` cell with `parallel(thunks)`, and run edits[[ ({editTools}, not cell code)]], side-effecting commands, deploys, and approvals one at a time, looking at each result before the next.",
    },
    ExecutionToolingRule {
        id: "eval-evidence-return",
        concern: "code-cell-routing",
        claude: "Name the state a cell should produce before running it; when it returns, COMPARE the returned evidence with that state, and for a cell that changed something also check that nothing changed beyond it. A result that hides a failed item or a truncated tail is not evidence.",
        kimi: "Name the state a cell should produce before running it; when it returns, compare the returned evidence with that state, and for a cell that changed something also check that nothing changed beyond it. A result that hides a failed item or a truncated tail is not evidence.",
    },
    ExecutionToolingRule {
        id: "perceived-state-loop",
        concern: "code-cell-routing",
        claude: "When the result must be SEEN rather than read - a page, a component, an image, a 3D scene, a layout - make one change, render or screenshot it, look, then make the next; check a 3D scene from several angles and a page at desktop and mobile widths. Compare what you see with the reference or the stated intent, and ask only where two readings of that intent diverge.",
        kimi: "When the result must be seen rather than read - a page, a component, an image, a 3D scene, a layout - make one change, render or screenshot it, look, then make the next; check a 3D scene from several angles and a page at desktop and mobile widths. Compare what you see with the reference or the stated intent, and ask only where two readings of that intent diverge.",
    },
    ExecutionToolingRule {
        id: "eval-stay-direct",
        concern: "code-cell-routing",
        claude: "Call a tool directly only [[for {editTools} or ]]when one call is enough, the result decides the next call, semantic judgment sits between calls, or the action needs approval.",
        kimi: "Use a direct tool call [[for {editTools} or ]]when one call is enough, when each result decides the next call, or when the action needs approval - then stop deliberating and make it.",
    },
];

const EDIT_TOOLS_CLAUSE_OPEN: &str = "[[";
const EDIT_TOOLS_CLAUSE_CLOSE: &str = "]]";

/// The rule's wording for these tools: its `[[...]]` clause names the session's file-editing tools, or is dropped when
/// the session offers none (senpi#7275).
pub fn render_execution_tooling_directive(
    rule: &ExecutionToolingRule,
    tool_names: &[String],
    dialect: ExecutionToolingDialect,
) -> String {
    let directive = match dialect {
        ExecutionToolingDialect::Claude => rule.claude,
        ExecutionToolingDialect::Kimi => rule.kimi,
    };
    let Some((before, rest)) = directive.split_once(EDIT_TOOLS_CLAUSE_OPEN) else {
        return directive.to_owned();
    };
    let Some((body, after)) = rest.split_once(EDIT_TOOLS_CLAUSE_CLOSE) else {
        return directive.to_owned();
    };
    let edit_tools = resolve_file_mutation_routing(tool_names).tools;
    let clause = if edit_tools.is_empty() {
        String::new()
    } else {
        body.replacen("{editTools}", &format_tool_list(&edit_tools), 1)
    };
    format!("{before}{clause}{after}")
}

pub fn build_execution_tooling_section(
    tool_names: &[String],
    dialect: ExecutionToolingDialect,
) -> String {
    if !tool_names.iter().any(|name| name == "eval") {
        return String::new();
    }
    let body = EXECUTION_TOOLING_RULES
        .iter()
        .map(|rule| render_execution_tooling_directive(rule, tool_names, dialect))
        .collect::<Vec<_>>()
        .join("\n\n");
    match dialect {
        ExecutionToolingDialect::Claude => {
            format!("<execution_tooling>\n{body}\n</execution_tooling>")
        }
        ExecutionToolingDialect::Kimi => body,
    }
}

pub fn build_execution_tooling_paragraph(
    tool_names: &[String],
    dialect: ExecutionToolingDialect,
) -> String {
    let section = build_execution_tooling_section(tool_names, dialect);
    if section.is_empty() {
        section
    } else {
        format!("{section}\n\n")
    }
}
