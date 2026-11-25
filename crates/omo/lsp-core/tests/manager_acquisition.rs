//! Translation of `manager-acquisition.test.ts`.

use crate::support::controlled_clock::after_yields;
use crate::support::controlled_clock::within;
use crate::support::manager_test_support::GatedManagerOptions;
use crate::support::manager_test_support::MICROTASK_SWEEP;
use crate::support::manager_test_support::entry;
use crate::support::manager_test_support::gated_manager;
use crate::support::manager_test_support::same_client;
use crate::support::manager_test_support::server;
use futures::FutureExt;
use lsp_core::abort::AbortController;
use lsp_core::lsp::errors::LspError;
use lsp_core::lsp::manager::SharedClient;
use pretty_assertions::assert_eq;
use std::sync::Arc;

fn holders(outcomes: &[&Result<SharedClient, LspError>]) -> usize {
    outcomes.iter().filter(|outcome| outcome.is_ok()).count()
}

#[tokio::test]
async fn given_a_resident_idle_client_at_the_cap_when_it_is_re_acquired_and_another_root_is_admitted_before_that_acquisition_resumes_then_the_caller_gets_the_pooled_running_client_with_its_reference()
 {
    // given
    let gated = gated_manager(GatedManagerOptions {
        max_resident_clients: Some(1),
        ..GatedManagerOptions::default()
    });
    let manager = Arc::clone(&gated.manager);
    let server = server();
    let first = manager
        .get_client("/root-a", &server, None)
        .await
        .expect("first acquisition");
    manager.release_client("/root-a", &server, Some(&first));

    // when: the admission of root-b runs right after the resident hit on root-a
    let (again, other) = within(
        async {
            tokio::join!(
                manager.get_client("/root-a", &server, None),
                manager.get_client("/root-b", &server, None)
            )
        },
        "the acquisitions never settled",
    )
    .await;
    let a = again.expect("root-a acquisition");
    let b = other.expect("root-b acquisition");

    // then
    assert!(Arc::ptr_eq(&a, &first));
    assert!(gated.client(0).running());
    assert_eq!(
        gated.pool(),
        vec![entry("/root-a", 1, 0, false), entry("/root-b", 1, 0, false)]
    );
    manager.release_client("/root-a", &server, Some(&a));
    manager.release_client("/root-b", &server, Some(&b));
    let held: Vec<usize> = gated
        .pool()
        .into_iter()
        .map(|(_, refs, waiters, _)| refs + waiters)
        .collect();
    assert_eq!(held, vec![0, 0]);
    manager.stop_all().await;
}

#[tokio::test]
async fn given_two_waiters_on_one_cold_start_when_one_aborts_at_any_point_while_the_start_finishes_then_the_survivor_gets_the_pooled_running_client_with_one_reference_and_the_start_is_not_stopped()
 {
    let mut failures = Vec::new();
    for aborting_first in [true, false] {
        for turns in MICROTASK_SWEEP {
            // given
            let gated = gated_manager(GatedManagerOptions {
                hold_first_start: true,
                ..GatedManagerOptions::default()
            });
            let manager = Arc::clone(&gated.manager);
            let server = server();
            let controller = AbortController::new();
            let aborting_signal = controller.signal();
            let surviving_signal = AbortController::new().signal();
            let aborting = manager.get_client("/root", &server, Some(&aborting_signal));
            let surviving = manager.get_client("/root", &server, Some(&surviving_signal));
            tokio::pin!(aborting);
            tokio::pin!(surviving);
            if aborting_first {
                assert!((&mut aborting).now_or_never().is_none());
                assert!((&mut surviving).now_or_never().is_none());
            } else {
                assert!((&mut surviving).now_or_never().is_none());
                assert!((&mut aborting).now_or_never().is_none());
            }

            // when
            let first = gated.client(0);
            let driver = async {
                first.started.resolve();
                after_yields(turns).await;
                controller.abort();
            };
            let (left, kept, ()) = within(
                async { tokio::join!(aborting, surviving, driver) },
                "the waiters never settled",
            )
            .await;

            // then
            let holder_count = holders(&[&left, &kept]);
            let survivor_got_pooled_running_client = kept
                .as_ref()
                .is_ok_and(|client| same_client(client, &first))
                && first.running();
            let pool_matches_holders = gated.pool() == vec![entry("/root", holder_count, 0, false)];
            if !survivor_got_pooled_running_client || !pool_matches_holders {
                failures.push((aborting_first, turns, gated.pool()));
            }
            let shared = gated.shared_client(0);
            for _ in 0..holder_count {
                manager.release_client("/root", &server, Some(&shared));
            }
            manager.stop_all().await;
        }
    }
    assert_eq!(failures, Vec::new());
}

#[tokio::test]
async fn given_a_waiter_on_a_cold_start_at_the_cap_when_another_root_is_admitted_at_any_point_while_the_start_finishes_then_the_waiter_gets_the_pooled_running_client()
 {
    let mut failures = Vec::new();
    for turns in MICROTASK_SWEEP {
        // given
        let gated = gated_manager(GatedManagerOptions {
            max_resident_clients: Some(1),
            hold_first_start: true,
            ..GatedManagerOptions::default()
        });
        let manager = Arc::clone(&gated.manager);
        let server = server();
        let waiter = manager.get_client("/root-a", &server, None);
        tokio::pin!(waiter);
        assert!((&mut waiter).now_or_never().is_none());

        // when
        let first = gated.client(0);
        let driver = async {
            first.started.resolve();
            after_yields(turns).await;
            manager.get_client("/root-b", &server, None).await
        };
        let (got, other) = within(
            async { tokio::join!(waiter, driver) },
            "the acquisitions never settled",
        )
        .await;

        // then
        let got_pooled_running_client =
            got.as_ref().is_ok_and(|client| same_client(client, &first)) && first.running();
        let referenced = gated
            .pool()
            .into_iter()
            .find(|(root, ..)| root == "/root-a")
            .map_or(0, |(_, refs, ..)| refs);
        if !got_pooled_running_client || referenced != 1 {
            failures.push((turns, gated.pool(), other.is_ok()));
        }
        manager.stop_all().await;
    }
    assert_eq!(failures, Vec::new());
}

#[tokio::test]
async fn given_a_holder_whose_client_was_invalidated_and_replaced_when_it_releases_the_client_it_held_then_the_replacement_keeps_its_holders_reference()
 {
    // given
    let gated = gated_manager(GatedManagerOptions::default());
    let manager = Arc::clone(&gated.manager);
    let server = server();
    let stale = manager
        .get_client("/root", &server, None)
        .await
        .expect("stale acquisition");
    manager.invalidate_client("/root", &server, Some(&stale));
    let replacement = within(
        manager.get_client("/root", &server, None),
        "the replacement never started",
    )
    .await
    .expect("replacement acquisition");

    // when
    manager.release_client("/root", &server, Some(&stale));

    // then
    assert!(!Arc::ptr_eq(&replacement, &stale));
    assert_eq!(gated.pool(), vec![entry("/root", 1, 0, false)]);
    manager.release_client("/root", &server, Some(&replacement));
    assert_eq!(gated.pool(), vec![entry("/root", 0, 0, false)]);
    manager.stop_all().await;
}
