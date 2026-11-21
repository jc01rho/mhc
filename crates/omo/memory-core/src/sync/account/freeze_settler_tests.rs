use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use serde_json::{Value, json};

use super::*;
use crate::sync::account::account_chain::open_account_chain;
use crate::sync::account::http::create_account_http;
use crate::sync::account::revocation_view::parse_revocation_view;
use crate::sync::crypto::anchor::{ChainAnchor, creator_anchor};
use crate::sync::crypto::rejection::RejectionCounter;
use crate::sync::crypto::signatures::{canonical, sha256_b64};
use crate::sync::engine::adapters::http_transport::{
    HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
};
use crate::sync::engine::error::EngineError;
use crate::sync::recovery::testing::{ACCOUNT, Server, device};

/// The service side of a freeze: the keyring rows, the `rotation_pending` flag, and how the next
/// rotation append is answered.
struct Service {
    server: Server,
    pending: Cell<bool>,
    /// Answer the next rotation `stale_base`, as when another member's rotation landed first.
    lose_race: Cell<bool>,
    refuse: Cell<bool>,
    appends: Cell<usize>,
}

impl Service {
    fn rows(&self) -> Vec<Value> {
        self.server.rows.lock().expect("rows").clone()
    }

    fn reply(status: u16, body: Value) -> HttpResponse {
        HttpResponse {
            status,
            body: body.to_string().into_bytes(),
        }
    }

    fn append(&self, body: &Value) -> HttpResponse {
        self.appends.set(self.appends.get() + 1);
        if self.lose_race.replace(false) {
            self.pending.set(false);
            return Self::reply(409, json!({"error":{"code":"stale_base"}}));
        }
        if self.refuse.get() {
            return Self::reply(403, json!({"error":{"code":"refused"}}));
        }
        let record = &body["record"];
        let mut rows = self.server.rows.lock().expect("rows");
        let seq = rows.len() as u64 + 1;
        let digest = sha256_b64(&canonical(record).expect("canonical"));
        rows.push(json!({"seq":seq,"base_seq":body["base_seq"],"kind":"keyring","account":ACCOUNT,"epoch":record["epoch"],"author":self.server.author,"digest":digest,"link":body["link"],"keyring_link":body["keyring_link"],"record_sig":body["record_sig"],"body":record,"wraps":body["wraps"]}));
        self.pending.set(false);
        Self::reply(200, json!({ "seq": seq }))
    }
}

struct Client(Rc<Service>);

impl HttpClient for Client {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let service = &self.0;
            let path = request.url.replace("https://test.invalid", "");
            let rows = service.rows();
            Ok(if request.method == "PUT" && path == "/v1/sync/keys" {
                let body: Value =
                    serde_json::from_slice(request.body.as_deref().unwrap_or(&b"{}"[..]))?;
                service.append(&body)
            } else if path == "/v1/sync/state" {
                let last = rows.last().cloned().unwrap_or(Value::Null);
                Service::reply(
                    200,
                    json!({"head":{"seq":last["seq"],"link":last["link"],"keyring_link":last["keyring_link"]},"current_epoch":last["epoch"],"rotation_pending":service.pending.get()}),
                )
            } else if path == "/v1/sync/keys" {
                Service::reply(200, json!({ "entries": rows }))
            } else {
                let after = path
                    .rsplit("after=")
                    .next()
                    .and_then(|n| n.parse::<u64>().ok())
                    .unwrap_or(0);
                let served: Vec<Value> = rows
                    .into_iter()
                    .filter(|r| r["seq"].as_u64().is_some_and(|seq| seq > after))
                    .collect();
                Service::reply(200, json!({ "entries": served }))
            })
        })
    }

    fn open_stream(&self, _request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(EngineError::Other("unused stream".into())) })
    }
}

struct World {
    service: Rc<Service>,
    settler: ChainFreezeSettler,
    chain: Rc<tokio::sync::Mutex<AccountChain>>,
    view: Rc<RefCell<RevocationView>>,
    events: Rc<RefCell<Vec<FreezeEvent>>>,
    self_id: String,
    _dir: tempfile::TempDir,
}

fn usable(self_id: &str, revoked: &[&str], self_revoked: bool) -> RevocationView {
    let revoked: BTreeSet<String> = revoked.iter().map(|id| id.to_string()).collect();
    let mut listed = revoked.clone();
    listed.insert(self_id.to_string());
    RevocationView::Usable {
        self_principal: self_id.to_string(),
        self_revoked,
        revoked,
        listed,
    }
}

