//! Port of omo-senpi `components/js-kernel-guard/{index,prelude}.test.ts`. The prelude runs in Bun,
//! the eval kernel's runtime: against stand-in `process` objects as upstream does, and once against
//! Bun's own `process`.
use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension,
    SourceInfo, ToolExecutionMode, ToolExposure,
};
use maho_omo_js_kernel_guard::{
    JS_KERNEL_GUARD_TOOL_NAME, JsKernelGuardComponent,
    prelude::{JS_KERNEL_GUARD_GLOBAL, PROCESS_EXIT_IN_CELL, js_kernel_guard_prelude_source},
};
use serde_json::{Value, json};

/// Runs `script` with Bun, `PRELUDE` and `GLOBAL` bound, and returns its exit code and printed JSON.
fn run_in_bun(prelude: &str, script: &str) -> (Option<i32>, Value) {
    let program = format!(
        "const PRELUDE = {};\nconst GLOBAL = {};\n{script}",
        json!(prelude),
        json!(JS_KERNEL_GUARD_GLOBAL)
    );
    let output = std::process::Command::new("bun")
        .args(["-e", &program])
        .output()
        .expect("bun runs");
    let printed = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "bun printed no JSON ({error}); stdout {:?}, stderr {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), printed)
}

const STAND_IN_CELL_EXIT: &str = r#"
const calls = [];
const stand_in = { exit(code) { calls.push(code ?? null); } };
const kernelExit = stand_in.exit.bind(stand_in);
const target = {};
new Function("process", "globalThis", PRELUDE)(stand_in, target);
kernelExit(0);
let cellError = null;
try { stand_in.exit(1); } catch (error) { cellError = { isError: error instanceof Error, name: error.name }; }
console.log(JSON.stringify({ calls, guard: target[GLOBAL] ?? null, cellError }));
"#;

#[test]
fn the_registered_tool_is_eval_only_and_its_prelude_rejects_process_exit() {
    let mut api = ExtensionApi::new(
        LoadedExtension::new(
            "js-kernel-guard",
            "/workspace".into(),
            SourceInfo::default(),
        ),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    JsKernelGuardComponent.register(&mut api);
    let tool = &api
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == JS_KERNEL_GUARD_TOOL_NAME)
        .expect("guard tool registered")
        .definition;
    let prelude = tool.kernel_prelude.as_ref().expect("kernel prelude");

    let (code, printed) = run_in_bun(&prelude.javascript, STAND_IN_CELL_EXIT);

    assert_eq!(tool.exposure, Some(ToolExposure::Eval));
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Parallel));
    let parser = tool.permission_parser.as_ref().expect("permission parser");
    assert!(parser(&json!({}), std::path::Path::new("/work")).is_empty());
    assert_eq!(prelude.exports, [JS_KERNEL_GUARD_GLOBAL]);
    assert_eq!(code, Some(0));
    assert_eq!(printed["guard"], true);
    assert_eq!(printed["cellError"]["name"], PROCESS_EXIT_IN_CELL);
}

#[test]
fn the_kernel_binding_still_exits_while_a_cell_call_throws() {
    let (code, printed) = run_in_bun(&js_kernel_guard_prelude_source(), STAND_IN_CELL_EXIT);

    assert_eq!(code, Some(0));
    assert_eq!(printed["calls"], json!([0]));
    assert_eq!(printed["guard"], true);
    assert_eq!(
        printed["cellError"],
        json!({"isError":true,"name":PROCESS_EXIT_IN_CELL})
    );
}

#[test]
fn an_imported_module_whose_catch_calls_process_exit_throws_instead_of_exiting() {
    let script = r#"
const calls = [];
const stand_in = { exit(code) { calls.push(code ?? 0); } };
new Function("process", "globalThis", PRELUDE)(stand_in, {});
const imported = () => {
  try { throw new Error("node-agent: missing required environment variable BUNSHIN_HUB_URL"); }
  catch { stand_in.exit(1); }
};
let caught = null;
try { imported(); } catch (error) { caught = { isError: error instanceof Error, name: error.name }; }
console.log(JSON.stringify({ calls, caught }));
"#;

    let (code, printed) = run_in_bun(&js_kernel_guard_prelude_source(), script);

    assert_eq!(code, Some(0));
    assert_eq!(printed["calls"], json!([]));
    assert_eq!(
        printed["caught"],
        json!({"isError":true,"name":PROCESS_EXIT_IN_CELL})
    );
}

#[test]
fn a_cell_exit_on_the_real_process_keeps_it_and_its_globals_while_the_early_binding_still_exits() {
    // A process.exit(3) that got through would end Bun with 3 before anything is printed; the
    // binding taken before the guard ends it with 7 once the JSON is flushed.
    let script = r#"
const exit = process.exit.bind(process);
new Function(PRELUDE)();
globalThis.kept = 41;
let name = null;
try { process.exit(3); } catch (error) { name = error.name; }
const printed = JSON.stringify({ name, kept: globalThis.kept + 1, guard: globalThis[GLOBAL] ?? null });
process.stdout.write(printed + "\n", () => exit(7));
"#;

    let (code, printed) = run_in_bun(&js_kernel_guard_prelude_source(), script);

    assert_eq!(code, Some(7));
    assert_eq!(
        printed,
        json!({"name":PROCESS_EXIT_IN_CELL,"kept":42,"guard":true})
    );
}
