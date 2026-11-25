//! One target's undelivered deliveries, bounded per lane.
//!
//! Upstream counterpart: `thread/gateway/backlog.ts` (omo#14428, #14466, #14431). Steers count only against their
//! own small reserve and everything else against the target cap, so follow-ups a session took but has not written
//! can never shut a steer out; a full lane answers why it does not drain.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::constants::{
    STEER_RESERVE_BYTES, STEER_RESERVE_MESSAGES, TARGET_MAX_BYTES, TARGET_MAX_MESSAGES,
};
use super::store_ops::{
    OPEN_STATES, SqlAccess, SqlRow, StoreContext, StoreError, claim_fingerprint, refused, row_i64,
    row_opt_i64, row_str, select_rows, v_i64, v_str,
};
use super::types::{EnqueueOutcome, GatewayDeliveryMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BacklogLane {
    Steer,
    FollowUp,
}

impl BacklogLane {
    fn of(mode: GatewayDeliveryMode) -> Self {
        if mode == GatewayDeliveryMode::Steer {
            Self::Steer
        } else {
            Self::FollowUp
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }

    fn limits(self) -> (i64, i64) {
        match self {
            Self::Steer => (STEER_RESERVE_MESSAGES, STEER_RESERVE_BYTES),
            Self::FollowUp => (TARGET_MAX_MESSAGES, TARGET_MAX_BYTES as i64),
        }
    }

    fn filter(self) -> &'static str {
        match self {
            Self::Steer => "mode_requested = 'steer'",
            Self::FollowUp => "mode_requested <> 'steer'",
        }
    }
}

/// One target's undelivered deliveries: in the session's own queue (taken, not yet written) or still in the gateway.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetBacklog {
    pub held: i64,
    pub held_bytes: i64,
    /// Rows the session's runtime took into its own queue and has not written yet.
    pub held_by_session: i64,
    /// Rows the session's drain has not taken from the gateway yet.
    pub waiting_in_gateway: i64,
    pub oldest_at: Option<i64>,
    /// When the earliest still-counting queued row expires and frees its slot; `None` when none is queued.
    pub frees_at: Option<i64>,
}

/// What counts (omo#14466): a queued row past its `expires_at` does not (an enqueue refuses it just before this read),
/// and neither does a row past its lifetime whose claimant is dead (`dead_claims`). A live claimant's row counts.
fn backlog_select() -> String {
    format!(
        "SELECT target_durable_id AS target, COUNT(*) AS n, COALESCE(SUM(bytes), 0) AS b, \
         COALESCE(SUM(CASE WHEN state = 'queued' THEN 0 ELSE 1 END), 0) AS by_session, \
         MIN(created_at) AS oldest, MIN(CASE WHEN state = 'queued' THEN expires_at END) AS frees \
         FROM deliveries WHERE state IN {OPEN_STATES} AND NOT (state = 'queued' AND expires_at <= ?)"
    )
}

fn backlog_of(row: Option<&SqlRow>) -> TargetBacklog {
    let Some(row) = row else {
        return TargetBacklog::default();
    };
    let held = row_i64(row, "n");
    let held_by_session = row_i64(row, "by_session");
    TargetBacklog {
        held,
        held_bytes: row_i64(row, "b"),
        held_by_session,
        waiting_in_gateway: held - held_by_session,
        oldest_at: row_opt_i64(row, "oldest"),
        frees_at: row_opt_i64(row, "frees"),
    }
}

fn read_target_backlog(
    ctx: &StoreContext,
    target: &str,
    lane: BacklogLane,
    now: i64,
    dead_claims: &BTreeMap<String, String>,
) -> Result<TargetBacklog, StoreError> {
    let found = ctx.one(
        &format!(
            "{} AND target_durable_id = ? AND {}",
            backlog_select(),
            lane.filter()
        ),
        &[v_i64(now), v_str(target)],
    )?;
    let counted = backlog_of(found.as_ref());
    if dead_claims.is_empty() {
        return Ok(counted);
    }
    let mut dead = 0;
    let mut dead_bytes = 0;
    for row in select_rows(
        ctx,
        &format!(
            "target_durable_id = ? AND state IN ('admitting', 'admitted') AND {}",
            lane.filter()
        ),
        &[v_str(target)],
        None,
    )? {
        if dead_claims.get(&row.delivery_id) == Some(&claim_fingerprint(&row)) {
            dead += 1;
            dead_bytes += row.bytes;
        }
    }
    Ok(TargetBacklog {
        held: counted.held - dead,
        held_bytes: counted.held_bytes - dead_bytes,
        held_by_session: counted.held_by_session - dead,
        ..counted
    })
}

