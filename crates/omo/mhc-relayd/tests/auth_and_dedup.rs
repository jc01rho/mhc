use std::sync::Arc;
use std::time::Duration;

use device_service_core::local_api::access::SOCKET_FILE;
use device_service_core::local_api::framing::encode_line;
use device_service_core::local_api::proof::mint_nonce;
use mhc_relayd::RelaydConfig;
use mhc_relayd::peer::{PeerError, peer_request};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};

use crate::support::{Device, FakeHost, client, listening_config};

async fn next_json<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
) -> Option<Value> {
    let line = lines.next_line().await.expect("read")?;
    Some(serde_json::from_str(&line).expect("json"))
}

#[tokio::test]
async fn a_device_the_listener_does_not_list_or_another_account_is_refused() {
    let listener_device = Device::new("box");
    let stranger = Device::new("stranger");
    let listener = listener_device
        .start(listening_config(vec![]), FakeHost::with_sessions(&["s1"]))
        .await;
    let address = listener.peer_address().expect("peer address").to_string();
    let stranger_relayd = stranger
        .start(
            RelaydConfig {
                listen: None,
                ..listening_config(vec![listener_device.as_peer(&address)])
            },
            FakeHost::with_sessions(&[]),
        )
        .await;
    let unknown = peer_request(
        stranger_relayd.state(),
        &listener_device.as_peer(&address),
        "describe",
        json!({}),
        Duration::from_secs(10),
    )
    .await
    .expect_err("an unlisted key is refused");
    assert_eq!(unknown, PeerError::Refused("unknown_device".into()));
    stranger_relayd.shutdown().await;
    listener.shutdown().await;

    let other_account = Device::new("box2");
    let known = Device::new("known");
    let listener = other_account
        .start(
            listening_config(vec![known.as_peer("127.0.0.1:9")]),
            FakeHost::with_sessions(&["s1"]),
        )
        .await;
    let address = listener.peer_address().expect("peer address").to_string();
    let dialer = known
        .start(
            RelaydConfig {
                account: "work".into(),
                listen: None,
                ..listening_config(vec![other_account.as_peer(&address)])
            },
            FakeHost::with_sessions(&[]),
        )
        .await;
    let mismatch = peer_request(
        dialer.state(),
        &other_account.as_peer(&address),
        "describe",
        json!({}),
        Duration::from_secs(10),
    )
    .await
    .expect_err("another account is refused");
    assert_eq!(mismatch, PeerError::Refused("account_mismatch".into()));
    dialer.shutdown().await;
    listener.shutdown().await;
}

#[tokio::test]
async fn a_dialer_that_cannot_sign_for_the_key_it_names_is_refused() {
    let listed = Device::new("laptop");
    let listener_device = Device::new("box");
    let listener = listener_device
        .start(
            listening_config(vec![listed.as_peer("127.0.0.1:9")]),
            FakeHost::with_sessions(&["s1"]),
        )
        .await;
    let address = listener.peer_address().expect("peer address");
    let stream = TcpStream::connect(address).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({
        "type": "peer.hello",
        "v": 1,
        "account": "home",
        "device": { "id": "x", "name": "x", "platform": "linux" },
        "public_key": listed.identity.public_key(),
        "nonce": mint_nonce().expect("nonce"),
    });
    write
        .write_all(&encode_line(&hello).expect("line"))
        .await
        .expect("write");
    let welcome = next_json(&mut lines).await.expect("welcome");
    assert_eq!(welcome["type"], "peer.welcome");
    assert_eq!(welcome["public_key"], listener_device.identity.public_key());
    let forged = Device::new("attacker").identity.sign(b"anything");
    let prove = json!({ "type": "peer.prove", "signature": forged });
    write
        .write_all(&encode_line(&prove).expect("line"))
        .await
        .expect("write");
    assert_eq!(
        next_json(&mut lines).await,
        Some(json!({ "type": "peer.refused", "reason": "bad_signature" }))
    );
    assert_eq!(next_json(&mut lines).await, None, "the link is closed");
    listener.shutdown().await;
}

