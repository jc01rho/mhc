//! Port of `components/task/model-routing-rpc.ts` (omo #14025): the desktop's read of which model
//! each category and task agent runs. An engine without the method answers the unknown-method
//! refusal the desktop reads as unsupported; `schemaVersion` versions the answer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use maho_ext_api::{ExtensionApi, ExtensionFailure};
use serde_json::{Value, json};

use crate::model_routing_rows::{ModelRoutingPlannerInputs, resolve_model_routing_rows};
use crate::planner::ResolveModelRegistry;

pub const MODEL_ROUTING_RESOLVE_RPC_METHOD: &str = "omo.modelRouting.resolve";

/// The merged config read for one answer. `unreadable` carries the message of every diagnostic that
/// means a layer could not be read or applied (`read`, `parse`, `validation`): rows built without
/// that layer would describe a config the user never wrote. Pruned values and unknown keys keep the
/// rest of the layer and still resolve as the engine does, so they are not listed.
#[derive(Debug, Clone, Default)]
pub struct ModelRoutingConfig {
    pub config: Value,
    pub unreadable: Vec<String>,
}

#[derive(Clone)]
pub struct ModelRoutingRpcDeps {
    /// The directory the planner's config is read from (the task component's register-time cwd).
    pub cwd: Arc<dyn Fn() -> PathBuf + Send + Sync>,
    pub load_config: Arc<dyn Fn(&Path) -> ModelRoutingConfig + Send + Sync>,
    pub registry: ResolveModelRegistry,
    /// The planner's own agent table and the parent's service tier, read per answer.
    pub planner: Arc<dyn Fn() -> ModelRoutingPlannerInputs + Send + Sync>,
}

fn refusal(code: &str, message: String) -> Value {
    json!({ "kind": "error", "code": code, "message": message })
}

pub fn answer_model_routing(deps: &ModelRoutingRpcDeps) -> Value {
    let cwd = (deps.cwd)();
    if !cwd.is_dir() {
        return refusal(
            "cwd_unavailable",
            format!(
                "The session's working directory {} is not a directory.",
                cwd.display()
            ),
        );
    }
    let loaded = (deps.load_config)(&cwd);
    if !loaded.unreadable.is_empty() {
        return refusal("config_invalid", loaded.unreadable.join("\n"));
    }
    let Some(registry) = (deps.registry)() else {
        return refusal(
            "registry_unavailable",
            "The session has no model registry yet.".to_string(),
        );
    };
    let rows = resolve_model_routing_rows(&loaded.config, registry.as_ref(), &(deps.planner)());
    json!({ "schemaVersion": 1, "rows": rows })
}

pub fn wire_model_routing_rpc(
    api: &mut ExtensionApi,
    deps: ModelRoutingRpcDeps,
) -> Result<(), ExtensionFailure> {
    api.rpc_handle(
        MODEL_ROUTING_RESOLVE_RPC_METHOD,
        Arc::new(move |_params| {
            let answer = answer_model_routing(&deps);
            Box::pin(async move { Ok(answer) })
        }),
    )
}
