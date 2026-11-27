use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use futures::executor::block_on;
use maho_omo_gateway::thread::adapter::{
    AdmitResult, DrainWakeEvent, EndpointLiveness, ExternalAdmissionKind, GatewayAdmissionKind,
    GatewayEndpointPort, RuntimePhase, SessionAdmissionGate, SessionRuntimePort, WakeReason,
};
use maho_omo_gateway::thread::constants::WAKE_LEASE_MS;
use maho_omo_gateway::thread::drain::{InboxDrain, InboxDrainOptions};
use maho_omo_gateway::thread::engine::{
    GatewayEngine, GatewayEngineOptions, GatewayResolution, GatewayTarget,
};
use maho_omo_gateway::thread::intake_admission::IntakeAdmission;
use maho_omo_gateway::thread::local_transport::{
    LocalTransport, OutboundDelivery, OutboundSender, OutboundTarget,
};
use maho_omo_gateway::thread::result::{result_from_row, result_from_row_with_wait};
use maho_omo_gateway::thread::same_device_wake::{
    SameDeviceCarrier, SameDeviceWake, SameDeviceWakeOptions,
};
use maho_omo_gateway::thread::sdk::{ThreadSdk, ThreadSdkOptions};
use maho_omo_gateway::thread::session_wait_types::{
    QueuedWaitingOn, QueuedWaitingOnKind, SessionWaitKind, SessionWaitWrite,
};
use maho_omo_gateway::thread::store::SqliteGatewayStore;
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::types::{
    AdmissionLedger, DeliveryRow, DeliveryState, DeliveryWait, EnvelopeOrigin,
    GatewayDeliveryDelivery, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance,
    StoredDeliveryEnvelope,
};
use maho_omo_gateway::thread::wake::carrier::MemoryProbe;
use maho_omo_gateway::thread::wake::runtime::{
    OnWake, ProcessSameDeviceWakeInput, RunningThreadWake, ThreadWakeLatch, ThreadWakeOptions,
    ThreadWakeStart, WakeSettings, process_intake_admission, process_same_device_wake,
    start_thread_wake_at_session,
};
use maho_omo_gateway::thread::woken_types::WokenSessionState;
use omo_config_core::ResolvedSessionWakeSettings;
use serde_json::{Value, json};

use crate::wake_support::{
    FakeEngineHost, FakeEngineOptions, ManualSchedule, WakeHarness, finished, host_stamped,
    streaming, write_session_file,
};

const DEV_ENGINE: &str = "2026.11.19-2-1-omob.6d05a3b.1975499+1795063675.1975499";
const WAIT: Duration = Duration::from_secs(10);

fn local_engine(store: Arc<SqliteGatewayStore>) -> GatewayEngine {
    GatewayEngine::new(GatewayEngineOptions {
        sender_provenance: Some(Arc::new(|| Provenance {
            workspace: "w".into(),
            project: "p".into(),
            lane: None,
            session: None,
        })),
        store,
        endpoints: GatewayEndpointPort::inert(),
        resolve: Arc::new(|target: &str, _all_scope: bool| {
            let durable_id = target.to_owned();
            Box::pin(async move {
                GatewayResolution::Ok(GatewayTarget {
                    durable_id,
                    endpoint: None,
                    liveness: EndpointLiveness::Dead,
                })
            })
        }),
        now: None,
    })
}

fn sender_session() -> GatewaySender {
    GatewaySender::Session {
        durable_id: "sender-a".into(),
        name: None,
        turn_id: None,
        cause_delivery_id: None,
    }
}

fn sample_admitted_row(admission_kind: &str, turn_epoch: i64) -> DeliveryRow {
    DeliveryRow {
        delivery_id: "d-1".into(),
        target_durable_id: "t".into(),
        seq: 1,
        sender: "session:sender-a".into(),
        sender_turn: None,
        envelope: StoredDeliveryEnvelope {
            provenance: None,
            origin: EnvelopeOrigin::Session {
                session: "sender-a".into(),
            },
            actor: "sender-a".into(),
            via: vec![],
            root_id: "root-1".into(),
            hop: 1,
            delivery_id: "d-1".into(),
        },
        body: "x".into(),
        bytes: 1,
        mode_requested: GatewayDeliveryMode::Auto,
        mode_effective: None,
        expected_turn_id: None,
        state: DeliveryState::Admitted,
        reason: None,
        admitted_by: None,
        claimed_at: None,
        attempt: 1,
        admission_kind: Some(admission_kind.into()),
        wait_kind: None,
        turn_epoch: Some(turn_epoch),
        root_id: "root-1".into(),
        hop: 1,
        created_at: 1,
        updated_at: 1,
        expires_at: 100,
        ledger_id: None,
        verbatim_text: None,
        verbatim_display_text: None,
        collapse_key: None,
    }
}

