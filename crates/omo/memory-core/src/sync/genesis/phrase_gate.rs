//! The client-side release gate for creating a recovery-phrase key (contract amendment A1, #13335).
//! `enabled` is the engine flag, off by default; it may be turned on only after the service floor
//! refuses older clients, and the floor is re-read on every creation. Fail-closed: no gate, the flag
//! off, or a floor that is not positively `on` all refuse creation.

use super::super::account::http::AccountHttp;

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
}
