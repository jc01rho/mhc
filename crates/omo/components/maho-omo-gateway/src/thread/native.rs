//! Native registration of the thread component in an mhc session: every thread tool over this device's session hosts,
//! the operator alerts, and the receiver half that lets another session's delivery reach this one.
//!
//! Upstream counterpart: `thread/component.ts` `createThreadComponent`. The engine side it reads off `pi` (the wire to
//! the session hosts and the session's own `pi.session`) is what the host binds as [`NativeThreadHost`].

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, ExtensionFailure,
    LoadedExtension, NotificationType,
};

use super::adapter::{
    BoxFuture, GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort,
};
use super::address_book::to_gateway_address_entries;
use super::attachment_fence::RpcTransport;
use super::component::{
    GatewayThreadComponent, GatewayThreadComponentOptions, create_gateway_thread_component,
    event_message,
};
use super::control::locate::{ThreadCall, book_of, send_address_book};
use super::engine::{GatewayResolution, GatewayResolve, resolve_from_entries};
use super::host_view::ThreadHost;
use super::live_surface::{LiveThreadSurface, LiveThreadSurfaceOptions};
use super::registration::{
    ControlEndpoint, SessionActivity, SessionControlActionsPort, register_control_endpoint,
    session_runtime_port, waits_for_answer,
};
use super::store::{GatewayStoreOptions, create_gateway_store};
use super::store_api::GatewayStoreApi;
use super::types::{AdmissionLedger, Provenance};
use super::wake_wiring::{WakeStartAtSessionArgs, start_wake_at_session_wired};

/// How a thread tool reaches this device's session hosts: the shared host's socket, which `thread_create` opens on
/// (senpi `resolveThreadSocket`), and the authenticated wire every request travels.
#[derive(Clone)]
pub struct ThreadHostWire {
    pub socket: String,
    pub transport: Arc<dyn RpcTransport>,
}

/// The session's `pi.session`, read when the session starts; `None` while no session is bound.
pub type SessionControlProvider =
    Arc<dyn Fn() -> Option<Arc<dyn SessionControlActionsPort>> + Send + Sync>;

/// What the engine side binds. With no wire every session-control tool answers `host_unavailable`; with no session
/// control the session takes no delivery, so a send to it waits queued.
#[derive(Clone, Default)]
pub struct NativeThreadHost {
    pub wire: Option<ThreadHostWire>,
    pub session_control: Option<SessionControlProvider>,
}

#[derive(Clone, Default)]
pub struct NativeThreadComponent {
    pub host: NativeThreadHost,
}

/// The session that started last: its component, and its control endpoint once registered.
#[derive(Default)]
struct Active {
    component: Option<Arc<GatewayThreadComponent>>,
    endpoint: Option<ControlEndpoint>,
}

type ActiveSlot = Arc<Mutex<Active>>;

fn current(active: &ActiveSlot) -> Option<Arc<GatewayThreadComponent>> {
    active
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .component
        .clone()
}

async fn release(active: Active) {
    let (Some(component), Some(endpoint)) = (active.component, active.endpoint) else {
        return;
    };
    if let Err(error) = endpoint.release(&component).await {
        maho_ext_api::diagnostic(&format!(
            "thread gateway: control endpoint teardown failed: {error}"
        ));
    }
}

/// The receiver half of a session whose host binds no `pi.session`: it admits nothing.
fn unconnected_runtime() -> SessionRuntimePort {
    SessionRuntimePort {
        phase: Arc::new(|| RuntimePhase::Idle),
        admission_gate: Arc::new(|| SessionAdmissionGate {
            can_admit: false,
            hold_reason: Some("native host admission is not connected".into()),
            editor_revision: 0,
            turn_epoch: 0,
        }),
        admit_external_message: Arc::new(|_| Err("native host admission is not connected".into())),
        list_admitted_deliveries: Arc::new(AdmissionLedger::default),
    }
}

fn unconnected_resolve() -> GatewayResolve {
    Arc::new(|_, _| {
        Box::pin(async {
            GatewayResolution::Error(super::errors::thread_tool_failure(
                "host_unavailable",
                "Native thread addressing is not connected",
                "Use a host with thread addressing",
                None,
            ))
        })
    })
}