#[test]
fn same_device_switch_controls_wake_request_and_offline_reason() {
    for (enabled, with_carrier, header, expect_wake_row, expect_carrier, expect_reason) in [
        (false, true, json!({}), false, false, None),
        (true, true, json!({}), true, true, None),
        (
            true,
            false,
            json!({}),
            true,
            false,
            Some("wake_unavailable"),
        ),
        (
            true,
            true,
            json!({ "spawnedBy": { "parentSessionId": "p", "rootSessionId": "p", "taskId": "x" } }),
            false,
            false,
            None,
        ),
        (true, true, json!({ "kind": "robot" }), false, false, None),
    ] {
        let h = WakeHarness::new();
        write_session_file(&h.sessions(), "t", &header, "--tmp-project--");
        let carried = Arc::new(Mutex::new(Vec::<String>::new()));
        let carrier: Option<SameDeviceCarrier> = with_carrier.then(|| {
            let carried = carried.clone();
            Arc::new(move |id: &str| -> Result<(), String> {
                carried.lock().expect("carried").push(id.to_owned());
                Ok(())
            }) as SameDeviceCarrier
        });
        let wake = Arc::new(SameDeviceWake::new(SameDeviceWakeOptions {
            store: h.store.clone(),
            sessions_directory: h.sessions(),
            enabled: Arc::new(move |_| enabled),
            carrier,
        }));
        let transport = LocalTransport::new(local_engine(h.store.clone()), Some(wake));
        let answer =
            block_on(transport.deliver(OutboundDelivery::local(sender_session(), "t", "hello")));
        match answer {
            GatewayDeliveryResult::Ok { delivery, .. } => {
                if expect_carrier {
                    assert_eq!(
                        delivery,
                        GatewayDeliveryDelivery::Queued {
                            queue_position: 1,
                            waiting_on: Some(DeliveryWait {
                                kind: "wake".into(),
                                device: None,
                            }),
                        }
                    );
                } else {
                    assert_eq!(
                        delivery,
                        GatewayDeliveryDelivery::QueuedOffline {
                            queue_position: 1,
                            reason: expect_reason.map(str::to_owned),
                        }
                    );
                }
            }
            other => panic!("unexpected result: {other:?}"),
        }
        let woken = h.store.woken().list().expect("list");
        if expect_wake_row {
            assert_eq!(woken.len(), 1);
            assert_eq!(woken[0].durable_id, "t");
            assert_eq!(woken[0].state, WokenSessionState::Requested);
        } else {
            assert!(woken.is_empty(), "{woken:?}");
        }
        let expected_carried = if expect_carrier {
            vec!["t".to_owned()]
        } else {
            Vec::new()
        };
        assert_eq!(*carried.lock().expect("carried"), expected_carried);
    }
}

/// omo#14972: a parked session is woken by a send from the CLI or the gateway's seat sends, as by an agent's
/// thread_send; a process with no carrier records the wake for the device's running carrier.
#[test]
fn an_sdk_send_to_a_parked_interactive_session_requests_its_wake() {
    let h = WakeHarness::new();
    write_session_file(&h.sessions(), "t", &json!({}), "--tmp-project--");
    let sdk = ThreadSdk::new(ThreadSdkOptions {
        agent_dir: h.dir.path().to_string_lossy().into_owned(),
        cwd: "/tmp/project".into(),
        env: BTreeMap::new(),
        uid: 501,
        user: "qa-user".into(),
        store: h.store.clone(),
        engine: local_engine(h.store.clone()),
        workspaces: None,
        same_device_wake: Some(SameDeviceWake::new(SameDeviceWakeOptions {
            store: h.store.clone(),
            sessions_directory: h.sessions(),
            enabled: Arc::new(|_| true),
            carrier: None,
        })),
    });
    match block_on(sdk.send("t", "wake up", None)) {
        GatewayDeliveryResult::Ok { delivery, .. } => assert_eq!(
            delivery,
            GatewayDeliveryDelivery::QueuedOffline {
                queue_position: 1,
                reason: Some("wake_unavailable".into()),
            }
        ),
        other => panic!("unexpected result: {other:?}"),
    }
    let woken = h.store.woken().list().expect("list");
    assert_eq!(
        woken
            .iter()
            .map(|row| (row.durable_id.as_str(), row.state))
            .collect::<Vec<_>>(),
        vec![("t", WokenSessionState::Requested)]
    );
}

