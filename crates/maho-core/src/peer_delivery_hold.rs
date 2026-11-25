//! Port of senpi `packages/coding-agent/src/core/peer-delivery-hold.ts` (senpi#7014 / #6490).
//!
//! Hold kinds on an idle session that keep a peer delivery from starting a turn right away
//! (a person's Stop, an interrupted run, or a parent run hold). `run_fence` never blocks peer
//! deliveries, and an `unattended`-only hold is released by a peer delivery.

use std::collections::BTreeSet;

use crate::engine_turn_hold::{HoldKind, SessionHold};

pub fn holds_keeping_peer_delivery(holds: &[SessionHold], interrupted_run: bool) -> Vec<String> {
    let mut kinds: BTreeSet<&'static str> = holds
        .iter()
        .filter(|hold| hold.kind != HoldKind::RunFence)
        .map(|hold| hold.kind.as_str())
        .collect();
    if interrupted_run {
        kinds.insert(HoldKind::SessionHold.as_str());
    }
    if kinds
        .iter()
        .any(|kind| *kind != HoldKind::Unattended.as_str())
    {
        kinds.into_iter().map(str::to_owned).collect()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn hold(kind: HoldKind, key: &str) -> SessionHold {
        SessionHold {
            kind,
            key: key.to_owned(),
            since: "2026-11-19T00:00:00Z".to_owned(),
            data: None,
            source_counts: BTreeMap::new(),
        }
    }

    #[test]
    fn unattended_and_run_fence_holds_alone_do_not_keep_a_peer_delivery_held() {
        assert!(holds_keeping_peer_delivery(&[], false).is_empty());
        assert!(
            holds_keeping_peer_delivery(&[hold(HoldKind::Unattended, "unattended")], false)
                .is_empty()
        );
        assert!(
            holds_keeping_peer_delivery(
                &[
                    hold(HoldKind::Unattended, "unattended"),
                    hold(HoldKind::RunFence, "run:1"),
                ],
                false,
            )
            .is_empty()
        );
    }

    #[test]
    fn stop_interrupted_run_and_inherited_holds_are_reported_sorted() {
        assert_eq!(
            holds_keeping_peer_delivery(
                &[
                    hold(HoldKind::Unattended, "unattended"),
                    hold(HoldKind::SessionHold, "session_hold:1"),
                    hold(HoldKind::RunFence, "run:1"),
                ],
                false,
            ),
            vec!["session_hold".to_owned(), "unattended".to_owned()]
        );
        assert_eq!(
            holds_keeping_peer_delivery(&[hold(HoldKind::Unattended, "unattended")], true),
            vec!["session_hold".to_owned(), "unattended".to_owned()]
        );
        assert_eq!(
            holds_keeping_peer_delivery(&[hold(HoldKind::InheritedHold, "parent")], false),
            vec!["inherited_hold".to_owned()]
        );
    }
}
