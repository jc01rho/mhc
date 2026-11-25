use crate::{
    context::MemoryIdentityContext,
    engine_session::{MemoryEngineSessionOptions, prepare_memory_engine_session},
    tool_call_bound::{BoundedCall, MEMORY_CALL_TIMEOUT_MS, MemoryCallStage, bound_memory_call},
    tool_metadata::*,
};
use maho_ext_api::{
    ExtensionApi, McpExposure, McpServerDeclaration, McpTransport, ToolDefinition, ToolError,
    ToolExecutionMode, ToolExposure, ToolResult,
};
use memory_core::tools::{
    memory::{MemoryToolParams, MemoryToolProvenanceInput, run_memory_tool_with_stage},
    memory_apply_patch::{MemoryApplyPatchParams, run_memory_apply_patch},
};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

pub type MemoryContextResolver = Arc<dyn Fn() -> Option<MemoryIdentityContext> + Send + Sync>;
pub type MemoryCommitNotice =
    Arc<dyn Fn(memory_core::tools::memory::MemoryToolCommit) + Send + Sync>;
/// `memory.write_notice.enabled` for the bound identity (pin `wiring-static.ts:142`).
pub type WriteNoticeGate = Arc<dyn Fn() -> bool + Send + Sync>;
/// `writeNotice.resolveSessionId`: the bound session, whose journal and snapshot feed the notice's
/// size and timeline numbers; without one only the affected files are gathered.
pub type WriteNoticeSessionResolver = Arc<dyn Fn() -> Option<String> + Send + Sync>;
#[derive(Clone, Default)]
pub struct MemoryToolsOptions {
    pub lock_wait_timeout_ms: Option<u64>,
    pub lock_retry_delay_ms: Option<u64>,
    pub call_timeout_ms: Option<u64>,
    pub on_commit: Option<MemoryCommitNotice>,
    /// Read per call rather than latched at registration: the gate is presentation-only, so a
    /// config edit takes effect on the next write instead of at the next restart.
    pub write_notice: Option<WriteNoticeGate>,
    pub write_notice_session: Option<WriteNoticeSessionResolver>,
    /// `memory.tool_exposure`; `Search` registers both tools behind tool discovery
    /// (pin `tools.ts` registerMemoryToolSurface exposure option).
    pub exposure: Option<ToolExposure>,
}

#[derive(Clone)]
struct SharedStage(Arc<AtomicU8>);

impl SharedStage {
    fn new() -> Self {
        Self(Arc::new(AtomicU8::new(0)))
    }

    fn set(&self, stage: MemoryCallStage) {
        let code = match stage {
            MemoryCallStage::Prepare => 0,
            MemoryCallStage::Lock => 1,
            MemoryCallStage::Check => 2,
            MemoryCallStage::Apply => 3,
            MemoryCallStage::Commit => 4,
            MemoryCallStage::Report => 5,
        };
        self.0.store(code, Ordering::SeqCst);
    }

    fn get(&self) -> MemoryCallStage {
        match self.0.load(Ordering::SeqCst) {
            1 => MemoryCallStage::Lock,
            2 => MemoryCallStage::Check,
            3 => MemoryCallStage::Apply,
            4 => MemoryCallStage::Commit,
            5 => MemoryCallStage::Report,
            _ => MemoryCallStage::Prepare,
        }
    }
}

fn result(message: String) -> ToolResult {
    let mut result = ToolResult::text(&message);
    result.details = Some(serde_json::json!({"message":message}));
    result
}

fn result_with_write_notice(
    message: String,
    write_notice: Option<serde_json::Value>,
) -> ToolResult {
    let mut result = ToolResult::text(&message);
    let mut details = serde_json::json!({"message":message});
    if let Some(notice) = write_notice
        && let Some(object) = details.as_object_mut()
    {
        object.insert("writeNotice".into(), notice);
    }
    result.details = Some(details);
    result
}

pub fn create_memory_tools(resolve: MemoryContextResolver) -> [ToolDefinition; 2] {
    create_memory_tools_with_options(resolve, MemoryToolsOptions::default())
}