#[tokio::test]
async fn the_local_api_honors_nothing_before_the_client_proof() {
    let device = Device::new("box");
    let relayd = device
        .start(RelaydConfig::default(), FakeHost::with_sessions(&["s1"]))
        .await;
    let socket = device.state_dir.join(SOCKET_FILE);

    let stream = UnixStream::connect(&socket).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({
        "type": "hello",
        "v": 2,
        "client": { "name": "omo", "version": "0.0.0" },
        "client_nonce": mint_nonce().expect("nonce"),
    });
    write
        .write_all(&encode_line(&hello).expect("line"))
        .await
        .expect("write");
    assert_eq!(
        next_json(&mut lines).await,
        Some(json!({ "type": "refused", "reason": "version" }))
    );

    let stream = UnixStream::connect(socket).await.expect("connect");
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let hello = json!({
        "type": "hello",
        "v": 1,
        "client": { "name": "omo", "version": "0.0.0" },
        "client_nonce": mint_nonce().expect("nonce"),
    });
    write
        .write_all(&encode_line(&hello).expect("line"))
        .await
        .expect("write");
    let welcome = next_json(&mut lines).await.expect("welcome");
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["owner"], "service");
    let early = json!({ "id": "1", "type": "status.get" });
    write
        .write_all(&encode_line(&early).expect("line"))
        .await
        .expect("write");
    let refused = next_json(&mut lines).await.expect("answer");
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unauthenticated");
    let wrong = json!({ "id": "2", "type": "authenticate", "client_proof": "AAAA" });
    write
        .write_all(&encode_line(&wrong).expect("line"))
        .await
        .expect("write");
    let refused = next_json(&mut lines).await.expect("answer");
    assert_eq!(refused["error"]["code"], "client_proof_invalid");
    assert_eq!(
        next_json(&mut lines).await,
        None,
        "a wrong proof closes the connection"
    );
    relayd.shutdown().await;
}

#[tokio::test]
async fn a_command_id_binds_once_to_its_target_and_payload() {
    let device = Device::new("box");
    let host = FakeHost::with_sessions(&["s1"]);
    let relayd = device
        .start(RelaydConfig::default(), Arc::clone(&host))
        .await;
    let api = client(&device.state_dir).await;
    let attach = api
        .attach(&device.identity.id(), "s1")
        .await
        .expect("attach");
    let command_id = "AAAAAAAAAAAAAAAAAAAAAA".to_owned();
    let prompt = json!({ "type": "prompt", "message": "hi" });

    let first = api
        .command(&attach, prompt.clone(), Some(command_id.clone()))
        .await
        .expect("first");
    assert!(first.applied(), "{first:?}");
    let repeat = api
        .command(&attach, prompt.clone(), Some(command_id.clone()))
        .await
        .expect("repeat");
    assert!(
        repeat.applied(),
        "a repeat is answered with the first outcome"
    );
    assert_eq!(repeat.command_id, command_id);
    assert_eq!(host.records().len(), 1, "a repeat dispatches nothing");

    let conflict = api
        .command(
            &attach,
            json!({ "type": "prompt", "message": "other" }),
            Some(command_id),
        )
        .await
        .expect_err("another payload under a used id");
    assert_eq!(conflict.code, "command_id_conflict");

    let unclassified = api
        .command(&attach, json!({ "type": "bash", "command": "ls" }), None)
        .await
        .expect_err("not a session method");
    assert_eq!(unclassified.code, "method_unclassified");
    let local_only = api
        .command(&attach, json!({ "type": "open_session" }), None)
        .await
        .expect_err("a local connection method");
    assert_eq!(local_only.code, "method_local_only");
    let not_attached = api
        .command("nope", prompt, None)
        .await
        .expect_err("another connection's handle");
    assert_eq!(not_attached.code, "not_attached");
    assert_eq!(host.records().len(), 1);

    api.close().await;
    relayd.shutdown().await;
}
