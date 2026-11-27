//! Waiting for a create's first prompt to start on this device.
//!
//! Upstream counterpart: `thread/control/prompt-start.ts` (`awaitPromptStart`). The wait polls the delivery's row until
//! the receiving drain admits it; the clock and the pause between reads are seams, so a caller that owns neither (a test)
//! never sleeps.

use crate::thread::adapter::BoxFuture;
use crate::thread::contracts_results::ThreadDelivery;
use crate::thread::result::admitted_wait;
use crate::thread::store_ops::{DeliveryView, StoreError};
use crate::thread::types::{DeliveryState, DeliveryWait};

/// How long a create waits for its first prompt to start on this device when the send answered it not started yet (a new
/// session whose control endpoint has not drained its inbox, or one the carrier has to wake). Cut to what is left of the
/// call's budget (`wait_within_call_budget`), so the create still answers inside the call.
pub const FIRST_PROMPT_START_WAIT_MS: i64 = 20_000;
/// What is kept of the call's budget for the create's own answer and receipt after the wait.
pub const FIRST_PROMPT_START_RESERVE_MS: i64 = 2_000;
const RECHECK_MS: i64 = 250;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptStart {
    Started {
        delivery: ThreadDelivery,
    },
    /// Admitted to the session's queue (behind a turn, a question or a hold): it runs after what it waits on.
    Queued {
        waiting_on: Option<DeliveryWait>,
    },
    NotStarted {
        state: String,
    },
}

/// Waits up to `wait_ms` for a gateway delivery on this device to be admitted: as `started` or `steered` (a turn runs
/// it), or `queued`/`held` (the session took it into its queue, #14363 review M1), each answered at once. A refused or
/// uncertain row ends the wait at once; so does a row that is gone. `read` is the store's `delivery_view`.
pub async fn await_prompt_start<Read, Now, Delay>(
    read: Read,
    delivery_id: &str,
    wait_ms: i64,
    now: Now,
    delay: Delay,
) -> Result<PromptStart, StoreError>
where
    Read: Fn(&str) -> Result<Option<DeliveryView>, StoreError>,
    Now: Fn() -> i64,
    Delay: Fn(u64) -> BoxFuture<()>,
{
    let deadline = now() + wait_ms;
    loop {
        let Some(view) = read(delivery_id)? else {
            return Ok(PromptStart::NotStarted {
                state: "missing".to_owned(),
            });
        };
        let row = view.row;
        let admitted = matches!(row.state, DeliveryState::Admitted | DeliveryState::Applied);
        let kind = row.admission_kind.as_deref();
        if admitted && matches!(kind, Some("started") | Some("steered")) {
            let turn_id = row.turn_epoch.unwrap_or(0).to_string();
            let delivery = if kind == Some("started") {
                ThreadDelivery::Started { turn_id }
            } else {
                ThreadDelivery::Steered { turn_id }
            };
            return Ok(PromptStart::Started { delivery });
        }
        if admitted && matches!(kind, Some("queued") | Some("held")) {
            return Ok(PromptStart::Queued {
                waiting_on: admitted_wait(&row),
            });
        }
        if matches!(row.state, DeliveryState::Refused | DeliveryState::Uncertain) {
            return Ok(PromptStart::NotStarted {
                state: row.state.as_str().to_owned(),
            });
        }
        let left = deadline - now();
        if left <= 0 {
            return Ok(PromptStart::NotStarted {
                state: row.state.as_str().to_owned(),
            });
        }
        delay(left.min(RECHECK_MS) as u64).await;
    }
}

