//! The pure decisions of a freeze settle (sync-design section 7, rotation on revoke; #12280 design
//! section 3.1). The settler reads the state, the verified chain and the device list; these functions
//! decide, in the design's rule order, whether this device rotates and with which members. Nothing
//! here performs I/O or opens a key.

use std::collections::BTreeSet;

use super::revocation_view::RevocationView;
use crate::sync::crypto::parse::ParsedBody;
use crate::sync::crypto::{chain::ChainState, entry::Head, fold::Authority, parse::Principal};
use crate::sync::engine::ports::FreezeReason;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Ok,
    NotMember,
    Frozen(FreezeReason),
}

/// Rule 2: the verified chain reached exactly the head the state route served.
pub fn at_served_head(state: &ChainState, served: &Head) -> bool {
    state.authority().is_some()
        && state.head().is_some_and(|head| {
            head.seq == served.seq
                && head.link == served.link
                && head.keyring_link == served.keyring_link
        })
}

/// Rules 3 and 4, before any key is opened. A non-member yields with no reason (a session open
/// across a recovery-join that excluded it keeps its `not_member` seal failure). A freeze set by a
/// recovery-join, a veto or a fork-resolution needs a rotation this settler cannot build, so it
/// waits.
pub fn chain_gate(state: &ChainState, self_id: &str, self_signing: &str) -> Gate {
    let member = state
        .authority()
        .and_then(|authority| authority.members.get(self_id));
    if member.map(|m| m.signing_public_key.as_str()) != Some(self_signing) {
        return Gate::NotMember;
    }
    match state.history().last().map(|record| &record.body) {
        Some(ParsedBody::RecoveryJoin { .. }) => Gate::Frozen(FreezeReason::WaitingRecovery),
        Some(ParsedBody::RecoveryJoinVetoed { .. } | ParsedBody::ForkResolution { .. }) => {
            Gate::Frozen(FreezeReason::WaitingUnsupportedFreeze)
        }
        _ => Gate::Ok,
    }
}

/// Rule 6, plus the view's own usability: the list must name this device, on a live row.
pub fn view_gate(view: &RevocationView, self_id: &str) -> Gate {
    match view {
        RevocationView::Usable {
            self_principal,
            self_revoked,
            ..
        } if self_principal == self_id => {
            if *self_revoked {
                Gate::Frozen(FreezeReason::SelfRevoked)
            } else {
                Gate::Ok
            }
        }
        _ => Gate::Frozen(FreezeReason::DeviceListUnusable),
    }
}

/// What a process remembers about its last rotation, or its last refused append, for one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettleMemory {
    pub keyring_link: String,
    pub revoked: BTreeSet<String>,
}

/// Rule 7 (and the bound after a refused append): append again only when the served keyring head
/// moved since `memory`, or the revoked set changed. Neither can be forged by a service, so a flag
/// kept set after every rotation buys one append per genuine new record or revoke.
pub fn unchanged_since(
    memory: Option<&SettleMemory>,
    served_keyring_link: &str,
    revoked: &BTreeSet<String>,
) -> bool {
    memory.is_some_and(|m| m.keyring_link == served_keyring_link && &m.revoked == revoked)
}

/// The rotation's member list: the folded membership minus every member on a revoked row. A member
/// no row names is kept and counted; the service's own register refuses a rotation that keeps a
/// revoked key.
pub fn rotation_members(
    authority: &Authority,
    revoked: &BTreeSet<String>,
    listed: &BTreeSet<String>,
) -> (Vec<Principal>, usize) {
    let members: Vec<Principal> = authority
        .members
        .values()
        .filter(|member| !revoked.contains(&member.id))
        .cloned()
        .collect();
    let unlisted = members.iter().filter(|m| !listed.contains(&m.id)).count();
    (members, unlisted)
}

/// The revoked set grew between two reads: a second revoke raced the first read.
pub fn revoked_grew(before: &BTreeSet<String>, after: &RevocationView) -> bool {
    match after {
        RevocationView::Usable { revoked, .. } => revoked.iter().any(|id| !before.contains(id)),
        RevocationView::Unusable => false,
    }
}
