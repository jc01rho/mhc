//! `thread_send`'s reply allowance (omo `thread/tools/reply-permission.ts`, #12886, decision 0027 section 6).
//!
//! A send back to an agent whose delivery the caller's current run took in, named by that sender's durable id
//! or the name its delivery stated, is approved from that evidence alone. The decision is made from the
//! consumed delivery, never from a listing: the send step then delivers only when the address resolves to that
//! sender (`reply_target_mismatch`), so a name that happens to match another session is never reached unapproved.

use super::addressing::normalize_thread_name;

/// A durable session id: the only spelling of a reply target that resolves exactly, never through an alias or a name.
fn is_session_id(target: &str) -> bool {
    let groups: Vec<&str> = target.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8usize, 4, 4, 4, 12])
            .all(|(group, length)| {
                group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
            })
}

/// One sender a delivery of this run's came from, as that delivery stated it. `device` absent = this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplySender {
    pub session_id: String,
    pub name: Option<String>,
    pub device: Option<String>,
}

/// The consumed senders an approved reply addresses, each with the device its delivery came from (absent = this
/// device): empty when `target` is not a reply. A target approved by a consumed sender id alone is that sender on
/// this device.
pub fn consumed_senders_of(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    target: &str,
) -> Vec<ReplySender> {
    if !is_reply_to_consumed_sender(reply_targets, reply_senders, target) {
        return Vec::new();
    }
    let named = senders_named_by(reply_senders, target);
    if !named.is_empty() {
        return named;
    }
    vec![ReplySender {
        session_id: target.to_owned(),
        name: None,
        device: None,
    }]
}

/// Whether `target` is an approved reply to a consumed sender: its durable id is one a delivery of this run stated,
/// or it names (by id or stated name) a sender of this run's deliveries.
pub fn is_reply_to_consumed_sender(
    reply_targets: &[String],
    reply_senders: &[ReplySender],
    target: &str,
) -> bool {
    if is_session_id(target) && reply_targets.iter().any(|candidate| candidate == target) {
        return true;
    }
    !senders_named_by(reply_senders, target).is_empty()
}

/// The consumed senders `target` names by its exact id or by the normalized name their delivery stated.
fn senders_named_by(reply_senders: &[ReplySender], target: &str) -> Vec<ReplySender> {
    let wanted = normalize_thread_name(target);
    reply_senders
        .iter()
        .filter(|sender| {
            sender.session_id == target
                || sender
                    .name
                    .as_deref()
                    .is_some_and(|name| !wanted.is_empty() && normalize_thread_name(name) == wanted)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender(session_id: &str, name: Option<&str>, device: Option<&str>) -> ReplySender {
        ReplySender {
            session_id: session_id.into(),
            name: name.map(str::to_owned),
            device: device.map(str::to_owned),
        }
    }

    const ID: &str = "01a1f000-0000-7000-8000-00000000000a";

    #[test]
    fn a_consumed_sender_id_is_a_reply_to_that_sender_here() {
        let senders = vec![sender(ID, Some("sender lane"), None)];
        assert_eq!(
            consumed_senders_of(&[ID.to_owned()], &senders, ID),
            vec![sender(ID, Some("sender lane"), None)]
        );
    }

    #[test]
    fn the_stated_name_is_a_reply_and_normalizes() {
        let senders = vec![sender(ID, Some("Sender  Lane"), None)];
        assert_eq!(
            consumed_senders_of(&[], &senders, "sender lane"),
            vec![sender(ID, Some("Sender  Lane"), None)]
        );
        assert_eq!(consumed_senders_of(&[], &senders, "SENDER LANE"), senders);
    }

    #[test]
    fn an_id_no_delivery_stated_is_no_reply_even_when_it_looks_like_one() {
        let senders = vec![sender(ID, None, None)];
        let other = "01a1f000-0000-7000-8000-00000000000b";
        assert!(consumed_senders_of(&[ID.to_owned()], &senders, other).is_empty());
        assert!(consumed_senders_of(&[], &[], ID).is_empty());
    }

    #[test]
    fn a_name_that_matches_no_sender_is_no_reply() {
        let senders = vec![sender(ID, Some("sender lane"), None)];
        assert!(consumed_senders_of(&[], &senders, "other lane").is_empty());
        assert!(consumed_senders_of(&[], &senders, "").is_empty());
    }

    #[test]
    fn an_id_in_reply_targets_without_a_sender_entry_is_that_sender_here() {
        assert_eq!(
            consumed_senders_of(&[ID.to_owned()], &[], ID),
            vec![sender(ID, None, None)]
        );
    }

    #[test]
    fn the_device_a_sender_stated_is_kept() {
        let senders = vec![sender(ID, Some("on b"), Some("b"))];
        assert_eq!(
            consumed_senders_of(&[], &senders, "on b"),
            vec![sender(ID, Some("on b"), Some("b"))]
        );
    }
}
