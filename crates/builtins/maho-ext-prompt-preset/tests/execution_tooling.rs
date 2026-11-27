use maho_ext_prompt_preset::execution_tooling::{
    EXECUTION_TOOLING_RULES, ExecutionToolingDialect, build_execution_tooling_paragraph,
    build_execution_tooling_section, render_execution_tooling_directive,
};

const FILE_EDITING_TOOLS: [&str; 3] = ["edit", "write", "apply_patch"];
const DIALECTS: [ExecutionToolingDialect; 2] = [
    ExecutionToolingDialect::Claude,
    ExecutionToolingDialect::Kimi,
];

// Session tools -> the file-editing tools the section must name (senpi#7275 review findings).
const EDIT_TOOL_PROFILES: [(&[&str], &[&str]); 4] = [
    (&["eval", "edit", "write"], &["edit", "write"]),
    (&["eval", "apply_patch"], &["apply_patch"]),
    (&["eval", "write"], &["write"]),
    (&["eval"], &[]),
];

fn tool_names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn backticked_tokens(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).collect()
}

fn named_editing_tools<'a>(tokens: &[&str]) -> Vec<&'a str> {
    FILE_EDITING_TOOLS
        .into_iter()
        .filter(|name| tokens.contains(name))
        .collect()
}

#[test]
fn absent_eval_suppresses_both_dialects() {
    for dialect in [
        ExecutionToolingDialect::Claude,
        ExecutionToolingDialect::Kimi,
    ] {
        assert!(build_execution_tooling_section(&["read".into()], dialect).is_empty());
        assert!(build_execution_tooling_paragraph(&[], dialect).is_empty());
    }
}
#[test]
fn claude_eval_routing_uses_tagged_envelope() {
    let result = build_execution_tooling_section(&["eval".into()], ExecutionToolingDialect::Claude);
    assert!(result.starts_with("<execution_tooling>\n"));
    assert!(result.ends_with("\n</execution_tooling>"));
}
#[test]
fn kimi_eval_routing_is_untagged_and_paragraph_adds_gap() {
    let tools = ["eval".into()];
    let result = build_execution_tooling_section(&tools, ExecutionToolingDialect::Kimi);
    assert!(!result.is_empty());
    assert!(!result.starts_with("<execution_tooling>"));
    assert_eq!(
        build_execution_tooling_paragraph(&tools, ExecutionToolingDialect::Kimi),
        format!("{result}\n\n")
    );
}
#[test]
fn machine_rule_ids_remain_unique_and_eval_scoped() {
    let ids: std::collections::BTreeSet<_> =
        EXECUTION_TOOLING_RULES.iter().map(|rule| rule.id).collect();
    assert_eq!(ids.len(), 4);
    assert!(
        EXECUTION_TOOLING_RULES
            .iter()
            .all(|rule| rule.concern == "code-cell-routing")
    );
}
#[test]
fn the_section_names_exactly_the_offered_editing_tools() {
    for dialect in DIALECTS {
        for (tools, expected) in EDIT_TOOL_PROFILES {
            let names = tool_names(tools);
            let section = build_execution_tooling_section(&names, dialect);
            let tokens = backticked_tokens(&section);
            assert_eq!(
                named_editing_tools(&tokens),
                expected,
                "{dialect:?} {tools:?}"
            );
            let mut foreign: Vec<&str> = tokens
                .iter()
                .copied()
                .filter(|token| !tools.contains(token))
                .collect();
            foreign.sort_unstable();
            foreign.dedup();
            assert_eq!(foreign, ["parallel(thunks)"], "{dialect:?} {tools:?}");
            for rule in EXECUTION_TOOLING_RULES
                .iter()
                .filter(|rule| matches!(rule.id, "eval-routing-decision" | "eval-stay-direct"))
            {
                let rendered = render_execution_tooling_directive(rule, &names, dialect);
                assert_eq!(
                    named_editing_tools(&backticked_tokens(&rendered)),
                    expected,
                    "{} {dialect:?} {tools:?}",
                    rule.id
                );
            }
        }
    }
}
#[test]
fn editing_tools_alone_render_no_section_and_no_tool_name() {
    for dialect in DIALECTS {
        assert!(
            build_execution_tooling_section(&tool_names(&FILE_EDITING_TOOLS), dialect).is_empty()
        );
    }
}
#[test]
fn clause_markers_never_reach_a_rendered_section() {
    for dialect in DIALECTS {
        for (tools, _) in EDIT_TOOL_PROFILES {
            let section = build_execution_tooling_section(&tool_names(tools), dialect);
            for marker in ["[[", "]]", "{editTools}"] {
                assert!(
                    !section.contains(marker),
                    "{dialect:?} {tools:?} leaked {marker}"
                );
            }
        }
    }
}
