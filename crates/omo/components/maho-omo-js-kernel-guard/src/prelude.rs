//! Port of omo-senpi `components/js-kernel-guard/prelude.ts`.
use maho_ext_api::KernelPrelude;
use serde_json::json;

/// Global the prelude installs. The kernel re-runs the snippet only while this name is missing.
pub const JS_KERNEL_GUARD_GLOBAL: &str = "omoJsKernelGuard";

pub const PROCESS_EXIT_IN_CELL: &str = "ProcessExitInCell";

/// Statements the JS kernel evals before a cell, once the guard global is missing.
/// The kernel binds the real `process.exit` at process start and uses that binding for its own
/// shutdown. This replacement is what user code and imported modules see, so `process.exit` inside
/// a cell throws instead of ending the worker.
pub fn js_kernel_guard_prelude_source() -> String {
    [
        format!("globalThis[{}] = true", json!(JS_KERNEL_GUARD_GLOBAL)),
        "process.exit = function omoCellProcessExit(code) {".to_owned(),
        "  const error = new Error(".to_owned(),
        "    \"process.exit(\" + String(code ?? 0) + \") ran inside a JavaScript cell. The kernel stayed up and its globals were kept.\"".to_owned(),
        "  )".to_owned(),
        format!("  error.name = {}", json!(PROCESS_EXIT_IN_CELL)),
        "  throw error".to_owned(),
        "}".to_owned(),
    ]
    .join("\n")
}

/// Upstream `jsKernelGuardPrelude`. Its documentation drops the bunshin fleet hint upstream
/// prepends: mhc ships no bunshin SDK (#12258 is not ported).
pub fn js_kernel_guard_prelude() -> KernelPrelude {
    KernelPrelude {
        javascript: js_kernel_guard_prelude_source(),
        python: format!("{JS_KERNEL_GUARD_GLOBAL} = True\n"),
        documentation: format!(
            "Calling process.exit inside a JS cell throws {PROCESS_EXIT_IN_CELL} and leaves the kernel and its globals in place."
        ),
        exports: vec![JS_KERNEL_GUARD_GLOBAL.to_owned()],
        ..Default::default()
    }
}
