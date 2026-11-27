//! The admitter decides a claimed row's lane again at its admission, and one drain pass starts one turn that carries
//! every queued delivery (omo v5.1.84 thread-gateway track).

use std::sync::{Arc, Mutex};

use super::super::adapter::{
    AdmitResult, DrainWakeEvent, SessionAdmissionGate, SessionRuntimePort, WakeReason,
};
use super::super::drain::{InboxDrainOptions, InboxDrainResult};
use super::super::store::{GatewayStoreOptions, create_gateway_store};
use super::super::store_api::GatewayStoreApi;
use super::super::types::{
    AdmissionLedger, ClaimOutcome, ClaimRequest, DeliveryState, EnqueueOutcome, EnqueueRequest,
    EnvelopeOrigin, Provenance, ReceiptRef,
};
use super::*;

struct World {
    phase: RuntimePhase,
    epoch: i64,
    starting: bool,
    conflicts: usize,
    calls: Vec<(&'static str, String)>,
    pending: Vec<String>,
    when_idle: Vec<(String, Option<WhenIdle>)>,
}

fn world() -> Arc<Mutex<World>> {
    Arc::new(Mutex::new(World {
        phase: RuntimePhase::Idle,
        epoch: 0,
        starting: false,
        conflicts: 0,
        calls: Vec::new(),
        pending: Vec::new(),
        when_idle: Vec::new(),
    }))
}

/// The runtime's admission contract as a session answers it: an idle session starts a turn unless the delivery asks to
/// be queued, a busy one takes a steer into its turn or a follow-up behind it, and a stale turn is a conflict.
fn admit_into(world: &mut World, input: &AdmitExternalMessageInput) -> Result<AdmitResult, String> {
    let epoch = world.epoch;
    let answer = |kind| {
        Ok(AdmitResult {
            kind,
            turn_epoch: epoch,
        })
    };
    if world.conflicts > 0 {
        world.conflicts -= 1;
        return answer(ExternalAdmissionKind::TurnConflict);
    }
    let id = input.delivery_id.clone();
    if world.pending.contains(&id) {
        return answer(ExternalAdmissionKind::AlreadyAdmitted);
    }
    if input
        .expected_turn_id
        .is_some_and(|expected| expected != epoch)
    {
        return answer(ExternalAdmissionKind::TurnConflict);
    }
    world.when_idle.push((id.clone(), input.when_idle));
    let busy = world.phase != RuntimePhase::Idle || world.starting;
    if !busy && input.when_idle != Some(WhenIdle::Queue) {
        world.pending.push(id.clone());
        world.calls.push(("start", id));
        world.starting = true;
        world.phase = RuntimePhase::MidTurn;
        world.epoch += 1;
        return answer(ExternalAdmissionKind::Started);
    }
    let steering = input.deliver_as == GatewayDeliveryMode::Steer;
    if !busy {
        world.pending.push(id.clone());
        world
            .calls
            .push((if steering { "steer" } else { "followUp" }, id));
        return answer(ExternalAdmissionKind::Queued);
    }
    if steering {
        if input.expected_turn_id.is_none() {
            return answer(ExternalAdmissionKind::TurnConflict);
        }
        world.pending.push(id.clone());
        world.calls.push(("steer", id));
        return answer(ExternalAdmissionKind::Steered);
    }
    world.pending.push(id.clone());
    world.calls.push(("followUp", id));
    answer(ExternalAdmissionKind::Queued)
}

fn port(world: &Arc<Mutex<World>>) -> SessionRuntimePort {
    let phase = Arc::clone(world);
    let gate = Arc::clone(world);
    let admit = Arc::clone(world);
    let ledger = Arc::clone(world);
    SessionRuntimePort {
        phase: Arc::new(move || phase.lock().expect("world").phase),
        admission_gate: Arc::new(move || SessionAdmissionGate {
            can_admit: true,
            hold_reason: None,
            editor_revision: 0,
            turn_epoch: gate.lock().expect("world").epoch,
        }),
        admit_external_message: Arc::new(move |input| {
            admit_into(&mut admit.lock().expect("world"), &input)
        }),
        list_admitted_deliveries: Arc::new(move || AdmissionLedger {
            pending: ledger.lock().expect("world").pending.clone(),
            emitted: Vec::new(),
        }),
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<dyn GatewayStoreApi>,
    drain: InboxDrain,
    world: Arc<Mutex<World>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let store: Arc<dyn GatewayStoreApi> = create_gateway_store(GatewayStoreOptions::new(
        dir.path().to_string_lossy().into_owned(),
    ))
    .expect("store");
    let world = world();
    let drain = InboxDrain::new(InboxDrainOptions {
        store: store.clone(),
        runtime: port(&world),
        durable_id: "receiver".into(),
        session_path: Arc::new(|| None),
        now: Some(Arc::new(|| 1_001)),
        log: None,
        notify: None,
        on_admitted: None,
        admit_intake: None,
        wake_gated: None,
    });
    Fixture {
        _dir: dir,
        store,
        drain,
        world,
    }
}

fn send(
    store: &Arc<dyn GatewayStoreApi>,
    id: &str,
    mode: GatewayDeliveryMode,
    expected_turn_id: Option<i64>,
) {
    let outcome = store
        .enqueue(&EnqueueRequest {
            provenance: Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            },
            now: 1_000,
            delivery_id: id.into(),
            target_durable_id: "receiver".into(),
            sender_principal: "session:sender".into(),
            rate_principal: None,
            sender_node: "sender".into(),
            sender_turn: None,
            cause_delivery_id: None,
            claimed_root_id: None,
            origin: EnvelopeOrigin::Session {
                session: "sender".into(),
            },
            actor: "planner".into(),
            body: id.into(),
            mode,
            expected_turn_id,
            receipt: ReceiptRef {
                idempotency_key: id.into(),
                args_hash: id.into(),
            },
            endpoint_kind: None,
            collapse_key: None,
        })
        .expect("enqueue");
    assert!(
        matches!(outcome, EnqueueOutcome::Inserted { .. }),
        "{outcome:?}"
    );
}

fn row(store: &Arc<dyn GatewayStoreApi>, id: &str) -> DeliveryRow {
    store.delivery_view(id).expect("view").expect("row").row
}

fn claim(store: &Arc<dyn GatewayStoreApi>, id: &str, lane: EffectiveLane) -> DeliveryRow {
    let claimed = store
        .claim(&ClaimRequest {
            now: 1_001,
            delivery_id: id.into(),
            self_identity: store.identity().expect("identity"),
            lane,
        })
        .expect("claim");
    match claimed {
        ClaimOutcome::Claimed { row } => *row,
        other => panic!("expected {id} claimed, got {other:?}"),
    }
}

fn start_pass(fixture: &Fixture) -> InboxDrainResult {
    fixture
        .drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Start,
            delivery_ids: None,
        })
        .expect("drain pass")
}

