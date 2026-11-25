//! The woken-session, session-wait and intake-mark statements of the store.
//!
//! Upstream counterpart: the `woken_*` half of `thread/gateway/store-worker.ts` (omo#13037 A3.1, A2.6, A3.5.3, A5.3).
//! A row leaves `requested` in one `BEGIN IMMEDIATE` transaction, with no count on how many rows may be past it (the
//! carrier defers a start only on measured host memory), and every later write is a compare-and-set on the generation a
//! takeover raises, so a carrier that lost its lease writes nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::constants::WAKE_LEASE_MS;
use super::session_wait_types::{
    DrainedMark, IntakeMark, IntakePair, OpenIntakeSession, QueuedWaitingOn, QueuedWaitingOnKind,
    SessionWaitKind, SessionWaitState, SessionWaitWrite, SessionWork, wait_blocks_delivery,
};
use super::store_ops::{
    OPEN_STATES, SqlAccess, SqlParams, SqlRow, StoreContext, StoreError, claimant_is_dead, new_id,
    row_i64, row_opt_i64, row_opt_str, row_str, transaction, v_i64, v_opt_i64, v_opt_str, v_str,
};
use super::types::ProcessIdentity;
use super::woken_types::{
    LostLease, WakeRequestOutcome, WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome,
    WokenSessionFence, WokenSessionRow, WokenSessionState,
};
use crate::paths::gateway_wake_marker_path;

/// A wake whose open failed for a transient reason is tried again this long after (A3.1.5).
pub const WAKE_RETRY_DELAY_MS: i64 = 10_000;
pub const WAKE_MAX_ATTEMPTS: i64 = 3;

const WOKEN_COLUMNS: &str = "durable_id, session_file, host_instance, state, requested_at, opened_at, first_waiting_at, lease_owner, lease_expires_at, generation, attempts, next_attempt_at";

/// The sender's device as an intake row's ledger id names it (`device:<device>/<id>`), when it does.
pub fn ledger_device(ledger_id: &str) -> Option<&str> {
    let rest = ledger_id.strip_prefix("device:")?;
    let cut = rest.find('/')?;
    if cut == 0 {
        return None;
    }
    Some(&rest[..cut])
}

fn identity_json(identity: &ProcessIdentity) -> String {
    serde_json::to_string(identity).unwrap_or_else(|_| "null".to_owned())
}

fn woken_row_from(record: &SqlRow) -> WokenSessionRow {
    WokenSessionRow {
        durable_id: row_str(record, "durable_id"),
        session_file: row_str(record, "session_file"),
        host_instance: row_opt_str(record, "host_instance"),
        state: WokenSessionState::parse(&row_str(record, "state"))
            .unwrap_or(WokenSessionState::Requested),
        requested_at: row_i64(record, "requested_at"),
        opened_at: row_opt_i64(record, "opened_at"),
        first_waiting_at: row_opt_i64(record, "first_waiting_at"),
        lease_owner: row_opt_str(record, "lease_owner")
            .and_then(|text| serde_json::from_str::<ProcessIdentity>(&text).ok()),
        lease_expires_at: row_opt_i64(record, "lease_expires_at"),
        generation: row_i64(record, "generation"),
        attempts: row_i64(record, "attempts"),
        next_attempt_at: row_opt_i64(record, "next_attempt_at"),
    }
}

fn select_woken_row(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<WokenSessionRow>, StoreError> {
    Ok(ctx
        .one(
            &format!("SELECT {WOKEN_COLUMNS} FROM woken_sessions WHERE durable_id = ?"),
            &[v_str(durable_id)],
        )?
        .map(|record| woken_row_from(&record)))
}

fn require_woken_row(ctx: &StoreContext, durable_id: &str) -> Result<WokenSessionRow, StoreError> {
    select_woken_row(ctx, durable_id)?.ok_or_else(|| {
        StoreError::Message(format!(
            "woken session {durable_id} vanished inside its own transaction"
        ))
    })
}

fn io_failure(error: std::io::Error) -> StoreError {
    StoreError::Message(error.to_string())
}

/// The wake hint carriers watch: a temp file renamed over `wake.marker` while the write lock is held, so like the inbox
/// marker it is only ever early. Written when a wake is requested and when a row ends (a place freed lets a `requested`
/// row start).
fn touch_wake_marker(ctx: &StoreContext, durable_id: &str, now: i64) -> Result<(), StoreError> {
    let marker = gateway_wake_marker_path(Path::new(&ctx.config.agent_dir));
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(io_failure)?;
    }
    let temporary = marker.with_file_name(format!("{}.tmp", new_id("wake.marker")));
    let payload = serde_json::json!({ "durable_id": durable_id, "written_at": now });
    std::fs::write(&temporary, payload.to_string()).map_err(io_failure)?;
    std::fs::rename(&temporary, &marker).map_err(io_failure)
}