/// The engine's resolve over this device's address book (upstream `createGatewayServices`): the send path's view, in
/// which nothing live is no failure, and the session files on disk the address names.
fn address_book_resolve(
    host: Arc<dyn ThreadHost>,
    store: Arc<dyn GatewayStoreApi>,
    caller_id: String,
    caller_workspace_root: String,
    sessions_directory: PathBuf,
) -> GatewayResolve {
    Arc::new(move |address: &str, all_scope: bool| {
        let host = host.clone();
        let now = super::store_ops::rfc3339(store.now());
        let caller_id = caller_id.clone();
        let root = caller_workspace_root.clone();
        let sessions_directory = sessions_directory.clone();
        let address = address.to_owned();
        Box::pin(async move {
            let view = host.list_view().await.unwrap_or_default();
            let book = book_of(&view, &[], &now);
            let call = ThreadCall {
                host,
                view,
                book,
                caller_id,
                caller_workspace_root: root.clone(),
                sessions_directory: Some(sessions_directory),
                remote: None,
                now,
            };
            let entries =
                to_gateway_address_entries(&send_address_book(&call, &address, all_scope));
            let resolve = resolve_from_entries(
                Arc::new(move || entries.clone()),
                Arc::new(move || root.clone()),
            );
            resolve(address.as_str(), all_scope).await
        }) as BoxFuture<GatewayResolution>
    })
}

