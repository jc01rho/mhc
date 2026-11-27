//! The reply-target enforcement half (omo `thread/tools/reply-target.ts`, decision 0027 section 6, amendment A4.3.1
//! of #13037).
//!
//! The allowance approved a send as a reply to a consumed delivery's sender (by id or stated name). The send step
//! delivers only when the address resolves to that sender on that sender's device; anything else is refused by name
//! without sending. The resolvers already narrow a reply to the sender's device, so this is the second layer.

use serde_json::json;

use super::addressing::ThreadAddressEntry;
use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::reply_permission::{ReplySender, consumed_senders_of};

/// A sender's device as this device reads it: a sender naming this device is a sender here.
pub fn other_device(device: Option<&str>, here: Option<&str>) -> Option<String> {
    match device {
        Some(device) if Some(device) == here => None,
        Some(device) => Some(device.to_owned()),
        None => None,
    }
}

/// `Some(failure)` when `address` is an approved reply whose resolved entry is not the consumed sender it was
/// approved for; `None` when it is no reply, or it resolves to the sender on the sender's device.
pub fn reply_target_mismatch(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    address: &str,
    entry: &ThreadAddressEntry,
    here: Option<&str>,
) -> Option<ThreadToolFailure> {
    let senders = consumed_senders_of(reply_targets, reply_senders, address);
    if senders.is_empty() {
        return None;
    }
    let entry_device = other_device(entry.device.as_deref(), here);
    let matched = senders.iter().any(|sender| {
        sender.session_id == entry.thread_id
            && other_device(sender.device.as_deref(), here) == entry_device
    });
    if matched {
        return None;
    }
    Some(reply_target_failure(address, entry))
}

/// The refusal of a reply whose address resolved to a session that is not the consumed sender.
pub fn reply_target_failure(address: &str, entry: &ThreadAddressEntry) -> ThreadToolFailure {
    let device = entry
        .device
        .as_deref()
        .map(|device| format!(" on {device}"))
        .unwrap_or_default();
    thread_tool_failure(
        "reply_target_mismatch",
        format!(
            "\"{address}\" was approved as a reply to a sender of this run's deliveries, but it resolves to {} ({}{device}), which is not that sender.",
            entry.name, entry.thread_id
        ),
        "Reply with the sender's durable id from the delivery header, or send to the other thread by its id as a normal send.",
        Some(match &entry.device {
            Some(device) => json!({ "thread": entry.thread_id, "device": device }),
            None => json!({ "thread": entry.thread_id }),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::contracts_results::ThreadStatus;
    use crate::thread::reply_permission::ReplySender;

    fn sender(session_id: &str, device: Option<&str>) -> ReplySender {
        ReplySender {
            session_id: session_id.into(),
            name: Some("sender".into()),
            device: device.map(str::to_owned),
        }
    }

    fn entry(device: Option<&str>) -> ThreadAddressEntry {
        ThreadAddressEntry {
            thread_id: "dur-sender".into(),
            name: "sender".into(),
            cwd: String::new(),
            status: ThreadStatus::Live,
            updated_at: None,
            session_ids: Vec::new(),
            device: device.map(str::to_owned),
            aliases: Vec::new(),
            row: None,
        }
    }

    #[test]
    fn a_sender_from_device_b_that_resolves_on_device_c_is_a_mismatch() {
        let failure = reply_target_mismatch(
            &[],
            &[sender("dur-sender", Some("b"))],
            "dur-sender",
            &entry(Some("c")),
            Some("here"),
        )
        .expect("mismatch");
        assert_eq!(failure.code.as_str(), "reply_target_mismatch");
        assert_eq!(
            failure.details,
            Some(json!({ "thread": "dur-sender", "device": "c" }))
        );
    }

    #[test]
    fn a_sender_from_device_b_that_resolves_on_b_passes() {
        assert_eq!(
            reply_target_mismatch(
                &[],
                &[sender("dur-sender", Some("b"))],
                "dur-sender",
                &entry(Some("b")),
                Some("here"),
            ),
            None
        );
    }

    #[test]
    fn a_sender_here_passes_whether_its_device_is_absent_or_named_as_this_one() {
        assert_eq!(
            reply_target_mismatch(
                &[],
                &[sender("dur-sender", None)],
                "dur-sender",
                &entry(None),
                Some("here")
            ),
            None
        );
        assert_eq!(
            reply_target_mismatch(
                &[],
                &[sender("dur-sender", Some("here"))],
                "dur-sender",
                &entry(None),
                Some("here"),
            ),
            None
        );
    }

    #[test]
    fn a_sender_here_that_resolves_on_another_device_is_a_mismatch() {
        assert_eq!(
            reply_target_mismatch(
                &[],
                &[sender("dur-sender", None)],
                "dur-sender",
                &entry(Some("b")),
                Some("here")
            )
            .map(|failure| failure.code.as_str().to_owned()),
            Some("reply_target_mismatch".into())
        );
    }

    #[test]
    fn an_address_no_delivery_approved_is_never_a_mismatch() {
        assert_eq!(
            reply_target_mismatch(&[], &[], "dur-sender", &entry(Some("c")), Some("here")),
            None
        );
    }

    #[test]
    fn the_refusal_names_the_resolved_thread_and_device() {
        let failure = reply_target_failure("dur-sender", &entry(Some("c")));
        assert!(failure.message.contains("dur-sender"));
        assert!(failure.message.contains("on c"));
        assert_eq!(
            failure.details,
            Some(json!({ "thread": "dur-sender", "device": "c" }))
        );
        let local = reply_target_failure("dur-sender", &entry(None));
        assert_eq!(local.details, Some(json!({ "thread": "dur-sender" })));
    }
}