fn fenced(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    set: &str,
    params: SqlParams,
) -> Result<bool, StoreError> {
    let mut all = params;
    all.push(v_str(&fence.durable_id));
    all.push(v_i64(fence.generation));
    Ok(ctx.run(
        &format!("UPDATE woken_sessions SET {set} WHERE durable_id = ? AND generation = ?"),
        &all,
    )? == 1)
}

pub fn request_wake(
    ctx: &StoreContext,
    durable_id: &str,
    session_file: &str,
    now: i64,
) -> Result<WakeRequestOutcome, StoreError> {
    transaction(ctx, "woken_request", || {
        let created = ctx.run(
            "INSERT OR IGNORE INTO woken_sessions (durable_id, session_file, state, requested_at) VALUES (?, ?, 'requested', ?)",
            &[v_str(durable_id), v_str(session_file), v_i64(now)],
        )? == 1;
        touch_wake_marker(ctx, durable_id, now)?;
        Ok(WakeRequestOutcome {
            row: Box::new(require_woken_row(ctx, durable_id)?),
            created,
        })
    })
}

pub fn start_wake(
    ctx: &StoreContext,
    durable_id: &str,
    now: i64,
) -> Result<WakeStartOutcome, StoreError> {
    transaction(ctx, "woken_start", || {
        let Some(row) = select_woken_row(ctx, durable_id)? else {
            return Ok(WakeStartOutcome::NotFound);
        };
        if row.state != WokenSessionState::Requested {
            return Ok(WakeStartOutcome::InProgress { row: Box::new(row) });
        }
        if let Some(next_attempt_at) = row.next_attempt_at.filter(|next| *next > now) {
            return Ok(WakeStartOutcome::NotYet { next_attempt_at });
        }
        ctx.run(
            "UPDATE woken_sessions SET state = 'opening', lease_owner = ?, lease_expires_at = ?, generation = generation + 1, next_attempt_at = NULL WHERE durable_id = ?",
            &[
                v_str(identity_json(&ctx.self_identity)),
                v_i64(now + WAKE_LEASE_MS),
                v_str(durable_id),
            ],
        )?;
        Ok(WakeStartOutcome::Started {
            row: Box::new(require_woken_row(ctx, durable_id)?),
        })
    })
}

pub fn renew_lease(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    now: i64,
) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
    transaction(ctx, "woken_renew", || {
        if fenced(
            ctx,
            fence,
            "lease_expires_at = ?",
            vec![v_i64(now + WAKE_LEASE_MS)],
        )? {
            Ok(Ok(require_woken_row(ctx, &fence.durable_id)?))
        } else {
            Ok(Err(LostLease))
        }
    })
}

/// Takes over a row whose lease ran out. The previous owner is probed before the lock, with the probe `reconcile` uses
/// for a claimant; the write then requires the generation that was judged.
pub fn take_over_wake(
    ctx: &StoreContext,
    durable_id: &str,
    now: i64,
) -> Result<WakeTakeOverOutcome, StoreError> {
    let Some(seen) = select_woken_row(ctx, durable_id)? else {
        return Ok(WakeTakeOverOutcome::NotFound);
    };
    if seen.lease_owner.is_none() || seen.lease_expires_at.is_none() {
        return Ok(WakeTakeOverOutcome::NotLeased {
            row: Box::new(seen),
        });
    }
    if seen.lease_expires_at.unwrap_or(0) > now {
        return Ok(WakeTakeOverOutcome::LeaseLive {
            row: Box::new(seen),
        });
    }
    let previous_owner_alive = seen
        .lease_owner
        .as_ref()
        .is_some_and(|owner| !claimant_is_dead(owner));
    let seen_generation = seen.generation;
    transaction(ctx, "woken_take_over", || {
        let Some(current) = select_woken_row(ctx, durable_id)? else {
            return Ok(WakeTakeOverOutcome::NotFound);
        };
        if current.generation != seen_generation {
            return Ok(WakeTakeOverOutcome::LostLease);
        }
        if current.lease_expires_at.is_some_and(|at| at > now) {
            return Ok(WakeTakeOverOutcome::LeaseLive {
                row: Box::new(current),
            });
        }
        fenced(
            ctx,
            &WokenSessionFence::new(durable_id, current.generation),
            "lease_owner = ?, lease_expires_at = ?, generation = generation + 1",
            vec![
                v_str(identity_json(&ctx.self_identity)),
                v_i64(now + WAKE_LEASE_MS),
            ],
        )?;
        Ok(WakeTakeOverOutcome::TakenOver {
            row: Box::new(require_woken_row(ctx, durable_id)?),
            previous_owner_alive,
        })
    })
}

