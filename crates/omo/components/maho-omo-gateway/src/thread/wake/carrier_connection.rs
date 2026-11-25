//! One kept connection to the engine host: the carrier's own attachment to a woken session lives on it (A3.1.3).
//!
//! Upstream counterpart: `thread/wake/carrier-connection.ts`. The socket half (`connectCarrierHost` over senpi-task's
//! authenticated `rpc-socket-client`) has no Rust counterpart in this crate; a host binds its connector through
//! `CarrierConnect`.

use std::sync::Arc;

use serde_json::Value;

pub type CarrierFrameListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub type ConnectionClosed = Box<dyn FnOnce() + Send>;

pub trait CarrierHostConnection: Send + Sync {
    /// Sends one frame and answers the host's reply to it, or why no reply came.
    fn request(&self, frame: Value) -> Result<Value, String>;
    /// Every line that answers no request (`session_lifecycle`, broadcasts).
    fn on_event(&self, listener: CarrierFrameListener) -> Unsubscribe;
    /// Runs once the connection ended; at once when it already has.
    fn on_closed(&self, callback: ConnectionClosed);
    fn close(&self);
}

pub type CarrierConnect =
    Arc<dyn Fn() -> Result<Arc<dyn CarrierHostConnection>, String> + Send + Sync>;
