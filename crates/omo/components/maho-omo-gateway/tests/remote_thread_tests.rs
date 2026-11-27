use std::sync::{Arc, Mutex, PoisonError};

use maho_omo_gateway::thread::GatewayDeliveryMode;
use maho_omo_gateway::thread::adapter::BoxFuture;
use maho_omo_gateway::thread::remote::{
    RemoteAddress, RemoteSendOptions, RemoteThreadPort, parse_remote_address, remote_failure,
    with_remote_thread_tools,
};
use maho_omo_gateway::thread::tools::ThreadTool;
use serde_json::{Value, json};

#[derive(Default)]
struct RecordingPort {
    sent: Mutex<Vec<(RemoteAddress, String, RemoteSendOptions)>>,
}

impl RemoteThreadPort for RecordingPort {
    fn send(
        &self,
        address: RemoteAddress,
        message: String,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<Value> {
        let options = RemoteSendOptions {
            mode,
            ..RemoteSendOptions::default()
        };
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((address.clone(), message, options));
        Box::pin(async move {
            json!({ "kind": "ok", "thread": address.session, "device": address.device })
        })
    }
}

fn local_tool(name: &str) -> ThreadTool {
    ThreadTool {
        name: name.to_owned(),
        description: "local".into(),
        parameters: json!({}),
        execute: Arc::new(|_call_id: &str, args: Value| {
            Box::pin(async move { json!({ "kind": "ok", "local": args["thread"] }) })
        }),
    }
}

#[test]
fn remote_addresses_are_device_slash_session_and_nothing_else() {
    assert_eq!(
        parse_remote_address("laptop/s-1"),
        Some(RemoteAddress {
            device: "laptop".into(),
            session: "s-1".into(),
        })
    );
    for local in [
        "s-1",
        "/abs/path.jsonl",
        "./rel/s",
        "a/b/c",
        "laptop/",
        "/s",
        "two words/s",
    ] {
        assert_eq!(parse_remote_address(local), None, "{local}");
    }
}

#[tokio::test]
async fn thread_send_routes_device_addresses_to_the_port_and_keeps_local_ones() {
    let port = Arc::new(RecordingPort::default());
    let tools = with_remote_thread_tools(
        vec![local_tool("thread_send"), local_tool("thread_read")],
        Arc::clone(&port) as Arc<dyn RemoteThreadPort>,
    );
    let send = tools
        .iter()
        .find(|tool| tool.name == "thread_send")
        .expect("thread_send");
    let remote = (send.execute)(
        "c1",
        json!({ "thread": "laptop/s1", "message": "hi", "delivery": "steer" }),
    )
    .await;
    assert_eq!(
        remote,
        json!({ "kind": "ok", "thread": "s1", "device": "laptop" })
    );
    let named_with_space = (send.execute)(
        "c1b",
        json!({ "thread": "Studio Laptop/s1", "message": "hi" }),
    )
    .await;
    assert_eq!(
        named_with_space,
        json!({ "kind": "ok", "thread": "s1", "device": "Studio Laptop" })
    );
    let here = (send.execute)("c1c", json!({ "thread": "here/s1", "message": "hi" })).await;
    assert_eq!(here["local"], "here/s1");

    let local = (send.execute)("c2", json!({ "thread": "s2", "message": "hi" })).await;
    assert_eq!(local["local"], "s2");
    let collapse = (send.execute)(
        "c2b",
        json!({ "thread": "laptop/s1", "message": "hi", "collapse_key": "k-1" }),
    )
    .await;
    assert_eq!(collapse["kind"], "error");
    assert_eq!(collapse["error"]["code"], "unsupported");
    assert_eq!(collapse["error"]["details"]["field"], "collapse_key");

    let read = tools
        .iter()
        .find(|tool| tool.name == "thread_read")
        .expect("thread_read");
    let untouched = (read.execute)("c3", json!({ "thread": "laptop/s1" })).await;
    assert_eq!(
        untouched["local"], "laptop/s1",
        "other tools are left as they were"
    );
    let sent = port
        .sent
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, "hi");
    assert_eq!(sent[0].2.mode, Some(GatewayDeliveryMode::Steer));
}

#[test]
fn a_remote_failure_is_a_thread_tool_failure() {
    let failure = remote_failure("not_resumable", "gone", "Call thread_list.", None);
    assert_eq!(failure["kind"], "error");
    assert_eq!(failure["error"]["code"], "not_resumable");
    assert_eq!(failure["error"]["next_action"], "Call thread_list.");
}