pub fn set_woken_state(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    state: WokenSessionState,
    host_instance: Option<&str>,
    now: i64,
) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
    transaction(ctx, "woken_set_state", || {
        let opened = (state == WokenSessionState::Open).then_some(now);
        let written = fenced(
            ctx,
            fence,
            "state = ?, opened_at = COALESCE(opened_at, ?), host_instance = COALESCE(?, host_instance)",
            vec![
                v_str(state.as_str()),
                v_opt_i64(opened),
                v_opt_str(host_instance),
            ],
        )?;
        if written {
            Ok(Ok(require_woken_row(ctx, &fence.durable_id)?))
        } else {
            Ok(Err(LostLease))
        }
    })
}

pub fn record_waiting(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    now: i64,
) -> Result<Result<WokenSessionRow, LostLease>, StoreError> {
    transaction(ctx, "woken_record_waiting", || {
        if fenced(
            ctx,
            fence,
            "first_waiting_at = COALESCE(first_waiting_at, ?)",
            vec![v_i64(now)],
        )? {
            Ok(Ok(require_woken_row(ctx, &fence.durable_id)?))
        } else {
            Ok(Err(LostLease))
        }
    })
}

/// A transient open failure (A3.1.5): back to `requested` with the next attempt's time, or deleted at the last attempt.
pub fn retry_wake_later(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    now: i64,
) -> Result<WakeRetryOutcome, StoreError> {
    transaction(ctx, "woken_retry_later", || {
        let Some(row) = select_woken_row(ctx, &fence.durable_id)? else {
            return Ok(WakeRetryOutcome::LostLease);
        };
        if row.generation != fence.generation {
            return Ok(WakeRetryOutcome::LostLease);
        }
        let attempts = row.attempts + 1;
        if attempts >= WAKE_MAX_ATTEMPTS {
            ctx.run(
                "DELETE FROM woken_sessions WHERE durable_id = ? AND generation = ?",
                &[v_str(&fence.durable_id), v_i64(fence.generation)],
            )?;
            touch_wake_marker(ctx, &fence.durable_id, now)?;
            return Ok(WakeRetryOutcome::Failed { attempts });
        }
        fenced(
            ctx,
            fence,
            "state = 'requested', lease_owner = NULL, lease_expires_at = NULL, attempts = ?, next_attempt_at = ?",
            vec![v_i64(attempts), v_i64(now + WAKE_RETRY_DELAY_MS)],
        )?;
        Ok(WakeRetryOutcome::Deferred {
            row: Box::new(require_woken_row(ctx, &fence.durable_id)?),
        })
    })
}

pub fn end_wake(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
    now: i64,
) -> Result<Result<(), LostLease>, StoreError> {
    transaction(ctx, "woken_end", || {
        let deleted = ctx.run(
            "DELETE FROM woken_sessions WHERE durable_id = ? AND generation = ?",
            &[v_str(&fence.durable_id), v_i64(fence.generation)],
        )?;
        if deleted != 1 {
            return Ok(Err(LostLease));
        }
        touch_wake_marker(ctx, &fence.durable_id, now)?;
        Ok(Ok(()))
    })
}

/// True only for an `opening` row whose lease is live: the later drain admits nothing for such a session (A3.1.5).
pub fn is_gated(ctx: &StoreContext, durable_id: &str, now: i64) -> Result<bool, StoreError> {
    Ok(select_woken_row(ctx, durable_id)?.is_some_and(|row| {
        row.state == WokenSessionState::Opening && row.lease_expires_at.is_some_and(|at| at > now)
    }))
}

pub fn list_woken(ctx: &StoreContext) -> Result<Vec<WokenSessionRow>, StoreError> {
    Ok(ctx
        .all(
            &format!(
                "SELECT {WOKEN_COLUMNS} FROM woken_sessions ORDER BY requested_at, durable_id"
            ),
            &[],
        )?
        .iter()
        .map(woken_row_from)
        .collect())
}

