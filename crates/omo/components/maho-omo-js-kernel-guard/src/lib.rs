//! Port of omo-senpi `components/js-kernel-guard/index.ts` (oh-my-openagent #14023).
pub mod prelude;

use maho_ext_api::{
    AgentToolResult, Extension, ExtensionApi, ExtensionToolExecutor, ToolDefinition,
    ToolExecutionMode, ToolExposure,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub const JS_KERNEL_GUARD_COMPONENT_NAME: &str = "js-kernel-guard";
pub const JS_KERNEL_GUARD_TOOL_NAME: &str = "js_kernel_guard";

/// What the tool says about itself. Upstream reuses the bunshin fleet SDK hint here; mhc ships no
/// bunshin SDK (#12258 is not ported), so the description names the guard instead.
pub const JS_KERNEL_GUARD_DESCRIPTION: &str = "Eval kernel guard: process.exit inside a JS cell throws ProcessExitInCell instead of ending the kernel, so the kernel and its globals stay. Not for direct use.";

pub fn parameters() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

/// Eval-only tool whose kernel prelude stops `process.exit` from killing the JS worker.
/// Senpi includes an eval-exposed tool's prelude even while the tool is withheld from the model, so
/// the guard is installed before the first cell and stays for the life of that kernel.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsKernelGuardComponent;

impl Extension for JsKernelGuardComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let mut tool = ToolDefinition::new(
            JS_KERNEL_GUARD_TOOL_NAME,
            JS_KERNEL_GUARD_DESCRIPTION,
            parameters(),
            Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
        );
        tool.label = "JS kernel guard".into();
        tool.exposure = Some(ToolExposure::Eval);
        tool.execution_mode = Some(ToolExecutionMode::Parallel);
        tool.permission_parser = Some(Arc::new(|_, _| Vec::new()));
        tool.kernel_prelude = Some(prelude::js_kernel_guard_prelude());
        let execute: ExtensionToolExecutor =
            Arc::new(|_id, _params, _signal, _update, _context| {
                Box::pin(async { Ok(AgentToolResult::text(JS_KERNEL_GUARD_DESCRIPTION)) })
            });
        if let Err(error) = api.register_tool_with_extension_context(tool, execute) {
            std::panic::panic_any(error);
        }
    }
}