async fn world(scope: &str) -> World {
    let dir = tempfile::tempdir().expect("temp");
    let keyring = Rc::new(device(1));
    let self_id = keyring.principal().id;
    let service = Rc::new(Service {
        server: Server::new(keyring.as_ref(), dir.path()),
        pending: Cell::new(true),
        lose_race: Cell::new(false),
        refuse: Cell::new(false),
        appends: Cell::new(0),
    });
    let http = create_account_http(HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
        client: Rc::new(Client(service.clone())),
    });
    let chain = Rc::new(tokio::sync::Mutex::new(
        open_account_chain(
            ACCOUNT.into(),
            ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), keyring.as_ref())),
            keyring.clone(),
            http.clone(),
            RejectionCounter::default(),
        )
        .await
        .expect("chain"),
    ));
    let view = Rc::new(RefCell::new(usable(&self_id, &["revoked-device"], false)));
    let events: Rc<RefCell<Vec<FreezeEvent>>> = Rc::new(RefCell::new(Vec::new()));
    let served_view = view.clone();
    let seen = events.clone();
    let settler = create_freeze_settler(FreezeSettlerInput {
        chain: Rc::new(SharedAccountChain {
            account: ACCOUNT.into(),
            chain: chain.clone(),
        }),
        http,
        keyring,
        revocation_view: Rc::new(move || {
            let view = served_view.borrow().clone();
            Box::pin(async move { Ok(view) })
        }),
        scope: scope.into(),
        on_event: Some(Rc::new(move |event: FreezeEvent| {
            seen.borrow_mut().push(event)
        })),
        fault: None,
        sleep: Some(Rc::new(|_: u64| Box::pin(async { Ok(()) }))),
    });
    World {
        service,
        settler,
        chain,
        view,
        events,
        self_id,
        _dir: dir,
    }
}

#[tokio::test]
async fn no_pending_rotation_is_clear_and_appends_nothing() {
    let w = world("freeze-clear").await;
    w.service.pending.set(false);
    assert_eq!(w.settler.settle().await, FreezeOutcome::Clear);
    assert_eq!(w.service.appends.get(), 0);
}

#[tokio::test]
async fn a_member_rotates_excluding_the_revoked_and_the_chain_shows_it() {
    let w = world("freeze-rotate").await;
    assert_eq!(w.settler.settle().await, FreezeOutcome::Rotated);
    assert_eq!(w.service.appends.get(), 1);
    let rows = w.service.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["body"]["kind"], "rotation");
    let members = rows[1]["body"]["members"].as_array().expect("members");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["id"], w.self_id.as_str());
    let state = w.chain.lock().await.state().clone();
    assert_eq!(state.authority().map(|a| a.epoch), Some(2));
    assert!(w.chain.lock().await.epoch_key(2).is_some());
    assert_eq!(*w.events.borrow(), vec![FreezeEvent::Rotated]);
    // Rule 7: a flag the service keeps set after this rotation buys no second append.
    w.service.pending.set(true);
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::FlagStuck)
    );
    assert_eq!(w.service.appends.get(), 1);
}

#[tokio::test]
async fn losing_the_rotation_race_adopts_the_winner_without_a_second_append() {
    let w = world("freeze-race").await;
    w.service.lose_race.set(true);
    assert_eq!(w.settler.settle().await, FreezeOutcome::Adopted);
    assert_eq!(w.service.appends.get(), 1);
    assert_eq!(*w.events.borrow(), vec![FreezeEvent::RotationAdopted]);
}

#[tokio::test]
async fn a_revoked_or_unlisted_device_never_rotates() {
    let w = world("freeze-self-revoked").await;
    *w.view.borrow_mut() = usable(&w.self_id, &[], true);
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::SelfRevoked)
    );
    *w.view.borrow_mut() = RevocationView::Unusable;
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::DeviceListUnusable)
    );
    *w.view.borrow_mut() = usable("someone-else", &[], false);
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::DeviceListUnusable)
    );
    assert_eq!(w.service.appends.get(), 0);
}

#[tokio::test]
async fn a_refused_rotation_is_remembered_until_the_head_or_revoked_set_moves() {
    let w = world("freeze-refused").await;
    w.service.refuse.set(true);
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::Refused)
    );
    assert_eq!(w.service.appends.get(), 1);
    assert_eq!(*w.events.borrow(), vec![FreezeEvent::RotationRefused]);
    assert_eq!(
        w.settler.settle().await,
        FreezeOutcome::Frozen(FreezeReason::Refused)
    );
    assert_eq!(w.service.appends.get(), 1);
    // A new revoke changes the set, so the device tries again.
    *w.view.borrow_mut() = usable(&w.self_id, &["revoked-device", "another"], false);
    w.service.refuse.set(false);
    assert_eq!(w.settler.settle().await, FreezeOutcome::Rotated);
    assert_eq!(w.service.appends.get(), 2);
}

#[test]
fn the_device_list_view_reads_only_its_three_fields() {
    let body = json!({"devices":[
        {"syncPrincipal":"a","revokedAt":null,"current":true,"platform":"unknown-os"},
        {"syncPrincipal":"b","revokedAt":"2026-11-01T00:00:00Z"},
        {"syncPrincipal":"","revokedAt":null},
        {"syncPrincipal":"c","revokedAt":7},
        "not a row"
    ]});
    let RevocationView::Usable {
        self_principal,
        self_revoked,
        revoked,
        listed,
    } = parse_revocation_view(&body)
    else {
        panic!("a list naming the current device is usable");
    };
    assert_eq!(self_principal, "a");
    assert!(!self_revoked);
    assert_eq!(revoked, BTreeSet::from(["b".to_string()]));
    assert_eq!(listed, BTreeSet::from(["a".to_string(), "b".to_string()]));
    assert_eq!(
        parse_revocation_view(&json!({"devices":[{"syncPrincipal":"a","revokedAt":null}]})),
        RevocationView::Unusable
    );
    assert_eq!(parse_revocation_view(&json!({})), RevocationView::Unusable);
}
