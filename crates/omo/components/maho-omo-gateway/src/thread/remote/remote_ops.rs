//! The status round every cross-device resolution starts with (omo `thread/remote/remote-ops.ts` `remoteReady`, and
//! `outcome.ts` `notSignedInFailure`).

use serde_json::json;

use crate::thread::address::refusals::{ThreadRefusal, refusal};

use super::ports::RemoteThreadSurface;

/// What a connected relay's status round names: this device's account id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub here: Option<String>,
}

/// A5: the relay is paid only; on any other plan the remote path answers a refusal, never a silent failure or a queue.
/// On a connected relay this device's account id is read in the same round.
pub async fn remote_ready(remote: &RemoteThreadSurface) -> Result<Ready, ThreadRefusal> {
    let status = match remote.client.service_status().await {
        Ok(status) => status,
        Err(failure) => {
            let details = if failure.service_not_installed {
                json!({ "reason": failure.message })
            } else {
                json!({ "reason": failure.message, "state": "unreachable" })
            };
            return Err(refusal(
                "host_unavailable",
                "This device's device service is not reachable.",
                "Install or start the device service, then retry.",
                Some(details),
            ));
        }
    };
    if status.state == "plan_required" {
        return Err(refusal(
            "relay_requires_paid_plan",
            "Reaching sessions on other devices needs a paid plan; this account's plan has no relay.",
            "Upgrade the plan to use other devices, or work with threads on this device.",
            None,
        ));
    }
    if status.state != "connected" {
        let state = status.state;
        return Err(refusal(
            "host_unavailable",
            format!("Other devices are not reachable from this device right now ({state})."),
            "Sign in, turn remote sessions on, or wait for the device service to connect, then retry.",
            Some(json!({ "state": state })),
        ));
    }
    Ok(Ready {
        here: status.device_id,
    })
}

const NOT_SIGNED_IN: [&str; 2] = ["sign_in_required", "signed_out"];

/// The refusal for a locate's or command's refusal reason that names this device's own missing relay sign-in.
pub fn not_signed_in_failure(reason: &str, device: &str) -> Option<ThreadRefusal> {
    NOT_SIGNED_IN.contains(&reason).then(|| {
        refusal(
            "host_unavailable",
            format!(
                "This device is not attached to the relay ({reason}), so device {device} cannot be reached; nothing was sent."
            ),
            "Retry when this device is signed in: sign in to omo here, then retry.",
            Some(json!({ "device": device, "reason": reason })),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_sign_in_reads_as_not_signed_in() {
        let refused = not_signed_in_failure("signed_out", "b7c41e09d2a64f1e").expect("signed out");
        assert_eq!(refused.code.as_str(), "host_unavailable");
        assert!(refused.message.contains("(signed_out)"));
        assert!(not_signed_in_failure("capability_missing", "b").is_none());
    }
}