#[test]
fn local_transport_refuses_remote_targets_person_senders_and_non_numeric_epochs() {
    let h = WakeHarness::new();
    let transport = LocalTransport::new(local_engine(h.store.clone()), None);

    let remote = block_on(transport.deliver(OutboundDelivery {
        target: OutboundTarget::Remote {
            device: "dev-b".into(),
            thread_id: "t".into(),
        },
        ..OutboundDelivery::local(sender_session(), "t", "hi")
    }));
    assert!(matches!(
        remote,
        GatewayDeliveryResult::Error { ref error } if error.code.as_str() == "unsupported"
    ));

    let person = block_on(transport.deliver(OutboundDelivery {
        sender: OutboundSender::Person {
            durable_id: "person".into(),
        },
        ..OutboundDelivery::local(sender_session(), "t", "hi")
    }));
    assert!(matches!(
        person,
        GatewayDeliveryResult::Error { ref error } if error.code.as_str() == "unsupported"
    ));

    let bad_epoch = block_on(transport.deliver(OutboundDelivery {
        expected_turn_id: Some("turn-1".into()),
        ..OutboundDelivery::local(sender_session(), "t", "hi")
    }));
    assert!(matches!(
        bad_epoch,
        GatewayDeliveryResult::Error { ref error } if error.code.as_str() == "invalid_arguments"
    ));
}

#[test]
fn queued_waiting_order_and_held_result_from_row_follow_upstream_precedence() {
    let h = WakeHarness::new();
    assert_eq!(h.store.session_wait().waiting_on("t").expect("none"), None);
    h.store
        .woken()
        .request("t", "/sessions/t.jsonl")
        .expect("request");
    assert_eq!(
        h.store.session_wait().waiting_on("t").expect("wake"),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Wake
        })
    );
    h.store
        .session_wait()
        .write(&SessionWaitWrite {
            durable_id: "t".into(),
            wait: SessionWaitKind::Held,
            hold_kinds: vec!["session_hold".into()],
            file_size: 1,
        })
        .expect("write hold");
    assert_eq!(
        h.store.session_wait().waiting_on("t").expect("hold"),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Hold
        })
    );
    h.store
        .session_wait()
        .write(&SessionWaitWrite {
            durable_id: "t".into(),
            wait: SessionWaitKind::Waiting,
            hold_kinds: vec![],
            file_size: 1,
        })
        .expect("write waiting");
    assert_eq!(
        h.store.session_wait().waiting_on("t").expect("approval"),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Approval
        })
    );

    let started = sample_admitted_row("started", 1);
    let device_hold = DeliveryWait {
        kind: "hold".into(),
        device: Some("dev-here".into()),
    };
    assert!(matches!(
        result_from_row_with_wait(&started, 1, None, false, false, Some(device_hold.clone())),
        GatewayDeliveryResult::Ok {
            delivery: GatewayDeliveryDelivery::Queued {
                waiting_on: Some(ref w),
                ..
            },
            ..
        } if w == &device_hold
    ));

    let held = sample_admitted_row("held", 0);
    assert!(matches!(
        result_from_row(&held, 1, None, false, false),
        GatewayDeliveryResult::Ok {
            delivery: GatewayDeliveryDelivery::Queued {
                queue_position: 1,
                waiting_on: Some(ref w),
            },
            ..
        } if w.kind == "hold"
    ));
}

