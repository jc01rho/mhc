//! omo `socket-client-redial.test.ts` (omo#14624 review M1/M2, omo#14809): which drops a read redials, and the
//! budget the redial gets.

use std::io::ErrorKind;

use maho_rpc::socket_client::with_rpc_socket;

use crate::socket_client_world::{World, drive, read_messages};

#[tokio::test]
async fn a_read_whose_connection_fails_with_eacces_after_a_caller_stall_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(2_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(200);
        host.drop_connection(ErrorKind::PermissionDenied, "dropped EACCES");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped EACCES"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_read_the_host_reset_after_a_caller_stall_is_redialed_once() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(2_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        for _ in 0..2 {
            let mut host = dials.next_dial().await;
            host.next_frame().await;
            world.clock.stall(200);
            host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        }
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the redial drops too").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 2);
}

/// The reset arrives with the clock 500 ms on: the meter charges 400 ms of that jump as caller stall (all but its
/// 50 ms period and 50 ms tolerance), so 100 ms of the 100 ms budget ran (omo#14624 review M1).
#[tokio::test]
async fn a_host_drop_handled_after_the_whole_budget_ran_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(100, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(500);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

/// omo#14809: the budget is read off each attempt's armed timers, not off the wall clock. The first connection runs
/// 200 ms, stalls the caller 200 ms (100 ms of it inside the meter's tolerance, so charged to the host) and is reset;
/// the second is accepted and never answers.
#[tokio::test]
async fn a_redialed_read_gets_only_what_is_left_of_the_calls_budget() {
    let (world, mut dials) = World::gated();
    let call = with_rpc_socket(
        "endpoint",
        world.options(400, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut first = dials.next_dial().await;
        // Fails loudly, not by hanging, if the probe or the first timer was armed outside the injected runtime.
        assert_eq!((world.clock.probes(), world.timers.armed()), (1, vec![400]));
        first.accept();
        first.next_frame().await;
        world.clock.responsive(200);
        world.clock.stall(200);
        first.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        second.accept();
        second.next_frame().await;
        assert_eq!(world.timers.armed(), vec![400, 400, 100, 100]);
        world.clock.responsive(100);
        world.timers.fire_last();
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the redial runs out of budget"),
        maho_rpc::socket_client::RpcSocketError::TimedOut
    );
    assert_eq!(world.dialed(), 2);
    // 400 ms budget - 200 ms run - 100 ms of the stall inside the tolerance = 100 ms for the redial; a fresh budget
    // would arm 400.
    assert_eq!(world.timers.armed(), vec![400, 400, 100, 100]);
}