impl Extension for NativeThreadComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let active: ActiveSlot = Arc::default();
        let questions: Arc<Mutex<BTreeSet<String>>> = Arc::default();
        let runtime = api.runtime.clone();
        let host = self.host.clone();
        let start_active = active.clone();
        let start_questions = questions.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let runtime = runtime.clone();
                let host = host.clone();
                let active = start_active.clone();
                let questions = start_questions.clone();
                Box::pin(async move {
                    // The process's wake starts at the session start (upstream `pi.on("session_start", () =>
                    // startWake(eventCtx))`), before the component's own store: one carrier per agent directory, and
                    // with it the delivery consumer. Every other session of the process joins the same wake; a mode
                    // other than `tui`/`rpc`, CI and a task child start none.
                    let _ = start_wake_at_session_wired(WakeStartAtSessionArgs {
                        mode: ctx.mode,
                        agent_dir: ctx.agent_dir.clone(),
                        sessions_directory: ctx.agent_dir.join("sessions"),
                        cwd: ctx.cwd.clone(),
                        env: maho_core::config::current_env().into_iter().collect(),
                        engine_version: Some(
                            maho_core::engine_build_identity::ENGINE_VERSION.to_owned(),
                        ),
                        warn: Arc::new(maho_ext_api::diagnostic),
                    });
                    let store = create_gateway_store(GatewayStoreOptions::new(
                        ctx.agent_dir.to_string_lossy(),
                    ))
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                    let caller = ctx.session_manager.session_id().to_owned();
                    let session_path = ctx
                        .session_manager
                        .session_file()
                        .map(|path| path.to_string_lossy().into_owned());
                    let project = ctx.cwd.to_string_lossy().into_owned();
                    let workspace_root = project.clone();
                    let sessions_directory = ctx.agent_dir.join("sessions");
                    let (thread_host, endpoints, resolve) = match &host.wire {
                        Some(wire) => {
                            let surface =
                                Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
                                    socket: wire.socket.clone(),
                                    transport: wire.transport.clone(),
                                    status_all: None,
                                    now: None,
                                }));
                            let thread_host: Arc<dyn ThreadHost> = surface.clone();
                            let resolve = address_book_resolve(
                                thread_host.clone(),
                                store.clone(),
                                caller.clone(),
                                workspace_root.clone(),
                                sessions_directory.clone(),
                            );
                            (Some(thread_host), surface.gateway(), resolve)
                        }
                        None => (None, GatewayEndpointPort::inert(), unconnected_resolve()),
                    };
                    let control = host
                        .session_control
                        .as_ref()
                        .and_then(|provider| provider());
                    let runtime_port = match &control {
                        Some(control) => session_runtime_port(
                            control.clone(),
                            SessionActivity {
                                is_idle: ctx.is_idle_fn.clone(),
                                is_compacting: ctx.is_compacting_fn.clone(),
                                open_questions: questions,
                            },
                        ),
                        None => unconnected_runtime(),
                    };
                    let manager = ctx.session_manager.clone();
                    let ui = ctx.ui.clone();
                    let caller_id = caller.clone();
                    let component = Arc::new(create_gateway_thread_component(Arc::new(
                        GatewayThreadComponentOptions {
                            store,
                            runtime: runtime_port,
                            durable_id: caller.clone(),
                            session_path: Arc::new(move || session_path.clone()),
                            caller_session_id: Arc::new(move || caller_id.clone()),
                            caller_name: Some(Arc::new(move || manager.get_session_name())),
                            caller_provenance: Arc::new(move || Provenance {
                                workspace: "unclassified".into(),
                                project: project.clone(),
                                lane: None,
                                session: None,
                            }),
                            resolve,
                            endpoints,
                            notify: Some(Arc::new(move |text: &str| {
                                ui.notify(text, NotificationType::Info)
                            })),
                            log: Some(Arc::new(maho_ext_api::diagnostic)),
                            host: thread_host,
                            caller_workspace_root: Arc::new(move || workspace_root.clone()),
                            sessions_directory: Some(sessions_directory),
                            // A device whose `relayd.json` peer carries an `agentd` object reaches other devices over
                            // agentd (listing, locate, open/fork, follow, delivery); with none it stays as it was.
                            remote: super::remote::agentd::locator_for_process(),
                            caller_reply_targets: Arc::new(Vec::new),
                            caller_reply_senders: Arc::new(Vec::new),
                        },
                    )));
                    {
                        let mut session_api = ExtensionApi::new(
                            LoadedExtension::new("thread", ctx.cwd.clone(), Default::default()),
                            Default::default(),
                            Default::default(),
                            runtime,
                        );
                        // With a device service on this device, thread_send reaches <device>/<session>.
                        let tools = if super::remote::device_service_available() {
                            super::remote::with_remote_thread_tools(
                                component.tools.clone(),
                                Arc::new(super::remote::DeviceServicePort::for_process()),
                            )
                        } else {
                            component.tools.clone()
                        };
                        component.register_tools(&mut session_api, tools, caller)?;
                    }
                    let previous = std::mem::replace(
                        &mut *active.lock().unwrap_or_else(PoisonError::into_inner),
                        Active {
                            component: Some(component.clone()),
                            endpoint: None,
                        },
                    );
                    release(previous).await;
                    if let Some(control) = control {
                        let agent_dir = ctx.agent_dir.clone();
                        // Off the session_start path, as upstream registers: the engine may run a pass while it
                        // registers, and that pass must not wait on this handler.
                        let registering = async move {
                            let Some(endpoint) = register_control_endpoint(
                                control,
                                component.clone(),
                                &agent_dir,
                                Arc::new(maho_ext_api::diagnostic),
                            )
                            .await
                            else {
                                return;
                            };
                            let stale = {
                                let mut slot =
                                    active.lock().unwrap_or_else(PoisonError::into_inner);
                                if slot
                                    .component
                                    .as_ref()
                                    .is_some_and(|started| Arc::ptr_eq(started, &component))
                                {
                                    slot.endpoint = Some(endpoint);
                                    None
                                } else {
                                    Some(endpoint)
                                }
                            };
                            if let Some(endpoint) = stale {
                                release(Active {
                                    component: Some(component),
                                    endpoint: Some(endpoint),
                                })
                                .await;
                            }
                        };
                        match tokio::runtime::Handle::try_current() {
                            Ok(handle) => {
                                handle.spawn(registering);
                            }
                            Err(_) => registering.await,
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let agent_start = active.clone();
        api.on(
            EventKind::AgentStart,
            Arc::new(move |_, _| {
                if let Some(component) = current(&agent_start) {
                    component.note_agent_start();
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let message_start = active.clone();
        api.on(
            EventKind::MessageStart,
            Arc::new(move |event, _| {
                if let (Some(component), Some(message)) =
                    (current(&message_start), event_message(event))
                {
                    component.note_message_start(&message);
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let message_end = active.clone();
        api.on(
            EventKind::MessageEnd,
            Arc::new(move |event, _| {
                if let (Some(component), Some(message)) =
                    (current(&message_end), event_message(event))
                {
                    component.note_message_end(&message);
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let question_start = questions.clone();
        api.on(
            EventKind::ToolExecutionStart,
            Arc::new(move |event, _| {
                if let ExtensionEvent::ToolExecutionStart {
                    tool_call_id,
                    tool_name,
                    args,
                    ..
                } = event
                    && waits_for_answer(tool_name, args)
                {
                    question_start
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(tool_call_id.clone());
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let question_end = questions;
        api.on(
            EventKind::ToolExecutionEnd,
            Arc::new(move |event, _| {
                if let ExtensionEvent::ToolExecutionEnd { tool_call_id, .. } = event {
                    question_end
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(tool_call_id.as_str());
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let end_active = active.clone();
        api.on(
            EventKind::AgentEnd,
            Arc::new(move |event, ctx| {
                let component = current(&end_active);
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
                        && let (Some(component), Some(facts)) = (component, facts)
                    {
                        component.alerts.agent_end(&durable_id, &facts);
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let settled_active = active.clone();
        api.on(
            EventKind::AgentSettled,
            Arc::new(move |_, ctx| {
                let component = current(&settled_active);
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if let Some(component) = component {
                        component.note_settled();
                        if !durable_id.is_empty() {
                            component.alerts.settled(&durable_id);
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let shutdown_active = active;
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let ended = std::mem::take(
                    &mut *shutdown_active
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner),
                );
                let durable_id = ctx.session_manager.session_id().to_owned();
                Box::pin(async move {
                    if let Some(component) = &ended.component
                        && !durable_id.is_empty()
                    {
                        component.alerts.ended(&durable_id);
                    }
                    release(ended).await;
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
