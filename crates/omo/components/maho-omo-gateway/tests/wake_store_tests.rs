//! The store's record of woken sessions, session waits and intake marks (the `woken_*` ops of
//! `thread/gateway/store-worker.ts` and `store-session-wait.test.ts`, omo#13037).

use maho_omo_gateway::paths::gateway_wake_marker_path;
use maho_omo_gateway::thread::constants::WAKE_LEASE_MS;
use maho_omo_gateway::thread::delivery_ledger::ledger_device;
use maho_omo_gateway::thread::session_wait_types::{
    DrainedMark, IntakeMark, IntakePair, OpenIntakeSession, QueuedWaitingOn, QueuedWaitingOnKind,
    SessionWaitKind, SessionWaitWrite, SessionWork,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::store_woken_ops::{WAKE_MAX_ATTEMPTS, WAKE_RETRY_DELAY_MS};
use maho_omo_gateway::thread::types::{ClaimOutcome, ClaimRequest, EffectiveLane, RefusalReason};
use maho_omo_gateway::thread::woken_types::{
    LostLease, WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome, WokenSessionFence,
    WokenSessionState,
};

use crate::wake_support::WakeHarness;

fn fence(durable_id: &str, generation: i64) -> WokenSessionFence {
    WokenSessionFence::new(durable_id, generation)
}

#[test]
fn given_a_requested_wake_when_it_is_started_renewed_and_taken_over_then_every_later_write_is_fenced_on_the_generation()
 {
    let h = WakeHarness::new();
    let requested = h
        .store
        .woken()
        .request("s1", "/sessions/s1.jsonl")
        .expect("request");
    assert!(requested.created);
    assert_eq!(requested.row.state, WokenSessionState::Requested);
    assert_eq!(requested.row.generation, 0);
    assert!(
        !h.store
            .woken()
            .request("s1", "/sessions/s1.jsonl")
            .expect("request")
            .created
    );
    assert!(gateway_wake_marker_path(h.dir.path()).exists());

    let WakeStartOutcome::Started { row } = h.store.woken().start("s1").expect("start") else {
        panic!("not started");
    };
    assert_eq!(row.state, WokenSessionState::Opening);
    assert_eq!(row.generation, 1);
    assert_eq!(row.lease_expires_at, Some(h.now() + WAKE_LEASE_MS));
    assert!(matches!(
        h.store.woken().start("s1").expect("start"),
        WakeStartOutcome::InProgress { .. }
    ));
    assert!(h.store.woken().gated("s1").expect("gated"));
    assert!(matches!(
        h.store.woken().take_over("s1").expect("take over"),
        WakeTakeOverOutcome::LeaseLive { .. }
    ));

    h.advance(WAKE_LEASE_MS + 1);
    assert!(!h.store.woken().gated("s1").expect("gated"));
    let other = h.other_store();
    let WakeTakeOverOutcome::TakenOver { row, .. } =
        other.woken().take_over("s1").expect("take over")
    else {
        panic!("not taken over");
    };
    assert_eq!(row.generation, 2);
    assert_eq!(
        h.store.woken().renew(&fence("s1", 1)).expect("renew"),
        Err(LostLease)
    );
    assert_eq!(
        h.store.woken().end(&fence("s1", 1)).expect("end"),
        Err(LostLease)
    );

    let opened = other
        .woken()
        .set_state(&fence("s1", 2), WokenSessionState::Open, Some("host-1"))
        .expect("set state")
        .expect("the lease is held");
    assert_eq!(opened.opened_at, Some(h.now()));
    assert_eq!(opened.host_instance.as_deref(), Some("host-1"));
    assert_eq!(other.woken().end(&fence("s1", 2)).expect("end"), Ok(()));
    assert!(h.store.woken().list().expect("list").is_empty());
}

#[test]
fn given_a_wake_whose_open_failed_when_it_is_retried_later_then_it_waits_its_delay_and_the_last_attempt_deletes_the_row()
 {
    let h = WakeHarness::new();
    h.store
        .woken()
        .request("s1", "/sessions/s1.jsonl")
        .expect("request");
    let mut generation = 0;
    for attempt in 1..WAKE_MAX_ATTEMPTS {
        let WakeStartOutcome::Started { row } = h.store.woken().start("s1").expect("start") else {
            panic!("not started");
        };
        generation = row.generation;
        let WakeRetryOutcome::Deferred { row } = h
            .store
            .woken()
            .retry_later(&fence("s1", generation))
            .expect("retry")
        else {
            panic!("not deferred");
        };
        assert_eq!(row.attempts, attempt);
        assert_eq!(row.state, WokenSessionState::Requested);
        assert_eq!(row.lease_owner, None);
        let next = row.next_attempt_at.expect("a next attempt");
        assert_eq!(next, h.now() + WAKE_RETRY_DELAY_MS);
        assert_eq!(
            h.store.woken().start("s1").expect("start"),
            WakeStartOutcome::NotYet {
                next_attempt_at: next
            }
        );
        h.set_now(next);
    }
    let WakeStartOutcome::Started { row } = h.store.woken().start("s1").expect("start") else {
        panic!("not started");
    };
    assert!(row.generation > generation);
    assert_eq!(
        h.store
            .woken()
            .retry_later(&fence("s1", row.generation))
            .expect("retry"),
        WakeRetryOutcome::Failed {
            attempts: WAKE_MAX_ATTEMPTS
        }
    );
    assert!(h.store.woken().list().expect("list").is_empty());
}

#[test]
fn given_stored_waits_when_a_queued_row_asks_what_it_waits_on_then_a_hold_reads_hold_a_persons_wait_reads_approval_and_an_unattended_hold_falls_to_the_wake()
 {
    let h = WakeHarness::new();
    let waiting_on = || h.store.session_wait().waiting_on("s1").expect("waiting on");
    let write = |wait: SessionWaitKind, kinds: &[&str]| {
        h.store
            .session_wait()
            .write(&SessionWaitWrite {
                durable_id: "s1".into(),
                wait,
                hold_kinds: kinds.iter().map(|kind| (*kind).to_owned()).collect(),
                file_size: 42,
            })
            .expect("write");
    };
    assert_eq!(waiting_on(), None);
    h.store
        .woken()
        .request("s1", "/sessions/s1.jsonl")
        .expect("request");
    let wake = Some(QueuedWaitingOn {
        kind: QueuedWaitingOnKind::Wake,
    });
    assert_eq!(waiting_on(), wake);

    write(SessionWaitKind::Held, &["unattended"]);
    assert_eq!(waiting_on(), wake);
    write(SessionWaitKind::Held, &["unattended", "session_hold"]);
    assert_eq!(
        waiting_on(),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Hold
        })
    );
    write(SessionWaitKind::Waiting, &[]);
    assert_eq!(
        waiting_on(),
        Some(QueuedWaitingOn {
            kind: QueuedWaitingOnKind::Approval
        })
    );
    let stored = h
        .store
        .session_wait()
        .read("s1")
        .expect("read")
        .expect("a stored wait");
    assert_eq!(stored.file_size, 42);
    assert_eq!(stored.written_at, h.now());

    h.store.session_wait().clear("s1").expect("clear");
    assert_eq!(h.store.session_wait().read("s1").expect("read"), None);
    assert_eq!(waiting_on(), wake);
}