fn kinds(result: &InboxDrainResult) -> Vec<(String, ExternalAdmissionKind)> {
    result
        .admitted
        .iter()
        .map(|admission| (admission.delivery_id.clone(), admission.kind))
        .collect()
}

#[test]
fn deliveries_to_an_idle_session_join_one_start_that_is_admitted_last() {
    let fixture = fixture();
    for id in ["d-1", "d-2", "d-3"] {
        send(&fixture.store, id, GatewayDeliveryMode::Auto, None);
    }

    let result = start_pass(&fixture);

    let world = fixture.world.lock().expect("world");
    assert_eq!(
        world.calls,
        vec![
            ("steer", "d-2".to_owned()),
            ("steer", "d-3".to_owned()),
            ("start", "d-1".to_owned())
        ],
        "the start carries the rows that joined it, and is admitted after them"
    );
    assert_eq!(
        world.when_idle,
        vec![
            ("d-2".to_owned(), Some(WhenIdle::Queue)),
            ("d-3".to_owned(), Some(WhenIdle::Queue)),
            ("d-1".to_owned(), None)
        ]
    );
    assert_eq!(
        kinds(&result),
        vec![
            ("d-1".to_owned(), ExternalAdmissionKind::Started),
            ("d-2".to_owned(), ExternalAdmissionKind::Steered),
            ("d-3".to_owned(), ExternalAdmissionKind::Steered)
        ],
        "the pass reports its rows in queue order"
    );
    let lane = |id: &str| row(&fixture.store, id).mode_effective;
    assert_eq!(lane("d-1"), Some(EffectiveLane::Start));
    assert_eq!(lane("d-2"), Some(EffectiveLane::Steer));
    assert_eq!(lane("d-3"), Some(EffectiveLane::Steer));
}

