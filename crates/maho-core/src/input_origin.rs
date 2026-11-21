//! Port of senpi packages/coding-agent/src/core/input-origin.ts.

use maho_ext_api::InputSource;

use crate::manual_continue::MANUAL_CONTINUE_CUSTOM_TYPE;

/// senpi continue-from-leaf.ts `CONTINUE_FROM_LEAF_CUSTOM_TYPE`.
const CONTINUE_FROM_LEAF_CUSTOM_TYPE: &str = "continue-from-leaf";

/// Who started an input, classified once at its entry point (senpi#3853).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// RPC prompt / steer / follow_up / answer and TUI submit.
    Person,
    /// Extension API sends (`pi.sendUserMessage`).
    Extension,
    /// Goal, monitor, codemode and other automation turns.
    Engine,
}

/// A `.` manual continue or a continue-from-leaf is the user's own request, not an automatic turn
/// (senpi engine-turn-limit.ts `isUserDirectedTurn`).
fn is_user_directed_turn(custom_type: &str) -> bool {
    custom_type == MANUAL_CONTINUE_CUSTOM_TYPE || custom_type == CONTINUE_FROM_LEAF_CUSTOM_TYPE
}

pub fn input_origin_of(source: Option<InputSource>) -> InputOrigin {
    if source == Some(InputSource::Extension) {
        InputOrigin::Extension
    } else {
        InputOrigin::Person
    }
}

pub fn custom_turn_origin(custom_type: &str) -> InputOrigin {
    if is_user_directed_turn(custom_type) {
        InputOrigin::Person
    } else {
        InputOrigin::Engine
    }
}

pub const SAME_ACCOUNT_DELEGATION_CAPABILITY: &str = "same_account_delegation_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionOrigin {
    Local,
    Delegated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOrigin {
    Local,
    Delegated,
    Engine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTurnMark {
    Local,
    Delegated,
    Engine,
    Join,
}

pub const fn resolve_connection_turn_origin(
    connection_origin: ConnectionOrigin,
    same_account: bool,
) -> ConnectionOrigin {
    match (connection_origin, same_account) {
        (ConnectionOrigin::Delegated, false) => ConnectionOrigin::Delegated,
        _ => ConnectionOrigin::Local,
    }
}

pub const fn record_turn_origin(
    origin: ConnectionOrigin,
    is_acting_method_class: bool,
) -> ConnectionOrigin {
    if matches!(origin, ConnectionOrigin::Delegated) && is_acting_method_class {
        ConnectionOrigin::Delegated
    } else {
        ConnectionOrigin::Local
    }
}

pub fn input_turn_mark(
    mode: Option<&str>,
    source: Option<InputSource>,
    connection_origin: Option<ConnectionOrigin>,
    record_delegated: bool,
) -> InputTurnMark {
    if mode == Some("steer") {
        return InputTurnMark::Join;
    }
    if let Some(origin) = connection_origin {
        return match origin {
            ConnectionOrigin::Local => InputTurnMark::Local,
            ConnectionOrigin::Delegated => InputTurnMark::Delegated,
        };
    }
    if record_delegated {
        return InputTurnMark::Delegated;
    }
    if source.is_none() || source == Some(InputSource::Interactive) {
        InputTurnMark::Local
    } else {
        InputTurnMark::Engine
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOriginTracker {
    current_origin: TurnOrigin,
    continuing_turn: bool,
    open_record_scope: Option<ConnectionOrigin>,
}

impl Default for TurnOriginTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnOriginTracker {
    pub const fn new() -> Self {
        Self {
            current_origin: TurnOrigin::Engine,
            continuing_turn: false,
            open_record_scope: None,
        }
    }

    pub fn begin_record_scope(&mut self, origin: ConnectionOrigin) {
        self.open_record_scope = Some(origin);
    }

    pub fn end_record_scope(&mut self) {
        self.open_record_scope = None;
    }

    pub fn record_is_delegated(&self) -> bool {
        self.open_record_scope == Some(ConnectionOrigin::Delegated)
    }

    pub fn note_run_start(&mut self) {
        if self.continuing_turn {
            return;
        }
        let delegated = self.record_is_delegated();
        self.open_record_scope = None;
        self.current_origin = if delegated {
            TurnOrigin::Delegated
        } else {
            TurnOrigin::Engine
        };
    }

    pub fn note_turn_continuation(&mut self, keeps_turn_origin: bool) {
        if keeps_turn_origin {
            self.continuing_turn = true;
        }
    }

    pub fn end_turn_continuation(&mut self) {
        self.continuing_turn = false;
    }

    pub fn note_user_message_start(&mut self, mark: Option<InputTurnMark>) {
        match mark {
            Some(InputTurnMark::Join) => {}
            Some(InputTurnMark::Local) => self.current_origin = TurnOrigin::Local,
            Some(InputTurnMark::Delegated) => self.current_origin = TurnOrigin::Delegated,
            Some(InputTurnMark::Engine) | None => self.current_origin = TurnOrigin::Engine,
        }
    }

    pub const fn current_origin(&self) -> TurnOrigin {
        self.current_origin
    }

    pub fn deciding_turn_is_delegated(&self) -> bool {
        self.current_origin == TurnOrigin::Delegated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_extension_sources_are_extension_origin() {
        assert_eq!(
            input_origin_of(Some(InputSource::Extension)),
            InputOrigin::Extension
        );
        assert_eq!(input_origin_of(Some(InputSource::Rpc)), InputOrigin::Person);
        assert_eq!(
            input_origin_of(Some(InputSource::Interactive)),
            InputOrigin::Person
        );
        assert_eq!(input_origin_of(None), InputOrigin::Person);
    }

    #[test]
    fn user_directed_custom_turns_are_person_origin() {
        assert_eq!(custom_turn_origin("manual-continue"), InputOrigin::Person);
        assert_eq!(
            custom_turn_origin("continue-from-leaf"),
            InputOrigin::Person
        );
        assert_eq!(custom_turn_origin("goal"), InputOrigin::Engine);
    }
}