fn runtime_for_drain(admitted: Arc<Mutex<Vec<String>>>) -> SessionRuntimePort {
    SessionRuntimePort {
        phase: Arc::new(|| RuntimePhase::Idle),
        admission_gate: Arc::new(|| SessionAdmissionGate {
            can_admit: true,
            hold_reason: None,
            editor_revision: 1,
            turn_epoch: 1,
        }),
        admit_external_message: Arc::new(move |input| {
            admitted
                .lock()
                .expect("admitted")
                .push(input.delivery_id.clone());
            Ok(AdmitResult {
                kind: ExternalAdmissionKind::Started,
                turn_epoch: 1,
            })
        }),
        list_admitted_deliveries: Arc::new(AdmissionLedger::default),
    }
}

#[test]
fn drain_intake_admission_admits_and_refuses_by_device_and_caches_per_pass() {
    let h = WakeHarness::new();
    h.intake_row("t", "d-ok-1", "dev-ok");
    h.intake_row("t", "d-ok-2", "dev-ok");
    h.intake_row("t", "d-refused", "dev-refused");
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let admitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let drain = InboxDrain::new(InboxDrainOptions {
        store: h.store.clone(),
        runtime: runtime_for_drain(admitted.clone()),
        durable_id: "t".into(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: Some({
            let asked = asked.clone();
            Arc::new(move |device| {
                asked.lock().expect("asked").push(device.to_owned());
                if device == "dev-ok" {
                    IntakeAdmission::Admitted
                } else {
                    IntakeAdmission::Refused
                }
            })
        }),
        wake_gated: None,
    });
    let pass = drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("drain");
    assert_eq!(
        *asked.lock().expect("asked"),
        vec!["dev-ok".to_owned(), "dev-refused".to_owned()]
    );
    assert_eq!(
        *admitted.lock().expect("admitted"),
        vec![
            "device:dev-ok/d-ok-2".to_owned(),
            "device:dev-ok/d-ok-1".to_owned(),
        ],
        "the row that would start the idle session is admitted after the row that joins its start"
    );
    assert!(
        pass.outcomes.contains(&(
            "d-refused".to_owned(),
            GatewayAdmissionKind::SenderNotAdmitted
        )),
        "{:?}",
        pass.outcomes
    );
    assert_eq!(
        pass.session_control_result()
            .admitted
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.delivery_id)
            .collect::<Vec<_>>(),
        vec!["d-ok-1".to_owned(), "d-ok-2".to_owned()]
    );
    assert_eq!(
        h.store
            .delivery_view("d-refused")
            .expect("view")
            .expect("refused row")
            .row
            .reason
            .as_deref(),
        Some("sender_not_admitted")
    );
}

