//! Port of `src/service/control-grants.ts`.
//!
//! Which foreground-control grants this service asked for may still hold (#9651 B5b), so a revoke is
//! sent only while one may be live and a run or turn end without one adds nothing to the engine audit.

use std::future::Future;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::Value;

use crate::rpc_client::DesktopError;

#[derive(Debug, Default)]
struct GrantState {
    sent: u64,
    /// Number of the newest grant that may still hold; a refused grant leaves it at the one before.
    possibly_live: u64,
    /// How many sent grants a successful `control.revoke` has covered.
    revoked: u64,
    /// Whether the newest possibly-live grant came from full access rather than a human confirm (#10788).
    automatic: bool,
}

#[derive(Debug, Default)]
pub struct ControlGrants {
    state: Mutex<GrantState>,
}

impl ControlGrants {
    fn state(&self) -> MutexGuard<'_, GrantState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn possibly_live(&self) -> bool {
        let state = self.state();
        state.possibly_live > state.revoked
    }

    /// A possibly-live grant that only full access authorized; it must not outlive full access.
    pub fn automatic_live(&self) -> bool {
        let state = self.state();
        state.possibly_live > state.revoked && state.automatic
    }

    /// Sends `control.grant`, counted as possibly live from the moment it is written: a grant whose reply
    /// is lost to a cancel may still hold. A request cancelled before sending and an engine refusal
    /// (`InputBusy`, `Suspended`, any engine error) granted nothing, so they stay uncounted (#9967).
    pub async fn grant<F, Fut>(
        &self,
        request: F,
        cancelled: bool,
        automatic: bool,
    ) -> Result<Value, DesktopError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, DesktopError>>,
    {
        if cancelled {
            return request().await;
        }
        let (grant, before) = {
            let mut state = self.state();
            state.sent += 1;
            let grant = state.sent;
            let before = (state.possibly_live, state.automatic);
            state.possibly_live = grant;
            state.automatic = automatic;
            (grant, before)
        };
        let result = request().await;
        if matches!(result, Err(DesktopError::Rpc(_))) {
            let mut state = self.state();
            if state.possibly_live == grant {
                state.possibly_live = before.0;
                state.automatic = before.1;
            }
        }
        result
    }

    /// Sends `control.revoke`; `request` is called before this yields, so the revoke is written ahead of
    /// anything sent afterwards. One that succeeded covers every grant sent before it; one that failed or
    /// was dropped covers none, so it is sent again later.
    pub async fn revoke<F, Fut>(&self, request: F) -> Result<Value, DesktopError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, DesktopError>>,
    {
        let covers = self.state().sent;
        let result = request().await?;
        let mut state = self.state();
        state.revoked = state.revoked.max(covers);
        Ok(result)
    }
}
