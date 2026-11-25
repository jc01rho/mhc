//! omo `socket-client-edge-cases.test.ts` (omo#14444, omo#14624): the redial rule and the caller-stall meter on a
//! controlled clock. "The caller stalled" moves the clock with no probe; "the caller was responsive" moves it one
//! probe period at a time and lets the probe look.

use std::io::ErrorKind;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_rpc::socket_client::{
    BoxFuture, ConnectFuture, RpcConnector, RpcListen, RpcRequest, RpcSocketError,
    RpcSocketOptions, RpcStream, with_rpc_socket,
};
use serde_json::{Value, json};

use crate::socket_client_world::{World, drive, read_messages, settle};

fn passive_listen() -> RpcListen {
    RpcListen {
        frame: Arc::new(|_frame: Value| {}),
        closed: Arc::new(|_error: &RpcSocketError| {}),
    }
}

/// The host drops the first read after the caller stalls 500 ms; it answers the second dial if there is one.
async fn read_dropped_as_the_stall_ends(
    kind: ErrorKind,
    message: &str,
) -> (Result<Value, RpcSocketError>, usize) {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        world.clock.stall(500);
        first.drop_connection(kind, message);
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    (outcome, world.dialed())
}

#[tokio::test]
async fn a_non_read_command_dropped_during_a_caller_stall_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("prompt")),
        |mut request: RpcRequest| async move { request.send(json!({"type": "prompt"}), None).await },
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(500);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the call").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_subscription_dropped_during_a_caller_stall_is_not_redialed() {
    let (world, mut dials) = World::open();
    let options = RpcSocketOptions {
        listen: Some(passive_listen()),
        ..world.options(10_000, Some("get_messages"))
    };
    let call = with_rpc_socket("endpoint", options, read_messages);
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(500);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the call").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_read_the_host_reset_as_the_callers_stall_ends_is_redialed_once_and_answers() {
    let (outcome, dialed) =
        read_dropped_as_the_stall_ends(ErrorKind::ConnectionReset, "dropped ECONNRESET").await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(dialed, 2);
}

#[tokio::test]
async fn a_read_the_host_broke_with_epipe_as_the_callers_stall_ends_is_redialed_once_and_answers() {
    let (outcome, dialed) =
        read_dropped_as_the_stall_ends(ErrorKind::BrokenPipe, "dropped EPIPE").await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(dialed, 2);
}

#[tokio::test]
async fn a_read_whose_connection_closed_without_an_error_as_the_stall_ends_is_redialed_once() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        world.clock.stall(500);
        first.close();
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

/// Timers run before I/O in one event-loop turn: the probe records the stall, then the drop is seen.
#[tokio::test]
async fn a_drop_handled_right_after_a_probe_saw_the_stall_end_is_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        world.clock.stall(500);
        world.clock.responsive(50);
        first.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

#[tokio::test]
async fn a_reset_after_the_caller_ran_on_time_again_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(500);
        world.clock.responsive(300);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

/// The probe that sees the stall arms the marker; the next one, on time, proves the caller runs again.
#[tokio::test]
async fn a_reset_right_after_the_first_on_time_probe_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(500);
        world.clock.responsive(50);
        world.clock.responsive(50);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