#[test]
fn a_single_delivery_to_an_idle_session_starts_it_without_a_join() {
    let fixture = fixture();
    send(&fixture.store, "d-1", GatewayDeliveryMode::Auto, None);

    let result = start_pass(&fixture);

    assert_eq!(
        kinds(&result),
        vec![("d-1".to_owned(), ExternalAdmissionKind::Started)]
    );
    let world = fixture.world.lock().expect("world");
    assert_eq!(world.calls, vec![("start", "d-1".to_owned())]);
    assert_eq!(world.when_idle, vec![("d-1".to_owned(), None)]);
}

#[test]
fn an_auto_row_decided_as_a_start_is_steered_into_a_run_that_began_before_its_admission() {
    let fixture = fixture();
    send(&fixture.store, "d-1", GatewayDeliveryMode::Auto, None);
    let queued = row(&fixture.store, "d-1");
    let claimed = claim(&fixture.store, "d-1", EffectiveLane::Start);
    {
        let mut world = fixture.world.lock().expect("world");
        world.phase = RuntimePhase::MidTurn;
        world.epoch = 1;
    }

    let mut out = DrainOutputs::default();
    let step = fixture
        .drain
        .admit_claimed(&mut out, &queued, &claimed, false)
        .expect("admit");

    assert_eq!(step, AdmitStep::Next);
    assert_eq!(
        out.admitted,
        vec![Admission {
            delivery_id: "d-1".into(),
            kind: ExternalAdmissionKind::Steered
        }]
    );
    assert_eq!(
        fixture.world.lock().expect("world").calls,
        vec![("steer", "d-1".to_owned())]
    );
    assert_eq!(
        row(&fixture.store, "d-1").mode_effective,
        Some(EffectiveLane::Steer)
    );
}

#[test]
fn an_explicit_follow_up_decided_as_a_start_still_queues_behind_a_run_that_began() {
    let fixture = fixture();
    send(&fixture.store, "d-1", GatewayDeliveryMode::FollowUp, None);
    let queued = row(&fixture.store, "d-1");
    let claimed = claim(&fixture.store, "d-1", EffectiveLane::Start);
    fixture.world.lock().expect("world").phase = RuntimePhase::MidTurn;

    let mut out = DrainOutputs::default();
    fixture
        .drain
        .admit_claimed(&mut out, &queued, &claimed, false)
        .expect("admit");

    assert_eq!(
        out.admitted,
        vec![Admission {
            delivery_id: "d-1".into(),
            kind: ExternalAdmissionKind::Queued
        }]
    );
    assert_eq!(
        fixture.world.lock().expect("world").calls,
        vec![("followUp", "d-1".to_owned())]
    );
    assert_eq!(
        row(&fixture.store, "d-1").mode_effective,
        Some(EffectiveLane::FollowUp)
    );
}