pub fn create_memory_tools_with_options(
    resolve: MemoryContextResolver,
    options: MemoryToolsOptions,
) -> [ToolDefinition; 2] {
    let exposure = options.exposure;
    let memory_resolve = Arc::clone(&resolve);
    let memory_options = options.clone();
    let mut memory = ToolDefinition::new(
        MEMORY_TOOL_NAME,
        MEMORY_TOOL_DESCRIPTION,
        serde_json::json!({"type":"object","required":["command","reason"],"properties":{"command":{"enum":["create","str_replace","insert","delete","rename","update_description"]},"reason":{"type":"string"},"file_path":{"type":"string"},"old_path":{"type":"string"},"new_path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"},"insert_line":{"type":"number"},"insert_text":{"type":"string"},"description":{"type":"string"},"file_text":{"type":"string"},"source":{"type":"string","enum":["user","agent","inferred"]},"user_quote":{"type":"string"}}}),
        Arc::new(move |call| {
            let resolve = Arc::clone(&memory_resolve);
            let options = memory_options.clone();
            Box::pin(async move {
                let context = resolve().ok_or_else(|| ToolError::Message("memory: no memory identity bound to this session; enable omo memory and restart the session so the memory tools can initialize".into()))?;
                if context.memory_write_denied {
                    return Err(ToolError::Message(
                        crate::write_policy::memory_write_denied_message(MEMORY_TOOL_NAME),
                    ));
                }
                let command_label = call
                    .params
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("memory")
                    .to_owned();
                let call_params = call.params;
                let call_signal = call.signal;
                let stage = SharedStage::new();
                let run_stage = stage.clone();
                let timeout_ms = options.call_timeout_ms.unwrap_or(MEMORY_CALL_TIMEOUT_MS);
                let outcome = bound_memory_call(
                    move |call_state| async move {
                        let write_stage = run_stage.clone();
                        let write_options = options.clone();
                        let write_context = context.clone();
                        let (value, engine_repo, reason, repair_note) =
                            tokio::task::spawn_blocking(move || -> Result<_, ToolError> {
                                let engine = prepare_memory_engine_session(
                                    &write_context.identity,
                                    &write_context.identity_paths,
                                    MemoryEngineSessionOptions {
                                        lock_wait_timeout_ms: write_options.lock_wait_timeout_ms,
                                        lock_retry_delay_ms: write_options.lock_retry_delay_ms,
                                        ..Default::default()
                                    },
                                )
                                .map_err(|e| ToolError::Message(e.message))?;
                                let params: MemoryToolParams = serde_json::from_value(call_params)?;
                                let repaired =
                                    memory_core::tools::leaked_arguments::repair_leaked_arguments(
                                        params,
                                    );
                                let repair_note =
                                    memory_core::tools::leaked_arguments::describe_repairs(
                                        &repaired.repairs,
                                    );
                                let params = repaired.params;
                                let reason = params.reason.clone();
                                let stage_cb = write_stage.clone();
                                let value = run_memory_tool_with_stage(
                                    &engine.repo,
                                    &engine.author,
                                    &params,
                                    Some(&engine.lock),
                                    &move |next| stage_cb.set(next.into()),
                                )
                                .map_err(|e| ToolError::Message(e.message))?;
                                Ok((value, engine.repo, reason, repair_note))
                            })
                            .await
                            .map_err(|e| {
                                ToolError::Message(format!("memory write task failed: {e}"))
                            })??;
                        run_stage.set(MemoryCallStage::Report);
                        // A write that lands after the call was answered stopped must not announce itself.
                        if call_state.is_stopped() {
                            return Ok(result(value.result));
                        }
                        if let (Some(commit), Some(notice)) =
                            (value.commit.as_ref(), options.on_commit.as_ref())
                        {
                            notice(commit.clone());
                        }
                        if call_state.is_stopped() {
                            return Ok(result(value.result));
                        }
                        let write_notice =
                            if options.write_notice.as_ref().is_some_and(|gate| gate()) {
                                if let Some(commit) = value.commit {
                                    let session = options
                                        .write_notice_session
                                        .as_ref()
                                        .and_then(|resolve| resolve());
                                    tokio::task::spawn_blocking(move || {
                                        let notice =
                                        crate::memory_notice_spec::gather_write_notice_for_session(
                                            &context,
                                            &engine_repo,
                                            &commit.sha,
                                            reason.as_str(),
                                            session.as_deref(),
                                        );
                                        serde_json::to_value(notice)
                                            .unwrap_or(serde_json::Value::Null)
                                    })
                                    .await
                                    .ok()
                                } else {
                                    None
                                }
                            } else {
                                None
                            };
                        let mut message = value.result;
                        message.push_str(&repair_note);
                        Ok(result_with_write_notice(message, write_notice))
                    },
                    &command_label,
                    || stage.get(),
                    timeout_ms,
                    Some(&call_signal),
                )
                .await;
                match outcome {
                    BoundedCall::Settled(value) => Ok(value),
                    BoundedCall::Failed(error) => Err(error),
                    BoundedCall::Stopped(message) => Err(ToolError::Message(message)),
                }
            })
        }),
    );
    memory.label = "Memory".into();
    memory.render_shell = Some(maho_tools::definition::RenderShell::Own);
    memory.exposure = exposure;
    memory.execution_mode = Some(ToolExecutionMode::Sequential);
    memory.prompt_snippet = Some("memory - edit omo memory blocks (create/str_replace/insert/delete/rename/update_description); auto-commits each change".into());
    memory.prompt_guidelines = Some(vec!["Record durable facts, preferences, and decisions with the memory tool as you learn them; every change is committed with the reason you provide.".into(), "Memory files are markdown with YAML frontmatter; keep each block's description accurate because the memory index surfaces it.".into(), "When creating, renaming, or deleting memory files, update [[path]] references in other memory files so they stay discoverable.".into()]);
    let mut patch = ToolDefinition::new(
        MEMORY_APPLY_PATCH_TOOL_NAME,
        MEMORY_APPLY_PATCH_DESCRIPTION,
        serde_json::json!({"type":"object","required":["reason","input"],"properties":{"reason":{"type":"string"},"input":{"type":"string"},"source":{"type":"string","enum":["user","agent","inferred"]},"user_quote":{"type":"string"}}}),
        Arc::new(move |call| {
            let resolve = Arc::clone(&resolve);
            let options = options.clone();
            Box::pin(async move {
                let context = resolve().ok_or_else(|| ToolError::Message("memory_apply_patch: no memory identity bound to this session; enable omo memory and restart the session so the memory tools can initialize".into()))?;
                if context.memory_write_denied {
                    return Err(ToolError::Message(
                        crate::write_policy::memory_write_denied_message(
                            MEMORY_APPLY_PATCH_TOOL_NAME,
                        ),
                    ));
                }
                let engine = prepare_memory_engine_session(
                    &context.identity,
                    &context.identity_paths,
                    MemoryEngineSessionOptions {
                        lock_wait_timeout_ms: options.lock_wait_timeout_ms,
                        lock_retry_delay_ms: options.lock_retry_delay_ms,
                        ..Default::default()
                    },
                )
                .map_err(|e| ToolError::Message(e.message))?;
                let reason = call
                    .params
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::Message("memory_apply_patch: reason must be a string".into())
                    })?;
                let input = call
                    .params
                    .get("input")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        ToolError::Message("memory_apply_patch: input must be a string".into())
                    })?;
                // The registered ToolCall hook writes the trusted provenance object into the call
                // arguments; consume it here and never read a model-supplied value (pin
                // memory-apply-patch.ts:242-249). Absent provenance stays a plain-reason commit.
                let provenance = call
                    .params
                    .get("provenance")
                    .cloned()
                    .map(serde_json::from_value::<MemoryToolProvenanceInput>)
                    .transpose()
                    .map_err(|error| {
                        ToolError::Message(format!(
                            "memory_apply_patch: invalid provenance: {error}"
                        ))
                    })?;
                let params = MemoryApplyPatchParams {
                    reason: reason.into(),
                    input: input.into(),
                    author: engine.author.clone(),
                    provenance,
                };
                engine
                    .lock
                    .run("memory-write", || {
                        run_memory_apply_patch(&engine.repo, &params, None)
                    })
                    .map(|value| {
                        if let (Some(commit), Some(notice)) = (value.commit, options.on_commit) {
                            notice(commit);
                        }
                        result(value.message)
                    })
                    .map_err(|e| ToolError::Message(e.message))
            })
        }),
    );
    patch.label = "Memory Apply Patch".into();
    patch.exposure = exposure;
    patch.execution_mode = Some(ToolExecutionMode::Sequential);
    patch.prompt_snippet = Some("memory_apply_patch - apply a codex-style patch to omo memory files; auto-commits the change".into());
    patch.prompt_guidelines = Some(vec!["Use memory_apply_patch for multi-file or multi-hunk memory edits; prefer the memory tool for single-block changes.".into(), "Patches may only target paths inside the memory repo, and read_only memory files cannot be modified.".into()]);
    [memory, patch]
}

