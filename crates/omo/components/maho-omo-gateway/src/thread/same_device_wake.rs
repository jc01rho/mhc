//! Same-device wake leg after a local offline queue (`thread/gateway/same-device-wake.ts`, omo#13037).

use std::path::PathBuf;
use std::sync::Arc;

use super::store::SqliteGatewayStore;
use super::types::{DeliveryWait, GatewayDeliveryDelivery, GatewayDeliveryResult};
use super::wake::carrier::CwdSwitch;
use super::wake::session_target::{WakeSessionKind, WakeTarget, resolve_wake_target};

pub type SameDeviceCarrier = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type SameDeviceCarrierLookup = Arc<dyn Fn() -> Option<SameDeviceCarrier> + Send + Sync>;

pub struct SameDeviceWakeOptions {
    pub store: Arc<SqliteGatewayStore>,
    pub sessions_directory: PathBuf,
    pub enabled: CwdSwitch,
    pub carrier: Option<SameDeviceCarrier>,
}

#[derive(Clone)]
pub struct SameDeviceWake {
    store: Arc<SqliteGatewayStore>,
    sessions_directory: PathBuf,
    enabled: CwdSwitch,
    carrier_lookup: SameDeviceCarrierLookup,
}

impl SameDeviceWake {
    pub fn new(options: SameDeviceWakeOptions) -> Self {
        let carrier = options.carrier;
        Self {
            store: options.store,
            sessions_directory: options.sessions_directory,
            enabled: options.enabled,
            carrier_lookup: Arc::new(move || carrier.clone()),
        }
    }

    pub fn with_carrier_lookup(
        store: Arc<SqliteGatewayStore>,
        sessions_directory: PathBuf,
        enabled: CwdSwitch,
        carrier_lookup: SameDeviceCarrierLookup,
    ) -> Self {
        Self {
            store,
            sessions_directory,
            enabled,
            carrier_lookup,
        }
    }

    pub fn after_send(&self, result: GatewayDeliveryResult) -> GatewayDeliveryResult {
        let GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            delivery: GatewayDeliveryDelivery::QueuedOffline { queue_position, .. },
            effective_mode,
            endpoint_kind,
            deduplicated,
            collapsed,
            outcome,
        } = &result
        else {
            return result;
        };
        let WakeTarget::Found {
            session_path,
            cwd,
            session_kind: WakeSessionKind::Interactive,
            ..
        } = resolve_wake_target(&self.sessions_directory, thread_id)
        else {
            return result;
        };
        if !(self.enabled)(&cwd) {
            return result;
        }
        if self
            .store
            .woken()
            .request(thread_id, &session_path)
            .is_err()
        {
            return result;
        }
        let Some(wake) = (self.carrier_lookup)() else {
            return GatewayDeliveryResult::Ok {
                thread_id: thread_id.clone(),
                delivery_id: delivery_id.clone(),
                message_seq: *message_seq,
                delivery: GatewayDeliveryDelivery::QueuedOffline {
                    queue_position: *queue_position,
                    reason: Some("wake_unavailable".to_owned()),
                },
                effective_mode: effective_mode.clone(),
                endpoint_kind: *endpoint_kind,
                deduplicated: *deduplicated,
                collapsed: *collapsed,
                outcome: *outcome,
            };
        };
        let _ = wake(thread_id);
        GatewayDeliveryResult::Ok {
            thread_id: thread_id.clone(),
            delivery_id: delivery_id.clone(),
            message_seq: *message_seq,
            delivery: GatewayDeliveryDelivery::Queued {
                queue_position: *queue_position,
                waiting_on: Some(DeliveryWait {
                    kind: "wake".to_owned(),
                    device: None,
                }),
            },
            effective_mode: effective_mode.clone(),
            endpoint_kind: *endpoint_kind,
            deduplicated: *deduplicated,
            collapsed: *collapsed,
            outcome: *outcome,
        }
    }
}
