//! The principals of the security design (#14945, section 3). A root is built for exactly one, fixed when the root is
//! constructed: no call of the root takes a principal, a sender or a caller as an input. A principal selects which
//! existing leg a call uses and what it stamps on what it sends; it is never evidence of authority.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrincipalKind {
    Person,
    Script,
    Connector,
    Session,
    Service,
}

pub const PRINCIPAL_KINDS: [PrincipalKind; 5] = [
    PrincipalKind::Person,
    PrincipalKind::Script,
    PrincipalKind::Connector,
    PrincipalKind::Session,
    PrincipalKind::Service,
];

impl PrincipalKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Script => "script",
            Self::Connector => "connector",
            Self::Session => "session",
            Self::Service => "service",
        }
    }

    pub fn parse(kind: &str) -> Option<Self> {
        PRINCIPAL_KINDS
            .into_iter()
            .find(|candidate| candidate.as_str() == kind)
    }

    /// The field that identifies a principal of this kind (`PRINCIPAL_ID`); a service has none. A session is
    /// named by the durable id the engine hands a tool, never an id a model supplies.
    pub const fn id_field(self) -> Option<&'static str> {
        match self {
            Self::Person | Self::Script => Some("uid"),
            Self::Connector => Some("binding"),
            Self::Session => Some("session_id"),
            Self::Service => None,
        }
    }
}

/// Whether `value` is one of the five principals: a closed object with its `kind` and that kind's id, a uid being a
/// non-negative integer and a binding or session id a non-empty string.
pub fn is_principal(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(kind) = object
        .get("kind")
        .and_then(Value::as_str)
        .and_then(PrincipalKind::parse)
    else {
        return false;
    };
    let Some(field) = kind.id_field() else {
        return object.len() == 1;
    };
    if object.len() != 2 {
        return false;
    }
    match kind {
        PrincipalKind::Person | PrincipalKind::Script => {
            object.get(field).is_some_and(|uid| uid.as_u64().is_some())
        }
        _ => object
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty()),
    }
}
