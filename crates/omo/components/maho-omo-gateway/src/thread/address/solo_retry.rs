//! A device the parallel probe left silent is asked again alone (omo `thread/address/solo-retry.ts`, #14748).
//!
//! Probes in flight together can hold a device past the cap it answers in time alone. The port sends a solo locate only
//! while no other locate of the port is in flight and holds every other lookup back meanwhile (`lookup_alone`), so
//! concurrent calls never overlap their retries. A retry is asked only while the call's budget still holds a lookup and
//! the reserve the step after the resolution needs; a device it could not ask alone keeps its silent answer, so the
//! refusal still names it.

use crate::thread::remote::ports::{RemoteLookupOutcome, RemoteSessionsPort};

/// One solo lookup's own bound: the client's cap on a single-probe locate.
pub const SOLO_LOOKUP_MS: i64 = 3_500;

/// Each `(device, answer)` of the parallel round, with every silent answer asked again alone, in order.
pub async fn ask_silent_alone(
    sessions: &dyn RemoteSessionsPort,
    thread: &str,
    answers: Vec<(String, RemoteLookupOutcome)>,
) -> Vec<(String, RemoteLookupOutcome)> {
    let mut settled = Vec::with_capacity(answers.len());
    for (device, answer) in answers {
        let again = if matches!(answer, RemoteLookupOutcome::Silent { .. }) {
            alone(sessions, &device, thread).await
        } else {
            None
        };
        // A retry the port could not make (`Unavailable`) says nothing about the device: its silent answer stands.
        let answer = match again {
            Some(again) if again != RemoteLookupOutcome::Unavailable => again,
            _ => answer,
        };
        settled.push((device, answer));
    }
    settled
}

async fn alone(
    sessions: &dyn RemoteSessionsPort,
    device: &str,
    thread: &str,
) -> Option<RemoteLookupOutcome> {
    let room = call_budget::wait_within_call_budget(
        i64::MAX,
        call_budget::READ_RESERVE_MS,
        call_budget::now_ms(),
    );
    if room < SOLO_LOOKUP_MS {
        return None;
    }
    sessions
        .lookup_alone(
            device.to_owned(),
            thread.to_owned(),
            SOLO_LOOKUP_MS.min(room - SOLO_LOOKUP_MS),
        )
        .await
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, PoisonError};

    use futures::executor::block_on;

    use super::*;
    use crate::thread::adapter::BoxFuture;
    use crate::thread::remote::ports::{RemoteLocateOutcome, RemoteSessionRow};

    struct Solo {
        answers: Vec<(&'static str, Option<RemoteLookupOutcome>)>,
        asked: Mutex<Vec<(String, i64)>>,
    }

    impl RemoteSessionsPort for Solo {
        fn sessions(
            &self,
            _device: Option<String>,
        ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn locate(&self, _device: String, _engine_id: String) -> BoxFuture<RemoteLocateOutcome> {
            Box::pin(async {
                RemoteLocateOutcome::Refused {
                    reason: "unknown_session".to_owned(),
                }
            })
        }

        fn lookup_alone(
            &self,
            device: String,
            _durable_id: String,
            timeout_ms: i64,
        ) -> BoxFuture<Option<RemoteLookupOutcome>> {
            let answer = self
                .answers
                .iter()
                .find(|(id, _)| *id == device)
                .and_then(|(_, answer)| answer.clone());
            self.asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((device, timeout_ms));
            Box::pin(async move { answer })
        }
    }

    fn silent() -> RemoteLookupOutcome {
        RemoteLookupOutcome::Silent {
            reason: "timeout".to_owned(),
        }
    }

    #[test]
    fn only_a_silent_device_is_asked_again_and_an_unmade_retry_keeps_its_silent_answer() {
        let port = Solo {
            answers: vec![
                (
                    "b",
                    Some(RemoteLookupOutcome::Found {
                        key: "rk-b".to_owned(),
                    }),
                ),
                ("c", Some(RemoteLookupOutcome::Unavailable)),
                ("d", None),
            ],
            asked: Mutex::new(Vec::new()),
        };
        let settled = block_on(ask_silent_alone(
            &port,
            "t-1",
            vec![
                ("a".to_owned(), RemoteLookupOutcome::NotHeld),
                ("b".to_owned(), silent()),
                ("c".to_owned(), silent()),
                ("d".to_owned(), silent()),
            ],
        ));
        assert_eq!(
            settled,
            vec![
                ("a".to_owned(), RemoteLookupOutcome::NotHeld),
                (
                    "b".to_owned(),
                    RemoteLookupOutcome::Found {
                        key: "rk-b".to_owned()
                    }
                ),
                ("c".to_owned(), silent()),
                ("d".to_owned(), silent()),
            ]
        );
        let asked = port
            .asked
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            asked,
            vec![
                ("b".to_owned(), SOLO_LOOKUP_MS),
                ("c".to_owned(), SOLO_LOOKUP_MS),
                ("d".to_owned(), SOLO_LOOKUP_MS),
            ]
        );
    }

    #[test]
    fn a_port_that_cannot_ask_alone_leaves_every_answer_as_it_was() {
        struct Plain;
        impl RemoteSessionsPort for Plain {
            fn sessions(
                &self,
                _device: Option<String>,
            ) -> BoxFuture<Result<Vec<RemoteSessionRow>, String>> {
                Box::pin(async { Ok(Vec::new()) })
            }

            fn locate(
                &self,
                _device: String,
                _engine_id: String,
            ) -> BoxFuture<RemoteLocateOutcome> {
                Box::pin(async {
                    RemoteLocateOutcome::Refused {
                        reason: "unknown_session".to_owned(),
                    }
                })
            }
        }
        let answers = vec![("b".to_owned(), silent())];
        assert_eq!(
            block_on(ask_silent_alone(&Plain, "t-1", answers.clone())),
            answers
        );
    }
}
