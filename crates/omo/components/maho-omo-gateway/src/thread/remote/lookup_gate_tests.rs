// allow: SIZE_OK — complete port of upstream lookup-gate.test.ts (E16..E46 and 7x16 pairwise interleavings)
//! Tests for `lookup_gate.rs` (ported from `remote/port/lookup-gate.test.ts`, omo #14748).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use futures::channel::oneshot;

use super::*;

#[derive(Default)]
struct RigOptions {
    capped: Vec<&'static str>,
    rejects: Vec<&'static str>,
    throws: Option<&'static str>,
    fence_wait_ms: Option<u64>,
}

struct Rig {
    gate: LookupGate,
    started: Arc<Mutex<Vec<(String, usize)>>>,
    finishers: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Rig {
    fn new(options: RigOptions) -> Self {
        let in_flight = Arc::new(Mutex::new(0usize));
        let started = Arc::new(Mutex::new(Vec::new()));
        let finishers: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let in_flight_c = Arc::clone(&in_flight);
        let started_c = Arc::clone(&started);
        let finishers_c = Arc::clone(&finishers);
        let capped: Vec<String> = options.capped.iter().map(|s| (*s).to_owned()).collect();
        let rejects: Vec<String> = options.rejects.iter().map(|s| (*s).to_owned()).collect();
        let throws = options.throws.map(str::to_owned);
        let fence_wait = Duration::from_millis(options.fence_wait_ms.unwrap_or(5_000));

        let gate = LookupGate::try_new(
            move |device, _durable| {
                if throws.as_deref() == Some(device) {
                    return Err(format!("start failed for {device}"));
                }
                let current = {
                    let mut count = lock(&in_flight_c);
                    *count += 1;
                    *count
                };
                lock(&started_c).push((device.to_owned(), current));
                let (tx, rx) = oneshot::channel::<bool>();
                lock(&finishers_c).insert(device.to_owned(), tx);

                let is_capped = capped.iter().any(|c| c == device);
                let is_reject = rejects.iter().any(|r| r == device);
                let in_flight_done = Arc::clone(&in_flight_c);
                let dev = device.to_owned();
                let (ans_tx, ans_rx) = oneshot::channel::<RemoteLookupOutcome>();

                let settled: BoxFuture<()> = Box::pin(async move {
                    let ok = rx.await.unwrap_or(false) && !is_reject;
                    *lock(&in_flight_done) -= 1;
                    if !is_capped {
                        let outcome = if ok {
                            RemoteLookupOutcome::Found {
                                key: format!("key-{dev}"),
                            }
                        } else {
                            RemoteLookupOutcome::Silent {
                                reason: "locate_failed".to_owned(),
                            }
                        };
                        let _ = ans_tx.send(outcome);
                    }
                });

                let answer: BoxFuture<RemoteLookupOutcome> = if is_capped {
                    Box::pin(async {
                        RemoteLookupOutcome::Silent {
                            reason: "lookup_timeout".to_owned(),
                        }
                    })
                } else {
                    Box::pin(async move {
                        ans_rx.await.unwrap_or(RemoteLookupOutcome::Silent {
                            reason: "locate_failed".to_owned(),
                        })
                    })
                };

                Ok(LookupStart { answer, settled })
            },
            fence_wait,
        );

        Self {
            gate,
            started,
            finishers,
        }
    }

    fn started_devices(&self) -> Vec<String> {
        lock(&self.started).iter().map(|(d, _)| d.clone()).collect()
    }

    fn started_entries(&self) -> Vec<(String, usize)> {
        lock(&self.started).clone()
    }

    fn finish(&self, device: &str) {
        if let Some(tx) = lock(&self.finishers).remove(device) {
            let _ = tx.send(true);
        }
    }
}

fn noop_waker() -> Waker {
    futures::task::noop_waker()
}

fn step<F: Future + Unpin>(fut: &mut F) -> Option<F::Output> {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    match Pin::new(fut).poll(&mut cx) {
        Poll::Ready(val) => Some(val),
        Poll::Pending => None,
    }
}

fn run_test<F: Future<Output = ()>>(future: F) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future);
}