pub fn register_memory_tools_with_options(
    api: &mut ExtensionApi,
    resolve: MemoryContextResolver,
    options: MemoryToolsOptions,
) {
    let renderers =
        crate::memory_tool_renderers::memory_tool_renderers(options.write_notice.clone());
    let [memory, patch] = create_memory_tools_with_options(resolve, options);
    if let Err(error) = api.register_tool_with_renderers(memory, renderers) {
        std::panic::panic_any(error);
    }
    api.register_tool(patch);
}

pub fn register_memory_tools(api: &mut ExtensionApi, resolve: MemoryContextResolver) {
    register_memory_tools_with_options(api, resolve, MemoryToolsOptions::default());
}

/// Pin `tools.ts` `registerMemoryToolSurface`: direct registration is the default surface; the
/// search exposure registers the `omo-memory` stdio MCP server (served by this binary behind
/// `MEMORY_MCP_ENTRY_FLAG`) with `exposure: search` and `enabled: true`, so the tools surface
/// through tool_search as `mcp_omo-memory_*`. A host that cannot take the server falls back to
/// direct registration.
pub fn register_memory_tool_surface(
    api: &mut ExtensionApi,
    resolve: MemoryContextResolver,
    options: MemoryToolsOptions,
) {
    if options.exposure == Some(ToolExposure::Search)
        && let Ok(executable) = std::env::current_exe()
        && api
            .try_register_mcp_server(
                MEMORY_MCP_SERVER_NAME,
                memory_mcp_server_declaration(&executable),
            )
            .is_ok()
    {
        return;
    }
    register_memory_tools_with_options(api, resolve, options);
}

