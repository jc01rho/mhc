use crate::{
    extension_owner::{
        ExtensionOwner, ExtensionOwnerIdentity, ExtensionOwnerWire, carries_extension_owner,
    },
    session_connection_context::SessionConnectionContext,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};
use uuid::Uuid;

struct GatesState {
    generation: String,
    issued: BTreeMap<String, Vec<ExtensionOwner>>,
}

pub struct WorkerOwnerGates {
    state: Mutex<GatesState>,
}

impl Default for WorkerOwnerGates {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerOwnerGates {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(GatesState {
                generation: Uuid::new_v4().to_string(),
                issued: BTreeMap::new(),
            }),
        }
    }

    pub fn capture(
        &self,
        context: &SessionConnectionContext,
        connection: Option<&str>,
        command: &Value,
    ) -> Option<ExtensionOwnerWire> {
        let connection = connection?;
        let transport = context.transport()?;
        if !carries_extension_owner(command) {
            return None;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut owner = transport.capture(&state.generation).ok()?;
        if owner.retired() {
            state.generation = Uuid::new_v4().to_string();
            owner = transport.capture(&state.generation).ok()?;
        }
        let entry = state.issued.entry(connection.to_owned()).or_default();
        if !entry.iter().any(|existing| {
            existing.wire().owner_id == owner.wire().owner_id
                && existing.wire().binding_generation == owner.wire().binding_generation
        }) {
            entry.push(owner.clone());
        }
        Some(owner.wire().clone())
    }

    pub fn detach(
        &self,
        context: &SessionConnectionContext,
        connection: &str,
    ) -> Vec<ExtensionOwnerIdentity> {
        let owners = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(owners) = state.issued.remove(connection) else {
                return Vec::new();
            };
            owners
        };
        let current = context.transport_of(connection);
        let mut kept = Vec::new();
        let mut retired = Vec::new();
        for owner in owners {
            if current
                .as_ref()
                .is_some_and(|cur| cur.owner_id() != owner.wire().owner_id)
                && !owner.retired()
            {
                kept.push(owner);
            } else {
                owner.retire();
                retired.push(owner.wire().identity());
            }
        }
        if !kept.is_empty() {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .issued
                .insert(connection.to_owned(), kept);
        }
        retired
    }

    pub fn retire_all(&self) -> Vec<ExtensionOwnerIdentity> {
        let issued = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.generation = Uuid::new_v4().to_string();
            std::mem::take(&mut state.issued)
        };
        let mut retired = Vec::new();
        for owners in issued.into_values() {
            for owner in owners {
                owner.retire();
                retired.push(owner.wire().identity());
            }
        }
        retired
    }
}

#[derive(Default)]
pub struct IssuedWorkerOwners {
    owners: Mutex<BTreeMap<(String, String), ExtensionOwner>>,
}

impl IssuedWorkerOwners {
    pub fn adopt(&self, wire: ExtensionOwnerWire) -> ExtensionOwner {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (wire.owner_id.clone(), wire.binding_generation.clone());
        if let Some(existing) = owners.get(&key) {
            return existing.clone();
        }
        let owner = ExtensionOwner::new(wire);
        owners.insert(key, owner.clone());
        owner
    }

    pub fn retire(&self, identities: &[ExtensionOwnerIdentity]) {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for identity in identities {
            let key = (
                identity.owner_id.clone(),
                identity.binding_generation.clone(),
            );
            if let Some(owner) = owners.remove(&key) {
                owner.retire();
            }
        }
    }
}