#[test]
fn drain_intake_admission_skips_unknown_or_unconfigured_devices_without_blocking_local_rows() {
    let h = WakeHarness::new();
    h.intake_row("t", "d-silent", "dev-silent");
    h.queue_row("t", "d-local");
    let admitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let drain_silent = InboxDrain::new(InboxDrainOptions {
        store: h.store.clone(),
        runtime: runtime_for_drain(admitted.clone()),
        durable_id: "t".into(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: Some(Arc::new(|_| IntakeAdmission::Unknown)),
        wake_gated: None,
    });
    drain_silent
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("silent drain");
    assert_eq!(
        *admitted.lock().expect("admitted"),
        vec!["d-local".to_owned()]
    );
    assert_eq!(
        h.store
            .delivery_view("d-silent")
            .expect("view")
            .expect("d-silent")
            .row
            .state,
        DeliveryState::Queued
    );

    let h_none = WakeHarness::new();
    h_none.intake_row("t", "d-intake", "dev-b");
    h_none.queue_row("t", "d-local-2");
    let admitted_none = Arc::new(Mutex::new(Vec::<String>::new()));
    let drain_no_intake = InboxDrain::new(InboxDrainOptions {
        store: h_none.store.clone(),
        runtime: runtime_for_drain(admitted_none.clone()),
        durable_id: "t".into(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: None,
        wake_gated: None,
    });
    drain_no_intake
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("no intake");
    assert_eq!(
        *admitted_none.lock().expect("admitted_none"),
        vec!["d-local-2".to_owned()]
    );
    assert_eq!(
        h_none
            .store
            .delivery_view("d-intake")
            .expect("view")
            .expect("d-intake")
            .row
            .state,
        DeliveryState::Queued
    );
}

#[test]
fn drain_wake_gate_holds_while_opening_lease_is_live_and_admits_after_expiry() {
    let h = WakeHarness::new();
    h.queue_row("t", "d-gated");
    h.store
        .woken()
        .request("t", "/sessions/t.jsonl")
        .expect("request");
    h.store.woken().start("t").expect("start");
    let admitted = Arc::new(Mutex::new(Vec::<String>::new()));
    let store_for_gate = h.store.clone();
    let drain_gated = InboxDrain::new(InboxDrainOptions {
        store: h.store.clone(),
        runtime: runtime_for_drain(admitted.clone()),
        durable_id: "t".into(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: None,
        wake_gated: Some(Arc::new(move || {
            store_for_gate.woken().gated("t").unwrap_or(false)
        })),
    });
    let held = drain_gated
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("gated");
    assert_eq!(
        held.outcomes,
        vec![("d-gated".to_owned(), GatewayAdmissionKind::HeldDraft)]
    );
    assert!(admitted.lock().expect("admitted").is_empty());

    h.advance(WAKE_LEASE_MS + 1);
    let released = drain_gated
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("released");
    assert_eq!(
        released.outcomes,
        vec![("d-gated".to_owned(), GatewayAdmissionKind::Started)]
    );
    assert_eq!(
        *admitted.lock().expect("admitted"),
        vec!["d-gated".to_owned()]
    );
}

struct ParkedResumeFixture {
    h: WakeHarness,
    parked_path: PathBuf,
    fake: FakeEngineHost,
    schedule: Arc<ManualSchedule>,
    running: Arc<RunningThreadWake>,
    transport: LocalTransport,
    drain: Arc<InboxDrain>,
    admitted_ids: Arc<Mutex<Vec<String>>>,
    wake_rx: mpsc::Receiver<String>,
    wake_gated_saw_true: Arc<AtomicBool>,
}

fn setup_parked_resume(parked_id: &str, hand_back_snapshot: Value) -> ParkedResumeFixture {
    let h = WakeHarness::new();
    let parked_path = h.file(parked_id);
    let schedule = ManualSchedule::new();
    let latch: Arc<ThreadWakeLatch> = Arc::default();
    let admitted_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let emitted_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let wake_gated_saw_true = Arc::new(AtomicBool::new(false));
    let store_for_gate = h.store.clone();
    let pid_for_gate = parked_id.to_owned();
    let gate_flag = wake_gated_saw_true.clone();
    let latch_for_intake = latch.clone();
    let agent_dir_for_intake = h.dir.path().to_path_buf();
    let admitted_for_rt = admitted_ids.clone();
    let emitted_for_rt = emitted_ids.clone();
    let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
        store: h.store.clone(),
        runtime: SessionRuntimePort {
            phase: Arc::new(|| RuntimePhase::Idle),
            admission_gate: Arc::new(|| SessionAdmissionGate {
                can_admit: true,
                hold_reason: None,
                editor_revision: 1,
                turn_epoch: 1,
            }),
            admit_external_message: Arc::new(move |input| {
                admitted_for_rt
                    .lock()
                    .expect("admitted")
                    .push(input.delivery_id.clone());
                emitted_for_rt
                    .lock()
                    .expect("emitted")
                    .push(input.delivery_id);
                Ok(AdmitResult {
                    kind: ExternalAdmissionKind::Started,
                    turn_epoch: 1,
                })
            }),
            list_admitted_deliveries: {
                let emitted = emitted_ids.clone();
                Arc::new(move || AdmissionLedger {
                    pending: Vec::new(),
                    emitted: emitted.lock().expect("emitted").clone(),
                })
            },
        },
        durable_id: parked_id.to_owned(),
        session_path: Arc::new(|| None),
        now: None,
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: Some(Arc::new(move |device| {
            process_intake_admission(Some(&latch_for_intake), &agent_dir_for_intake, device)
        })),
        wake_gated: Some(Arc::new(move || {
            let gated = store_for_gate.woken().gated(&pid_for_gate).unwrap_or(false);
            if gated {
                gate_flag.store(true, Ordering::SeqCst);
            }
            gated
        })),
    }));
    let drain_for_host = drain.clone();
    let fake = FakeEngineHost::new(FakeEngineOptions {
        wake_pass: Some(Arc::new(move |_path| {
            let res = drain_for_host
                .drain(&DrainWakeEvent {
                    reason: WakeReason::Inbox,
                    delivery_ids: None,
                })
                .expect("wake pass drain");
            res.admitted
                .into_iter()
                .map(|entry| entry.delivery_id)
                .collect()
        })),
        ..FakeEngineOptions::default()
    });
    fake.set_snapshot(&parked_path, hand_back_snapshot);
    let (wake_tx, wake_rx) = mpsc::channel();
    let on_wake: OnWake = Arc::new(move |durable_id, outcome| {
        let _ = wake_tx.send(format!("{durable_id}:{}", outcome.kind()));
    });
    let no_memory: MemoryProbe = Arc::new(|| None);
    let settings: WakeSettings = Arc::new(|_| ResolvedSessionWakeSettings {
        same_device: true,
        wake_limit: 0,
    });
    let running = start_thread_wake_at_session(
        "tui",
        ThreadWakeStart {
            options: ThreadWakeOptions {
                env: BTreeMap::new(),
                latch: Some(latch.clone()),
                store: Some(h.store.clone()),
                connect: fake.connect(),
                ensure_host: None,
                deliveries: None,
                create_consumer: None,
                engine_version: Some(DEV_ENGINE.into()),
                minimum_engine: None,
                chain_count_limit: None,
                memory_headroom: Some(no_memory),
                schedule: Some(schedule.schedule()),
                on_wake: Some(on_wake),
            },
            agent_dir: h.dir.path().to_path_buf(),
            sessions_directory: h.sessions(),
            settings: settings.clone(),
            engine_version: Some(DEV_ENGINE.into()),
            warn: Arc::new(|_| {}),
        },
    )
    .expect("dev engine stamp starts wake");
    schedule.run_immediate();
    running.wait_ready();

    let same_device = Arc::new(process_same_device_wake(ProcessSameDeviceWakeInput {
        latch: Some(latch),
        store: h.store.clone(),
        agent_dir: h.dir.path().to_path_buf(),
        sessions_directory: h.sessions(),
        settings,
    }));
    let transport = LocalTransport::new(local_engine(h.store.clone()), Some(same_device));
    ParkedResumeFixture {
        h,
        parked_path,
        fake,
        schedule,
        running,
        transport,
        drain,
        admitted_ids,
        wake_rx,
        wake_gated_saw_true,
    }
}

