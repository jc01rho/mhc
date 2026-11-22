//! The client-side release gate for creating a recovery-phrase key (contract amendment A1, #13335).
//! `enabled` is the engine flag, off by default; it may be turned on only after the service floor
//! refuses older clients, and the floor is re-read on every creation. Fail-closed: no gate, the flag
//! off, or a floor that is not positively `on` all refuse creation.

use std::time::Duration;

use super::super::account::http::AccountHttp;

pub const FLOOR_TIMEOUT_MS: u64 = 10_000;

/// The service's account-scoped client floor (omo-service #1069/#1070) as this device reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFloor {
    On,
    Off,
    Unknown,
}

impl ServiceFloor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseGateRefusal {
    Disabled,
    ServiceFloorOff,
}

impl PhraseGateRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ServiceFloorOff => "service-floor-off",
        }
    }
}

/// `GET /v1/sync/state` `accepts_phrase_keys`: true is on; false or absent (an older service) is off;
/// an error or timeout is unknown.
pub async fn read_service_floor_within(chain: &AccountHttp, timeout: Duration) -> ServiceFloor {
    match tokio::time::timeout(timeout, chain.state()).await {
        Ok(Ok(Some(state))) => {
            if state.accepts_phrase_keys {
                ServiceFloor::On
            } else {
                ServiceFloor::Off
            }
        }
        _ => ServiceFloor::Unknown,
    }
}

pub async fn read_service_floor(chain: &AccountHttp) -> ServiceFloor {
    read_service_floor_within(chain, Duration::from_millis(FLOOR_TIMEOUT_MS)).await
}

pub struct PhraseGate {
    /// The engine flag: phrase creation stays off until the host turns it on.
    pub enabled: bool,
    /// The floor this ceremony read, fresh from [`read_service_floor`]; never cached across ceremonies.
    pub service_floor: ServiceFloor,
}

impl PhraseGate {
    /// The default gate: phrase creation off, no floor read.
    pub fn off() -> Self {
        Self {
            enabled: false,
            service_floor: ServiceFloor::Unknown,
        }
    }
}

/// Fail-closed: no gate, the flag off, or a floor that is not positively on all refuse creation;
/// `None` is the only answer that allows it.
pub fn phrase_creation_refusal(
    gate: Option<&PhraseGate>,
    service_floor: ServiceFloor,
) -> Option<PhraseGateRefusal> {
    let Some(gate) = gate.filter(|gate| gate.enabled) else {
        return Some(PhraseGateRefusal::Disabled);
    };
    match (gate.service_floor, service_floor) {
        (ServiceFloor::On, ServiceFloor::On) => None,
        _ => Some(PhraseGateRefusal::ServiceFloorOff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(floor: ServiceFloor) -> PhraseGate {
        PhraseGate {
            enabled: true,
            service_floor: floor,
        }
    }

    #[test]
    fn creation_needs_the_flag_and_a_floor_read_as_on() {
        assert_eq!(
            phrase_creation_refusal(None, ServiceFloor::On),
            Some(PhraseGateRefusal::Disabled)
        );
        assert_eq!(
            phrase_creation_refusal(Some(&PhraseGate::off()), ServiceFloor::On),
            Some(PhraseGateRefusal::Disabled)
        );
        for floor in [ServiceFloor::Off, ServiceFloor::Unknown] {
            assert_eq!(
                phrase_creation_refusal(Some(&enabled(floor)), floor),
                Some(PhraseGateRefusal::ServiceFloorOff)
            );
        }
        assert_eq!(
            phrase_creation_refusal(Some(&enabled(ServiceFloor::On)), ServiceFloor::On),
            None
        );
    }

    enum FloorReply {
        Json(u16, serde_json::Value),
        Stall,
    }

    struct StateClient(FloorReply);

    impl crate::sync::engine::adapters::http_transport::HttpClient for StateClient {
        fn send(
            &self,
            _request: crate::sync::engine::adapters::http_transport::HttpRequest,
        ) -> crate::sync::engine::ports::PortFuture<
            '_,
            crate::sync::engine::adapters::http_transport::HttpResponse,
        > {
            match &self.0 {
                FloorReply::Json(status, body) => {
                    let status = *status;
                    let bytes = body.to_string().into_bytes();
                    Box::pin(async move {
                        Ok(
                            crate::sync::engine::adapters::http_transport::HttpResponse {
                                status,
                                body: bytes,
                            },
                        )
                    })
                }
                FloorReply::Stall => Box::pin(std::future::pending()),
            }
        }

        fn open_stream(
            &self,
            _request: crate::sync::engine::adapters::http_transport::HttpRequest,
        ) -> crate::sync::engine::ports::PortFuture<
            '_,
            crate::sync::engine::adapters::http_transport::StreamResponse,
        > {
            Box::pin(async { Err(crate::sync::engine::error::EngineError::Network) })
        }
    }

    fn account_http(reply: FloorReply) -> AccountHttp {
        use std::rc::Rc;
        crate::sync::account::http::create_account_http(
            crate::sync::engine::adapters::http_transport::HttpTransportOptions {
                base_url: "https://test.invalid".into(),
                token: Rc::new(|| Box::pin(async { Ok("token".into()) })),
                client: Rc::new(StateClient(reply)),
            },
        )
    }

    fn state_json(accepts: Option<bool>) -> serde_json::Value {
        let mut value = serde_json::json!({
            "head": {"seq": 1, "link": "link", "keyring_link": "keyring"},
            "current_epoch": 1,
            "rotation_pending": false,
        });
        if let Some(flag) = accepts {
            value["accepts_phrase_keys"] = serde_json::json!(flag);
        }
        value
    }

    #[tokio::test]
    async fn the_service_floor_reads_accepts_phrase_keys() {
        assert_eq!(
            read_service_floor(&account_http(FloorReply::Json(200, state_json(Some(true))))).await,
            ServiceFloor::On
        );
        assert_eq!(
            read_service_floor(&account_http(FloorReply::Json(
                200,
                state_json(Some(false))
            )))
            .await,
            ServiceFloor::Off
        );
        assert_eq!(
            read_service_floor(&account_http(FloorReply::Json(200, state_json(None)))).await,
            ServiceFloor::Off
        );
        assert_eq!(
            read_service_floor(&account_http(FloorReply::Json(
                500,
                serde_json::json!({"error": {"code": "unavailable"}})
            )))
            .await,
            ServiceFloor::Unknown
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_floor_read_counts_as_unknown() {
        let floor =
            read_service_floor_within(&account_http(FloorReply::Stall), Duration::from_millis(1))
                .await;
        assert_eq!(floor, ServiceFloor::Unknown);
        assert_eq!(
            phrase_creation_refusal(Some(&enabled(floor)), floor),
            Some(PhraseGateRefusal::ServiceFloorOff)
        );
    }
}