/// Every listed target's undelivered backlog in one read; a target holding nothing is absent.
pub fn read_target_backlogs(
    ctx: &StoreContext,
    targets: &[String],
    now: i64,
) -> Result<BTreeMap<String, TargetBacklog>, StoreError> {
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = vec!["?"; targets.len()].join(", ");
    let mut params = vec![v_i64(now)];
    params.extend(targets.iter().map(|target| v_str(target.as_str())));
    let rows = ctx.all(
        &format!(
            "{} AND target_durable_id IN ({placeholders}) GROUP BY target_durable_id",
            backlog_select()
        ),
        &params,
    )?;
    Ok(rows
        .iter()
        .map(|row| (row_str(row, "target"), backlog_of(Some(row))))
        .collect())
}

pub struct BacklogCheck<'a> {
    pub target: &'a str,
    pub mode: GatewayDeliveryMode,
    pub bytes: i64,
    pub now: i64,
    /// The target's rows a dead claimant left past their lifetime, by delivery id and claim fingerprint.
    pub dead_claims: &'a BTreeMap<String, String>,
    /// Whether a live endpoint holds the target: a target nobody runs frees slots only as its rows expire.
    pub target_running: bool,
}

/// The `queue_full` refusal of a send whose lane is full, naming why the lane does not drain; `None` when it fits.
pub fn backlog_refusal(
    ctx: &StoreContext,
    check: &BacklogCheck<'_>,
) -> Result<Option<EnqueueOutcome>, StoreError> {
    let lane = BacklogLane::of(check.mode);
    let (max_messages, max_bytes) = lane.limits();
    let backlog = read_target_backlog(ctx, check.target, lane, check.now, check.dead_claims)?;
    // The byte budget bounds what waits together: a lone delivery the engine accepted always fits an empty lane.
    if backlog.held < max_messages
        && (backlog.held == 0 || backlog.held_bytes + check.bytes <= max_bytes)
    {
        return Ok(None);
    }
    // A full lane's rows sit in the session's own queue, taken but not written (it has not reached the turn
    // boundary that writes them), or wait in the gateway for a drain that has not taken them.
    let not_writing = backlog.held_by_session >= backlog.waiting_in_gateway;
    let message = if not_writing {
        format!(
            "The target's {} backlog is full: the session took {} of them into its own queue and has not written them, because it has not reached a turn boundary since.",
            lane.as_str(),
            backlog.held_by_session
        )
    } else {
        format!(
            "The target's {} backlog is full: {} of them wait in the gateway for the session's drain, which has not taken them.",
            lane.as_str(),
            backlog.waiting_in_gateway
        )
    };
    let reason = if not_writing {
        "session_not_writing"
    } else {
        "session_not_draining"
    };
    let oldest_age_ms = backlog
        .oldest_at
        .map_or(0, |oldest| (check.now - oldest).max(0));
    Ok(Some(refused(
        "queue_full",
        message,
        Some(serde_json::json!({
            "lane": lane.as_str(),
            "max_messages": max_messages,
            "max_bytes": max_bytes,
            "held": backlog.held,
            "held_bytes": backlog.held_bytes,
            "held_by_session": backlog.held_by_session,
            "waiting_in_gateway": backlog.waiting_in_gateway,
            "oldest_age_ms": oldest_age_ms,
            "reason": reason,
            "target_running": check.target_running,
            "frees_at": backlog.frees_at,
        })),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::constants::QUEUED_TTL_MS;
    use crate::thread::schema::GATEWAY_MIGRATIONS;
    use crate::thread::store_ops::{refuse_expired_queued, v_opt_str};
    use crate::thread::types::{
        EnvelopeOrigin, ProcessIdentity, Provenance, StoredDeliveryEnvelope,
    };

    const START: i64 = 1_000_000;

    fn context() -> StoreContext {
        let ctx = StoreContext::new(
            rusqlite::Connection::open_in_memory().expect("memory database"),
            StoreContext::test_config(),
            StoreContext::test_identity(),
        );
        for step in GATEWAY_MIGRATIONS {
            for statement in *step {
                ctx.exec(statement).expect("migration step");
            }
        }
        ctx
    }

    /// One 5-byte row from `notifier` to `lead`; `held_by` makes it a row the session's runtime took (`admitted`).
    fn seed(
        ctx: &StoreContext,
        seq: i64,
        mode: &str,
        held_by: Option<&ProcessIdentity>,
        created_at: i64,
        expires_at: i64,
    ) {
        let id = format!("seeded-{seq}");
        let envelope = serde_json::to_string(&StoredDeliveryEnvelope {
            provenance: Some(Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            }),
            origin: EnvelopeOrigin::Session {
                session: "notifier".into(),
            },
            actor: "notifier".into(),
            via: Vec::new(),
            root_id: "root-seeded".into(),
            hop: 1,
            delivery_id: id.clone(),
        })
        .expect("envelope");
        let state = if held_by.is_some() {
            "admitted"
        } else {
            "queued"
        };
        let claimant = held_by.map(|identity| serde_json::to_string(identity).expect("claimant"));
        ctx.run(
            "INSERT INTO deliveries (delivery_id, target_durable_id, seq, sender, envelope, body, bytes, mode_requested, state, admitted_by, attempt, root_id, hop, created_at, updated_at, expires_at) VALUES (?, 'lead', ?, 'session:notifier', ?, 'alert', 5, ?, ?, ?, 1, 'root-seeded', 1, ?, ?, ?)",
            &[
                v_str(&id),
                v_i64(seq),
                v_str(envelope),
                v_str(mode),
                v_str(state),
                v_opt_str(claimant),
                v_i64(created_at),
                v_i64(created_at),
                v_i64(expires_at),
            ],
        )
        .expect("seed row");
    }

    fn check(
        mode: GatewayDeliveryMode,
        bytes: i64,
        now: i64,
        dead_claims: &BTreeMap<String, String>,
    ) -> BacklogCheck<'_> {
        BacklogCheck {
            target: "lead",
            mode,
            bytes,
            now,
            dead_claims,
            target_running: true,
        }
    }

    fn queue_full_details(outcome: Option<EnqueueOutcome>) -> serde_json::Value {
        match outcome {
            Some(EnqueueOutcome::Refused { code, details, .. }) => {
                assert_eq!(code.as_str(), "queue_full");
                details.expect("queue_full details")
            }
            other => panic!("expected queue_full, got {other:?}"),
        }
    }

    #[test]
    fn a_follow_up_backlog_the_session_holds_shuts_out_follow_ups_but_never_a_steer() {
        let ctx = context();
        let runtime = StoreContext::test_identity();
        for seq in 1..=TARGET_MAX_MESSAGES {
            let at = START + seq * 180_000;
            seed(
                &ctx,
                seq,
                "follow_up",
                Some(&runtime),
                at,
                at + QUEUED_TTL_MS,
            );
        }
        let none = BTreeMap::new();
        let now = START + (TARGET_MAX_MESSAGES + 1) * 180_000;
        let steer = backlog_refusal(&ctx, &check(GatewayDeliveryMode::Steer, 20, now, &none));
        assert_eq!(steer.expect("steer check"), None);
        let follow_up =
            backlog_refusal(&ctx, &check(GatewayDeliveryMode::FollowUp, 20, now, &none));
        let details = queue_full_details(follow_up.expect("follow-up check"));
        assert_eq!(details["lane"], "follow_up");
        assert_eq!(details["held"], TARGET_MAX_MESSAGES);
        assert_eq!(details["held_by_session"], TARGET_MAX_MESSAGES);
        assert_eq!(details["waiting_in_gateway"], 0);
        assert_eq!(details["oldest_age_ms"], now - (START + 180_000));
        assert_eq!(details["reason"], "session_not_writing");
    }

    #[test]
    fn steers_fill_only_their_own_reserve() {
        let ctx = context();
        for seq in 1..=STEER_RESERVE_MESSAGES {
            seed(&ctx, seq, "steer", None, START, START + QUEUED_TTL_MS);
        }
        let none = BTreeMap::new();
        let steer = backlog_refusal(&ctx, &check(GatewayDeliveryMode::Steer, 20, START, &none));
        let details = queue_full_details(steer.expect("steer check"));
        assert_eq!(details["lane"], "steer");
        assert_eq!(details["held"], STEER_RESERVE_MESSAGES);
        assert_eq!(details["reason"], "session_not_draining");
        let follow_up = backlog_refusal(
            &ctx,
            &check(GatewayDeliveryMode::FollowUp, 20, START, &none),
        );
        assert_eq!(follow_up.expect("follow-up check"), None);
    }

    #[test]
    fn a_backlog_waiting_in_the_gateway_names_when_its_oldest_row_frees_a_slot() {
        let ctx = context();
        for seq in 1..=TARGET_MAX_MESSAGES {
            seed(
                &ctx,
                seq,
                "follow_up",
                None,
                START + seq,
                START + QUEUED_TTL_MS + seq,
            );
        }
        let none = BTreeMap::new();
        let mut stopped = check(
            GatewayDeliveryMode::FollowUp,
            20,
            START + TARGET_MAX_MESSAGES + 1,
            &none,
        );
        stopped.target_running = false;
        let details = queue_full_details(backlog_refusal(&ctx, &stopped).expect("check"));
        assert_eq!(details["held_by_session"], 0);
        assert_eq!(details["waiting_in_gateway"], TARGET_MAX_MESSAGES);
        assert_eq!(details["reason"], "session_not_draining");
        assert_eq!(details["target_running"], false);
        assert_eq!(details["frees_at"], START + QUEUED_TTL_MS + 1);
    }

    #[test]
    fn queued_rows_past_their_lifetime_leave_the_lane_and_are_refused_expired() {
        let ctx = context();
        for seq in 1..=TARGET_MAX_MESSAGES {
            seed(&ctx, seq, "follow_up", None, START, START + QUEUED_TTL_MS);
        }
        let none = BTreeMap::new();
        let later = START + QUEUED_TTL_MS;
        let fits = backlog_refusal(
            &ctx,
            &check(GatewayDeliveryMode::FollowUp, 20, later, &none),
        );
        assert_eq!(fits.expect("check"), None);
        let first = refuse_expired_queued(&ctx, later, 10, "lead").expect("first batch");
        assert_eq!(first.len(), 10);
        let rest = refuse_expired_queued(&ctx, later, 1_000, "lead").expect("the rest");
        assert_eq!(first.len() + rest.len(), TARGET_MAX_MESSAGES as usize);
        let refused_rows = ctx
            .one(
                "SELECT COUNT(*) AS n FROM deliveries WHERE state = 'refused' AND reason = 'expired'",
                &[],
            )
            .expect("count")
            .expect("count row");
        assert_eq!(row_i64(&refused_rows, "n"), TARGET_MAX_MESSAGES);
    }

    #[test]
    fn a_lone_delivery_fits_an_empty_lane_whatever_its_size() {
        let ctx = context();
        let none = BTreeMap::new();
        let large = check(
            GatewayDeliveryMode::Steer,
            STEER_RESERVE_BYTES + 1_024,
            START,
            &none,
        );
        assert_eq!(backlog_refusal(&ctx, &large).expect("check"), None);
    }

    #[test]
    fn rows_a_dead_runtime_held_past_their_lifetime_leave_the_count() {
        let ctx = context();
        let gone = ProcessIdentity {
            pid: 999_999_999,
            process_start_time: None,
            instance_id: "gone".into(),
            runtime_instance: None,
        };
        for seq in 1..=TARGET_MAX_MESSAGES {
            seed(&ctx, seq, "follow_up", Some(&gone), START, START + 1_000);
        }
        let now = START + 2_000;
        let none = BTreeMap::new();
        let counted = backlog_refusal(&ctx, &check(GatewayDeliveryMode::FollowUp, 20, now, &none));
        let details = queue_full_details(counted.expect("check without dead claims"));
        assert_eq!(details["held_by_session"], TARGET_MAX_MESSAGES);
        let dead: BTreeMap<String, String> =
            select_rows(&ctx, "target_durable_id = 'lead'", &[], None)
                .expect("rows")
                .iter()
                .map(|row| (row.delivery_id.clone(), claim_fingerprint(row)))
                .collect();
        let freed = backlog_refusal(&ctx, &check(GatewayDeliveryMode::FollowUp, 20, now, &dead));
        assert_eq!(freed.expect("check with dead claims"), None);
    }

    #[test]
    fn backlogs_name_each_target_holding_undelivered_rows() {
        let ctx = context();
        let runtime = StoreContext::test_identity();
        seed(
            &ctx,
            1,
            "follow_up",
            Some(&runtime),
            START,
            START + QUEUED_TTL_MS,
        );
        seed(
            &ctx,
            2,
            "follow_up",
            None,
            START + 10,
            START + QUEUED_TTL_MS + 10,
        );
        let targets = ["lead".to_owned(), "quiet".to_owned()];
        let backlogs = read_target_backlogs(&ctx, &targets, START + 20).expect("backlogs");
        assert_eq!(backlogs.len(), 1);
        assert_eq!(
            backlogs.get("lead"),
            Some(&TargetBacklog {
                held: 2,
                held_bytes: 10,
                held_by_session: 1,
                waiting_in_gateway: 1,
                oldest_at: Some(START),
                frees_at: Some(START + QUEUED_TTL_MS + 10),
            })
        );
    }
}