#[test]
fn an_auto_row_decided_as_a_start_queues_as_an_approval_wait_when_the_session_waits_on_a_question()
{
    let fixture = fixture();
    send(&fixture.store, "d-1", GatewayDeliveryMode::Auto, None);
    let queued = row(&fixture.store, "d-1");
    let claimed = claim(&fixture.store, "d-1", EffectiveLane::Start);
    fixture.world.lock().expect("world").phase = RuntimePhase::WaitingQuestion;

    let mut out = DrainOutputs::default();
    fixture
        .drain
        .admit_claimed(&mut out, &queued, &claimed, false)
        .expect("admit");

    assert_eq!(
        out.admitted,
        vec![Admission {
            delivery_id: "d-1".into(),
            kind: ExternalAdmissionKind::Queued
        }]
    );
    assert_eq!(
        row(&fixture.store, "d-1").wait_kind.as_deref(),
        Some("approval")
    );
}

#[test]
fn rows_to_a_compacting_session_queue_and_none_is_reported_as_steered() {
    let fixture = fixture();
    fixture.world.lock().expect("world").phase = RuntimePhase::Compacting;
    for id in ["d-1", "d-2"] {
        send(&fixture.store, id, GatewayDeliveryMode::Auto, None);
    }

    let result = start_pass(&fixture);

    assert_eq!(
        kinds(&result),
        vec![
            ("d-1".to_owned(), ExternalAdmissionKind::Queued),
            ("d-2".to_owned(), ExternalAdmissionKind::Queued)
        ]
    );
}

#[test]
fn an_auto_row_the_engine_answers_turn_conflict_is_queued_again_once_and_admitted_once_by_the_next_pass()
 {
    let fixture = fixture();
    fixture.world.lock().expect("world").conflicts = 1;
    send(&fixture.store, "d-1", GatewayDeliveryMode::Auto, None);

    let first = start_pass(&fixture);

    assert!(first.admitted.is_empty(), "{first:?}");
    assert_eq!(row(&fixture.store, "d-1").state, DeliveryState::Queued);
    let second = fixture
        .drain
        .drain(&DrainWakeEvent {
            reason: WakeReason::Inbox,
            delivery_ids: None,
        })
        .expect("second pass");
    assert_eq!(
        kinds(&second),
        vec![("d-1".to_owned(), ExternalAdmissionKind::Started)]
    );
    let world = fixture.world.lock().expect("world");
    assert_eq!(
        world.calls.iter().filter(|(_, id)| id == "d-1").count(),
        1,
        "the engine took the delivery exactly once"
    );
}

#[test]
fn an_explicit_row_the_engine_answers_turn_conflict_is_refused_not_queued_again() {
    let fixture = fixture();
    fixture.world.lock().expect("world").conflicts = 1;
    send(&fixture.store, "d-1", GatewayDeliveryMode::FollowUp, None);

    let result = start_pass(&fixture);

    assert_eq!(
        kinds(&result),
        vec![("d-1".to_owned(), ExternalAdmissionKind::TurnConflict)]
    );
    let refused = row(&fixture.store, "d-1");
    assert_eq!(refused.state, DeliveryState::Refused);
    assert_eq!(refused.reason.as_deref(), Some("turn_conflict"));
}

#[test]
fn a_turn_pinned_steer_whose_turn_ended_never_joins_a_new_turn_and_is_refused() {
    let fixture = fixture();
    send(&fixture.store, "d-1", GatewayDeliveryMode::Steer, Some(1));
    let queued = row(&fixture.store, "d-1");
    let claimed = claim(&fixture.store, "d-1", EffectiveLane::Steer);

    let mut out = DrainOutputs::default();
    fixture
        .drain
        .admit_claimed(&mut out, &queued, &claimed, true)
        .expect("admit");

    assert_eq!(
        out.outcomes,
        vec![("d-1".to_owned(), GatewayAdmissionKind::NotSteerable)]
    );
    assert!(fixture.world.lock().expect("world").calls.is_empty());
    let refused = row(&fixture.store, "d-1");
    assert_eq!(refused.state, DeliveryState::Refused);
    assert_eq!(refused.reason.as_deref(), Some("not_steerable"));
}
