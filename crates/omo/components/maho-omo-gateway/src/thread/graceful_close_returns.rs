//! What a graceful close answers for the messages its session did not read.
//!
//! Upstream counterpart: `thread/gateway/graceful-close-returns.ts` (omo#14313). The closed-intake settle it ends
//! with (`wake/closed-intake.ts`) is not ported: mhc's gateway keeps no intake ledger.

use std::collections::BTreeSet;

use maho_ext_api::SessionEntry;
use serde_json::Value;

use super::store_api::GatewayStoreApi;
use super::store_ops::StoreError;
use super::types::RefusalReason;

/// senpi's clean-stop record of a graceful close (senpi#7046 `session-closed`).
pub const SESSION_CLOSED_ENTRY: &str = "session-closed";

/// The delivery ids the session's newest `session-closed` record handed back unread, read from the shutting
/// session's own entries; empty when it names none.
pub fn returned_at_close(entries: &[SessionEntry]) -> Vec<String> {
    let Some(record) = entries.iter().rev().find(|entry| {
        entry.kind == "custom"
            && entry.data["customType"] == SESSION_CLOSED_ENTRY
            && entry.data["data"].is_object()
    }) else {
        return Vec::new();
    };
    record.data["data"]["returned"]["deliveries"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// A graceful close ended the session: every delivery the engine took in and handed back unread (`returned`) is
/// refused `target_closed`, so its sender reads "not delivered", never "admitted". A row the session read is never
/// touched.
pub fn settle_graceful_close(
    store: &dyn GatewayStoreApi,
    durable_id: &str,
    returned: &[String],
) -> Result<(), StoreError> {
    let ids: BTreeSet<&str> = returned.iter().map(String::as_str).collect();
    let now = store.now();
    for row in store.list(Some(durable_id), None)? {
        if ids.contains(row.delivery_id.as_str()) {
            store.refuse_returned(now, &row.delivery_id, RefusalReason::TargetClosed)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::thread::store::{GatewayStoreOptions, create_gateway_store};
    use crate::thread::types::{
        ClaimOutcome, ClaimRequest, DeliveryState, EffectiveLane, EnqueueOutcome, EnqueueRequest,
        EnvelopeOrigin, GatewayDeliveryMode, OutcomeRecord, Provenance, ReceiptRef,
        RecordOutcomeRequest,
    };

    fn entry(kind: &str, data: Value) -> SessionEntry {
        SessionEntry {
            id: "entry".into(),
            parent_id: None,
            timestamp: "2026-11-25T00:00:00.000Z".into(),
            kind: kind.into(),
            data,
        }
    }

    fn closed(deliveries: Value) -> SessionEntry {
        entry(
            "custom",
            json!({
                "type": "custom",
                "customType": SESSION_CLOSED_ENTRY,
                "data": { "returned": { "deliveries": deliveries } },
            }),
        )
    }

    #[test]
    fn the_newest_clean_stop_record_names_the_returned_deliveries() {
        let entries = vec![
            closed(json!(["old"])),
            entry("message", json!({ "type": "message" })),
            closed(json!(["d-1", "d-2"])),
        ];
        assert_eq!(
            returned_at_close(&entries),
            vec!["d-1".to_owned(), "d-2".to_owned()]
        );
        assert!(returned_at_close(&[]).is_empty());
    }

    fn send(delivery_id: &str) -> EnqueueRequest {
        EnqueueRequest {
            provenance: Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            },
            now: 1_000,
            delivery_id: delivery_id.into(),
            target_durable_id: "t-closing".into(),
            sender_principal: "session:sender".into(),
            rate_principal: None,
            sender_node: "sender".into(),
            sender_turn: None,
            cause_delivery_id: None,
            claimed_root_id: None,
            origin: EnvelopeOrigin::Session {
                session: "sender".into(),
            },
            actor: "sender".into(),
            body: delivery_id.into(),
            mode: GatewayDeliveryMode::FollowUp,
            expected_turn_id: None,
            binding: None,
            receipt: ReceiptRef {
                idempotency_key: delivery_id.into(),
                args_hash: delivery_id.into(),
            },
            endpoint_kind: None,
            collapse_key: None,
        }
    }

    #[test]
    fn a_graceful_close_refuses_only_the_deliveries_its_session_handed_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store");
        let me = store.identity().expect("identity");
        for id in ["d-handed-back", "d-read", "d-queued"] {
            let sent = store.enqueue(&send(id)).expect("enqueue");
            assert!(
                matches!(sent, EnqueueOutcome::Inserted { .. }),
                "{id}: {sent:?}"
            );
        }
        for id in ["d-handed-back", "d-read"] {
            let claimed = store
                .claim(&ClaimRequest {
                    now: 1_000,
                    delivery_id: id.into(),
                    self_identity: me.clone(),
                    lane: EffectiveLane::FollowUp,
                })
                .expect("claim");
            assert!(
                matches!(claimed, ClaimOutcome::Claimed { .. }),
                "{id}: {claimed:?}"
            );
        }
        for (id, outcome) in [
            (
                "d-handed-back",
                OutcomeRecord::Admitted {
                    admission_kind: "queued".into(),
                    turn_epoch: 1,
                },
            ),
            (
                "d-read",
                OutcomeRecord::Applied {
                    admission_kind: "started".into(),
                    turn_epoch: 1,
                },
            ),
        ] {
            store
                .record_outcome(&RecordOutcomeRequest {
                    now: 1_000,
                    delivery_id: id.into(),
                    self_identity: me.clone(),
                    outcome,
                })
                .expect("record outcome");
        }

        settle_graceful_close(
            &*store,
            "t-closing",
            &["d-handed-back".to_owned(), "d-read".to_owned()],
        )
        .expect("settle");

        let states: Vec<(String, DeliveryState, Option<String>)> = store
            .list(Some("t-closing"), None)
            .expect("list")
            .into_iter()
            .map(|row| (row.delivery_id, row.state, row.reason))
            .collect();
        assert_eq!(
            states,
            vec![
                (
                    "d-handed-back".to_owned(),
                    DeliveryState::Refused,
                    Some("target_closed".to_owned()),
                ),
                ("d-read".to_owned(), DeliveryState::Applied, None),
                ("d-queued".to_owned(), DeliveryState::Queued, None),
            ]
        );
    }
}
