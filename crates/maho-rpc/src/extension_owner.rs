use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionOwnerIdentity {
    pub owner_id: String,
    pub binding_generation: String,
}

#[derive(Debug, Clone)]
pub struct ExtensionOwnerWire {
    pub owner_id: String,
    pub binding_generation: String,
    pub retirement: Arc<AtomicI32>,
}

impl ExtensionOwnerWire {
    pub fn identity(&self) -> ExtensionOwnerIdentity {
        ExtensionOwnerIdentity {
            owner_id: self.owner_id.clone(),
            binding_generation: self.binding_generation.clone(),
        }
    }
}

pub fn carries_extension_owner(command: &Value) -> bool {
    let kind = command
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    kind == "extension_request" || kind.starts_with("eval_cells.")
}

#[derive(Clone)]
pub struct ExtensionRpcRequestContext {
    owner_id: String,
    disconnect_signal: AbortSignal,
    state: Arc<AtomicI32>,
}

impl ExtensionRpcRequestContext {
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn disconnect_signal(&self) -> &AbortSignal {
        &self.disconnect_signal
    }

    pub fn admit<T>(&self, dispatch: impl FnOnce() -> T) -> Result<T, &'static str> {
        if self.state.load(Ordering::SeqCst) != 0 {
            return Err("RPC owner retired");
        }
        Ok(dispatch())
    }
}

#[derive(Clone)]
pub struct ExtensionOwner {
    state: Arc<AtomicI32>,
    cancellation: AbortController,
    context: ExtensionRpcRequestContext,
    wire: ExtensionOwnerWire,
}

impl ExtensionOwner {
    pub fn new(wire: ExtensionOwnerWire) -> Self {
        let cancellation = AbortController::new();
        let state = wire.retirement.clone();
        let context = ExtensionRpcRequestContext {
            owner_id: wire.owner_id.clone(),
            disconnect_signal: cancellation.signal(),
            state: state.clone(),
        };
        Self {
            state,
            cancellation,
            context,
            wire,
        }
    }

    pub fn context(&self) -> &ExtensionRpcRequestContext {
        &self.context
    }

    pub fn wire(&self) -> &ExtensionOwnerWire {
        &self.wire
    }

    pub fn retired(&self) -> bool {
        self.state.load(Ordering::SeqCst) != 0
    }

    pub fn retire(&self) {
        self.state.store(1, Ordering::SeqCst);
        self.cancellation.abort(None);
    }
}

struct TransportState {
    bindings: BTreeMap<String, ExtensionOwner>,
    closed: bool,
}

#[derive(Clone)]
pub struct ExtensionTransportOwner {
    owner_id: String,
    state: Arc<Mutex<TransportState>>,
}

impl Default for ExtensionTransportOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionTransportOwner {
    pub fn new() -> Self {
        Self {
            owner_id: Uuid::new_v4().to_string(),
            state: Arc::new(Mutex::new(TransportState {
                bindings: BTreeMap::new(),
                closed: false,
            })),
        }
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn retired(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
    }

    pub fn capture(&self, binding_generation: &str) -> Result<ExtensionOwner, &'static str> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            return Err("RPC transport retired");
        }
        if let Some(existing) = state.bindings.get(binding_generation) {
            return Ok(existing.clone());
        }
        let owner = ExtensionOwner::new(ExtensionOwnerWire {
            owner_id: self.owner_id.clone(),
            binding_generation: binding_generation.to_owned(),
            retirement: Arc::new(AtomicI32::new(0)),
        });
        state
            .bindings
            .insert(binding_generation.to_owned(), owner.clone());
        Ok(owner)
    }

    pub fn peek(&self, binding_generation: &str) -> Option<ExtensionOwner> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.closed {
            None
        } else {
            state.bindings.get(binding_generation).cloned()
        }
    }

    pub fn retire_binding(&self, binding_generation: &str) {
        let owner = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .bindings
            .get(binding_generation)
            .cloned();
        if let Some(owner) = owner {
            owner.retire();
        }
    }

    pub fn retire(&self) {
        let owners: Vec<ExtensionOwner> = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            std::mem::take(&mut state.bindings).into_values().collect()
        };
        for owner in owners {
            owner.retire();
        }
    }
}

struct LifetimeState {
    generation: String,
    captures: Vec<ExtensionOwner>,
    seen: HashSet<(String, String)>,
}

pub struct ExtensionOwnerLifetime {
    classic: Option<ExtensionTransportOwner>,
    state: Mutex<LifetimeState>,
    source: Option<Arc<dyn Fn() -> Option<ExtensionOwner> + Send + Sync>>,
    on_retire_binding: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ExtensionOwnerLifetime {
    pub fn new(
        is_classic: bool,
        source: Option<Arc<dyn Fn() -> Option<ExtensionOwner> + Send + Sync>>,
        on_retire_binding: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let classic = (is_classic && source.is_none()).then(ExtensionTransportOwner::new);
        Self {
            classic,
            state: Mutex::new(LifetimeState {
                generation: Uuid::new_v4().to_string(),
                captures: Vec::new(),
                seen: HashSet::new(),
            }),
            source,
            on_retire_binding,
        }
    }

    pub fn capture(&self) -> Option<ExtensionOwner> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = match &self.source {
            Some(source) => source(),
            None => self
                .classic
                .as_ref()
                .and_then(|classic| classic.capture(&state.generation).ok()),
        };
        if let Some(owner) = &owner {
            let key = (
                owner.wire.owner_id.clone(),
                owner.wire.binding_generation.clone(),
            );
            if state.seen.insert(key) {
                state.captures.push(owner.clone());
            }
        }
        owner
    }

    pub fn retire_binding(&self) {
        let (captures, had_captures) = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let had = !state.captures.is_empty();
            state.generation = Uuid::new_v4().to_string();
            state.seen.clear();
            (std::mem::take(&mut state.captures), had)
        };
        for owner in captures {
            owner.retire();
        }
        if had_captures && let Some(hook) = &self.on_retire_binding {
            hook();
        }
    }

    pub fn dispose(&self) {
        self.retire_binding();
        if let Some(classic) = &self.classic {
            classic.retire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn carries_extension_owner_matches_only_extension_requests_and_eval_cells() {
        assert!(carries_extension_owner(
            &json!({"type": "extension_request"})
        ));
        assert!(carries_extension_owner(&json!({"type": "eval_cells.list"})));
        assert!(!carries_extension_owner(&json!({"type": "get_state"})));
    }

    #[test]
    fn owner_gate_admits_until_retired_and_aborts_disconnect_signal() {
        let transport = ExtensionTransportOwner::new();
        let owner = transport.capture("gen-1").expect("captured");
        assert!(!owner.retired());
        assert!(!owner.context().disconnect_signal().aborted());
        assert_eq!(owner.context().admit(|| 42), Ok(42));

        transport.retire_binding("gen-1");
        assert!(owner.retired());
        assert!(owner.context().disconnect_signal().aborted());
        assert_eq!(owner.context().admit(|| 42), Err("RPC owner retired"));
    }
}
