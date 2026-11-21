//! The command_id ledger (local API "Commands"): a command_id binds once to its target session and
//! the SHA-256 of its payload. A repeat with the same binding answers the same id and dispatches
//! nothing more; any other repeat is `command_id_conflict`.

use std::collections::{HashMap, VecDeque};

use device_service_core::local_api::limits::sha256_hex;
use device_service_core::local_api::vocabulary::COMMAND_ID_CHARS;
use serde_json::Value;

/// Bindings kept; the oldest goes first once full.
pub const LEDGER_CAPACITY: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    target: String,
    digest: String,
    outcome: Option<Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Admission {
    /// First use: dispatch it.
    New,
    /// The same binding again: dispatch nothing; its outcome so far, if any.
    Repeat(Option<Value>),
    /// Another target or payload under a used id.
    Conflict,
}

#[derive(Debug, Default)]
pub struct CommandLedger {
    bindings: HashMap<String, Binding>,
    order: VecDeque<String>,
}

pub fn payload_digest(payload: &Value) -> String {
    sha256_hex(&serde_json::to_vec(payload).unwrap_or_default())
}

/// Exactly 22 base64url characters.
pub fn valid_command_id(command_id: &str) -> bool {
    command_id.len() == COMMAND_ID_CHARS
        && command_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

impl CommandLedger {
    pub fn admit(&mut self, command_id: &str, target: &str, payload: &Value) -> Admission {
        let digest = payload_digest(payload);
        if let Some(binding) = self.bindings.get(command_id) {
            return if binding.target == target && binding.digest == digest {
                Admission::Repeat(binding.outcome.clone())
            } else {
                Admission::Conflict
            };
        }
        if self.order.len() >= LEDGER_CAPACITY
            && let Some(oldest) = self.order.pop_front()
        {
            self.bindings.remove(&oldest);
        }
        self.order.push_back(command_id.to_owned());
        self.bindings.insert(
            command_id.to_owned(),
            Binding {
                target: target.to_owned(),
                digest,
                outcome: None,
            },
        );
        Admission::New
    }

    /// Records the terminal outcome a repeat is answered with.
    pub fn settle(&mut self, command_id: &str, outcome: Value) {
        if let Some(binding) = self.bindings.get_mut(command_id) {
            binding.outcome = Some(outcome);
        }
    }
}