/// The host dropped the silent connection during the stall; under load its reset can reach the caller a probe
/// later (omo#14444).
#[tokio::test]
async fn a_connection_stalled_on_before_any_write_is_redialed_even_if_its_reset_comes_after_on_time_probes()
 {
    let (world, mut dials) = World::gated();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut silent = dials.next_dial().await;
        world.clock.stall(500);
        world.clock.responsive(100);
        silent.refuse(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        second.accept();
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

#[tokio::test]
async fn a_second_stall_that_ends_with_the_hosts_drop_is_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        world.clock.stall(500);
        world.clock.responsive(100);
        world.clock.stall(500);
        first.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

/// The operation stalls the caller 500 ms before its first request, so the request's own clock read sees the stall.
fn stalled_first_read(
    world: &World,
) -> impl FnMut(RpcRequest) -> BoxFuture<Result<Value, RpcSocketError>> {
    let clock = Arc::clone(&world.clock);
    let mut attempts = 0;
    move |mut request: RpcRequest| -> BoxFuture<Result<Value, RpcSocketError>> {
        attempts += 1;
        let stall_first = attempts == 1;
        let clock = Arc::clone(&clock);
        Box::pin(async move {
            if stall_first {
                clock.stall(500);
            }
            request.send(json!({"type": "get_messages"}), None).await
        })
    }
}

#[tokio::test]
async fn a_drop_right_after_a_stall_the_requests_own_clock_read_saw_is_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        stalled_first_read(&world),
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        first.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

/// The overdue probe is measured against the previous probe, not against the request's clock read just before it
/// (omo#14624 review M5).
#[tokio::test]
async fn an_overdue_probe_after_the_requests_clock_read_is_not_recovery_so_the_drop_is_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        stalled_first_read(&world),
    );
    let host = async {
        let mut first = dials.next_dial().await;
        first.next_frame().await;
        world.clock.overdue_probe();
        first.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
        let mut second = dials.next_dial().await;
        let frame = second.next_frame().await;
        second.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(outcome.expect("the redial answers")["success"], true);
    assert_eq!(world.dialed(), 2);
}

#[tokio::test]
async fn an_overdue_probe_then_an_on_time_probe_before_the_reset_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        stalled_first_read(&world),
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.overdue_probe();
        world.clock.responsive(50);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_read_the_host_dropped_with_no_caller_stall_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_gap_no_longer_than_the_probe_period_plus_its_tolerance_is_no_stall() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.stall(100);
        host.drop_connection(ErrorKind::ConnectionReset, "dropped ECONNRESET");
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "dropped ECONNRESET"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_read_that_already_received_response_bytes_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        host.data(&format!("{}\n", json!({"type": "broadcast"})));
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

#[tokio::test]
async fn a_read_that_times_out_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(100, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        world.clock.responsive(100);
        world.timers.fire_last();
        host
    };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the read times out"),
        RpcSocketError::TimedOut
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_step_timeout_shorter_than_the_calls_times_out_on_its_own_budget() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        |mut request: RpcRequest| async move {
            request
                .send(json!({"type": "get_messages"}), Some(100))
                .await
        },
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        assert_eq!(world.timers.armed().last(), Some(&100));
        world.clock.responsive(100);
        world.timers.fire_last();
        host
    };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the step times out"),
        RpcSocketError::TimedOut
    );
}

/// The caller blocks 400 ms on a 300 ms budget: the overdue timer fires first, finds its deadline moved by the
/// stall and waits again, so the answer behind it settles the read.
#[tokio::test]
async fn a_caller_stall_past_the_deadline_is_not_charged_and_the_late_answer_settles_the_read() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(300, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        let frame = host.next_frame().await;
        world.clock.stall(400);
        world.timers.fire_last();
        host.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect("the late answer settles the read")["success"],
        true
    );
    // connect 300, request 300, then the wait again: 300 ms budget + 300 ms excused stall - 400 ms passed = 200 ms.
    assert_eq!(world.timers.armed(), vec![300, 300, 200]);
}

#[tokio::test]
async fn a_call_that_ended_stops_its_stall_probe() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        read_messages,
    );
    let host = async {
        let mut host = dials.next_dial().await;
        let frame = host.next_frame().await;
        host.reply(&frame);
    };
    let (outcome, ()) = drive(call, host).await;
    outcome.expect("the read answers");
    assert_eq!(world.clock.probes(), 0);
}

/// omo#14624 review L2: a subscription between requests waits only for frames, so no probe runs until the next
/// request arms one.
#[tokio::test]
async fn a_subscription_between_requests_runs_no_stall_probe_and_the_next_request_runs_one_again() {
    let (world, mut dials) = World::open();
    let clock = Arc::clone(&world.clock);
    let options = RpcSocketOptions {
        listen: Some(passive_listen()),
        ..world.options(10_000, None)
    };
    let call = with_rpc_socket("endpoint", options, move |mut request: RpcRequest| {
        let clock = Arc::clone(&clock);
        async move {
            request.send(json!({"type": "subscribe"}), None).await?;
            let between = clock.probes();
            request.send(json!({"type": "get_state"}), None).await?;
            Ok::<_, RpcSocketError>((between, clock.probes()))
        }
    });
    let host = async {
        let mut host = dials.next_dial().await;
        let subscribe = host.next_frame().await;
        host.reply(&subscribe);
        let get_state = host.next_frame().await;
        let during = world.clock.probes();
        host.reply(&get_state);
        during
    };
    let (outcome, during) = drive(call, host).await;
    let (between, after) = outcome.expect("both requests answer");
    assert_eq!([between, during, after], [0, 1, 0]);
}

