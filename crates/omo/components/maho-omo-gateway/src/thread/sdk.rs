//! The gateway SDK: sends over one store, for a CLI or a connector.
//!
//! Upstream counterpart: `thread/sdk.ts`. A send resolves sender provenance in the sender's own
//! runtime, and a send that queued for a parked session wakes it as an agent's `thread_send` does.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::adapter::BoxFuture;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::provenance_resolver::{ProvenanceRuntime, ProvenanceWorkspace, resolve_provenance};
use super::same_device_wake::SameDeviceWake;
use super::store_api::GatewayStoreApi;
use super::store_extensions::{
    DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult,
};
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance};

pub struct ThreadSdkOptions {
    pub agent_dir: String,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub uid: i64,
    pub user: String,
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub workspaces: Option<Vec<ProvenanceWorkspace>>,
    /// The local transport's same-device wake (omo#14972): the CLI and the gateway's seat sends wake a parked
    /// session through it, as an agent's `thread_send` does. `None` answers a queued send as it queued.
    pub same_device_wake: Option<SameDeviceWake>,
}

/// The sender's own provenance, resolved once per send in the sender's runtime.
pub fn sdk_provenance(
    cwd: &str,
    env: &BTreeMap<String, String>,
    workspaces: Option<Vec<ProvenanceWorkspace>>,
) -> Provenance {
    let mut runtime = ProvenanceRuntime {
        cwd: std::path::PathBuf::from(cwd),
        env: env.clone(),
        workspaces,
        ..Default::default()
    };
    if runtime.env.is_empty() {
        runtime.env = std::env::vars().collect();
    }
    resolve_provenance(&runtime)
}

#[derive(Clone)]
pub struct ThreadSdk {
    options: Arc<ThreadSdkOptions>,
}

impl ThreadSdk {
    pub fn new(options: ThreadSdkOptions) -> Self {
        Self {
            options: Arc::new(options),
        }
    }

    fn sender(&self) -> GatewaySender {
        GatewaySender::Cli {
            uid: self.options.uid,
            user: self.options.user.clone(),
        }
    }

    /// `thread_send` through the SDK: the same engine path a session tool takes.
    pub fn send(
        &self,
        thread: &str,
        text: &str,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<GatewayDeliveryResult> {
        let provenance = sdk_provenance(
            &self.options.cwd,
            &self.options.env,
            self.options.workspaces.clone(),
        );
        let provenance = Arc::new(provenance);
        let options = self.options.clone();
        let request = GatewayDeliverRequest {
            mode,
            ..GatewayDeliverRequest::new(self.sender(), thread, text)
        };
        Box::pin(async move {
            let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
                sender_provenance: Some(Arc::new(move || (*provenance).clone())),
                store: options.store.clone(),
                endpoints: options.engine.options().endpoints.clone(),
                resolve: options.engine.options().resolve.clone(),
                now: options.engine.options().now.clone(),
            });
            let result = engine.deliver(request).await;
            match &options.same_device_wake {
                Some(wake) => wake.after_send(result),
                None => result,
            }
        })
    }

    pub fn register_store_extension(
        &self,
        descriptor: &StoreExtensionRegistration,
    ) -> StoreExtensionResult<i64> {
        self.options
            .store
            .register_store_extension(descriptor, self.options.store.now(), None)
    }

    pub fn extension_call(
        &self,
        name: &str,
        op: &str,
        args: &Value,
    ) -> StoreExtensionResult<Value> {
        self.options
            .store
            .extension_call(name, op, args, self.options.store.now())
    }

    pub fn session_callable_ops(&self) -> Vec<DeclaredSessionOp> {
        self.options.store.session_callable_ops()
    }

    pub fn extension_session_await(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
    ) -> StoreExtensionResult<Value> {
        self.options
            .store
            .extension_session_await(name, op, args, caller, self.options.store.now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_environment_labels_reach_the_sender_provenance() {
        let env = BTreeMap::from([
            (
                "OMO_PROVENANCE_WORKSPACE".to_owned(),
                "Runtime workspace".to_owned(),
            ),
            (
                "OMO_PROVENANCE_PROJECT".to_owned(),
                "runtime-project".to_owned(),
            ),
            ("OMO_PROVENANCE_LANE".to_owned(), "review".to_owned()),
        ]);
        let provenance = sdk_provenance("/tmp/whatever", &env, None);
        assert_eq!(provenance.workspace, "Runtime workspace");
        assert_eq!(provenance.project, "runtime-project");
        assert_eq!(provenance.lane.as_deref(), Some("review"));
        assert!(provenance.session.is_none());
    }
}
