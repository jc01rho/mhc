//! The seam an extension uses to serve rpc watches on another device's sessions (`<device>/<session>`,
//! `<device>/*`). senpi knows no relay: it hands the source the target and the callbacks, renders what
//! comes back with the local renderer, and keeps the source's resume position in the monitor checkpoint.
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type RpcRemoteBoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRemoteTarget {
    pub device: String,
    pub session: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRemoteSettledTurn {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RpcRemoteState {
    pub rows: Vec<Value>,
    pub settled_turns: Option<Vec<RpcRemoteSettledTurn>>,
    pub generation: Option<String>,
    pub cursor: Option<String>,
}

pub type RpcRemoteRecord = Value;

#[derive(Clone)]
pub struct RpcRemoteHandlers {
    pub record: Arc<dyn Fn(Value) + Send + Sync>,
    pub resynced: Arc<dyn Fn(RpcRemoteState) + Send + Sync>,
    pub cursor: Option<Arc<dyn Fn(String) + Send + Sync>>,
    pub lost: Arc<dyn Fn(String) + Send + Sync>,
    pub ended: Arc<dyn Fn(String) + Send + Sync>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RpcRemoteEvents {
    All,
    Kinds(Vec<String>),
}

#[derive(Clone)]
pub struct RpcRemoteOpenOptions {
    pub events: RpcRemoteEvents,
    pub exclude: Vec<String>,
    pub resume_from: Option<String>,
}

#[derive(Clone)]
pub struct RpcRemoteSubscription {
    pub initial: RpcRemoteState,
    pub stop: Arc<dyn Fn() + Send + Sync>,
}

impl RpcRemoteSubscription {
    pub fn stop(&self) {
        (self.stop)();
    }
}

#[derive(Clone)]
pub struct RpcRemoteSource {
    pub open: Arc<
        dyn Fn(
                RpcRemoteTarget,
                RpcRemoteOpenOptions,
                RpcRemoteHandlers,
            ) -> RpcRemoteBoxFuture<Result<RpcRemoteSubscription, String>>
            + Send
            + Sync,
    >,
}

// Global registry: latest registration wins.
use std::sync::Mutex;
static SOURCES: Mutex<Vec<RpcRemoteSource>> = Mutex::new(Vec::new());

pub fn register_rpc_remote_source(source: RpcRemoteSource) -> Arc<dyn Fn() + Send + Sync> {
    let mut sources = SOURCES.lock().unwrap_or_else(|p| p.into_inner());
    sources.push(source);
    let index = sources.len() - 1;
    drop(sources);
    Arc::new(move || {
        let mut sources = SOURCES.lock().unwrap_or_else(|p| p.into_inner());
        if index < sources.len() {
            sources.remove(index);
        }
    })
}

pub fn current_rpc_remote_source() -> Option<RpcRemoteSource> {
    let sources = SOURCES.lock().unwrap_or_else(|p| p.into_inner());
    sources.last().cloned()
}