#[test]
fn e16_two_callers_asking_alone_at_once_run_one_after_the_other() {
    run_test(async {
        let rig = Rig::new(RigOptions::default());
        let mut first = Box::pin(rig.gate.lookup_alone("c", "id", 5_000));
        let mut second = Box::pin(rig.gate.lookup_alone("d", "id", 5_000));

        assert!(step(&mut first).is_none());
        assert!(step(&mut second).is_none());
        assert_eq!(rig.started_devices(), vec!["c"]);

        rig.finish("c");
        assert_eq!(
            step(&mut first),
            Some(Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            }))
        );
        assert!(step(&mut second).is_none());
        assert_eq!(rig.started_devices(), vec!["c", "d"]);

        rig.finish("d");
        assert_eq!(
            step(&mut second),
            Some(Some(RemoteLookupOutcome::Found {
                key: "key-d".into()
            }))
        );
        let counts: Vec<usize> = rig.started_entries().into_iter().map(|(_, c)| c).collect();
        assert_eq!(counts, vec![1, 1]);
    });
}

#[test]
fn e17_e31_earlier_locate_still_running_past_its_cap_holds_solo_lookup_until_finished() {
    run_test(async {
        let rig = Rig::new(RigOptions {
            capped: vec!["b"],
            ..RigOptions::default()
        });
        assert_eq!(
            rig.gate.lookup("b", "id").await,
            RemoteLookupOutcome::Silent {
                reason: "lookup_timeout".into()
            }
        );
        let mut alone = Box::pin(rig.gate.lookup_alone("c", "id", 5_000));
        assert!(step(&mut alone).is_none());
        assert_eq!(rig.started_devices(), vec!["b"]);

        rig.finish("b");
        assert!(step(&mut alone).is_none());
        assert_eq!(rig.started_devices(), vec!["b", "c"]);

        rig.finish("c");
        assert_eq!(
            step(&mut alone),
            Some(Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            }))
        );
        let c_in_flight = rig
            .started_entries()
            .into_iter()
            .find(|(d, _)| d == "c")
            .map(|(_, c)| c);
        assert_eq!(c_in_flight, Some(1));
    });
}

#[test]
fn e18_plain_lookup_waits_while_solo_lookup_is_in_flight() {
    run_test(async {
        let rig = Rig::new(RigOptions::default());
        let mut alone = Box::pin(rig.gate.lookup_alone("c", "id", 5_000));
        assert!(step(&mut alone).is_none());
        assert_eq!(rig.started_devices(), vec!["c"]);

        let mut plain = Box::pin(rig.gate.lookup("e", "id"));
        assert!(step(&mut plain).is_none());
        assert_eq!(rig.started_devices(), vec!["c"]);

        rig.finish("c");
        assert_eq!(
            step(&mut alone),
            Some(Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            }))
        );
        assert!(step(&mut plain).is_none());
        assert_eq!(rig.started_devices(), vec!["c", "e"]);

        rig.finish("e");
        assert_eq!(
            step(&mut plain),
            Some(RemoteLookupOutcome::Found {
                key: "key-e".into()
            })
        );
    });
}

#[test]
fn e19_earlier_locate_that_does_not_finish_within_wait_ms_causes_solo_to_send_nothing() {
    run_test(async {
        let rig = Rig::new(RigOptions {
            capped: vec!["b"],
            ..RigOptions::default()
        });
        rig.gate.lookup("b", "id").await;
        assert_eq!(rig.gate.lookup_alone("c", "id", 30).await, None);
        assert_eq!(rig.started_devices(), vec!["b"]);

        let mut plain = Box::pin(rig.gate.lookup("e", "id"));
        assert!(step(&mut plain).is_none());
        assert_eq!(rig.started_devices(), vec!["b", "e"]);
        rig.finish("e");
        assert_eq!(
            plain.await,
            RemoteLookupOutcome::Found {
                key: "key-e".into()
            }
        );
    });
}