/// The person's wait ended before its 30 minutes (A3.2.6): the first time it was seen is forgotten.
pub fn clear_waiting(
    ctx: &StoreContext,
    fence: &WokenSessionFence,
) -> Result<Result<(), LostLease>, StoreError> {
    transaction(ctx, "woken_clear_waiting", || {
        if fenced(ctx, fence, "first_waiting_at = NULL", Vec::new())? {
            Ok(Ok(()))
        } else {
            Ok(Err(LostLease))
        }
    })
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn wait_state_from(record: &SqlRow) -> SessionWaitState {
    let hold_kinds: Vec<String> = serde_json::from_str::<Value>(&row_str(record, "hold_kinds"))
        .ok()
        .and_then(|value| {
            value
                .as_array()
                .map(|kinds| kinds.iter().map(text_of).collect())
        })
        .unwrap_or_default();
    SessionWaitState {
        durable_id: row_str(record, "durable_id"),
        wait: if row_str(record, "wait") == "held" {
            SessionWaitKind::Held
        } else {
            SessionWaitKind::Waiting
        },
        hold_kinds,
        file_size: row_i64(record, "file_size"),
        written_at: row_i64(record, "written_at"),
    }
}

pub fn read_wait(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<SessionWaitState>, StoreError> {
    Ok(ctx
        .one(
            "SELECT durable_id, wait, hold_kinds, file_size, written_at FROM session_wait WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?
        .map(|record| wait_state_from(&record)))
}

pub fn write_wait(
    ctx: &StoreContext,
    write: &SessionWaitWrite,
    now: i64,
) -> Result<(), StoreError> {
    let hold_kinds = serde_json::to_string(&write.hold_kinds).unwrap_or_else(|_| "[]".to_owned());
    transaction(ctx, "wait_write", || {
        ctx.run(
            "INSERT INTO session_wait (durable_id, wait, hold_kinds, file_size, written_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT (durable_id) DO UPDATE SET wait = excluded.wait, hold_kinds = excluded.hold_kinds, file_size = excluded.file_size, written_at = excluded.written_at",
            &[
                v_str(&write.durable_id),
                v_str(write.wait.as_str()),
                v_str(&hold_kinds),
                v_i64(write.file_size),
                v_i64(now),
            ],
        )?;
        Ok(())
    })
}

pub fn clear_wait(ctx: &StoreContext, durable_id: &str) -> Result<(), StoreError> {
    transaction(ctx, "wait_clear", || {
        ctx.run(
            "DELETE FROM session_wait WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        Ok(())
    })
}

/// A hold is reported before the wake, and a person's wait reads as approval (A2.6).
pub fn waiting_on(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<QueuedWaitingOn>, StoreError> {
    if let Some(wait) = read_wait(ctx, durable_id)?
        && wait_blocks_delivery(&wait)
    {
        let kind = if wait.wait == SessionWaitKind::Held {
            QueuedWaitingOnKind::Hold
        } else {
            QueuedWaitingOnKind::Approval
        };
        return Ok(Some(QueuedWaitingOn { kind }));
    }
    let woken = ctx
        .one(
            "SELECT COUNT(*) AS n FROM woken_sessions WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?
        .map_or(0, |row| row_i64(&row, "n"));
    Ok((woken > 0).then_some(QueuedWaitingOn {
        kind: QueuedWaitingOnKind::Wake,
    }))
}

/// Counts a wake that opened the session (A3.5.3); a moved input revision starts the count again at one.
pub fn count_wake(
    ctx: &StoreContext,
    durable_id: &str,
    human_input_revision: Option<i64>,
) -> Result<i64, StoreError> {
    transaction(ctx, "wake_count", || {
        let found = ctx.one(
            "SELECT count, human_input_revision FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        let stored = found
            .as_ref()
            .and_then(|row| row_opt_i64(row, "human_input_revision"));
        let moved = match (&found, human_input_revision) {
            (None, _) => true,
            (Some(_), Some(revision)) => stored.unwrap_or(0) != revision,
            (Some(_), None) => false,
        };
        let count = if moved {
            1
        } else {
            found.as_ref().map_or(0, |row| row_i64(row, "count")) + 1
        };
        ctx.run(
            "INSERT INTO wake_counts (durable_id, count, human_input_revision) VALUES (?, ?, ?) ON CONFLICT (durable_id) DO UPDATE SET count = excluded.count, human_input_revision = excluded.human_input_revision",
            &[
                v_str(durable_id),
                v_i64(count),
                v_opt_i64(human_input_revision.or(stored)),
            ],
        )?;
        Ok(count)
    })
}

pub fn wake_count(ctx: &StoreContext, durable_id: &str) -> Result<i64, StoreError> {
    Ok(ctx
        .one(
            "SELECT count FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?
        .map_or(0, |row| row_i64(&row, "count")))
}

pub fn reset_count(ctx: &StoreContext, durable_id: &str) -> Result<(), StoreError> {
    transaction(ctx, "wake_count_reset", || {
        ctx.run(
            "DELETE FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        Ok(())
    })
}

/// The session's rows the store still holds open: queued, claimed (`admitting`), admitted and not yet applied.
pub fn session_work(ctx: &StoreContext, durable_id: &str) -> Result<SessionWork, StoreError> {
    let count = |state: &str| -> Result<i64, StoreError> {
        Ok(ctx
            .one(
                "SELECT COUNT(*) AS n FROM deliveries WHERE target_durable_id = ? AND state = ?",
                &[v_str(durable_id), v_str(state)],
            )?
            .map_or(0, |row| row_i64(&row, "n")))
    };
    Ok(SessionWork {
        queued: count("queued")?,
        claimed: count("admitting")?,
        unapplied: count("admitted")?,
    })
}

/// The intake rows of one (sender device, session) pair: their ledger id is `device:<attested device>/<sender's id>`.
const MARK_PAIR_ROWS: &str =
    "target_durable_id = ? AND ledger_id IS NOT NULL AND substr(ledger_id, 1, length(?)) = ?";

fn mark_pair_params(pair: &IntakePair) -> SqlParams {
    let prefix = format!("device:{}/", pair.sender_device);
    vec![
        v_str(&pair.session_id),
        v_str(prefix.clone()),
        v_str(prefix),
    ]
}

/// A NEW committed intake row (never a deduplicated answer): its command id becomes the pair's newest (A5.3).
pub fn record_intake_command(
    ctx: &StoreContext,
    pair: &IntakePair,
    command_id: &str,
    ledger_id: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    transaction(ctx, "intake_mark", || {
        if let Some(ledger_id) = ledger_id {
            ctx.run(
                "INSERT OR IGNORE INTO intake_commands (ledger_id, command_id) VALUES (?, ?)",
                &[v_str(ledger_id), v_str(command_id)],
            )?;
        }
        ctx.run(
            "INSERT INTO intake_marks (sender_device, session_id, last_command_id, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT (sender_device, session_id) DO UPDATE SET last_command_id = excluded.last_command_id, updated_at = excluded.updated_at",
            &[
                v_str(&pair.sender_device),
                v_str(&pair.session_id),
                v_str(command_id),
                v_i64(now),
            ],
        )?;
        Ok(())
    })
}

fn open_mark_rows(ctx: &StoreContext, pair: &IntakePair) -> Result<i64, StoreError> {
    Ok(ctx
        .one(
            &format!(
                "SELECT COUNT(*) AS n FROM deliveries WHERE {MARK_PAIR_ROWS} AND state IN {OPEN_STATES}"
            ),
            &mark_pair_params(pair),
        )?
        .map_or(0, |row| row_i64(&row, "n")))
}

fn mark_rows(ctx: &StoreContext) -> Result<Vec<IntakeMark>, StoreError> {
    Ok(ctx
        .all(
            "SELECT sender_device, session_id, last_command_id FROM intake_marks ORDER BY sender_device, session_id",
            &[],
        )?
        .iter()
        .map(|row| IntakeMark {
            sender_device: row_str(row, "sender_device"),
            session_id: row_str(row, "session_id"),
            last_command_id: row_str(row, "last_command_id"),
        })
        .collect())
}

pub fn marked_pairs(ctx: &StoreContext) -> Result<Vec<IntakePair>, StoreError> {
    Ok(mark_rows(ctx)?
        .into_iter()
        .map(|mark| IntakePair {
            sender_device: mark.sender_device,
            session_id: mark.session_id,
        })
        .collect())
}

/// Every pair that still holds an intake row not yet applied, with the newest command the consumer answered queued.
pub fn pending_marks(ctx: &StoreContext) -> Result<Vec<IntakeMark>, StoreError> {
    let mut pending = Vec::new();
    for mark in mark_rows(ctx)? {
        let pair = IntakePair {
            sender_device: mark.sender_device.clone(),
            session_id: mark.session_id.clone(),
        };
        if open_mark_rows(ctx, &pair)? > 0 {
            pending.push(mark);
        }
    }
    Ok(pending)
}

/// One transaction: when the pair holds no open intake row, its mark is removed and the newest command returned.
pub fn drained_mark(ctx: &StoreContext, pair: &IntakePair) -> Result<DrainedMark, StoreError> {
    transaction(ctx, "intake_drained", || {
        if open_mark_rows(ctx, pair)? > 0 {
            return Ok(DrainedMark::Pending);
        }
        let Some(mark) = ctx.one(
            "SELECT last_command_id FROM intake_marks WHERE sender_device = ? AND session_id = ?",
            &[v_str(&pair.sender_device), v_str(&pair.session_id)],
        )?
        else {
            return Ok(DrainedMark::Pending);
        };
        ctx.run(
            "DELETE FROM intake_marks WHERE sender_device = ? AND session_id = ?",
            &[v_str(&pair.sender_device), v_str(&pair.session_id)],
        )?;
        Ok(DrainedMark::Drained {
            last_command_id: row_str(&mark, "last_command_id"),
        })
    })
}

/// Sessions a sweep may wake (5.4 step 6): intake rows that are queued, or claimed by a claimant that is dead, and
/// whether same-device rows are queued for the session too. A row whose envelope names no device is read by its
/// ledger id's device, the device the intake attested.
pub fn open_intake_sessions(ctx: &StoreContext) -> Result<Vec<OpenIntakeSession>, StoreError> {
    let rows = ctx.all(
        &format!(
            "SELECT target_durable_id, json_extract(envelope, '$.provenance.device') AS device, ledger_id, state, admitted_by FROM deliveries WHERE ledger_id IS NOT NULL AND state IN {OPEN_STATES} ORDER BY target_durable_id"
        ),
        &[],
    )?;
    let mut sessions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in rows {
        if row_str(&row, "state") != "queued"
            && let Some(claimant) = row_opt_str(&row, "admitted_by")
                .and_then(|text| serde_json::from_str::<ProcessIdentity>(&text).ok())
            && !claimant_is_dead(&claimant)
        {
            continue;
        }
        let device = row_opt_str(&row, "device").or_else(|| {
            row_opt_str(&row, "ledger_id")
                .and_then(|ledger_id| ledger_device(&ledger_id).map(str::to_owned))
        });
        let devices = sessions
            .entry(row_str(&row, "target_durable_id"))
            .or_default();
        if let Some(device) = device
            && !devices.contains(&device)
        {
            devices.push(device);
        }
    }
    let local: BTreeSet<String> = ctx
        .all(
            "SELECT DISTINCT target_durable_id FROM deliveries WHERE ledger_id IS NULL AND state = 'queued'",
            &[],
        )?
        .iter()
        .map(|row| row_str(row, "target_durable_id"))
        .collect();
    for durable_id in &local {
        sessions.entry(durable_id.clone()).or_default();
    }
    Ok(sessions
        .into_iter()
        .map(|(durable_id, devices)| OpenIntakeSession {
            local: local.contains(&durable_id),
            durable_id,
            devices,
        })
        .collect())
}

pub fn command_for_row(ctx: &StoreContext, ledger_id: &str) -> Result<Option<String>, StoreError> {
    Ok(ctx
        .one(
            "SELECT command_id FROM intake_commands WHERE ledger_id = ?",
            &[v_str(ledger_id)],
        )?
        .map(|row| row_str(&row, "command_id")))
}

/// Whether an open intake row of this sender device was committed under this command id.
pub fn open_for_mark_command(
    ctx: &StoreContext,
    sender_device: &str,
    command_id: &str,
) -> Result<bool, StoreError> {
    let prefix = format!("device:{sender_device}/");
    Ok(ctx
        .one(
            &format!(
                "SELECT COUNT(*) AS n FROM intake_commands c JOIN deliveries d ON d.ledger_id = c.ledger_id WHERE c.command_id = ? AND substr(c.ledger_id, 1, length(?)) = ? AND d.state IN {OPEN_STATES}"
            ),
            &[v_str(command_id), v_str(prefix.clone()), v_str(prefix)],
        )?
        .map_or(0, |row| row_i64(&row, "n"))
        > 0)
}
