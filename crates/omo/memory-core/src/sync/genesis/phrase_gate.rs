use crate::sync::account::http::AccountHttp;
use std::{future::Future, pin::Pin, time::Duration};

/// The service's account-scoped client floor as this device reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceFloor {
    On,
    Off,
    Unknown,
}

const FLOOR_TIMEOUT_MS: u64 = 10_000;

/// The client-side release gate for creating a recovery-phrase key (contract amendment A1). `enabled`
/// is the engine flag, off by default; it may be turned on only after the service floor refuses older
/// clients, and the floor is re-read on every creation. `refused` is the loud log line a refused
/// creation writes; it receives a reason and the account id, never a secret. `service_floor` is
/// injected only by tests and, once the A1 delta conditions hold, by the host with
/// [`read_service_floor`]; absent, the floor is unknown.
pub trait ServiceFloorPort {
    fn read<'a>(
        &'a self,
        chain: &'a AccountHttp,
    ) -> Pin<Box<dyn Future<Output = ServiceFloor> + 'a>>;
}

pub type RefusedLog<'a> = &'a dyn Fn(PhraseGateRefusal, &str);

pub struct PhraseGate<'a> {
    pub enabled: bool,
    pub service_floor: Option<&'a dyn ServiceFloorPort>,
    /// How long a floor read may take before it counts as unknown; tests shorten it.
    pub floor_timeout_ms: Option<u64>,
    pub refused: Option<RefusedLog<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhraseGateRefusal {
    Disabled,
    ServiceFloorOff,
}

pub struct AccountStateFloor;

/// `GET /v1/sync/state` `accepts_phrase_keys`: true is on; false or absent (an older service) is off;
/// an error is unknown.
pub async fn read_service_floor(chain: &AccountHttp) -> ServiceFloor {
    match chain.state().await {
        Ok(Some(state)) => {
            if state.accepts_phrase_keys {
                ServiceFloor::On
            } else {
                ServiceFloor::Off
            }
        }
        _ => ServiceFloor::Unknown,
    }
}

impl ServiceFloorPort for AccountStateFloor {
    fn read<'a>(
        &'a self,
        chain: &'a AccountHttp,
    ) -> Pin<Box<dyn Future<Output = ServiceFloor> + 'a>> {
        Box::pin(read_service_floor(chain))
    }
}

/// One fresh floor read per ceremony, never cached (A1.1-M4). A port that outlasts its timeout or
/// answers anything but on/off reads as unknown; with no port the answer is unknown, because
/// production wires [`AccountStateFloor`] only after the A1 delta conditions hold.
async fn floor_of(gate: &PhraseGate<'_>, chain: &AccountHttp) -> ServiceFloor {
    let Some(port) = gate.service_floor else {
        return ServiceFloor::Unknown;
    };
    let read = port.read(chain);
    match tokio::time::timeout(
        Duration::from_millis(gate.floor_timeout_ms.unwrap_or(FLOOR_TIMEOUT_MS)),
        read,
    )
    .await
    {
        Ok(ServiceFloor::On) => ServiceFloor::On,
        Ok(ServiceFloor::Off) => ServiceFloor::Off,
        _ => ServiceFloor::Unknown,
    }
}

/// Fail-closed: no gate, the flag off, or a floor that is not positively on all refuse creation.
pub async fn phrase_creation_refusal(
    gate: Option<&PhraseGate<'_>>,
    account: &str,
    chain: &AccountHttp,
) -> Option<PhraseGateRefusal> {
    let Some(gate) = gate.filter(|gate| gate.enabled) else {
        return Some(PhraseGateRefusal::Disabled);
    };
    if floor_of(gate, chain).await == ServiceFloor::On {
        return None;
    }
    if let Some(log) = gate.refused {
        log(PhraseGateRefusal::ServiceFloorOff, account);
    }
    Some(PhraseGateRefusal::ServiceFloorOff)
}
