//! The delivery decision table.
//!
//! Upstream counterpart: `thread/gateway/decision.ts`. Question answers never travel through a
//! delivery, so a steer into a waiting question is `not_steerable`, like a steer into an idle or
//! compacting session.

use super::adapter::RuntimePhase;
use super::types::GatewayDeliveryMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    Phase(RuntimePhase),
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Started,
    Queued,
    Steered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDecision {
    Admit {
        deliver_as: GatewayDeliveryMode,
        expected_turn_id: Option<i64>,
        expect: Expect,
    },
    Refuse {
        reason: RefusalKind,
    },
    QueuedOffline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    NotSteerable,
    TurnConflict,
}

impl RefusalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSteerable => "not_steerable",
            Self::TurnConflict => "turn_conflict",
        }
    }
}

/// The 15-cell table {idle, mid_turn, waiting_question, compacting, offline} x {auto, steer, follow_up}.
///
/// `auto`, the default every send takes, steers the turn the target is running (omo #12921: a message to a busy
/// session reaches the turn in flight, as typing into its terminal does) and starts a turn when the target is idle; it
/// queues behind only what cannot be steered (a waiting question, a compaction, or a running turn whose epoch the
/// engine does not report), and `follow_up` queues on request.
pub fn decide_delivery(
    state: TargetState,
    mode: GatewayDeliveryMode,
    expected_turn_id: Option<i64>,
    turn_epoch: Option<i64>,
) -> DeliveryDecision {
    if state == TargetState::Offline {
        return if mode == GatewayDeliveryMode::Steer {
            DeliveryDecision::Refuse {
                reason: RefusalKind::TurnConflict,
            }
        } else {
            DeliveryDecision::QueuedOffline
        };
    }
    if mode == GatewayDeliveryMode::Steer {
        if state != TargetState::Phase(RuntimePhase::MidTurn) {
            return DeliveryDecision::Refuse {
                reason: RefusalKind::NotSteerable,
            };
        }
        if expected_turn_id.is_none() || turn_epoch.is_none() || expected_turn_id != turn_epoch {
            return DeliveryDecision::Refuse {
                reason: RefusalKind::TurnConflict,
            };
        }
        return DeliveryDecision::Admit {
            deliver_as: GatewayDeliveryMode::Steer,
            expected_turn_id,
            expect: Expect::Steered,
        };
    }
    if mode == GatewayDeliveryMode::Auto
        && state == TargetState::Phase(RuntimePhase::MidTurn)
        && turn_epoch.is_some()
    {
        return DeliveryDecision::Admit {
            deliver_as: GatewayDeliveryMode::Steer,
            expected_turn_id: turn_epoch,
            expect: Expect::Steered,
        };
    }
    let idle = state == TargetState::Phase(RuntimePhase::Idle);
    DeliveryDecision::Admit {
        deliver_as: GatewayDeliveryMode::FollowUp,
        expected_turn_id: None,
        expect: if idle {
            Expect::Started
        } else {
            Expect::Queued
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steer_needs_a_running_turn_at_the_expected_epoch() {
        let mid_turn = TargetState::Phase(RuntimePhase::MidTurn);
        assert_eq!(
            decide_delivery(mid_turn, GatewayDeliveryMode::Steer, Some(4), Some(4)),
            DeliveryDecision::Admit {
                deliver_as: GatewayDeliveryMode::Steer,
                expected_turn_id: Some(4),
                expect: Expect::Steered
            }
        );
        assert_eq!(
            decide_delivery(mid_turn, GatewayDeliveryMode::Steer, Some(4), Some(5)),
            DeliveryDecision::Refuse {
                reason: RefusalKind::TurnConflict
            }
        );
        assert_eq!(
            decide_delivery(mid_turn, GatewayDeliveryMode::Steer, None, Some(5)),
            DeliveryDecision::Refuse {
                reason: RefusalKind::TurnConflict
            }
        );
    }

    #[test]
    fn a_steer_into_a_waiting_question_is_not_steerable() {
        assert_eq!(
            decide_delivery(
                TargetState::Phase(RuntimePhase::WaitingQuestion),
                GatewayDeliveryMode::Steer,
                Some(1),
                Some(1)
            ),
            DeliveryDecision::Refuse {
                reason: RefusalKind::NotSteerable
            }
        );
        assert_eq!(
            decide_delivery(
                TargetState::Phase(RuntimePhase::Idle),
                GatewayDeliveryMode::Steer,
                Some(1),
                Some(1)
            ),
            DeliveryDecision::Refuse {
                reason: RefusalKind::NotSteerable
            }
        );
    }

    #[test]
    fn auto_and_follow_up_admit_in_every_live_state() {
        assert_eq!(
            decide_delivery(
                TargetState::Phase(RuntimePhase::Idle),
                GatewayDeliveryMode::Auto,
                None,
                None
            ),
            DeliveryDecision::Admit {
                deliver_as: GatewayDeliveryMode::FollowUp,
                expected_turn_id: None,
                expect: Expect::Started
            }
        );
        assert_eq!(
            decide_delivery(
                TargetState::Phase(RuntimePhase::Compacting),
                GatewayDeliveryMode::FollowUp,
                None,
                None
            ),
            DeliveryDecision::Admit {
                deliver_as: GatewayDeliveryMode::FollowUp,
                expected_turn_id: None,
                expect: Expect::Queued
            }
        );
    }

    fn decided(decision: DeliveryDecision) -> String {
        match decision {
            DeliveryDecision::Admit {
                deliver_as, expect, ..
            } => format!("admit:{}:{expect:?}", deliver_as.as_str()),
            DeliveryDecision::Refuse { reason } => format!("refuse:{}", reason.as_str()),
            DeliveryDecision::QueuedOffline => "queued_offline".to_owned(),
        }
    }

    #[test]
    fn every_cell_of_the_fifteen_cell_table() {
        let states = [
            ("idle", TargetState::Phase(RuntimePhase::Idle)),
            ("mid_turn", TargetState::Phase(RuntimePhase::MidTurn)),
            (
                "waiting_question",
                TargetState::Phase(RuntimePhase::WaitingQuestion),
            ),
            ("compacting", TargetState::Phase(RuntimePhase::Compacting)),
            ("offline", TargetState::Offline),
        ];
        let modes = [
            GatewayDeliveryMode::Auto,
            GatewayDeliveryMode::Steer,
            GatewayDeliveryMode::FollowUp,
        ];
        let expected = [
            [
                "admit:follow_up:Started",
                "refuse:not_steerable",
                "admit:follow_up:Started",
            ],
            [
                "admit:steer:Steered",
                "admit:steer:Steered",
                "admit:follow_up:Queued",
            ],
            [
                "admit:follow_up:Queued",
                "refuse:not_steerable",
                "admit:follow_up:Queued",
            ],
            [
                "admit:follow_up:Queued",
                "refuse:not_steerable",
                "admit:follow_up:Queued",
            ],
            ["queued_offline", "refuse:turn_conflict", "queued_offline"],
        ];
        for ((label, state), row) in states.into_iter().zip(expected) {
            for (mode, cell) in modes.into_iter().zip(row) {
                assert_eq!(
                    decided(decide_delivery(state, mode, Some(3), Some(3))),
                    cell,
                    "{label} x {}",
                    mode.as_str()
                );
            }
        }
    }

    #[test]
    fn auto_steers_a_running_turn_by_its_own_epoch_whatever_the_sender_named() {
        let mid_turn = TargetState::Phase(RuntimePhase::MidTurn);
        for named in [None, Some(2)] {
            assert_eq!(
                decide_delivery(mid_turn, GatewayDeliveryMode::Auto, named, Some(7)),
                DeliveryDecision::Admit {
                    deliver_as: GatewayDeliveryMode::Steer,
                    expected_turn_id: Some(7),
                    expect: Expect::Steered
                }
            );
        }
    }

    #[test]
    fn auto_queues_behind_a_running_turn_whose_epoch_is_unknown() {
        assert_eq!(
            decide_delivery(
                TargetState::Phase(RuntimePhase::MidTurn),
                GatewayDeliveryMode::Auto,
                None,
                None
            ),
            DeliveryDecision::Admit {
                deliver_as: GatewayDeliveryMode::FollowUp,
                expected_turn_id: None,
                expect: Expect::Queued
            }
        );
    }

    #[test]
    fn an_offline_target_queues_a_send_and_refuses_a_steer() {
        assert_eq!(
            decide_delivery(TargetState::Offline, GatewayDeliveryMode::Auto, None, None),
            DeliveryDecision::QueuedOffline
        );
        assert_eq!(
            decide_delivery(
                TargetState::Offline,
                GatewayDeliveryMode::Steer,
                Some(1),
                Some(1)
            ),
            DeliveryDecision::Refuse {
                reason: RefusalKind::TurnConflict
            }
        );
    }
}