pub fn memory_mcp_server_declaration(executable: &std::path::Path) -> McpServerDeclaration {
    McpServerDeclaration {
        transport: Some(McpTransport::Stdio),
        command: Some(executable.to_string_lossy().into_owned()),
        args: Some(vec![crate::mcp_memory_server::MEMORY_MCP_ENTRY_FLAG.into()]),
        exposure: Some(McpExposure::Search),
        enabled: Some(true),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(params: serde_json::Value) -> maho_ext_api::ToolCall<'static> {
        maho_ext_api::ToolCall {
            id: "call",
            params,
            signal: Default::default(),
            on_update: None,
            context: None,
        }
    }
    #[test]
    fn definitions_are_sequential_with_schemas() {
        let tools = create_memory_tools(Arc::new(|| None));
        assert_eq!(tools[0].name, "memory");
        assert_eq!(tools[1].name, "memory_apply_patch");
        for tool in tools {
            assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Sequential));
            assert!(tool.parameters.is_object());
            assert!(tool.prompt_snippet.is_some());
            assert!(!tool.prompt_guidelines.unwrap().is_empty());
        }
    }
    #[test]
    fn search_exposure_marks_both_tools() {
        for tool in create_memory_tools_with_options(
            Arc::new(|| None),
            MemoryToolsOptions {
                exposure: Some(ToolExposure::Search),
                ..Default::default()
            },
        ) {
            assert_eq!(tool.exposure, Some(ToolExposure::Search));
        }
    }
    fn api() -> ExtensionApi {
        ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        )
    }
    #[test]
    fn search_surface_registers_the_memory_mcp_server_instead_of_tools() {
        let mut api = api();
        register_memory_tool_surface(
            &mut api,
            Arc::new(|| None),
            MemoryToolsOptions {
                exposure: Some(ToolExposure::Search),
                ..Default::default()
            },
        );
        assert!(api.registered.tools.is_empty());
        let [server] = api.registered.mcp_servers.as_slice() else {
            panic!("expected one server")
        };
        assert_eq!(server.name, "omo-memory");
        assert_eq!(
            server.config,
            memory_mcp_server_declaration(&std::env::current_exe().unwrap())
        );
        assert_eq!(
            server.config.args.as_deref(),
            Some(&["--omo-memory-mcp".to_owned()][..])
        );
    }
    #[test]
    fn direct_surface_registers_tools_and_no_server() {
        let mut api = api();
        register_memory_tool_surface(&mut api, Arc::new(|| None), MemoryToolsOptions::default());
        assert!(api.registered.mcp_servers.is_empty());
        assert_eq!(
            api.registered
                .tools
                .iter()
                .map(|tool| tool.definition.name.as_str())
                .collect::<Vec<_>>(),
            ["memory", "memory_apply_patch"]
        );
        assert_eq!(
            api.registered.tools[0].definition.render_shell,
            Some(maho_tools::definition::RenderShell::Own)
        );
        assert!(api.registered.erased_tool_renderers.contains_key("memory"));
    }
    #[tokio::test]
    async fn unbound_tools_fail_before_storage() {
        for tool in create_memory_tools(Arc::new(|| None)) {
            assert!((tool.execute)(call(serde_json::json!({}))).await.is_err());
        }
    }
    #[tokio::test]
    async fn real_create_and_patch_commit_clean_repo() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let context = MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let commits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let notices = Arc::clone(&commits);
        let [memory, patch] = create_memory_tools_with_options(
            Arc::new(move || Some(context.clone())),
            MemoryToolsOptions {
                on_commit: Some(Arc::new(move |commit| notices.lock().unwrap().push(commit))),
                ..Default::default()
            },
        );
        let created = (memory.execute)(call(serde_json::json!({"command":"create","reason":"Track fact","file_path":"reference/fact.md","description":"Fact","file_text":"before"}))).await.unwrap();
        assert_eq!(
            created.details.as_ref().unwrap()["message"],
            match &created.content[0] {
                maho_ext_api::ToolContent::Text { text, .. } => text.as_str(),
                _ => panic!("expected text"),
            }
        );
        (patch.execute)(call(serde_json::json!({"reason":"Update fact","input":"*** Begin Patch\n*** Update File: reference/fact.md\n@@\n-before\n+after\n*** End Patch"}))).await.unwrap();
        assert!(
            std::fs::read_to_string(paths.repo.join("reference/fact.md"))
                .unwrap()
                .contains("after")
        );
        let repo = memory_core::git::GitMemoryRepo::open(&paths.repo, "agent").unwrap();
        repo.clean_check().unwrap();
        assert!(!memory_core::locks::memory_writer_lock_path(&paths.locks).exists());
        assert_eq!(commits.lock().unwrap().len(), 2);
        assert_eq!(
            repo.head().unwrap().unwrap(),
            commits.lock().unwrap()[1].sha
        );
        assert!(
            (memory.execute)(call(
                serde_json::json!({"command":"delete","reason":"missing","file_path":"missing.md"})
            ))
            .await
            .is_err()
        );
        assert_eq!(commits.lock().unwrap().len(), 2);
    }

    fn bound_context(root: &std::path::Path) -> MemoryIdentityContext {
        let paths = memory_core::identity::layout::build_identity_paths(root, "agent");
        MemoryIdentityContext::new(
            "agent".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        )
    }

    #[tokio::test]
    async fn pre_aborted_signal_refuses_before_touching_repository() {
        let root = tempfile::tempdir().expect("tempdir");
        let context = bound_context(root.path());
        let repo_dir = context.identity_paths.repo.clone();
        let [memory, _] = create_memory_tools(Arc::new(move || Some(context.clone())));
        let signal = maho_ext_api::AbortSignal::default();
        signal.abort();

        let err = (memory.execute)(maho_ext_api::ToolCall {
            id: "call-1",
            params: serde_json::json!({
                "command": "create",
                "reason": "Never",
                "file_path": "never.md",
                "description": "d"
            }),
            signal,
            on_update: None,
            context: None,
        })
        .await
        .expect_err("pre-aborted signal must refuse");

        assert!(
            err.to_string()
                .contains("memory: create was cancelled while preparing the memory repository"),
            "{err}"
        );
        assert!(!repo_dir.exists());
    }

    #[tokio::test]
    async fn cancelled_while_reporting_write_names_report_stage() {
        let root = tempfile::tempdir().expect("tempdir");
        let context = bound_context(root.path());
        let repo_dir = context.identity_paths.repo.clone();
        let signal = maho_ext_api::AbortSignal::default();
        let abort_on_commit = signal.clone();
        let [memory, _] = create_memory_tools_with_options(
            Arc::new(move || Some(context.clone())),
            MemoryToolsOptions {
                on_commit: Some(Arc::new(move |_| abort_on_commit.abort())),
                ..Default::default()
            },
        );

        let err = (memory.execute)(maho_ext_api::ToolCall {
            id: "call-1",
            params: serde_json::json!({
                "command": "create",
                "reason": "Landed write",
                "file_path": "landed.md",
                "description": "d"
            }),
            signal,
            on_update: None,
            context: None,
        })
        .await
        .expect_err("abort during report stage must error");

        assert!(
            err.to_string()
                .contains("memory: create was cancelled while reporting the write"),
            "{err}"
        );
        let repo = memory_core::git::GitMemoryRepo::open(&repo_dir, "agent").expect("repo");
        let log = repo.log(None).expect("git log");
        assert_eq!(log[0].subject, "Landed write");
    }

    #[cfg(unix)]
    struct HeldCommit {
        started: std::path::PathBuf,
        release: std::path::PathBuf,
        _fifos: tempfile::TempDir,
    }

    #[cfg(unix)]
    async fn hold_next_commit(context: &MemoryIdentityContext) -> HeldCommit {
        use std::os::unix::fs::PermissionsExt;
        let warm_context = context.clone();
        let [warm_up_tool, _] = create_memory_tools(Arc::new(move || Some(warm_context.clone())));
        (warm_up_tool.execute)(call(serde_json::json!({
            "command": "create",
            "reason": "Warm up",
            "file_path": "warm-up.md",
            "description": "d"
        })))
        .await
        .expect("warm-up write");

        let fifos = tempfile::tempdir().expect("fifo tempdir");
        let started = fifos.path().join("started");
        let release = fifos.path().join("release");
        for fifo in [&started, &release] {
            let status = std::process::Command::new("mkfifo")
                .arg(fifo)
                .status()
                .expect("mkfifo");
            assert!(status.success(), "mkfifo failed for {}", fifo.display());
        }
        let hooks = fifos.path().join("hooks");
        std::fs::create_dir_all(&hooks).expect("hooks dir");
        let hook = hooks.join("pre-commit");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\nrm -f \"$0\"\nprintf started > '{}'\nread line < '{}'\nexit 0\n",
                started.display(),
                release.display()
            ),
        )
        .expect("write pre-commit hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("chmod pre-commit");
        let repo = memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo, "agent")
            .expect("open repo");
        repo.config_set("core.hooksPath", &hooks.to_string_lossy())
            .expect("set core.hooksPath");
        HeldCommit {
            started,
            release,
            _fifos: fifos,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn abort_mid_commit_names_commit_stage_and_suppresses_late_commit_announcement() {
        let root = tempfile::tempdir().expect("tempdir");
        let context = bound_context(root.path());
        let held = hold_next_commit(&context).await;
        let announced = Arc::new(std::sync::Mutex::new(Vec::new()));
        let memory_context = context.clone();
        let announced_cb = Arc::clone(&announced);
        let [memory, _] = create_memory_tools_with_options(
            Arc::new(move || Some(memory_context.clone())),
            MemoryToolsOptions {
                on_commit: Some(Arc::new(move |commit| {
                    announced_cb
                        .lock()
                        .expect("announced lock")
                        .push(commit.subject);
                })),
                ..Default::default()
            },
        );
        let signal = maho_ext_api::AbortSignal::default();
        let call_signal = signal.clone();
        let started_path = held.started.clone();
        let wait_started =
            tokio::task::spawn_blocking(move || std::fs::read_to_string(started_path));

        let exec_task = tokio::spawn(async move {
            (memory.execute)(maho_ext_api::ToolCall {
                id: "call-1",
                params: serde_json::json!({
                    "command": "create",
                    "reason": "Held write",
                    "file_path": "held.md",
                    "description": "d"
                }),
                signal: call_signal,
                on_update: None,
                context: None,
            })
            .await
        });

        let entered = tokio::time::timeout(std::time::Duration::from_secs(30), wait_started)
            .await
            .expect("timed out waiting for pre-commit hook")
            .expect("join started reader")
            .expect("read started fifo");
        assert_eq!(entered, "started");

        signal.abort();
        let err = exec_task
            .await
            .expect("join aborted task")
            .expect_err("aborted commit must error");
        assert!(
            err.to_string()
                .contains("memory: create was cancelled while committing to git"),
            "{err}"
        );

        let release_path = held.release.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || std::fs::write(release_path, "go\n")),
        )
        .await
        .expect("timed out releasing pre-commit hook")
        .expect("join release writer")
        .expect("write release fifo");

        let next_context = context.clone();
        let next_announced = Arc::clone(&announced);
        let [next_memory, _] = create_memory_tools_with_options(
            Arc::new(move || Some(next_context.clone())),
            MemoryToolsOptions {
                on_commit: Some(Arc::new(move |commit| {
                    next_announced
                        .lock()
                        .expect("announced lock")
                        .push(commit.subject);
                })),
                ..Default::default()
            },
        );
        (next_memory.execute)(call(serde_json::json!({
            "command": "create",
            "reason": "Next write",
            "file_path": "next.md",
            "description": "d"
        })))
        .await
        .expect("next write succeeds after late commit finishes");

        let repo = memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo, "agent")
            .expect("open repo");
        let log = repo.log(None).expect("git log");
        let subjects: Vec<&str> = log.iter().take(3).map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, vec!["Next write", "Held write", "Warm up"]);
        assert_eq!(
            *announced.lock().expect("announced lock"),
            vec!["Next write".to_owned()]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_mid_commit_reports_timeout_and_recovers_on_next_write() {
        let root = tempfile::tempdir().expect("tempdir");
        let context = bound_context(root.path());
        let held = hold_next_commit(&context).await;
        let timed_context = context.clone();
        let [memory, _] = create_memory_tools_with_options(
            Arc::new(move || Some(timed_context.clone())),
            MemoryToolsOptions {
                call_timeout_ms: Some(1_000),
                ..Default::default()
            },
        );
        let unbounded_context = context.clone();
        let [unbounded, _] = create_memory_tools(Arc::new(move || Some(unbounded_context.clone())));

        let started_path = held.started.clone();
        let wait_started =
            tokio::task::spawn_blocking(move || std::fs::read_to_string(started_path));

        let err = (memory.execute)(call(serde_json::json!({
            "command": "create",
            "reason": "Held write",
            "file_path": "held.md",
            "description": "d"
        })))
        .await
        .expect_err("held commit must hit timeout");

        let msg = err.to_string();
        assert!(
            msg.starts_with("memory: create did not finish within 1s (stopped while "),
            "{msg}"
        );

        let entered = tokio::time::timeout(std::time::Duration::from_secs(30), wait_started)
            .await
            .expect("timed out waiting for pre-commit hook")
            .expect("join started reader")
            .expect("read started fifo");
        assert_eq!(entered, "started");

        let release_path = held.release.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || std::fs::write(release_path, "go\n")),
        )
        .await
        .expect("timed out releasing pre-commit hook")
        .expect("join release writer")
        .expect("write release fifo");

        (unbounded.execute)(call(serde_json::json!({
            "command": "create",
            "reason": "Next write",
            "file_path": "next.md",
            "description": "d"
        })))
        .await
        .expect("next write succeeds after timed-out commit finishes");
    }
}