#[test]
fn given_a_parked_session_when_local_send_wakes_it_then_it_answers_queued_waiting_on_wake_drains_as_started_and_resumes_again()
 {
    let parked_id = "0198b2c3-d4e5-7f60-8a1b-2c3d4e5f6a7b";
    let f = setup_parked_resume(parked_id, streaming());

    let first = block_on(f.transport.deliver(OutboundDelivery {
        idempotency_key: Some("k-1".into()),
        ..OutboundDelivery::local(sender_session(), parked_id, "first")
    }));
    let first_id = match first {
        GatewayDeliveryResult::Ok {
            delivery_id,
            delivery:
                GatewayDeliveryDelivery::Queued {
                    waiting_on: Some(wait),
                    ..
                },
            ..
        } => {
            assert_eq!(wait.kind, "wake");
            delivery_id
        }
        other => panic!("expected queued waiting on wake, got {other:?}"),
    };
    assert_eq!(
        f.wake_rx.recv_timeout(WAIT).expect("first wake event"),
        format!("{parked_id}:open")
    );
    assert!(!f.wake_gated_saw_true.load(Ordering::SeqCst));
    assert_eq!(f.fake.frames("open_session").len(), 1);
    assert_eq!(
        f.fake.frames("open_session")[0]["retain_on_disconnect"],
        json!(true)
    );
    assert_eq!(f.fake.frames("wake").len(), 1);
    assert_eq!(
        *f.admitted_ids.lock().expect("admitted"),
        vec![first_id.clone()]
    );

    f.fake.set_snapshot(&f.parked_path, finished());
    f.drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Idle,
            delivery_ids: None,
        })
        .expect("settle applied");
    assert_eq!(
        f.h.store
            .delivery_view(&first_id)
            .expect("view")
            .expect("row")
            .row
            .state,
        DeliveryState::Applied
    );

    assert_eq!(f.schedule.run_queued(), 1);
    assert!(f.h.store.woken().list().expect("list").is_empty());
    assert_eq!(f.fake.frames("release_session").len(), 1);

    let second = block_on(f.transport.deliver(OutboundDelivery {
        idempotency_key: Some("k-2".into()),
        ..OutboundDelivery::local(sender_session(), parked_id, "second")
    }));
    let second_id = match second {
        GatewayDeliveryResult::Ok {
            delivery_id,
            delivery:
                GatewayDeliveryDelivery::Queued {
                    waiting_on: Some(wait),
                    ..
                },
            ..
        } => {
            assert_eq!(wait.kind, "wake");
            delivery_id
        }
        other => panic!("expected queued waiting on wake, got {other:?}"),
    };
    assert_eq!(
        f.wake_rx.recv_timeout(WAIT).expect("second wake event"),
        format!("{parked_id}:open")
    );
    assert_eq!(f.fake.frames("open_session").len(), 2);
    assert_eq!(f.fake.frames("wake").len(), 2);
    assert_eq!(
        *f.admitted_ids.lock().expect("admitted"),
        vec![first_id, second_id]
    );
    f.running.stop();
}

