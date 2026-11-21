use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};

use super::device_switch::*;
use super::http::create_account_http;
use super::pairing_http::create_pairing_http;
use crate::sync::engine::adapters::http_transport::{
    HttpClient, HttpRequest, HttpResponse, HttpTransportOptions, StreamResponse,
};
use crate::sync::engine::error::EngineError;
use crate::sync::engine::ports::PortFuture;

struct Service {
    confirm: (u16, Value),
    seen: RefCell<Vec<(String, String, Value)>>,
}

impl HttpClient for Service {
    fn send(&self, request: HttpRequest) -> PortFuture<'_, HttpResponse> {
        Box::pin(async move {
            let path = request.url.replace("https://test.invalid", "");
            let body: Value = request
                .body
                .as_deref()
                .map(serde_json::from_slice)
                .transpose()?
                .unwrap_or(Value::Null);
            self.seen
                .borrow_mut()
                .push((request.method.to_string(), path.clone(), body));
            let (status, reply) = if path == "/v1/confirm" {
                self.confirm.clone()
            } else {
                (204, Value::Null)
            };
            Ok(HttpResponse {
                status,
                body: reply.to_string().into_bytes(),
            })
        })
    }
    fn open_stream(&self, _request: HttpRequest) -> PortFuture<'_, StreamResponse> {
        Box::pin(async { Err(EngineError::Other("unused stream".into())) })
    }
}

fn pairing(service: Rc<Service>) -> super::pairing_http::PairingHttp {
    create_pairing_http(create_account_http(HttpTransportOptions {
        base_url: "https://test.invalid".into(),
        token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
        client: service,
    }))
}

#[tokio::test]
async fn disable_turns_the_service_switch_off_then_marks_only_this_device()
-> Result<(), Box<dyn std::error::Error>> {
    let pins = tempfile::tempdir()?;
    let service = Rc::new(Service {
        confirm: (200, json!({"ready":true,"token":"once"})),
        seen: RefCell::new(Vec::new()),
    });
    let http = pairing(service.clone());
    assert!(!is_device_sync_off(pins.path(), "acct")?);
    assert_eq!(
        disable_sync(&http, "dev 1", pins.path(), "acct").await?,
        DisableOutcome::Off
    );
    assert!(is_device_sync_off(pins.path(), "acct")?);
    assert!(!is_device_sync_off(pins.path(), "other")?);
    {
        let seen = service.seen.borrow();
        assert_eq!(seen[1].0, "PUT");
        assert_eq!(seen[1].1, "/v1/devices/dev%201/sync");
        assert_eq!(seen[1].2, json!({"enabled":false,"confirmToken":"once"}));
    }
    turn_device_sync_on(&http, "dev 1", pins.path(), "acct").await?;
    assert!(!is_device_sync_off(pins.path(), "acct")?);
    Ok(())
}

#[tokio::test]
async fn a_refused_confirmation_leaves_this_device_on() -> Result<(), Box<dyn std::error::Error>> {
    let pins = tempfile::tempdir()?;
    for confirm in [
        (200, json!({"ready":false,"code":"last_device"})),
        (403, json!({"error":{"code":"forbidden"}})),
    ] {
        let service = Rc::new(Service {
            confirm,
            seen: RefCell::new(Vec::new()),
        });
        let outcome = disable_sync(&pairing(service.clone()), "d", pins.path(), "acct").await?;
        assert!(matches!(outcome, DisableOutcome::Refused { .. }));
        assert_eq!(service.seen.borrow().len(), 1);
        assert!(!is_device_sync_off(pins.path(), "acct")?);
    }
    let unreachable = Rc::new(Service {
        confirm: (503, json!({})),
        seen: RefCell::new(Vec::new()),
    });
    assert!(
        disable_sync(&pairing(unreachable), "d", pins.path(), "acct")
            .await
            .is_err()
    );
    assert!(!is_device_sync_off(pins.path(), "acct")?);
    Ok(())
}
