use std::sync::Arc;

use mhc_relayd::RelaydConfig;
use serde_json::json;

use crate::support::{Device, FakeHost, client, listening_config};

#[tokio::test]
async fn two_daemons_discover_list_attach_and_command_each_other() {
    let alpha = Device::new("alpha");
    let beta = Device::new("beta");
    let alpha_host = FakeHost::with_sessions(&["a1"]);
    let beta_host = FakeHost::with_sessions(&["b1", "b2"]);
    let beta_relayd = beta
        .start(
            listening_config(vec![alpha.as_peer("127.0.0.1:9")]),
            Arc::clone(&beta_host),
        )
        .await;
    let beta_address = beta_relayd
        .peer_address()
        .expect("beta listens")
        .to_string();
    let alpha_relayd = alpha
        .start(
            RelaydConfig {
                listen: None,
                ..listening_config(vec![beta.as_peer(&beta_address)])
            },
            Arc::clone(&alpha_host),
        )
        .await;
    let api = client(&alpha.state_dir).await;

    let devices = api.devices().await.expect("devices");
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0]["device"], alpha.identity.id());
    assert_eq!(devices[0]["this_device"], true);
    assert_eq!(devices[0]["online"], true);
    assert_eq!(devices[1]["device"], beta.identity.id());
    assert_eq!(devices[1]["label"], "beta");
    assert_eq!(devices[1]["online"], true);

    let listing = api.sessions_list(None, Some(3)).await.expect("listing");
    assert!(listing.complete);
    let mut keys: Vec<String> = listing
        .rows
        .iter()
        .map(|row| {
            format!(
                "{}/{}",
                row["device"].as_str().unwrap_or_default(),
                row["session_id"].as_str().unwrap_or_default()
            )
        })
        .collect();
    keys.sort();
    let mut expected = vec![
        format!("{}/a1", alpha.identity.id()),
        format!("{}/b1", beta.identity.id()),
        format!("{}/b2", beta.identity.id()),
    ];
    expected.sort();
    assert_eq!(keys, expected);
    let b1 = listing
        .rows
        .iter()
        .find(|row| row["session_id"] == "b1")
        .expect("b1 listed");
    assert_eq!(b1["durable_id"], "durable-b1");
    assert_eq!(b1["source"], "live");
    assert_eq!(listing.devices[beta.identity.id()], "answered");

    let only_beta = api
        .sessions_list(Some(json!({ "device": beta.identity.id() })), None)
        .await
        .expect("filtered listing");
    assert_eq!(only_beta.rows.len(), 2);
    assert!(
        only_beta
            .rows
            .iter()
            .all(|row| row.get("durable_id").is_none())
    );

    let attach = api
        .attach(&beta.identity.id(), "durable-b2")
        .await
        .expect("attach by durable id");
    let prompt = json!({ "type": "prompt", "message": "hello from alpha" });
    let outcome = api
        .command(&attach, prompt.clone(), None)
        .await
        .expect("command");
    assert_eq!(outcome.state, "engine_applied");
    assert_eq!(outcome.receipt, Some(json!({ "disposition": "started" })));
    assert_eq!(beta_host.records(), vec![("b2".to_owned(), prompt)]);
    assert!(alpha_host.records().is_empty());

    beta_host
        .sessions
        .lock()
        .expect("sessions")
        .retain(|session| session.session_id != "b2");
    let parked = api
        .command(
            &attach,
            json!({ "type": "prompt", "message": "again" }),
            None,
        )
        .await
        .expect("command");
    assert_eq!(parked.state, "engine_refused");
    assert_eq!(parked.reason.as_deref(), Some("route_unlisted"));

    let beta_api = client(&beta.state_dir).await;
    let control = beta_api
        .control_attach(Some(false))
        .await
        .expect("control off");
    assert_eq!(control["attached"], false);
    let refused = api
        .command(&attach, json!({ "type": "abort" }), None)
        .await
        .expect("command");
    assert_eq!(refused.state, "engine_refused");
    assert_eq!(refused.reason.as_deref(), Some("peer_unauthorized"));
    let listing = api.sessions_list(None, None).await.expect("listing");
    assert_eq!(listing.devices[beta.identity.id()], "undelivered");
    assert_eq!(listing.incomplete, vec![json!(beta.identity.id())]);
    assert_eq!(
        beta_host.records().len(),
        1,
        "nothing ran while control was off"
    );

    api.close().await;
    beta_api.close().await;
    alpha_relayd.shutdown().await;
    beta_relayd.shutdown().await;
}