#[test]
fn e20_solo_lookup_giving_up_in_queue_still_leaves_next_waiting_for_first() {
    run_test(async {
        let rig = Rig::new(RigOptions::default());
        let mut first = Box::pin(rig.gate.lookup_alone("a", "id", 5_000));
        assert!(step(&mut first).is_none());
        assert_eq!(rig.started_devices(), vec!["a"]);

        assert_eq!(rig.gate.lookup_alone("b", "id", 30).await, None);

        let mut third = Box::pin(rig.gate.lookup_alone("c", "id", 5_000));
        assert!(step(&mut third).is_none());
        assert_eq!(rig.started_devices(), vec!["a"]);

        rig.finish("a");
        assert_eq!(
            step(&mut first),
            Some(Some(RemoteLookupOutcome::Found {
                key: "key-a".into()
            }))
        );
        assert!(step(&mut third).is_none());
        assert_eq!(rig.started_devices(), vec!["a", "c"]);

        rig.finish("c");
        assert_eq!(
            third.await,
            Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            })
        );
        assert!(!rig.started_devices().contains(&"b".to_owned()));
    });
}

#[test]
fn e21_earlier_locate_that_fails_still_lets_solo_lookup_proceed() {
    run_test(async {
        let rig = Rig::new(RigOptions {
            capped: vec!["b"],
            rejects: vec!["b"],
            ..RigOptions::default()
        });
        rig.gate.lookup("b", "id").await;
        let mut alone = Box::pin(rig.gate.lookup_alone("c", "id", 5_000));
        assert!(step(&mut alone).is_none());
        rig.finish("b");
        assert!(step(&mut alone).is_none());
        rig.finish("c");
        assert_eq!(
            alone.await,
            Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            })
        );
    });
}

#[test]
fn e29_nothing_in_flight_with_zero_wait_ms_sends_at_once() {
    run_test(async {
        let rig = Rig::new(RigOptions::default());
        let mut alone = Box::pin(rig.gate.lookup_alone("c", "id", 0));
        assert!(step(&mut alone).is_none());
        assert_eq!(rig.started_devices(), vec!["c"]);
        rig.finish("c");
        assert_eq!(
            alone.await,
            Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            })
        );
    });
}

#[test]
fn e32_locate_that_cannot_be_started_fails_and_holds_nothing_back() {
    run_test(async {
        let rig = Rig::new(RigOptions {
            throws: Some("c"),
            ..RigOptions::default()
        });
        let err = rig
            .gate
            .try_lookup_alone("c", "id", 5_000)
            .await
            .expect_err("start fails");
        assert_eq!(err, "start failed for c");

        let mut plain = Box::pin(rig.gate.lookup("e", "id"));
        assert!(step(&mut plain).is_none());
        assert_eq!(rig.started_devices(), vec!["e"]);
        rig.finish("e");
        assert_eq!(
            plain.await,
            RemoteLookupOutcome::Found {
                key: "key-e".into()
            }
        );
    });
}

