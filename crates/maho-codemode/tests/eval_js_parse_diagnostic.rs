use maho_codemode::{
    kernels::js::context_manager::JavaScriptKernel,
    tool::{
        detached_cell_manager::EvalDetachedCellManager, eval_tool::create_eval_tool,
        eval_tool_options::*, image_resize::*, types::*,
    },
};
use maho_tools::definition::{AbortSignal, ToolCall, ToolContent, ToolResult};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

struct Manager(Arc<JavaScriptKernel>);
impl EvalKernelManager for Manager {
    fn get_kernel(&self, _: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> {
        Box::pin(async { Ok(self.0.clone() as Arc<dyn EvalKernel>) })
    }
}

struct Images;
impl EvalImageSdk for Images {
    fn resize_image<'a>(
        &'a self,
        _: Vec<u8>,
        _: &'a str,
        _: Option<usize>,
    ) -> ImageFuture<'a, Option<ResizedImage>> {
        Box::pin(async { panic!("text-only scenario") })
    }
    fn convert_to_png<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> ImageFuture<'a, Option<EvalImageContent>> {
        Box::pin(async { panic!("text-only scenario") })
    }
}

struct CountingExecutor(Arc<AtomicUsize>);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for CountingExecutor {
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _: serde_json::Value,
        _: maho_ext_api::ExecuteToolOptions,
    ) -> maho_ext_api::ExecuteToolFuture<'a> {
        let count = self.0.clone();
        let tool_name = name.to_owned();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            Err(maho_ext_api::ExecuteToolError {
                code: maho_ext_api::ExecuteToolErrorCode::Blocked,
                tool_name,
                message: "unexpected tool execution".into(),
                active_tools: vec![],
            })
        })
    }
}

fn first_text(result: &ToolResult) -> &str {
    match &result.content[0] {
        ToolContent::Text { text, .. } => text.as_str(),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[tokio::test]
async fn locates_malformed_shell_interpolation_in_submitted_js_cell_without_running_it() {
    let kernel = Arc::new(
        JavaScriptKernel::start(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            "js-parse-diagnostic",
            4,
            None,
        )
        .await
        .expect("kernel"),
    );
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let cells = Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default())));
    let mut options = CreateEvalToolOptions::new(
        Arc::new(Manager(kernel.clone())),
        Arc::new(CountingExecutor(tool_calls.clone())),
        cells,
        Arc::new(Images),
    );
    options.settings.cell_timeout_seconds = 30.0;
    let tool = create_eval_tool(Arc::new(options)).expect("eval tool");
    let run = |id: &'static str, code: &str| {
        let execute = tool.execute.clone();
        let code = code.to_owned();
        async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                (execute)(ToolCall {
                    id,
                    params: json!({"language":"js","code":code,"summary":"syntax diagnostic"}),
                    signal: AbortSignal::default(),
                    on_update: None,
                    context: None,
                }),
            )
            .await
            .expect("bounded eval")
            .expect("eval result")
        }
    };

    let shell_line = "print('${sample:-fixture}')";
    let broken_cell = [
        "globalThis.ranBrokenCell = true;",
        "const cmd = `python3 - <<'PY'",
        shell_line,
        "PY`;",
    ]
    .join("\n");
    let failed = run("c1", &broken_cell).await;
    assert_eq!(
        failed.details.as_ref().expect("details")["isError"],
        json!(true)
    );
    assert!(
        first_text(&failed).contains(&format!(
            "line 3, column 16:\n{shell_line}\n               ^"
        )),
        "{}",
        first_text(&failed)
    );
    assert!(
        first_text(&failed).contains("shell heredoc"),
        "{}",
        first_text(&failed)
    );

    let nested = run(
        "c2",
        "const args = { command: `python3 - <<'PY'\n# sql`...` -> sql.exec(`...`)\nPY` };",
    )
    .await;
    assert!(
        first_text(&nested).contains("line 2, column 7:"),
        "{}",
        first_text(&nested)
    );

    let shift = run("c3", "const t = `x`;\nconst b = (a << c\nconst d = ;").await;
    assert!(
        !first_text(&shift).contains("shell heredoc"),
        "{}",
        first_text(&shift)
    );

    let after = run("c4", "typeof globalThis.ranBrokenCell").await;
    assert!(
        first_text(&after).contains("undefined"),
        "{}",
        first_text(&after)
    );

    let runtime_err = run("c5", r#"throw new SyntaxError("runtime sentinel")"#).await;
    assert_eq!(first_text(&runtime_err), "runtime sentinel");
    assert_eq!(tool_calls.load(Ordering::SeqCst), 0);

    kernel.close().await.expect("close kernel");
}
