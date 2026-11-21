//! `components/call-budget/call-deadline.ts` contract (omo#14130), on tokio's paused clock.

use std::time::Duration;

use call_budget::{
    READ_RESERVE_MS, TOOL_CALL_BUDGET_MS, command_wait_ms, outside_call_deadline,
    outside_call_deadline_async, wait_within_call_budget, with_call_deadline, within_call_budget,
};

const NOW: i64 = 1_800_000_000_000;

#[test]
fn outside_any_call_every_wait_is_as_asked() {
    assert_eq!(command_wait_ms(30_000, NOW), 30_000);
    assert_eq!(
        wait_within_call_budget(20_000, READ_RESERVE_MS, NOW),
        20_000
    );
}

#[tokio::test]
async fn inside_a_call_a_command_wait_is_cut_to_what_is_left_of_the_deadline() {
    with_call_deadline(NOW + TOOL_CALL_BUDGET_MS, async {
        assert_eq!(command_wait_ms(30_000, NOW), 30_000);
        assert_eq!(command_wait_ms(30_000, NOW + 40_000), 10_000);
        assert_eq!(command_wait_ms(30_000, NOW + 60_000), -10_000);
    })
    .await;
}

#[tokio::test]
async fn a_wait_inside_a_call_leaves_the_read_reserve_and_never_goes_negative() {
    with_call_deadline(NOW + TOOL_CALL_BUDGET_MS, async {
        assert_eq!(
            wait_within_call_budget(40_000, READ_RESERVE_MS, NOW),
            35_000
        );
        assert_eq!(
            wait_within_call_budget(10_000, READ_RESERVE_MS, NOW),
            10_000
        );
        assert_eq!(
            wait_within_call_budget(40_000, READ_RESERVE_MS, NOW + 40_000),
            0
        );
    })
    .await;
}

#[tokio::test]
async fn a_nested_call_bounds_its_own_commands_and_the_outer_deadline_applies_after_it() {
    with_call_deadline(NOW + 50_000, async {
        with_call_deadline(NOW + 5_000, async {
            assert_eq!(command_wait_ms(30_000, NOW), 5_000);
        })
        .await;
        assert_eq!(command_wait_ms(30_000, NOW), 30_000);
    })
    .await;
}

#[tokio::test]
async fn work_run_outside_the_call_deadline_is_not_bounded_by_it() {
    with_call_deadline(NOW + 5_000, async {
        assert_eq!(
            outside_call_deadline(|| command_wait_ms(30_000, NOW)),
            30_000
        );
        assert_eq!(
            outside_call_deadline_async(async { command_wait_ms(30_000, NOW) }).await,
            30_000
        );
        assert_eq!(command_wait_ms(30_000, NOW), 5_000);
    })
    .await;
}

#[tokio::test]
async fn outside_any_call_within_call_budget_is_the_work_itself() {
    assert_eq!(within_call_budget(async { 3 }, NOW).await, Some(3));
}

#[tokio::test(start_paused = true)]
async fn work_that_settles_inside_the_budget_returns_its_value_or_its_error() {
    let value = with_call_deadline(
        NOW + TOOL_CALL_BUDGET_MS,
        within_call_budget(
            async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                7
            },
            NOW,
        ),
    )
    .await;
    assert_eq!(value, Some(7));
    let failed = with_call_deadline(
        NOW + TOOL_CALL_BUDGET_MS,
        within_call_budget(async { Err::<(), _>("refused") }, NOW),
    )
    .await;
    assert_eq!(failed, Some(Err("refused")));
}

#[tokio::test(start_paused = true)]
async fn work_that_outlives_the_call_budget_yields_none_and_keeps_running() {
    let (finished, finish) = tokio::sync::oneshot::channel();
    let work = async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        finished
            .send(())
            .expect("the test still waits for the work");
        "late"
    };
    let outcome =
        with_call_deadline(NOW + TOOL_CALL_BUDGET_MS, within_call_budget(work, NOW)).await;
    assert_eq!(outcome, None);
    finish
        .await
        .expect("the work kept running past the call's budget");
}

#[tokio::test(start_paused = true)]
async fn work_cut_off_by_the_budget_still_sees_the_call_deadline() {
    let seen = with_call_deadline(
        NOW + 5_000,
        within_call_budget(async { command_wait_ms(30_000, NOW) }, NOW),
    )
    .await;
    assert_eq!(seen, Some(5_000));
}
