//! The only place a control tool reads the device.
//!
//! Upstream counterpart: `thread/control/locate.ts` (omo #12879, #13215). An address resolved through this device's
//! address book becomes the control of the thread it names over this device's endpoint. A thread no engine holds (its
//! session file here, or an ended session) is `NotLive`. An address this device's book does not name is asked of the
//! account's other devices through [`RemoteControlLocator`], the boundary the remote leg implements.

use std::path::PathBuf;
use std::sync::Arc;

use crate::thread::adapter::BoxFuture;
use crate::thread::address_book::{
    AddressEntry, DiskSession, assemble_address_book, find_disk_sessions, thread_title,
};
use crate::thread::addressing::{
    ResolveOptions, resolve_self, resolve_target, to_thread_address_entries, workspace_directories,
    workspace_entries,
};
use crate::thread::errors::ThreadToolFailure;
use crate::thread::host_view::{ThreadHost, ThreadHostView};

use super::local_control::LocalControl;
use super::port::SessionControl;

/// What an address names for a control tool.
pub enum LocatedControl {
    /// A live thread, with the controls of the leg that holds it.
    Control(Arc<dyn SessionControl>),
    /// A thread no engine holds live: its session file here, or an ended session on another device.
    NotLive {
        thread_id: String,
        title: String,
        device: Option<String>,
    },
    /// The address resolved to nothing a control can act on; the failure is the call's answer.
    Error(ThreadToolFailure),
}

/// The account-wide half of locating a thread, implemented by the remote leg. It is asked only for an address this
/// device's address book names no thread for (`not_found` here); `None` keeps that answer. A thread it finds on another
/// device comes back as a `SessionControl` over that device's relay, or `NotLive` for an ended session.
pub trait RemoteControlLocator: Send + Sync {
    fn locate(
        &self,
        address: String,
        all_scope: bool,
        caller_id: String,
    ) -> BoxFuture<Option<LocatedControl>>;
}

/// One control call's reading of this device: the host view and the address book assembled from it, the caller, and
/// where the session files live.
pub struct ThreadCall {
    pub host: Arc<dyn ThreadHost>,
    pub view: ThreadHostView,
    pub book: Vec<AddressEntry>,
    /// The calling session's durable id, read by the engine, never taken from an argument.
    pub caller_id: String,
    pub caller_workspace_root: String,
    /// `<agentDir>/sessions`: a thread no endpoint lists is still found by its session file.
    pub sessions_directory: Option<PathBuf>,
    pub remote: Option<Arc<dyn RemoteControlLocator>>,
    /// The call's time as RFC 3339, standing in for a creation time nothing reports.
    pub now: String,
}

/// The address book of one view, with `extra` session files beside the ones a dead endpoint left.
pub fn book_of(view: &ThreadHostView, extra: &[DiskSession], now: &str) -> Vec<AddressEntry> {
    let mut disk = view.disk.clone();
    disk.extend_from_slice(extra);
    assemble_address_book(&view.hosts, &disk, now)
}

/// The session file an address names when no endpoint lists it.
pub fn find_session_file(call: &ThreadCall, address: &str, all_scope: bool) -> Option<DiskSession> {
    let directory = call.sessions_directory.as_ref()?;
    let roots = workspace_directories(&call.caller_workspace_root);
    find_disk_sessions(directory, address, all_scope, &roots)
        .into_iter()
        .next()
}

async fn located_elsewhere(
    call: &ThreadCall,
    address: &str,
    all_scope: bool,
    failure: ThreadToolFailure,
) -> LocatedControl {
    if failure.code.as_str() != "not_found" {
        return LocatedControl::Error(failure);
    }
    if let Some(found) = find_session_file(call, address, all_scope) {
        return LocatedControl::NotLive {
            thread_id: found.durable_id.clone(),
            title: thread_title(found.name.as_deref(), found.first_user_text.as_deref())
                .unwrap_or_default(),
            device: None,
        };
    }
    if let Some(remote) = &call.remote
        && let Some(located) = remote
            .locate(address.to_owned(), all_scope, call.caller_id.clone())
            .await
    {
        return located;
    }
    LocatedControl::Error(failure)
}

/// Resolves `address` (`self`, a durable id, a session id, a unique name or id prefix) to the thread a control acts on.
pub async fn locate_control(call: &ThreadCall, address: &str, all_scope: bool) -> LocatedControl {
    let entries = to_thread_address_entries(&call.book);
    let target = if address.trim() == "self" {
        match resolve_self(&entries, &call.caller_id) {
            Ok(entry) => entry.thread_id.clone(),
            Err(failure) => return LocatedControl::Error(*failure),
        }
    } else {
        address.to_owned()
    };
    let options = ResolveOptions {
        all_scope,
        caller_workspace_root: &call.caller_workspace_root,
        id_prefix: true,
        exclude_from_prefix: None,
    };
    let entry = match resolve_target(&entries, &target, &options) {
        Ok((entry, _)) => entry,
        Err(failure) => return located_elsewhere(call, address, all_scope, *failure).await,
    };
    let book_entry = call
        .book
        .iter()
        .find(|candidate| candidate.thread_id == entry.thread_id)
        .cloned();
    let title = if entry.name.is_empty() {
        book_entry
            .as_ref()
            .and_then(|candidate| candidate.title.clone())
            .unwrap_or_default()
    } else {
        entry.name.clone()
    };
    let Some(session) = call.view.target_session(&entry.thread_id) else {
        return LocatedControl::NotLive {
            thread_id: entry.thread_id,
            title,
            device: None,
        };
    };
    let visible = if all_scope {
        entries.clone()
    } else {
        workspace_entries(&entries, &call.caller_workspace_root)
    };
    LocatedControl::Control(Arc::new(LocalControl::new(
        call.host.session_port(session),
        session.clone(),
        title,
        visible,
        book_entry,
        call.now.clone(),
    )))
}
