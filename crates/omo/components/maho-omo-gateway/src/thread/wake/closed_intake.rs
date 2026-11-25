use crate::thread::store_api::GatewayStoreApi;
use crate::thread::store_ops::StoreError;
use crate::thread::types::{DeliveryRow, DeliveryState, RefusalReason};

pub fn settle_closed_intake(
    store: &dyn GatewayStoreApi,
    durable_id: &str,
    closed_at: Option<i64>,
) -> Result<Vec<DeliveryRow>, StoreError> {
    let queued: Vec<DeliveryRow> = store
        .list(Some(durable_id), None)?
        .into_iter()
        .filter(|row| row.state == DeliveryState::Queued)
        .collect();
    let Some(closed_at) = closed_at else {
        return Ok(queued);
    };
    let mut remaining = Vec::new();
    for row in queued {
        if row.created_at > closed_at {
            remaining.push(row);
        } else {
            store.refuse_queued(store.now(), &row.delivery_id, RefusalReason::TargetClosed)?;
        }
    }
    Ok(remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::store::{GatewayStoreOptions, create_gateway_store};
    use crate::thread::types::{
        ClaimOutcome, ClaimRequest, EffectiveLane, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin,
        GatewayDeliveryMode, Provenance, ReceiptRef,
    };

    fn enqueue_at(target: &str, delivery_id: &str, now: i64) -> EnqueueRequest {
        EnqueueRequest {
            provenance: Provenance {
                workspace: "w".into(),
                project: "p".into(),
                lane: None,
                session: None,
            },
            now,
            delivery_id: delivery_id.into(),
            target_durable_id: target.into(),
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
        }
    }

    #[test]
    fn given_queued_rows_before_and_after_close_when_settled_then_only_rows_after_close_remain_queued()
     {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store");
        for (id, at) in [("d-before", 1_000), ("d-at", 2_000), ("d-after", 3_000)] {
            let outcome = store
                .enqueue(&enqueue_at("t-closed", id, at))
                .expect("enqueue");
            assert!(matches!(outcome, EnqueueOutcome::Inserted { .. }));
        }

        let untouched = settle_closed_intake(store.as_ref(), "t-closed", None).expect("no close");
        assert_eq!(untouched.len(), 3);

        let remaining =
            settle_closed_intake(store.as_ref(), "t-closed", Some(2_000)).expect("settle");
        assert_eq!(
            remaining
                .iter()
                .map(|row| row.delivery_id.as_str())
                .collect::<Vec<_>>(),
            vec!["d-after"]
        );

        for id in ["d-before", "d-at"] {
            let view = store
                .delivery_view(id)
                .expect("view")
                .expect("delivery exists");
            assert_eq!(view.row.state, DeliveryState::Refused);
            assert_eq!(view.row.refusal_reason, Some(RefusalReason::TargetClosed));
        }
    }

    #[test]
    fn given_a_row_already_claimed_before_refusal_when_settled_then_it_is_dropped_from_remaining() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = create_gateway_store(GatewayStoreOptions::new(
            dir.path().to_string_lossy().into_owned(),
        ))
        .expect("store");
        let me = store.identity().expect("identity");
        store
            .enqueue(&enqueue_at("t-race", "d-raced", 1_000))
            .expect("enqueue");
        let claimed = store
            .claim(&ClaimRequest {
                now: 1_500,
                delivery_id: "d-raced".into(),
                target_durable_id: "t-race".into(),
                effective_lane: EffectiveLane::FollowUp,
                expected_turn_id: None,
                claimant_incarnation: "inc-1".into(),
                claimant_pid: me.pid,
                claimant_start_time: me.start_time,
            })
            .expect("claim");
        assert!(matches!(claimed, ClaimOutcome::Claimed { .. }));

        let remaining =
            settle_closed_intake(store.as_ref(), "t-race", Some(2_000)).expect("settle");
        assert!(remaining.is_empty());
    }
}