/// A pause of `milliseconds` on its own thread, so the waiting task never blocks an executor thread.
pub fn thread_delay(milliseconds: u64) -> BoxFuture<()> {
    let (sender, receiver) = futures::channel::oneshot::channel::<()>();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
        let _ = sender.send(());
    });
    Box::pin(async move {
        let _ = receiver.await;
    })
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use futures::executor::block_on;

    use super::*;
    use crate::thread::types::{
        DeliveryRow, EnvelopeOrigin, GatewayDeliveryMode, StoredDeliveryEnvelope,
    };

    fn row(
        state: DeliveryState,
        admission_kind: Option<&str>,
        wait_kind: Option<&str>,
    ) -> DeliveryView {
        DeliveryView {
            queue_position: 1,
            row: DeliveryRow {
                delivery_id: "d-1".into(),
                target_durable_id: "dur-new".into(),
                seq: 1,
                sender: "session:dur-caller".into(),
                sender_turn: None,
                envelope: StoredDeliveryEnvelope {
                    provenance: None,
                    origin: EnvelopeOrigin::Session {
                        session: "dur-caller".into(),
                    },
                    actor: "caller".into(),
                    via: vec![],
                    root_id: "root".into(),
                    hop: 1,
                    delivery_id: "d-1".into(),
                },
                body: "build it".into(),
                bytes: 8,
                mode_requested: GatewayDeliveryMode::Auto,
                mode_effective: None,
                expected_turn_id: None,
                state,
                reason: None,
                admitted_by: None,
                claimed_at: None,
                attempt: 0,
                admission_kind: admission_kind.map(str::to_owned),
                wait_kind: wait_kind.map(str::to_owned),
                turn_epoch: Some(4),
                root_id: "root".into(),
                hop: 1,
                created_at: 1,
                updated_at: 1,
                expires_at: 9,
                ledger_id: None,
                verbatim_text: None,
                verbatim_display_text: None,
                collapse_key: None,
            },
        }
    }

    struct Seams {
        clock: Cell<i64>,
        pauses: RefCell<Vec<u64>>,
    }

    fn seams() -> Seams {
        Seams {
            clock: Cell::new(1_000),
            pauses: RefCell::new(Vec::new()),
        }
    }

    fn run(
        seams: &Seams,
        rows: Vec<Option<DeliveryView>>,
        wait_ms: i64,
    ) -> Result<PromptStart, StoreError> {
        let rows = RefCell::new(VecDeque::from(rows));
        let last = RefCell::new(None::<DeliveryView>);
        let read = |_id: &str| -> Result<Option<DeliveryView>, StoreError> {
            if let Some(next) = rows.borrow_mut().pop_front() {
                *last.borrow_mut() = next.clone();
                return Ok(next);
            }
            Ok(last.borrow().clone())
        };
        let now = || seams.clock.get();
        let delay = |milliseconds: u64| -> BoxFuture<()> {
            seams.pauses.borrow_mut().push(milliseconds);
            seams.clock.set(seams.clock.get() + milliseconds as i64);
            Box::pin(async {})
        };
        block_on(await_prompt_start(read, "d-1", wait_ms, now, delay))
    }

    #[test]
    fn an_admitted_start_or_steer_answers_at_once_with_its_turn() {
        let seams = seams();
        for (kind, expected) in [
            (
                "started",
                ThreadDelivery::Started {
                    turn_id: "4".into(),
                },
            ),
            (
                "steered",
                ThreadDelivery::Steered {
                    turn_id: "4".into(),
                },
            ),
        ] {
            let answered = run(
                &seams,
                vec![Some(row(DeliveryState::Applied, Some(kind), None))],
                20_000,
            )
            .expect("read");
            assert_eq!(answered, PromptStart::Started { delivery: expected });
        }
        assert!(seams.pauses.borrow().is_empty());
    }

    #[test]
    fn a_row_admitted_to_the_queue_answers_with_what_it_waits_on_once_the_drain_takes_it() {
        let seams = seams();
        let answered = run(
            &seams,
            vec![
                Some(row(DeliveryState::Queued, None, None)),
                Some(row(DeliveryState::Admitting, None, None)),
                Some(row(
                    DeliveryState::Admitted,
                    Some("queued"),
                    Some("approval"),
                )),
            ],
            20_000,
        )
        .expect("read");
        assert_eq!(
            answered,
            PromptStart::Queued {
                waiting_on: Some(DeliveryWait {
                    kind: "approval".into(),
                    device: None
                })
            }
        );
        assert_eq!(*seams.pauses.borrow(), vec![250, 250]);

        let plain = run(
            &seams,
            vec![Some(row(DeliveryState::Admitted, Some("queued"), None))],
            20_000,
        )
        .expect("read");
        assert_eq!(plain, PromptStart::Queued { waiting_on: None });

        let held = run(
            &seams,
            vec![Some(row(DeliveryState::Admitted, Some("held"), None))],
            20_000,
        )
        .expect("read");
        assert_eq!(
            held,
            PromptStart::Queued {
                waiting_on: Some(DeliveryWait {
                    kind: "hold".into(),
                    device: None
                })
            }
        );
    }

    #[test]
    fn a_row_nobody_drains_is_not_started_once_the_wait_ends_and_names_its_last_state() {
        let seams = seams();
        let answered = run(
            &seams,
            vec![Some(row(DeliveryState::Queued, None, None))],
            600,
        )
        .expect("read");
        assert_eq!(
            answered,
            PromptStart::NotStarted {
                state: "queued".into()
            }
        );
        assert_eq!(*seams.pauses.borrow(), vec![250, 250, 100]);
        assert_eq!(seams.clock.get(), 1_600);
    }

    #[test]
    fn a_refused_uncertain_or_missing_row_ends_the_wait_at_once() {
        let seams = seams();
        for (state, expected) in [
            (DeliveryState::Refused, "refused"),
            (DeliveryState::Uncertain, "uncertain"),
        ] {
            let answered = run(&seams, vec![Some(row(state, None, None))], 20_000).expect("read");
            assert_eq!(
                answered,
                PromptStart::NotStarted {
                    state: expected.into()
                }
            );
        }
        let missing = run(&seams, vec![None], 20_000).expect("read");
        assert_eq!(
            missing,
            PromptStart::NotStarted {
                state: "missing".into()
            }
        );
        assert!(seams.pauses.borrow().is_empty());
    }

    #[test]
    fn a_store_failure_is_the_waits_failure() {
        let failed = block_on(await_prompt_start(
            |_id: &str| -> Result<Option<DeliveryView>, StoreError> {
                Err(StoreError::Message("store is gone".into()))
            },
            "d-1",
            20_000,
            || 0,
            |_milliseconds: u64| -> BoxFuture<()> { Box::pin(async {}) },
        ));
        assert_eq!(failed.expect_err("failed").message(), "store is gone");
    }
}
