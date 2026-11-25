//! The listing row an alias holder names (omo `thread/address/holder.ts`, #14305 U7).
//!
//! A thread claimed on its own device is held by its durable id, while the account listing keys every session by the
//! relay key that device's connector assigned and names only its project, so the row cannot always be matched by durable
//! id: the holding device maps it (`locate`, the same lookup a create on another device uses). An offline holder device
//! is `device_offline`; one that is online but cannot map the holder to a session it lists is `not_found`. A holder
//! device the account's roster no longer names (removed from the account, #13505) is `not_found` without that probe.

use std::sync::Arc;

use serde_json::json;

use crate::thread::remote::ports::{
    AliasHolder, RemoteLocateOutcome, RemoteSessionRow, RemoteThreadSurface, ended_row,
    probe_may_build_ended_row,
};
use crate::thread::remote::remote_ops::not_signed_in_failure;

use super::account::account_devices;
use super::device::AccountDevice;
use super::refusals::{ThreadRefusal, refusal};

/// Where a session address points once the account is consulted (A3): another device's session, or this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTarget {
    Remote { row: Arc<RemoteSessionRow> },
    Here { session_id: String },
}

fn names_holder(row: &RemoteSessionRow, holder: &AliasHolder) -> bool {
    row.device == holder.device
        && (row.durable_id.as_deref() == Some(holder.session_id.as_str())
            || row.session_id == holder.session_id)
}

pub async fn holder_target(
    remote: &RemoteThreadSurface,
    holder: &AliasHolder,
    here: Option<&str>,
    rows: &[RemoteSessionRow],
    roster: Option<&[AccountDevice]>,
) -> Result<RemoteTarget, ThreadRefusal> {
    if Some(holder.device.as_str()) == here {
        return Ok(RemoteTarget::Here {
            session_id: holder.session_id.clone(),
        });
    }
    if let Some(listed) = rows.iter().find(|row| names_holder(row, holder)) {
        return Ok(RemoteTarget::Remote {
            row: Arc::new(listed.clone()),
        });
    }
    let device = match roster {
        Some(roster) => roster
            .iter()
            .find(|candidate| candidate.device == holder.device)
            .cloned(),
        None => match account_devices(&remote.client).await {
            Ok(read) => read
                .devices
                .into_iter()
                .find(|candidate| candidate.device == holder.device),
            Err(message) => {
                return Err(refusal(
                    "host_unavailable",
                    "This device's device service could not list the account's devices.",
                    "Retry when the device service is reachable.",
                    Some(json!({ "device": holder.device, "reason": message })),
                ));
            }
        },
    };
    if roster.is_some() && device.is_none() {
        return Err(refusal(
            "not_found",
            format!(
                "The alias names thread {} on device {}, which is no longer on this account.",
                holder.session_id, holder.device
            ),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find the live thread, then claim the alias for it again with thread_rename (tool.thread_rename()).",
            Some(json!({ "device": holder.device, "thread": holder.session_id })),
        ));
    }
    if device.as_ref().is_some_and(|known| !known.online) {
        return Err(refusal(
            "device_offline",
            format!("Device {} holding this thread is offline.", holder.device),
            "Retry when that device is online; nothing was queued.",
            Some(json!({ "device": holder.device, "thread": holder.session_id })),
        ));
    }
    let located = remote
        .sessions
        .locate(holder.device.clone(), holder.session_id.clone())
        .await;
    if let RemoteLocateOutcome::Located { session_id } = &located {
        let mapped = rows.iter().find(|candidate| {
            candidate.device == holder.device && candidate.session_id == *session_id
        });
        if let Some(row) = mapped {
            return Ok(RemoteTarget::Remote {
                row: Arc::new(row.clone()),
            });
        }
        if probe_may_build_ended_row(&remote.sessions).await {
            return Ok(RemoteTarget::Remote {
                row: Arc::new(ended_row(&holder.device, session_id, &holder.session_id)),
            });
        }
    }
    let refused_reason = match &located {
        RemoteLocateOutcome::Refused { reason } => Some(reason.as_str()),
        RemoteLocateOutcome::Located { .. } => None,
    };
    if let Some(reason) = refused_reason
        && let Some(not_signed_in) = not_signed_in_failure(reason, &holder.device)
    {
        return Err(not_signed_in);
    }
    let details = match refused_reason {
        Some(reason) => json!({
            "device": holder.device,
            "thread": holder.session_id,
            "reason": reason,
        }),
        None => json!({ "device": holder.device, "thread": holder.session_id }),
    };
    Err(refusal(
        "not_found",
        format!(
            "The alias names thread {} on device {}, which that device does not list.",
            holder.session_id, holder.device
        ),
        "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find the live thread, then claim the alias for it again with thread_rename (tool.thread_rename() in an eval cell).",
        Some(details),
    ))
}
