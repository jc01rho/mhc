//! The delivery consumer's contract, as the wake runtime starts and asks it (omo#13037 design 5.4, A2.4, A2.14).
//!
//! Upstream counterpart: `thread/consumer/consumer.ts`. Only the contract is ported here: `createDeliveryConsumer`
//! itself (resolving a handed record, committing it through the store's intake and answering from the row, the held
//! asks, and the pending and drained reports) needs the store's intake write (`store-worker.ts` `intake`,
//! `store-ops.ts` `IntakeFields`), which this crate does not have. The runtime takes a factory, so a host that brings
//! a consumer binds it, and the tests drive the runtime with one of their own.

use std::path::PathBuf;
use std::sync::Arc;

use super::port::DeliveryServicePort;
use crate::thread::store::SqliteGatewayStore;
use crate::thread::wake::pending_sweep::WakeSession;

pub const DELIVERY_PROTOCOL_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerStart {
    Serving,
    Unsupported,
    SchemaTooNew,
}

impl ConsumerStart {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Serving => "serving",
            Self::Unsupported => "unsupported",
            Self::SchemaTooNew => "schema_too_new",
        }
    }
}

pub trait DeliveryConsumer: Send + Sync {
    fn start(&self) -> Result<ConsumerStart, String>;
    /// From this start's acknowledgement until the consumer stops: the service hands this consumer its deliveries.
    fn serving(&self) -> bool;
    /// One `delivery.drained` report per marked pair whose intake rows are all applied.
    fn report_drained(&self) -> Result<(), String>;
    fn stop(&self);
}

/// The threads' chain count limit for a target session's cwd; `None` when the user set none.
pub type ChainCountLimit = Arc<dyn Fn(&str) -> Option<u64> + Send + Sync>;

pub struct DeliveryConsumerOptions {
    pub store: Arc<SqliteGatewayStore>,
    pub port: Arc<dyn DeliveryServicePort>,
    pub agent_dir: String,
    pub sessions_directory: PathBuf,
    pub chain_count_limit: ChainCountLimit,
    /// Starts the session (the carrier's wake); called only after the sender passed the admission check.
    pub wake: WakeSession,
}

pub type CreateDeliveryConsumer =
    Arc<dyn Fn(DeliveryConsumerOptions) -> Arc<dyn DeliveryConsumer> + Send + Sync>;
