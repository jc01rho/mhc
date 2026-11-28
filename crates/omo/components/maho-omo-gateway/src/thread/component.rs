//! The thread component: it wires the store, engine, drain and tools of one session.
//!
//! Upstream counterpart: `thread/component.ts`.

pub type NotifyCallback = Option<Arc<dyn Fn(&str) + Send + Sync>>;
use std::sync::Arc;

use serde_json::Value;

use super::adapter::SessionRuntimePort;
use super::drain::{InboxDrain, InboxDrainOptions};
use super::engine::{GatewayEngine, GatewayEngineOptions, GatewayResolve};
use super::store_api::GatewayStoreApi;
use super::tools::{
    ThreadSendAs, ThreadTool, ThreadToolsOptions, build_extension_tools, create_thread_send_as,
    create_thread_tools,
};

pub struct GatewayThreadComponentOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub runtime: SessionRuntimePort,
    pub durable_id: String,
    pub session_path: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    pub caller_provenance: Arc<dyn Fn() -> super::types::Provenance + Send + Sync>,
    pub resolve: GatewayResolve,
    pub endpoints: super::adapter::GatewayEndpointPort,
    pub notify: NotifyCallback,
    pub log: NotifyCallback,
    /// The host surface the session-control tools read; `None` answers them `host_unavailable`.
    pub host: Option<Arc<dyn super::host_view::ThreadHost>>,
    pub caller_workspace_root: Arc<dyn Fn() -> String + Send + Sync>,
    pub sessions_directory: Option<std::path::PathBuf>,
    pub remote: Option<Arc<dyn super::control::locate::RemoteControlLocator>>,
    /// The agent sessions whose deliveries the caller's current run took in (#12886); empty when none was consumed.
    pub caller_reply_targets: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// The senders of those deliveries, as each delivery stated them (decision 0027 section 6).
    pub caller_reply_senders:
        Arc<dyn Fn() -> Vec<super::reply_permission::ReplySender> + Send + Sync>,
}

/// One session's gateway: its drain and the tools its model calls.
#[derive(Clone)]
pub struct GatewayThreadComponent {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub drain: Arc<InboxDrain>,
    pub tools: Vec<ThreadTool>,
    pub send_as: ThreadSendAs,
    pub alerts: Arc<super::run_alerts::RunAlerts>,
    pub durable_id: String,
    /// The agentd-backed remote surface a monitor source can be registered for.
    pub(crate) remote: Option<Arc<dyn super::control::locate::RemoteControlLocator>>,
    /// The current run's reply allowance, driven by `agent_start`, `message_start`, `message_end` and `agent_settled`.
    run: Arc<std::sync::Mutex<super::run_context::RunContext>>,
    registration: Arc<std::sync::Mutex<super::tools::ExtensionToolRegistration>>,
    log: NotifyCallback,
}

pub fn create_gateway_thread_component(
    options: Arc<GatewayThreadComponentOptions>,
) -> GatewayThreadComponent {
    create_gateway_thread_component_with_alerts(options, None)
}

