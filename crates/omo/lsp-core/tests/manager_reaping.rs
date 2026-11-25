//! Translation of `manager-reaping.test.ts`.

use crate::support::controlled_clock::after_yields;
use crate::support::controlled_clock::within;
use crate::support::manager_test_support::GatedManagerOptions;
use crate::support::manager_test_support::IDLE_TIMEOUT_MS;
use crate::support::manager_test_support::INIT_TIMEOUT_MS;
use crate::support::manager_test_support::MICROTASK_SWEEP;
use crate::support::manager_test_support::REAPER_INTERVAL_MS;
use crate::support::manager_test_support::entry;
use crate::support::manager_test_support::gated_manager;
use crate::support::manager_test_support::same_client;
use crate::support::manager_test_support::server;
use futures::FutureExt;
use lsp_core::abort::AbortController;
use pretty_assertions::assert_eq;
use std::sync::Arc;

const PAST_IDLE_TIMEOUT_MS: u64 = IDLE_TIMEOUT_MS + REAPER_INTERVAL_MS;

#[tokio::test]
async fn given_a_waiter_on_a_cold_start_when_the_idle_reaper_runs_at_any_point_while_the_start_finishes_then_the_waiter_gets_the_pooled_running_client_which_is_reaped_only_once_released_and_idle()
 {
    let mut failures = Vec::new();
    for turns in MICROTASK_SWEEP {
        // given
        let gated = gated_manager(GatedManagerOptions {
            hold_first_start: true,
            ..GatedManagerOptions::default()
        });
        let manager = Arc::clone(&gated.manager);
        let server = server();
        let waiter = manager.get_client("/root", &server, None);
        tokio::pin!(waiter);
        assert!((&mut waiter).now_or_never().is_none());

        // when
        let first = gated.client(0);
        let driver = async {
            first.started.resolve();
            after_yields(turns).await;
            gated.clock.advance_by(PAST_IDLE_TIMEOUT_MS);
        };
        let (got, ()) = within(
            async { tokio::join!(waiter, driver) },
            "the waiter never settled",
        )
        .await;
        let got_pooled_running_client =
            got.as_ref().is_ok_and(|client| same_client(client, &first)) && first.running();
        gated.clock.advance_by(PAST_IDLE_TIMEOUT_MS);
        let kept_while_referenced = gated.pool();
        manager.release_client("/root", &server, Some(&gated.shared_client(0)));
        gated.clock.advance_by(PAST_IDLE_TIMEOUT_MS);
        if manager.client_count() == 0 {
            first.wait_stop_count(1).await;
        }

        // then
        let reaped_once_idle = manager.client_count() == 0 && first.stop_count() == 1;
        if !got_pooled_running_client
            || kept_while_referenced != vec![entry("/root", 1, 0, false)]
            || !reaped_once_idle
        {
            failures.push((turns, kept_while_referenced, first.stop_count()));
        }
        manager.stop_all().await;
    }
    assert_eq!(failures, Vec::new());
}

#[tokio::test]
async fn given_a_start_the_init_timeout_reaper_stopped_while_its_stop_is_pending_when_that_start_still_resolves_then_its_waiter_acquires_a_fresh_client_after_the_stop_instead_of_the_stopped_one()
 {
    // given
    let gated = gated_manager(GatedManagerOptions {
        hold_first_start: true,
        hold_stops: true,
        ..GatedManagerOptions::default()
    });
    let manager = Arc::clone(&gated.manager);
    let server = server();
    let waiter = manager.get_client("/root", &server, None);
    tokio::pin!(waiter);
    assert!((&mut waiter).now_or_never().is_none());
    gated.clock.advance_by(INIT_TIMEOUT_MS + REAPER_INTERVAL_MS);
    let first = gated.client(0);
    first.wait_stop_count(1).await;
    assert_eq!(first.stop_count(), 1);
    assert_eq!(manager.client_count(), 0);

    // when
    first.started.resolve();
    first.stopped.resolve();
    let got = within(waiter, "the waiter never settled")
        .await
        .expect("fresh acquisition");

    // then
    assert_eq!(gated.client_total(), 2);
    let second = gated.client(1);
    assert!(same_client(&got, &second));
    assert!(second.running());
    assert_eq!(gated.pool(), vec![entry("/root", 1, 0, false)]);
    manager.release_client("/root", &server, Some(&got));
    gated.open_all_stops();
    within(manager.stop_all(), "stopAll never finished").await;
}

#[tokio::test]
async fn given_a_waiter_on_a_cold_start_when_stop_all_runs_and_then_the_start_resolves_then_the_waiter_rejects_instead_of_getting_the_stopped_client()
 {
    // given
    let gated = gated_manager(GatedManagerOptions {
        hold_first_start: true,
        hold_stops: true,
        ..GatedManagerOptions::default()
    });
    let manager = Arc::clone(&gated.manager);
    let server = server();
    let waiter = manager.get_client("/root", &server, None);
    tokio::pin!(waiter);
    assert!((&mut waiter).now_or_never().is_none());
    let first = gated.client(0);

    // when
    let stopping = manager.stop_all();
    tokio::pin!(stopping);
    assert!((&mut stopping).now_or_never().is_none());
    first.started.resolve();
    let got = within(waiter, "the waiter never settled").await;
    first.stopped.resolve();
    within(stopping, "stopAll never finished").await;

    // then
    let error = got.err().expect("the waiter rejects");
    assert!(
        error.to_string().contains("LspManager has been disposed"),
        "{error}"
    );
    assert_eq!(first.stop_count(), 1);
    assert_eq!(manager.client_count(), 0);
}

#[tokio::test]
async fn given_a_warmup_start_joined_by_a_waiter_when_the_waiter_aborts_at_any_point_while_the_start_finishes_then_the_warmup_client_keeps_running_in_the_pool()
 {
    let mut failures = Vec::new();
    for turns in MICROTASK_SWEEP {
        // given
        let gated = gated_manager(GatedManagerOptions {
            hold_first_start: true,
            ..GatedManagerOptions::default()
        });
        let manager = Arc::clone(&gated.manager);
        let server = server();
        manager.warmup_client("/root", &server);
        let controller = AbortController::new();
        let signal = controller.signal();
        let joiner = manager.get_client("/root", &server, Some(&signal));
        tokio::pin!(joiner);
        assert!((&mut joiner).now_or_never().is_none());

        // when
        let first = gated.client(0);
        let driver = async {
            first.started.resolve();
            after_yields(turns).await;
            controller.abort();
        };
        let (got, ()) = within(
            async { tokio::join!(joiner, driver) },
            "the joiner never settled",
        )
        .await;

        within(first.initialized.wait(), "the warmup start never finished").await;

        // then
        let holder_count = usize::from(got.is_ok());
        let warmup_running = first.running();
        let pool = gated.pool();
        if !warmup_running || pool != vec![entry("/root", holder_count, 0, false)] {
            failures.push((turns, pool));
        }
        manager.stop_all().await;
    }
    assert_eq!(failures, Vec::new());
}