#[tokio::test]
async fn a_connect_that_fails_at_once_rejects_with_its_error_and_dials_once() {
    let (world, _dials) = World::open();
    let dials = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&dials);
    let connect: RpcConnector = Arc::new(move |_endpoint: &str| -> ConnectFuture {
        counted.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err::<Box<dyn RpcStream>, _>(std::io::Error::new(
                ErrorKind::ConnectionReset,
                "connect refused",
            ))
        })
    });
    let options = RpcSocketOptions {
        connect: Some(connect),
        ..world.options(10_000, Some("get_messages"))
    };
    let outcome = settle(with_rpc_socket("endpoint", options, read_messages)).await;
    assert_eq!(
        outcome.expect_err("the connect fails").to_string(),
        "connect refused"
    );
    assert_eq!(dials.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn an_operation_that_fails_on_its_own_is_not_redialed() {
    let (world, mut dials) = World::open();
    let call = with_rpc_socket(
        "endpoint",
        world.options(10_000, Some("get_messages")),
        |_request: RpcRequest| async {
            Err::<Value, _>(RpcSocketError::Operation("operation failed".to_owned()))
        },
    );
    let host = async { dials.next_dial().await };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the operation fails").to_string(),
        "operation failed"
    );
    assert_eq!(world.dialed(), 1);
}

#[tokio::test]
async fn a_secret_is_written_before_the_first_request() {
    let (world, mut dials) = World::open();
    let secret = vec![7_u8; 32];
    let options = RpcSocketOptions {
        secret: Some(secret.clone()),
        ..world.options(10_000, Some("get_messages"))
    };
    let call = with_rpc_socket("endpoint", options, read_messages);
    let host = async {
        let mut host = dials.next_dial().await;
        let first = host.next_write().await;
        let frame = host.next_frame().await;
        host.reply(&frame);
        (first, frame)
    };
    let (outcome, (first, frame)) = drive(call, host).await;
    outcome.expect("the read answers");
    assert_eq!(first, secret);
    assert_eq!(frame["type"], "get_messages");
}

/// The production path end to end: a Unix socket, the system clock and timers, the spawned stall probe.
#[cfg(unix)]
#[tokio::test]
async fn a_real_local_socket_writes_the_secret_first_and_settles_the_correlated_read() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("host.sock");
    let listener = tokio::net::UnixListener::bind(&path).expect("bind");
    let host = async {
        let (stream, _) = listener.accept().await.expect("accept");
        let mut stream = tokio::io::BufReader::new(stream);
        let mut secret = [0_u8; 32];
        stream.read_exact(&mut secret).await.expect("secret");
        let mut line = String::new();
        stream.read_line(&mut line).await.expect("request");
        let request: Value = serde_json::from_str(&line).expect("a JSON request");
        let reply = json!({"id": request["id"], "type": "response", "success": true, "data": {"sessions": []}});
        stream
            .get_mut()
            .write_all(format!("stray output\n{{\"type\":\"broadcast\"}}\n{reply}\n").as_bytes())
            .await
            .expect("reply");
        (secret, request)
    };
    let options = RpcSocketOptions {
        timeout_ms: 5_000,
        read_command: Some("list_sessions".to_owned()),
        secret: Some(vec![9; 32]),
        ..RpcSocketOptions::default()
    };
    let call = with_rpc_socket(
        path.to_str().expect("a UTF-8 path"),
        options,
        |mut request: RpcRequest| async move {
            request.send(json!({"type": "list_sessions"}), None).await
        },
    );
    let (outcome, (secret, request)) = drive(call, host).await;
    assert_eq!(
        outcome.expect("the read answers")["data"]["sessions"],
        json!([])
    );
    assert_eq!(secret, [9_u8; 32]);
    assert_eq!(request["type"], "list_sessions");
}