#[test]
fn e40_e41_e42_e43_capped_solo_locate_holds_fence_until_settled_or_fence_wait_expires() {
    run_test(async {
        // E40 & E43: plain and next solo wait until capped solo locate settles
        let rig = Rig::new(RigOptions {
            capped: vec!["c"],
            rejects: vec!["c"],
            ..RigOptions::default()
        });
        assert_eq!(
            rig.gate.lookup_alone("c", "id", 5_000).await,
            Some(RemoteLookupOutcome::Silent {
                reason: "lookup_timeout".into()
            })
        );
        let mut plain = Box::pin(rig.gate.lookup("e", "id"));
        let mut next_solo = Box::pin(rig.gate.lookup_alone("d", "id", 5_000));
        assert!(step(&mut plain).is_none());
        assert!(step(&mut next_solo).is_none());
        assert_eq!(rig.started_devices(), vec!["c"]);

        rig.finish("c");
        assert!(step(&mut plain).is_none());
        assert_eq!(rig.started_devices(), vec!["c", "e"]);
        rig.finish("e");
        assert_eq!(
            plain.await,
            RemoteLookupOutcome::Found {
                key: "key-e".into()
            }
        );

        assert!(step(&mut next_solo).is_none());
        assert_eq!(rig.started_devices(), vec!["c", "e", "d"]);
        rig.finish("d");
        assert_eq!(
            next_solo.await,
            Some(RemoteLookupOutcome::Found {
                key: "key-d".into()
            })
        );

        // E42: fence timeout on plain lookup
        let timeout_rig = Rig::new(RigOptions {
            capped: vec!["c"],
            fence_wait_ms: Some(30),
            ..RigOptions::default()
        });
        timeout_rig.gate.lookup_alone("c", "id", 5_000).await;
        assert_eq!(
            timeout_rig.gate.lookup("e", "id").await,
            RemoteLookupOutcome::Silent {
                reason: "lookup_timeout".into()
            }
        );
        assert_eq!(timeout_rig.started_devices(), vec!["c"]);
    });
}

#[test]
fn e44_e45_e46_admitted_locates_respect_solo_fence_and_refuse_on_fence_timeout() {
    run_test(async {
        // E44: admitted locate waits for capped solo to settle
        let rig = Rig::new(RigOptions {
            capped: vec!["c"],
            ..RigOptions::default()
        });
        rig.gate.lookup_alone("c", "id", 5_000).await;
        let dispatched = Arc::new(Mutex::new(false));
        let dispatched_c = Arc::clone(&dispatched);
        let mut admitted = Box::pin(rig.gate.admit(move || async move {
            *lock(&dispatched_c) = true;
            "key-x"
        }));
        assert!(step(&mut admitted).is_none());
        assert!(!*lock(&dispatched));
        rig.finish("c");
        assert_eq!(admitted.await, Ok("key-x"));
        assert!(*lock(&dispatched));

        // E45: solo lookup waits for admitted locate in flight
        let rig2 = Rig::new(RigOptions::default());
        let (tx, rx) = oneshot::channel::<()>();
        let mut other = Box::pin(rig2.gate.admit(move || async move {
            let _ = rx.await;
        }));
        assert!(step(&mut other).is_none());
        let mut alone = Box::pin(rig2.gate.lookup_alone("c", "id", 5_000));
        assert!(step(&mut alone).is_none());
        assert!(rig2.started_devices().is_empty());
        let _ = tx.send(());
        assert_eq!(other.await, Ok(()));
        assert!(step(&mut alone).is_none());
        assert_eq!(rig2.started_devices(), vec!["c"]);
        rig2.finish("c");
        assert_eq!(
            alone.await,
            Some(RemoteLookupOutcome::Found {
                key: "key-c".into()
            })
        );

        // E46: admitted locate is refused with LookupFenceRefused when solo never settles
        let rig3 = Rig::new(RigOptions {
            capped: vec!["c"],
            fence_wait_ms: Some(30),
            ..RigOptions::default()
        });
        rig3.gate.lookup_alone("c", "id", 5_000).await;
        let ran = Arc::new(Mutex::new(false));
        let ran_c = Arc::clone(&ran);
        let res = rig3
            .gate
            .admit(move || async move {
                *lock(&ran_c) = true;
            })
            .await;
        assert_eq!(res, Err(LookupFenceRefused));
        assert!(!*lock(&ran));
    });
}

#[derive(Clone, Copy)]
enum Op {
    Solo,
    Lookup,
    Admit,
}

