//! Port of senpi `packages/senpi-codemode/src/extension/reachable-tools.ts` (6d4426a28; senpi#6276, senpi#6887).
use crate::bridges::schema_bridge::EvalSchemaToolInfo;
use maho_tools::definition::ToolExposure;

/// The tools a cell can call: the model-visible active tools plus every registered tool whose declared
/// exposure is `eval`. The session withholds an eval-exposed tool from the model whenever eval itself is
/// registered (bash, grep, glob, generate_image, a plugin's thread_* or task_* family), so it is absent
/// from `get_active_tools()` while `execute_tool` still reaches it; reading only the active list made
/// `agent()`/`output()` report the task tool unavailable and dropped its kernel prelude the moment it
/// became eval-only (senpi#6269 follow-up).
///
/// Active tools keep their order; eval-exposed tools follow in registration order.
pub fn reachable_tool_names(
    active_tools: &[String],
    all_tools: &[EvalSchemaToolInfo],
) -> Vec<String> {
    let mut names = active_tools.to_vec();
    for tool in all_tools {
        if tool.exposure == Some(ToolExposure::Eval) && !names.contains(&tool.name) {
            names.push(tool.name.clone());
        }
    }
    names
}

/// The tools the session withholds from the model and routes through eval only, in registration order
/// (senpi#6886).
pub fn eval_only_tool_names(all_tools: &[EvalSchemaToolInfo]) -> Vec<String> {
    all_tools
        .iter()
        .filter(|tool| tool.eval_only)
        .map(|tool| tool.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, exposure: Option<ToolExposure>, eval_only: bool) -> EvalSchemaToolInfo {
        EvalSchemaToolInfo {
            name: name.into(),
            description: None,
            parameters: None,
            kernel_prelude: None,
            exposure,
            eval_only,
        }
    }

    #[test]
    fn eval_exposed_tools_are_reachable_beside_the_active_ones() {
        let all = vec![
            tool("read", Some(ToolExposure::Direct), false),
            tool("task", Some(ToolExposure::Eval), true),
            tool("tool_search", Some(ToolExposure::Search), false),
        ];
        let reachable = reachable_tool_names(&["read".into(), "eval".into()], &all);
        assert_eq!(reachable, vec!["read", "eval", "task"]);
    }

    #[test]
    fn an_active_eval_exposed_tool_is_listed_once() {
        let all = vec![tool("task", Some(ToolExposure::Eval), false)];
        assert_eq!(reachable_tool_names(&["task".into()], &all), vec!["task"]);
    }

    #[test]
    fn eval_only_names_keep_registration_order() {
        let all = vec![
            tool("grep", Some(ToolExposure::Direct), true),
            tool("read", Some(ToolExposure::Direct), false),
            tool("bash", Some(ToolExposure::Direct), true),
        ];
        assert_eq!(eval_only_tool_names(&all), vec!["grep", "bash"]);
    }
}
