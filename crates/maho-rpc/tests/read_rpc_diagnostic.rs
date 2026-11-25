//! omo `read-rpc-diagnostic.test.ts` (omo 1c280530f6, omo#14498): the opt-in `[thread-read-rpc]` line names only
//! fixed fields, and only for a transport failure of a traced read.

use std::io::ErrorKind;
use std::sync::Arc;

use maho_rpc::socket_client::{
    ConnectFuture, RpcConnector, RpcRequest, RpcSocketError, RpcSocketOptions, RpcStream,
    with_rpc_socket,
};
use serde_json::json;

use crate::socket_client_world::{World, drive, read_messages, settle};

async fn read_through_a_broken_connect(
    world: &World,
    trace: Option<&str>,
    read_command: &str,
) -> RpcSocketError {
    let connect: RpcConnector = Arc::new(|_endpoint: &str| -> ConnectFuture {
        Box::pin(async {
            Err::<Box<dyn RpcStream>, _>(std::io::Error::new(
                ErrorKind::BrokenPipe,
                "private error detail",
            ))
        })
    });
    let options = RpcSocketOptions {
        timeout_ms: 100,
        read_command: Some(read_command.to_owned()),
        connect: Some(connect),
        runtime: Some(world.runtime(trace)),
        ..RpcSocketOptions::default()
    };
    settle(with_rpc_socket(
        "private socket path",
        options,
        |_request: RpcRequest| async { Ok::<(), RpcSocketError>(()) },
    ))
    .await
    .expect_err("the connect fails")
}

#[tokio::test]
async fn only_explicitly_traced_read_rpc_failures_log_fixed_fields_and_keep_the_error() {
    let (world, _dials) = World::open();
    let error = RpcSocketError::Io {
        kind: ErrorKind::BrokenPipe,
        message: "private error detail".to_owned(),
    };
    assert_eq!(
        read_through_a_broken_connect(&world, None, "list_sessions").await,
        error
    );
    assert_eq!(
        read_through_a_broken_connect(&world, Some("true"), "list_sessions").await,
        error
    );
    assert_eq!(
        read_through_a_broken_connect(&world, Some("1"), "private command").await,
        error
    );
    assert!(world.lines().is_empty());
    assert_eq!(
        read_through_a_broken_connect(&world, Some("1"), "list_sessions").await,
        error
    );
    assert_eq!(
        world.lines(),
        vec![
            "[thread-read-rpc] command=list_sessions phase=connect elapsed_ms=0 code=EPIPE caller_stall_ms=0 attempts=1"
                .to_owned()
        ]
    );
}

/// The drop is queued before the connect completes, and the operation yields once before it writes, so the drop
/// lands while the connection has done nothing but connect (the error event Node emits in the connect's tick).
#[tokio::test]
async fn a_connected_socket_without_an_auth_exchange_never_reports_a_handshake() {
    let (world, mut dials) = World::gated();
    let mut options = world.options(100, Some("get_messages"));
    options.runtime = Some(world.runtime(Some("1")));
    let call = with_rpc_socket(
        "private endpoint",
        options,
        |mut request: RpcRequest| async move {
            tokio::task::yield_now().await;
            request.send(json!({"type": "get_messages"}), None).await
        },
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.drop_connection(ErrorKind::ConnectionReset, "private detail");
        host.accept();
        host
    };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the drop fails the read").to_string(),
        "private detail"
    );
    assert_eq!(
        world.lines(),
        vec![
            "[thread-read-rpc] command=get_messages phase=connected elapsed_ms=0 code=ECONNRESET caller_stall_ms=0 attempts=1"
                .to_owned()
        ]
    );
}

#[tokio::test]
async fn an_error_thrown_by_the_callers_own_operation_is_not_logged_as_a_socket_failure() {
    let (world, mut dials) = World::open();
    let mut options = world.options(1_000, Some("get_messages"));
    options.runtime = Some(world.runtime(Some("1")));
    let call = with_rpc_socket("private endpoint", options, |_request: RpcRequest| async {
        Err::<(), _>(RpcSocketError::Operation("caller logic".to_owned()))
    });
    let host = async { dials.next_dial().await };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the operation fails"),
        RpcSocketError::Operation("caller logic".to_owned())
    );
    assert!(world.lines().is_empty());
}

#[tokio::test]
async fn a_partial_response_names_the_byte_stage_deadline_without_logging_response_data() {
    let (world, mut dials) = World::open();
    let mut options = world.options(50, Some("get_messages"));
    options.runtime = Some(world.runtime(Some("1")));
    let call = with_rpc_socket(
        "private endpoint",
        options,
        |mut request: RpcRequest| async move {
            request
                .send(
                    json!({"type": "get_messages", "sessionId": "private session"}),
                    None,
                )
                .await
        },
    );
    let host = async {
        let mut host = dials.next_dial().await;
        host.next_frame().await;
        host.data("private response fragment");
        host.delivered().await;
        world.clock.responsive(50);
        world.timers.fire_last();
        host
    };
    let (outcome, _host) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("the read times out"),
        RpcSocketError::TimedOut
    );
    assert_eq!(
        world.lines(),
        vec![
            "[thread-read-rpc] command=get_messages phase=response-bytes elapsed_ms=50 code=DEADLINE caller_stall_ms=0 attempts=1"
                .to_owned()
        ]
    );
}

/// A redial starts the trace over at `connect` and counts the second attempt; the stall charged to the caller covers
/// both attempts.
#[tokio::test]
async fn a_redialed_read_traces_its_second_attempt() {
    let (world, mut dials) = World::open();
    let mut options = world.options(10_000, Some("get_messages"));
    options.runtime = Some(world.runtime(Some("1")));
    let call = with_rpc_socket("private endpoint", options, read_messages);
    let host = async {
        for _ in 0..2 {
            let mut host = dials.next_dial().await;
            host.next_frame().await;
            world.clock.stall(500);
            host.drop_connection(ErrorKind::ConnectionReset, "private detail");
        }
    };
    let (outcome, ()) = drive(call, host).await;
    assert_eq!(
        outcome.expect_err("both attempts drop").to_string(),
        "private detail"
    );
    assert_eq!(world.dialed(), 2);
    assert_eq!(
        world.lines(),
        vec![
            "[thread-read-rpc] command=get_messages phase=request elapsed_ms=1000 code=ECONNRESET caller_stall_ms=800 attempts=2"
                .to_owned()
        ]
    );
}
