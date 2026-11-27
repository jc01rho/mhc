//! The only place a control tool reads the device.
//!
//! Upstream counterpart: `thread/control/locate.ts` (omo #12879, #13215). An address resolved through this device's
//! address book becomes the control of the thread it names over this device's endpoint. A thread no engine holds (its
//! session file here, or an ended session) is `NotLive`. An address this device's book does not name is asked of the
//! account's other devices through [`RemoteControlLocator`], the boundary the remote leg implements.

use std::path::PathBuf;
use std::sync::Arc;

use maho_thread_address_core::row_key;
use serde_json::json;

use crate::thread::adapter::BoxFuture;
use crate::thread::address::{
    ResolveContext, ResolveOptions as AddressResolveOptions, ResolvedWhere, resolve_address,
};
use crate::thread::address_book::{
    AddressEntry, DiskSession, HostSessionStatus, assemble_address_book, find_disk_sessions,
};
use crate::thread::addressing::{
    ResolveOptions, ThreadAddressEntry, resolve_target, to_thread_address_entries,
    workspace_directories, workspace_entries,
};
use crate::thread::errors::{ThreadToolFailure, thread_tool_failure};
use crate::thread::host_view::{ThreadHost, ThreadHostView};
use crate::thread::remote::ports::{RemoteThreadSurface, escape_for_display, keyed};

use super::local_control::LocalControl;
use super::port::SessionControl;
use super::relay_control::create_relay_control;

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

    fn remote_surface(&self) -> Option<&RemoteThreadSurface> {
        None
    }

    fn remote_surface_arc(&self) -> Option<Arc<RemoteThreadSurface>> {
        None
    }

    /// The `open` leg of a device this account reaches over a transport that can open sessions there (agentd), when the
    /// locator has one; `None` keeps placement's `relay_session_host_unsupported` refusal.
    fn session_host(&self, _device: &str) -> Option<Arc<dyn SessionHost>> {
        None
    }
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

/// The address book a send or control lookup resolves `address` against: this device's entries, plus (only when the
/// address names none of them) the session files on disk it names, so a thread whose process exited is still found by
/// id or name.
pub fn send_address_book(call: &ThreadCall, address: &str, all_scope: bool) -> Vec<AddressEntry> {
    let Some(directory) = call.sessions_directory.as_ref() else {
        return call.book.clone();
    };
    if address == "self" || call.book.iter().any(|entry| entry.durable_id == address) {
        return call.book.clone();
    }
    let entries = to_thread_address_entries(&call.book);
    let resolved = resolve_target(
        &entries,
        address,
        &ResolveOptions {
            all_scope,
            caller_workspace_root: &call.caller_workspace_root,
            ..ResolveOptions::default()
        },
    );
    let id_only = !matches!(&resolved, Err(failure) if failure.code.as_str() == "not_found");
    let roots = workspace_directories(&call.caller_workspace_root);
    let found: Vec<DiskSession> = find_disk_sessions(directory, address, all_scope, &roots)
        .into_iter()
        .filter(|session| !id_only || session.durable_id == address)
        .filter(|session| {
            !call
                .book
                .iter()
                .any(|entry| entry.durable_id == session.durable_id)
        })
        .collect();
    if found.is_empty() {
        return call.book.clone();
    }
    book_of(&call.view, &found, &call.now)
}

/// Resolves `address` (`self`, a bare address, `here/<thread>`, or `<device>/<thread>`) through `resolve_address` to
/// the thread a control acts on.
pub async fn locate_control(call: &ThreadCall, address: &str, all_scope: bool) -> LocatedControl {
    let remote_surface = call
        .remote
        .as_deref()
        .and_then(RemoteControlLocator::remote_surface);
    let local_for = |part: &str| -> Vec<ThreadAddressEntry> {
        to_thread_address_entries(&send_address_book(call, part, all_scope))
    };
    let local = local_for(address);
    let context = ResolveContext {
        remote: remote_surface,
        caller_workspace_root: &call.caller_workspace_root,
        local: &local,
        local_for: Some(&local_for),
        caller_id: &call.caller_id,
    };
    let options = AddressResolveOptions {
        all_scope,
        probe: true,
        need_online: true,
        ..AddressResolveOptions::default()
    };
    match resolve_address(&context, address, &options).await {
        Ok(resolved) => match resolved.where_ {
            ResolvedWhere::Here { .. } => {
                let book_entry = call
                    .book
                    .iter()
                    .find(|candidate| candidate.thread_id == resolved.thread)
                    .cloned();
                let title = if !resolved.entry.name.is_empty() {
                    resolved.entry.name.clone()
                } else {
                    book_entry
                        .as_ref()
                        .and_then(|candidate| candidate.title.clone())
                        .unwrap_or_default()
                };
                let Some(session) = call.view.target_session(&resolved.thread) else {
                    return LocatedControl::NotLive {
                        thread_id: resolved.thread,
                        title,
                        device: None,
                    };
                };
                if session.status == Some(HostSessionStatus::Opening) {
                    return LocatedControl::Error(thread_tool_failure(
                        "host_unavailable",
                        format!(
                            "Thread {} is opening on its host and cannot accept controls yet.",
                            resolved.thread
                        ),
                        "Retry once the thread is open.",
                        Some(json!({ "thread": resolved.thread, "reason": "session_opening" })),
                    ));
                }
                let entries = to_thread_address_entries(&call.book);
                let visible = if all_scope {
                    entries
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
            ResolvedWhere::Device { row } => {
                if row.state == "ended" {
                    LocatedControl::NotLive {
                        thread_id: row_key(&keyed(&row)),
                        title: escape_for_display(&row.title),
                        device: Some(row.device.clone()),
                    }
                } else if let Some(surface) = call
                    .remote
                    .as_deref()
                    .and_then(RemoteControlLocator::remote_surface_arc)
                {
                    LocatedControl::Control(Arc::new(create_relay_control(surface, (*row).clone())))
                } else {
                    LocatedControl::Error(thread_tool_failure(
                        "host_unavailable",
                        "Remote thread surface is not available for relay control.",
                        "Retry when the remote surface is configured.",
                        None,
                    ))
                }
            }
        },
        Err(failure) => {
            if remote_surface.is_none()
                && failure.code.as_str() == "not_found"
                && let Some(remote) = &call.remote
                && let Some(located) = remote
                    .locate(address.to_owned(), all_scope, call.caller_id.clone())
                    .await
            {
                return located;
            }
            LocatedControl::Error(*failure)
        }
    }
}