pub fn create_gateway_thread_component_with_alerts(
    options: Arc<GatewayThreadComponentOptions>,
    operator_alerts: Option<super::run_alerts::OperatorAlertsTestSeams>,
) -> GatewayThreadComponent {
    let run = Arc::new(std::sync::Mutex::new(
        super::run_context::RunContext::default(),
    ));
    let engine = GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(options.caller_provenance.clone()),
        store: options.store.clone(),
        endpoints: options.endpoints.clone(),
        resolve: options.resolve.clone(),
        now: None,
    });
    let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
        store: options.store.clone(),
        runtime: options.runtime.clone(),
        durable_id: options.durable_id.clone(),
        session_path: options.session_path.clone(),
        now: None,
        log: options.log.clone(),
        notify: options.notify.clone(),
        on_admitted: None,
        admit_intake: None,
        wake_gated: None,
    }));
    let tool_options = Arc::new(ThreadToolsOptions {
        store: options.store.clone(),
        engine: engine.clone(),
        caller_session_id: options.caller_session_id.clone(),
        caller_name: options.caller_name.clone(),
        caller_provenance: options.caller_provenance.clone(),
        host: options.host.clone(),
        caller_workspace_root: options.caller_workspace_root.clone(),
        sessions_directory: options.sessions_directory.clone(),
        remote: options.remote.clone(),
        caller_reply_targets: merged_reply_targets(
            options.caller_reply_targets.clone(),
            run.clone(),
        ),
        caller_reply_senders: merged_reply_senders(
            options.caller_reply_senders.clone(),
            run.clone(),
        ),
    });
    let tools = create_thread_tools(tool_options.clone());
    let send_as = create_thread_send_as(tool_options);
    let seams = operator_alerts.unwrap_or_default();
    let alert_env = seams.env.unwrap_or_else(omo_config_core::process_env);
    let settings_env = alert_env.clone();
    let settings: super::run_alerts::SettingsProvider =
        Arc::new(move || super::run_alerts::load_user_error_streak_settings(&settings_env));
    let store_for_clock = options.store.clone();
    let now = seams
        .now
        .unwrap_or_else(|| Arc::new(move || store_for_clock.now()));
    let caller_name = options.caller_name.clone();
    let session_name: super::run_alerts::SessionNameProvider =
        Arc::new(move || caller_name.as_ref().and_then(|name| name()));
    let device = seams
        .device
        .unwrap_or_else(|| Arc::new(|| Box::pin(async { None })));
    let default_log = options.log.clone();
    let default_warn = options.log.clone();
    let log = seams
        .log
        .unwrap_or_else(|| super::run_alerts::RunAlertsLogger {
            info: Arc::new(move |line| {
                if let Some(cb) = &default_log {
                    cb(line);
                }
            }),
            warn: Arc::new(move |line| {
                if let Some(cb) = &default_warn {
                    cb(line);
                } else {
                    maho_ext_api::diagnostic(line);
                }
            }),
        });
    let alerts = super::run_alerts::create_run_alerts(super::run_alerts::RunAlertsOptions {
        settings,
        env: alert_env,
        now,
        send_as: send_as.clone(),
        session_name,
        device,
        fetch: seams.fetch,
        delay: seams.delay,
        log,
    });
    if let Some(log) = options.log.clone() {
        options.store.on_after_commit_error(Arc::new(move |extension, error| log(&format!("thread gateway: extension {extension} committed, but an after-commit effect (its wake hint) failed: {error}"))));
    }
    GatewayThreadComponent {
        registration: Default::default(),
        remote: options.remote.clone(),
        log: options.log.clone(),
        store: options.store.clone(),
        engine,
        drain,
        tools,
        send_as,
        alerts,
        durable_id: options.durable_id.clone(),
        run,
    }
}

/// The tool options' reply targets, unioned with the current run's consumed senders (deduplicated by id).
fn merged_reply_targets(
    host: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    run: Arc<std::sync::Mutex<super::run_context::RunContext>>,
) -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
    Arc::new(move || {
        let mut targets = host();
        if let Ok(run) = run.lock() {
            for sender in &run.reply_to {
                if !targets.iter().any(|target| target == sender) {
                    targets.push(sender.clone());
                }
            }
        }
        targets
    })
}

/// The tool options' reply senders, unioned with the current run's consumed senders (deduplicated).
fn merged_reply_senders(
    host: Arc<dyn Fn() -> Vec<super::reply_permission::ReplySender> + Send + Sync>,
    run: Arc<std::sync::Mutex<super::run_context::RunContext>>,
) -> Arc<dyn Fn() -> Vec<super::reply_permission::ReplySender> + Send + Sync> {
    Arc::new(move || {
        let mut senders = host();
        if let Ok(run) = run.lock() {
            for sender in &run.reply_senders {
                if !senders.iter().any(|existing| {
                    existing.session_id == sender.session_id
                        && existing.device == sender.device
                        && existing.name == sender.name
                }) {
                    senders.push(sender.clone());
                }
            }
        }
        senders
    })
}

impl maho_ext_api::Extension for GatewayThreadComponent {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        Arc::new(self.clone()).register_session_tools(api);
    }
}

/// The message an engine event carries, as JSON, for the run's reply allowance.
pub(super) fn event_message(event: &mut maho_ext_api::ExtensionEvent) -> Option<Value> {
    match event {
        maho_ext_api::ExtensionEvent::MessageStart { message }
        | maho_ext_api::ExtensionEvent::MessageEnd { message } => {
            serde_json::to_value(message).ok()
        }
        _ => None,
    }
}

