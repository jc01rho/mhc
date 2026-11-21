//! Port of `src/service/global-input.ts`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use senpi_desktop_core::error::ErrorCode;
use senpi_desktop_protocol::JsonRpcErrorData;
use serde_json::{Value, json};

use crate::control_grants::ControlGrants;
use crate::rpc_client::{CallOptions, DesktopError, RpcClient};
use crate::timeouts::REVOKE_TIMEOUT;

/// Pointer and keyboard methods; on the desktop or a display target every backend posts them globally (#10788).
const GLOBAL_INPUT_METHODS: &[&str] = &[
    "click",
    "moveMouse",
    "drag",
    "scroll",
    "typeText",
    "keyChord",
    "holdKeys",
    "holdMouse",
];

pub const FULL_ACCESS_GRANT_REASON: &str =
    "full access: the session's permission preset consents to desktop input";

/// The session's live full-access decision (#10022), read fresh on every global input.
pub type FullAccessReader = Arc<dyn Fn() -> bool + Send + Sync>;

/// The engine's `Target::parse`: `desktop` compared ASCII case-insensitively, `display:` as an exact prefix.
fn is_global_target(target: &str) -> bool {
    target.eq_ignore_ascii_case("desktop") || target.starts_with("display:")
}

/// True for input the engine admits only with the control grant whatever delivery was asked for.
pub fn is_global_input(method: &str, params: &Value) -> bool {
    GLOBAL_INPUT_METHODS.contains(&method)
        && params
            .get("target")
            .and_then(Value::as_str)
            .is_some_and(is_global_target)
}

fn is_control_required(error: &DesktopError) -> bool {
    matches!(
        error,
        DesktopError::Rpc(rpc)
            if matches!(&rpc.data, Some(JsonRpcErrorData::Engine(data)) if data.code == ErrorCode::ControlRequired)
    )
}

/// Full access is the consent for desktop input (#10022): there the service takes the control grant itself,
/// without a confirm and headless included, and a grant only full access authorized is revoked as soon as the
/// session leaves full access. Outside full access the service adds nothing: the engine's admission gate is the
/// single, audited refusal (#10788).
pub struct FullAccessConsent {
    grants: Arc<ControlGrants>,
    full_access: OnceLock<FullAccessReader>,
}

impl FullAccessConsent {
    pub fn new(grants: Arc<ControlGrants>) -> Self {
        Self {
            grants,
            full_access: OnceLock::new(),
        }
    }

    /// Binds the session's full-access decision; absent counts as not full access.
    pub fn set_full_access(&self, reader: FullAccessReader) {
        let _ = self.full_access.set(reader);
    }

    fn full_access(&self) -> bool {
        self.full_access.get().is_some_and(|read| read())
    }

    /// Revokes a full-access grant once the session is no longer in full access; a failed revoke fails the call.
    pub async fn fence(&self, rpc: &Arc<RpcClient>) -> Result<(), DesktopError> {
        if !self.grants.automatic_live() || self.full_access() {
            return Ok(());
        }
        self.revoke(rpc).await.map(|_| ())
    }

    /// Sends global input. In full access it grants first when no grant may be live, and retries exactly once when
    /// the engine refused `ControlRequired` at admission (before any input), as after a stop the user resumed.
    /// `Suspended`, cancellation, and every other failure are never retried, and resume is never sent.
    ///
    /// The consent read and the send are one step: every await (a revoke, a grant) loops back to read full access
    /// again before the request is written.
    pub async fn send(
        &self,
        rpc: &Arc<RpcClient>,
        method: &str,
        params: Value,
        options: CallOptions,
    ) -> Result<Value, DesktopError> {
        let mut retried = false;
        loop {
            let full_access = self.full_access();
            if !full_access && self.grants.automatic_live() {
                self.revoke(rpc).await?;
                continue;
            }
            if full_access && !self.grants.possibly_live() {
                self.grant(rpc, &options).await?;
                continue;
            }
            match rpc.request(method, params.clone(), options.clone()).await {
                Ok(value) => return Ok(value),
                Err(error) if full_access && !retried && is_control_required(&error) => {}
                Err(error) => return Err(error),
            }
            retried = true;
            self.grant(rpc, &options).await?;
        }
    }

    async fn revoke(&self, rpc: &Arc<RpcClient>) -> Result<Value, DesktopError> {
        let options = CallOptions {
            timeout: Some(REVOKE_TIMEOUT),
            cancel_rx: None,
        };
        self.grants
            .revoke(|| rpc.request("control.revoke", json!({}), options))
            .await
    }

    async fn grant(&self, rpc: &Arc<RpcClient>, options: &CallOptions) -> Result<(), DesktopError> {
        static NEXT_CONFIRMATION: AtomicU64 = AtomicU64::new(1);
        let confirmation = NEXT_CONFIRMATION.fetch_add(1, Ordering::Relaxed);
        let params = json!({
            "reason": FULL_ACCESS_GRANT_REASON,
            "confirmationId": format!("full-access-{}-{confirmation}", std::process::id()),
        });
        let options = CallOptions {
            timeout: None,
            cancel_rx: options.cancel_rx.clone(),
        };
        self.grants
            .grant(
                || rpc.request("control.grant", params, options),
                false,
                true,
            )
            .await
            .map(|_| ())
    }
}
