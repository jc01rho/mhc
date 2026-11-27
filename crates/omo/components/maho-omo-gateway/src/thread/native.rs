//! Native registration of the thread surface a host without session admission can serve: the operator alerts and,
//! with a device service, `thread_send` to another device.
//! Cross-session admission requires a host adapter; it is not simulated here.
use super::adapter::{GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort};
use super::component::{GatewayThreadComponentOptions, create_gateway_thread_component};
use super::engine::GatewayResolution;
use super::store::{GatewayStoreOptions, create_gateway_store};
use super::types::{AdmissionLedger, Provenance};
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ToolDefinition, ToolResult,
};
use std::sync::{Arc, Mutex};

type ActiveAlerts = Arc<Mutex<Option<Arc<super::run_alerts::RunAlerts>>>>;

pub struct NativeThreadComponent;

impl Extension for NativeThreadComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let runtime = api.runtime.clone();
        let active_alerts: ActiveAlerts = Arc::default();
        let start_alerts = active_alerts.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let runtime = runtime.clone();
                let start_alerts = start_alerts.clone();
                Box::pin(async move {
                    let store = create_gateway_store(GatewayStoreOptions::new(
                        ctx.agent_dir.to_string_lossy(),
                    ))
                    .map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                    let caller = ctx.session_manager.session_id().to_owned();
                    let project = ctx.cwd.to_string_lossy().into_owned();
                    let workspace_root = project.clone();
                    let component =
                        create_gateway_thread_component(Arc::new(GatewayThreadComponentOptions {
                            store,
                            runtime: SessionRuntimePort {
                                phase: Arc::new(|| RuntimePhase::Idle),
                                admission_gate: Arc::new(|| SessionAdmissionGate {
                                    can_admit: false,
                                    hold_reason: Some(
                                        "native host admission is not connected".into(),
                                    ),
                                    editor_revision: 0,
                                    turn_epoch: 0,
                                }),
                                admit_external_message: Arc::new(|_| {
                                    Err("native host admission is not connected".into())
                                }),
                                list_admitted_deliveries: Arc::new(AdmissionLedger::default),
                            },
                            durable_id: caller.clone(),
                            session_path: Arc::new(|| None),
                            caller_session_id: Arc::new(move || caller.clone()),
                            caller_name: None,
                            caller_provenance: Arc::new(move || Provenance {
                                workspace: "unclassified".into(),
                                project: project.clone(),
                                lane: None,
                                session: None,
                            }),
                            resolve: Arc::new(|_, _| {
                                Box::pin(async {
                                    GatewayResolution::Error(super::errors::thread_tool_failure(
                                        "host_unavailable",
                                        "Native thread addressing is not connected",
                                        "Use a host with thread addressing",
                                        None,
                                    ))
                                })
                            }),
                            endpoints: GatewayEndpointPort::inert(),
                            notify: None,
                            log: Some(Arc::new(maho_ext_api::diagnostic)),
                            host: None,
                            caller_workspace_root: Arc::new(move || workspace_root.clone()),
                            sessions_directory: Some(ctx.agent_dir.join("sessions")),
                            // A device whose `relayd.json` peer carries an `agentd` object reaches other devices over
                            // agentd (listing, locate, open/fork, follow, delivery); with none it stays as it was.
                            remote: super::remote::agentd::locator_for_process(),
                            caller_reply_targets: Arc::new(Vec::new),
                            caller_reply_senders: Arc::new(Vec::new),
                        }));
                    *start_alerts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(component.alerts.clone());
                    let mut session_api = ExtensionApi::new(
                        maho_ext_api::LoadedExtension::new(
                            "thread",
                            ctx.cwd.clone(),
                            Default::default(),
                        ),
                        Default::default(),
                        Default::default(),
                        runtime,
                    );
                    // With a device service on this device, thread_send reaches <device>/<session>.
                    let reach_devices = super::remote::device_service_available();
                    let tools = if reach_devices {
                        super::remote::with_remote_thread_tools(
                            component.tools,
                            Arc::new(super::remote::DeviceServicePort::for_process()),
                        )
                    } else {
                        component.tools
                    };
                    for tool in tools
                        .into_iter()
                        .filter(|tool| reach_devices && tool.name == "thread_send")
                    {
                        let execute = tool.execute;
                        session_api.try_register_tool(ToolDefinition::new(
                            &tool.name,
                            &tool.description,
                            tool.parameters,
                            Arc::new(move |call| {
                                let execute = execute.clone();
                                Box::pin(async move {
                                    let result = execute(call.id, call.params).await;
                                    let mut output = ToolResult::text(result.to_string());
                                    output.details = Some(serde_json::json!({"result": result}));
                                    Ok(output)
                                })
                            }),
                        ))?;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let end_alerts = active_alerts.clone();
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let alerts = end_alerts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
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
                        && let (Some(alerts), Some(facts)) = (alerts, facts)
                    {
                        alerts.agent_end(&durable_id, &facts);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let settled_alerts = active_alerts.clone();
        api.on(
            EventKind::AgentSettled,
            Arc::new(move |_, ctx| {
                let alerts = settled_alerts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if !durable_id.is_empty()
                        && let Some(alerts) = alerts
                    {
                        alerts.settled(&durable_id);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let shutdown_alerts = active_alerts;
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let alerts = shutdown_alerts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if !durable_id.is_empty()
                        && let Some(alerts) = alerts
                    {
                        alerts.ended(&durable_id);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