#[test]
fn given_consecutive_wakes_when_the_persons_input_revision_moves_then_the_count_starts_again_at_one()
 {
    let h = WakeHarness::new();
    let count = |revision: Option<i64>| {
        h.store
            .woken()
            .count_wake("s1", revision)
            .expect("count wake")
    };
    assert_eq!(count(Some(0)), 1);
    assert_eq!(count(Some(0)), 2);
    assert_eq!(count(None), 3);
    assert_eq!(count(Some(7)), 1);
    assert_eq!(h.store.woken().wake_count("s1").expect("count"), 1);
    h.store.woken().reset_count("s1").expect("reset");
    assert_eq!(h.store.woken().wake_count("s1").expect("count"), 0);
}

#[test]
fn given_intake_marks_when_their_rows_settle_then_the_pair_drains_once_with_its_newest_command() {
    let h = WakeHarness::new();
    h.intake_row("t", "d-1", "dev-b");
    h.intake_row("t", "d-2", "dev-b");
    h.queue_row("t", "d-local");
    h.queue_row("u", "d-u");
    let marks = h.store.intake_marks();
    let pair = IntakePair {
        sender_device: "dev-b".into(),
        session_id: "t".into(),
    };
    marks
        .record(&pair, "c-1", Some("device:dev-b/d-1"))
        .expect("record");
    marks
        .record(&pair, "c-2", Some("device:dev-b/d-2"))
        .expect("record");
    assert_eq!(marks.marked().expect("marked"), vec![pair.clone()]);
    assert_eq!(
        marks.pending().expect("pending"),
        vec![IntakeMark {
            sender_device: "dev-b".into(),
            session_id: "t".into(),
            last_command_id: "c-2".into(),
        }]
    );
    assert_eq!(
        marks.command_for("device:dev-b/d-1").expect("command"),
        Some("c-1".to_owned())
    );
    assert_eq!(
        marks.command_for("device:dev-b/d-9").expect("command"),
        None
    );
    assert!(marks.open_for_command("dev-b", "c-2").expect("open"));
    assert!(!marks.open_for_command("dev-x", "c-2").expect("open"));
    assert_eq!(
        marks.open_sessions().expect("open sessions"),
        vec![
            OpenIntakeSession {
                durable_id: "t".into(),
                devices: vec!["dev-b".into()],
                local: true,
            },
            OpenIntakeSession {
                durable_id: "u".into(),
                devices: Vec::new(),
                local: true,
            },
        ]
    );
    assert_eq!(marks.drained(&pair).expect("drained"), DrainedMark::Pending);

    for row in h.store.list(Some("t"), None).expect("rows") {
        if row.ledger_id.is_some() {
            h.store
                .refuse_queued(h.now(), &row.delivery_id, RefusalReason::SenderNotAdmitted)
                .expect("refuse");
        }
    }
    assert_eq!(
        marks.drained(&pair).expect("drained"),
        DrainedMark::Drained {
            last_command_id: "c-2".into()
        }
    );
    assert!(marks.marked().expect("marked").is_empty());
    assert!(marks.pending().expect("pending").is_empty());
    assert_eq!(
        marks.open_sessions().expect("open sessions"),
        vec![
            OpenIntakeSession {
                durable_id: "t".into(),
                devices: Vec::new(),
                local: true,
            },
            OpenIntakeSession {
                durable_id: "u".into(),
                devices: Vec::new(),
                local: true,
            },
        ]
    );
}