async fn interleave(first: Op, second: Op, offset: usize) -> Vec<String> {
    let violations = Arc::new(Mutex::new(Vec::<String>::new()));
    let running: Arc<Mutex<Vec<(String, oneshot::Sender<()>)>>> = Arc::new(Mutex::new(Vec::new()));
    let solo_running = Arc::new(Mutex::new(0usize));

    let begin = {
        let violations = Arc::clone(&violations);
        let running = Arc::clone(&running);
        let solo_running = Arc::clone(&solo_running);
        Arc::new(move |device: String| -> BoxFuture<()> {
            let is_solo = device.starts_with('s');
            let active_names: Vec<String> = lock(&running).iter().map(|(d, _)| d.clone()).collect();
            if is_solo && !active_names.is_empty() {
                lock(&violations).push(format!(
                    "{device} started with {} in flight",
                    active_names.join(",")
                ));
            }
            if !is_solo && *lock(&solo_running) > 0 {
                lock(&violations).push(format!("{device} started while a solo locate ran"));
            }
            if is_solo {
                *lock(&solo_running) += 1;
            }
            let (tx, rx) = oneshot::channel::<()>();
            lock(&running).push((device.clone(), tx));
            let running_done = Arc::clone(&running);
            let solo_done = Arc::clone(&solo_running);
            Box::pin(async move {
                let _ = rx.await;
                lock(&running_done).retain(|(d, _)| d != &device);
                if is_solo {
                    *lock(&solo_done) -= 1;
                }
            })
        })
    };

    let begin_gate = Arc::clone(&begin);
    let gate = LookupGate::new(
        move |device, _durable| {
            let settled = begin_gate(device.to_owned());
            let dev = device.to_owned();
            let (tx, rx) = oneshot::channel();
            let settled_wrap: BoxFuture<()> = Box::pin(async move {
                settled.await;
                let _ = tx.send(());
            });
            let answer: BoxFuture<RemoteLookupOutcome> = Box::pin(async move {
                let _ = rx.await;
                RemoteLookupOutcome::Found {
                    key: format!("key-{dev}"),
                }
            });
            LookupStart {
                answer,
                settled: settled_wrap,
            }
        },
        Duration::from_secs(5),
    );

    let make_op = |op: Op, suffix: &'static str| -> BoxFuture<()> {
        let gate = gate.clone();
        let begin = Arc::clone(&begin);
        match op {
            Op::Solo => Box::pin(async move {
                let _ = gate.lookup_alone(&format!("s{suffix}"), "id", 5_000).await;
            }),
            Op::Lookup => Box::pin(async move {
                let _ = gate.lookup(&format!("p{suffix}"), "id").await;
            }),
            Op::Admit => Box::pin(async move {
                let _ = gate.admit(move || begin(format!("a{suffix}"))).await;
            }),
        }
    };

    let mut a = Some(make_op(first, "1"));
    for _ in 0..offset {
        if let Some(fut) = a.as_mut()
            && step(fut).is_some()
        {
            a = None;
        }
    }
    let mut b = Some(make_op(second, "2"));
    for _ in 0..200 {
        if a.is_none() && b.is_none() {
            break;
        }
        if let Some(fut) = a.as_mut()
            && step(fut).is_some()
        {
            a = None;
        }
        if let Some(fut) = b.as_mut()
            && step(fut).is_some()
        {
            b = None;
        }
        let finishers: Vec<oneshot::Sender<()>> =
            lock(&running).drain(..).map(|(_, tx)| tx).collect();
        for tx in finishers {
            let _ = tx.send(());
        }
    }
    let out = lock(&violations).clone();
    out
}

#[test]
fn gate_admission_is_atomic_under_every_interleaving() {
    run_test(async {
        let pairs = [
            (Op::Lookup, Op::Solo),
            (Op::Solo, Op::Lookup),
            (Op::Admit, Op::Solo),
            (Op::Solo, Op::Admit),
            (Op::Admit, Op::Admit),
            (Op::Solo, Op::Solo),
            (Op::Lookup, Op::Admit),
        ];
        for (first, second) in pairs {
            for offset in 0..=15 {
                let violations = interleave(first, second, offset).await;
                assert!(violations.is_empty(), "offset={offset}: {violations:?}");
            }
        }
    });
}