#[test]
fn given_a_session_handed_back_held_unattended_when_a_later_send_arrives_then_it_wakes_and_starts_again()
 {
    let parked_id = "0198b2c3-d4e5-7f60-8a1b-2c3d4e5f6a7c";
    let f = setup_parked_resume(parked_id, streaming());

    let first = block_on(f.transport.deliver(OutboundDelivery {
        idempotency_key: Some("u-1".into()),
        ..OutboundDelivery::local(sender_session(), parked_id, "first")
    }));
    assert!(matches!(
        first,
        GatewayDeliveryResult::Ok {
            delivery: GatewayDeliveryDelivery::Queued {
                waiting_on: Some(ref w),
                ..
            },
            ..
        } if w.kind == "wake"
    ));
    assert_eq!(
        f.wake_rx.recv_timeout(WAIT).expect("first wake"),
        format!("{parked_id}:open")
    );

    f.fake.set_snapshot(&f.parked_path, host_stamped());
    assert_eq!(f.schedule.run_queued(), 1);
    let stored_wait =
        f.h.store
            .session_wait()
            .read(parked_id)
            .expect("read")
            .expect("stored wait");
    assert_eq!(stored_wait.wait, SessionWaitKind::Held);
    assert_eq!(stored_wait.hold_kinds, vec!["unattended".to_owned()]);
    assert!(f.h.store.woken().list().expect("list").is_empty());

    f.fake.set_snapshot(&f.parked_path, finished());
    let second = block_on(f.transport.deliver(OutboundDelivery {
        idempotency_key: Some("u-2".into()),
        ..OutboundDelivery::local(sender_session(), parked_id, "second")
    }));
    let second_id = match second {
        GatewayDeliveryResult::Ok {
            delivery_id,
            delivery:
                GatewayDeliveryDelivery::Queued {
                    waiting_on: Some(wait),
                    ..
                },
            ..
        } => {
            assert_eq!(wait.kind, "wake");
            delivery_id
        }
        other => panic!("expected queued waiting on wake, got {other:?}"),
    };
    assert_eq!(
        f.wake_rx.recv_timeout(WAIT).expect("second wake"),
        format!("{parked_id}:open")
    );
    assert_eq!(f.fake.frames("open_session").len(), 2);
    assert_eq!(f.fake.frames("wake").len(), 2);
    assert_eq!(f.admitted_ids.lock().expect("admitted").len(), 2);

    f.fake.set_snapshot(&f.parked_path, finished());
    f.drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Idle,
            delivery_ids: None,
        })
        .expect("settle applied");
    assert_eq!(
        f.h.store
            .delivery_view(&second_id)
            .expect("view")
            .expect("row")
            .row
            .state,
        DeliveryState::Applied
    );
    f.running.stop();
}