#[test]
fn given_queued_and_claimed_rows_when_the_session_work_is_read_then_each_open_state_is_counted() {
    let h = WakeHarness::new();
    h.queue_row("t", "d-1");
    h.queue_row("t", "d-2");
    assert_eq!(
        h.store.woken().session_work("t").expect("work"),
        SessionWork {
            queued: 2,
            claimed: 0,
            unapplied: 0,
        }
    );
    let claimed = h
        .store
        .claim(&ClaimRequest {
            now: h.now(),
            delivery_id: "d-1".into(),
            self_identity: h.store.identity().expect("identity"),
            lane: EffectiveLane::FollowUp,
        })
        .expect("claim");
    assert!(
        matches!(claimed, ClaimOutcome::Claimed { .. }),
        "{claimed:?}"
    );
    assert_eq!(
        h.store.woken().session_work("t").expect("work"),
        SessionWork {
            queued: 1,
            claimed: 1,
            unapplied: 0,
        }
    );
}

#[test]
fn given_four_rows_past_requested_when_two_stores_start_a_fifth_and_a_sixth_then_both_start() {
    let h = WakeHarness::new();
    for id in ["o1", "o2", "o3", "o4", "s5", "s6"] {
        h.store
            .woken()
            .request(id, &format!("/sessions/{id}.jsonl"))
            .expect("request");
    }
    for id in ["o1", "o2", "o3", "o4"] {
        assert!(matches!(
            h.store.woken().start(id).expect("start"),
            WakeStartOutcome::Started { .. }
        ));
    }
    let other = h.other_store();
    let (first, second) = std::thread::scope(|scope| {
        let a = scope.spawn(|| h.store.woken().start("s5").expect("start s5"));
        let b = scope.spawn(|| other.woken().start("s6").expect("start s6"));
        (a.join().expect("join s5"), b.join().expect("join s6"))
    });
    assert!(
        matches!(first, WakeStartOutcome::Started { .. }),
        "{first:?}"
    );
    assert!(
        matches!(second, WakeStartOutcome::Started { .. }),
        "{second:?}"
    );
}

#[test]
fn given_ledger_ids_when_their_device_is_read_then_only_a_named_device_counts() {
    assert_eq!(ledger_device("device:dev-b/d-1").as_deref(), Some("dev-b"));
    assert_eq!(ledger_device("device:/d-1"), None);
    assert_eq!(ledger_device("device:dev-b").as_deref(), Some("dev-b"));
    assert_eq!(ledger_device("d-1"), None);
}
