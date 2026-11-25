//! The address module's input and output shapes.
//!
//! Upstream counterpart: `types.ts`. `ResolveContext.remote` is the account surface; without it a resolution reads
//! this device's book alone, and `Resolved.where_` can name another device's listing row once the account was read.

use std::sync::Arc;

use maho_thread_address_core::DeviceRef;

use crate::thread::addressing::ThreadAddressEntry;
use crate::thread::remote::ports::{RemoteSessionRow, RemoteThreadSurface};

/// What one resolution reads: the account (absent: this device only), this device's book, the book asked for one
/// address part, and the caller's identity.
///
/// `local_for` is upstream's `ResolveContext.localFor`: once a device part names this device, the thread part is asked
/// of this device's book on its own, so a send address book that only surfaces a session file for the text it is asked
/// about still answers (#14305 review M2).
/// Per-part local address book lookup callback (#14305 review M2).
pub type LocalBookFor<'a> = &'a (dyn Fn(&str) -> Vec<ThreadAddressEntry> + Send + Sync);

#[derive(Clone, Copy)]
pub struct ResolveContext<'a> {
    pub remote: Option<&'a RemoteThreadSurface>,
    pub caller_workspace_root: &'a str,
    pub local: &'a [ThreadAddressEntry],
    pub local_for: Option<LocalBookFor<'a>>,
    pub caller_id: &'a str,
}

/// An approved reply's target: the device its delivery came from, and the thread it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pin<'a> {
    pub device: &'a str,
    pub thread: &'a str,
}

/// The knobs one resolution takes (design section 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveOptions<'a> {
    /// Address threads of every workspace instead of only the caller's.
    pub all_scope: bool,
    /// Upstream's `match: "fuzzy"`: a hand-off's ranking after the exact ladder misses. The ranking itself is not
    /// ported (see `bare.rs`), so a fuzzy resolution answers what the exact ladder finds after checking the caller's
    /// workspace root.
    pub fuzzy: bool,
    /// Resolve a unique id prefix of at least 8 hex characters once the name step misses.
    pub id_prefix: bool,
    /// Leave this thread id out of the prefix candidates (a send never targets its own caller by prefix).
    pub exclude_from_prefix: Option<&'a str>,
    /// Ask each reachable device for a thread id nothing lists (one probe per device).
    pub probe: bool,
    /// An approved reply resolves only `thread` on `device`.
    pub pin: Option<Pin<'a>>,
    /// #14048: the resolved thread must be listed by one device only (this device's book included); a sender whose
    /// delivery names no device cannot be told from a copy of its id elsewhere, so two holders are ambiguous_target.
    pub unique_holder: bool,
    /// `need: "online"`: a thread on a device that is offline is `device_offline`, nothing is queued.
    pub need_online: bool,
}

impl Default for ResolveOptions<'_> {
    fn default() -> Self {
        Self {
            all_scope: false,
            fuzzy: false,
            id_prefix: true,
            exclude_from_prefix: None,
            probe: false,
            pin: None,
            unique_holder: false,
            need_online: false,
        }
    }
}

/// How a resolution found its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    SelfThread,
    Id,
    SessionId,
    Alias,
    Name,
    IdPrefix,
    Probe,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfThread => "self",
            Self::Id => "id",
            Self::SessionId => "session_id",
            Self::Alias => "alias",
            Self::Name => "name",
            Self::IdPrefix => "id_prefix",
            Self::Probe => "probe",
        }
    }
}

/// Where the resolved thread runs: in this device's book, or as another device's listing row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedWhere {
    Here { entry: ThreadAddressEntry },
    Device { row: Arc<RemoteSessionRow> },
}

/// A ladder hit before it becomes a `Resolved` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub entry: ThreadAddressEntry,
    pub resolution: Resolution,
    /// `Some(true)` once the hit came through the account (an alias, another device); else the caller's own `all_scope`.
    pub all_scope: Option<bool>,
}

/// A resolved address, in the shape every thread tool passes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The thread id, or the empty string for a session that publishes none (see `address`).
    pub thread: String,
    /// `<device id>/<thread id>` (or `<device id>/<listing key>`): accepted by every tool. With no account read it is the
    /// thread id alone.
    pub address: String,
    /// `None`: this device, with no account read.
    pub device: Option<DeviceRef>,
    pub where_: ResolvedWhere,
    /// The address book entry the thread resolved to.
    pub entry: ThreadAddressEntry,
    pub resolution: Resolution,
    pub all_scope: Option<bool>,
}