/// A thread tool as the extension API registers it: its result is the tool's text, and whole in `details.result`.
fn tool_definition(tool: ThreadTool) -> maho_ext_api::ToolDefinition {
    let execute = tool.execute;
    maho_ext_api::ToolDefinition::new(
        &tool.name,
        &tool.description,
        tool.parameters,
        Arc::new(move |call| {
            let execute = execute.clone();
            Box::pin(async move {
                let result = execute(call.id, call.params).await;
                let mut output = maho_ext_api::ToolResult::text(result.to_string());
                output.details = Some(serde_json::json!({"result": result}));
                Ok(output)
            })
        }),
    )
}

impl GatewayThreadComponent {
    /// A run started (`agent_start`).
    pub fn note_agent_start(&self) {
        if let Ok(mut state) = self.run.lock() {
            state.turn += 1;
        }
    }

    /// A message started (`message_start`): a consumed delivery joins the run's reply allowance.
    pub fn note_message_start(&self, message: &Value) {
        if let Ok(mut state) = self.run.lock() {
            state.note_input(message);
        }
    }

    /// A message ended (`message_end`).
    pub fn note_message_end(&self, message: &Value) {
        if let Ok(mut state) = self.run.lock() {
            state.note_finished(message);
        }
    }

    /// The run settled (`agent_settled`). A settle alone keeps the consumed delivery's senders, hop and cause (#13042):
    /// the next input decides whether the work continues.
    pub fn note_settled(&self) {
        if let Ok(mut state) = self.run.lock() {
            state.answered = true;
        }
    }

    /// Registers `tools` (the thread tools, as the host decorated them) and the extension tools the store declares for
    /// `caller` (upstream `registerThreadTools` with `registerExtensionTools`). A declared tool whose name the session
    /// already has is reported, never registered.
    pub fn register_tools(
        &self,
        api: &mut maho_ext_api::ExtensionApi,
        tools: Vec<ThreadTool>,
        caller: String,
    ) -> Result<(), maho_ext_api::ExtensionFailure> {
        if let Some(remote) = self.remote.clone()
            && let Some(surface) = remote.remote_surface_arc()
        {
            let register = maho_ext_api::register_rpc_remote_source;
            let source = maho_ext_api::RpcRemoteSource {
                open: Arc::new(move |target, options, handlers| {
                    let surface = surface.clone();
                    Box::pin(async move {
                        super::remote::monitor_source::create_remote_monitor_source(surface)
                            .open(target, options, handlers)
                            .await
                    })
                }),
            };
            register(source);
        }
        let mut present = api
            .get_all_tools()?
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        present.extend(tools.iter().map(|tool| tool.name.clone()));
        let admitted = self.extension_tools(Arc::new(move || caller.clone()), &present);
        for tool in tools.into_iter().chain(admitted) {
            api.try_register_tool(tool_definition(tool))?;
        }
        Ok(())
    }

    /// The extension tools the store declares for `caller` whose names no tool in `present` has.
    fn extension_tools(
        &self,
        caller: Arc<dyn Fn() -> String + Send + Sync>,
        present: &[String],
    ) -> Vec<ThreadTool> {
        let resolve = self.engine.options().resolve.clone();
        let resolver: super::tools::CallerResolver = Arc::new(move |runtime_id| {
            let resolve = resolve.clone();
            Box::pin(
                async move { super::tools::resolve_caller_durable_id(&resolve, &runtime_id).await },
            )
        });
        let declared = build_extension_tools(self.store.clone(), caller, resolver);
        let log = self.log.clone();
        self.registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .session_start(declared, present, &|line| {
                if let Some(log) = &log {
                    log(line);
                } else {
                    maho_ext_api::diagnostic(line);
                }
            })
    }

    /// The session's own extension tools, on top of the thread tools.
    pub fn tools_with_extensions(
        &self,
        caller: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Vec<ThreadTool> {
        let mut tools = self.tools.clone();
        let present = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        tools.extend(self.extension_tools(caller, &present));
        tools
    }

    /// Session-start registration reads declarations only; extension modules load on execution.
    pub fn register_session_tools(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi) {
        use maho_ext_api::*;
        let component = Arc::clone(self);
        let runtime = api.runtime.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let component = Arc::clone(&component);
                let runtime = runtime.clone();
                Box::pin(async move {
                    let mut session_api = ExtensionApi::new(
                        LoadedExtension::new(
                            "thread-session-tools",
                            ctx.cwd.clone(),
                            SourceInfo::default(),
                        ),
                        ExtensionSessionProfile::default(),
                        EventBus::default(),
                        runtime,
                    );
                    component.register_tools(
                        &mut session_api,
                        Vec::new(),
                        ctx.session_manager.session_id().to_owned(),
                    )?;
                    Ok(EventResult::None)
                })
            }),
        );
        let agent_start = Arc::clone(self);
        api.on(
            EventKind::AgentStart,
            Arc::new(move |_, _ctx| {
                let component = agent_start.clone();
                Box::pin(async move {
                    component.note_agent_start();
                    Ok(EventResult::None)
                })
            }),
        );
        let message_start = Arc::clone(self);
        api.on(
            EventKind::MessageStart,
            Arc::new(move |event, _ctx| {
                let component = message_start.clone();
                let message = event_message(event);
                Box::pin(async move {
                    if let Some(message) = message {
                        component.note_message_start(&message);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let message_end = Arc::clone(self);
        api.on(
            EventKind::MessageEnd,
            Arc::new(move |event, _ctx| {
                let component = message_end.clone();
                let message = event_message(event);
                Box::pin(async move {
                    if let Some(message) = message {
                        component.note_message_end(&message);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let settled = Arc::clone(self);
        api.on(
            EventKind::AgentSettled,
            Arc::new(move |_, _ctx| {
                let component = settled.clone();
                Box::pin(async move {
                    component.note_settled();
                    Ok(EventResult::None)
                })
            }),
        );
        let alerts_end = self.alerts.clone();
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let alerts = alerts_end.clone();
                let durable_id = ctx.session_manager.session_id().to_owned();
                let facts = match event {
                    ExtensionEvent::AgentEnd {
                        messages, aborted, ..
                    } => Some(super::run_alerts::agent_end_facts_from_messages(
                        *aborted, messages,
                    )),
                    _ => None,
                };
                Box::pin(async move {
                    if !durable_id.is_empty()
                        && let Some(facts) = facts
                    {
                        alerts.agent_end(&durable_id, &facts);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let alerts_settled = self.alerts.clone();
        api.on(
            EventKind::AgentSettled,
            Arc::new(move |_, ctx| {
                let alerts = alerts_settled.clone();
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if !durable_id.is_empty() {
                        alerts.settled(&durable_id);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let alerts_shutdown = self.alerts.clone();
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let alerts = alerts_shutdown.clone();
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if !durable_id.is_empty() {
                        alerts.ended(&durable_id);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }

    /// Open a native child and record its engine-resolved creator only after success.
    pub async fn thread_create(
        &self,
        surface: &super::live_surface::LiveThreadSurface,
        runtime_id: &str,
        params: Value,
        name: Option<&str>,
    ) -> Result<Value, String> {
        let opened = surface.open_session(params, name).await?;
        let id = opened
            .get("durableSessionId")
            .or_else(|| opened.get("durable_session_id"))
            .and_then(Value::as_str)
            .ok_or_else(|| "opened session has no durable identity".to_owned())?;
        let result = serde_json::json!({"kind":"ok", "thread":{"thread_id":id}, "session":opened});
        super::tools::record_thread_creator(
            self.store.as_ref(),
            &self.engine.options().resolve,
            runtime_id,
            &result,
        )
        .await
        .map_err(|error| error.message())?;
        Ok(result)
    }

    /// Publishes this session's control endpoint and registers the drain's first pass.
    pub fn start(
        &self,
        incarnation: &str,
        endpoint_socket: Option<&str>,
        endpoint_kind: Option<&str>,
    ) -> Result<(), super::store_ops::StoreError> {
        self.store.register_incarnation(
            &self.durable_id,
            incarnation,
            endpoint_socket,
            endpoint_kind,
        )?;
        self.drain.drain(&super::adapter::DrainWakeEvent {
            reason: super::adapter::WakeReason::Start,
            delivery_ids: None,
        })?;
        Ok(())
    }

    /// Retires the drain and clears the published endpoint.
    pub fn stop(&self, incarnation: &str) -> Result<(), super::store_ops::StoreError> {
        self.store.clear_endpoint(&self.durable_id, incarnation)
    }

    /// A tool call by name, as a host dispatches it.
    pub fn execute(
        &self,
        name: &str,
        call_id: &str,
        args: Value,
    ) -> Option<super::adapter::BoxFuture<Value>> {
        self.tools
            .iter()
            .find(|tool| tool.name == name)
            .map(|tool| (tool.execute)(call_id, args))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A component over a throwaway store: its `tools` is the list both registration paths
    /// (`register_session_tools` here and `native.rs`) draw from.
    fn component() -> GatewayThreadComponent {
        let store: Arc<dyn GatewayStoreApi> = super::super::store::create_gateway_store(
            super::super::store::GatewayStoreOptions::new(
                std::env::temp_dir()
                    .join("maho-component-test")
                    .to_string_lossy()
                    .into_owned(),
            ),
        )
        .expect("store");
        let runtime = SessionRuntimePort {
            phase: Arc::new(|| super::super::adapter::RuntimePhase::Idle),
            admission_gate: Arc::new(|| super::super::adapter::SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(|_| Err("not in this test".to_owned())),
            list_admitted_deliveries: Arc::new(super::super::types::AdmissionLedger::default),
        };
        create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
            store,
            runtime,
            durable_id: "receiver".into(),
            session_path: Arc::new(|| None),
            caller_session_id: Arc::new(|| "sender".into()),
            caller_name: None,
            caller_provenance: Arc::new(|| super::super::types::Provenance {
                workspace: "unclassified".into(),
                project: "test".into(),
                lane: None,
                session: None,
            }),
            resolve: Arc::new(|_, _| {
                Box::pin(async {
                    super::super::engine::GatewayResolution::Error(
                        super::super::errors::thread_tool_failure(
                            "not_found",
                            "unused",
                            "none",
                            None,
                        ),
                    )
                })
            }),
            endpoints: super::super::adapter::GatewayEndpointPort::inert(),
            notify: None,
            log: None,
            host: None,
            caller_workspace_root: Arc::new(String::new),
            sessions_directory: None,
            remote: None,
            caller_reply_targets: Arc::new(Vec::new),
            caller_reply_senders: Arc::new(Vec::new),
        }))
    }

    #[test]
    fn the_tool_set_covers_send_and_control_and_no_binding_tool_is_reachable() {
        let component = component();
        let names: Vec<&str> = component
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        for expected in [
            "thread_send",
            "thread_handoff",
            "thread_list",
            "thread_read",
            "thread_interrupt",
            "thread_close",
            "thread_rename",
            "thread_set_model",
            "thread_set_reasoning",
            "thread_archive",
            "thread_create",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        for removed in [
            "thread_report",
            "thread_bind",
            "thread_unbind",
            "thread_rebind",
            "thread_bindings",
            "thread_outbox",
            "thread_outbox_ack",
            "thread_answer",
        ] {
            assert!(!names.contains(&removed), "{removed} is still reachable");
            assert!(component.execute(removed, "call-1", Value::Null).is_none());
        }
        assert!(
            component
                .execute("thread_missing", "call-1", Value::Null)
                .is_none()
        );
    }

    /// The five legacy binding tools are superseded by thread-core's send/control surface: upstream
    /// removed them in `4f6a56bbc0` (ported by `docs/omo-sync/v5.1.84/thread-core.md`) and mhc has no
    /// `omo thread bind` CLI to keep a binding path alive. Neither the name nor a mention of one may
    /// survive in the tool surface, so a stale description cannot advertise a call that no longer runs.
    #[test]
    fn the_legacy_thread_binding_tools_are_absent_from_the_registered_tool_set() {
        let component = component();
        for legacy in [
            "thread_report",
            "thread_bind",
            "thread_bindings",
            "thread_outbox",
            "thread_outbox_ack",
        ] {
            assert!(
                !component.tools.iter().any(|tool| tool.name == legacy),
                "{legacy} is still registered"
            );
            assert!(
                component.execute(legacy, "call-1", Value::Null).is_none(),
                "{legacy} is still executable"
            );
            for tool in &component.tools {
                assert!(
                    !tool.description.contains(legacy),
                    "{} still advertises {legacy}",
                    tool.name
                );
                assert!(
                    !tool.parameters.to_string().contains(legacy),
                    "{} still takes {legacy} in its schema",
                    tool.name
                );
            }
        }
    }
}
