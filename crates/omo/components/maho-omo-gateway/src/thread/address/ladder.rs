//! `self` -> the caller, then the exact ladder of `addressing.rs`.
//!
//! Upstream counterpart: `ladder.ts` (#14305).

use crate::thread::addressing::{
    ResolveOptions as TargetResolveOptions, TargetResolution, ThreadAddressEntry, resolve_self,
    resolve_target,
};
use crate::thread::errors::thread_tool_failure;
use crate::thread::store_extension_session_ops::UNKNOWN_CALLER;

use super::refusals::ThreadRefusal;
use super::types::{Resolution, ResolveContext, ResolveOptions};

/// The ladder's answer with the resolution named the way `Resolved` names it (`self`, `session_id`).
pub type LadderResult = Result<(ThreadAddressEntry, Resolution), ThreadRefusal>;

fn named(
    text: &str,
    entry: ThreadAddressEntry,
    resolution: TargetResolution,
) -> (ThreadAddressEntry, Resolution) {
    let mapped = if text == "self" {
        Resolution::SelfThread
    } else if resolution == TargetResolution::Id && entry.thread_id != text {
        Resolution::SessionId
    } else {
        match resolution {
            TargetResolution::Id => Resolution::Id,
            TargetResolution::Name => Resolution::Name,
            TargetResolution::IdPrefix => Resolution::IdPrefix,
        }
    };
    (entry, mapped)
}

/// `self` -> the caller (never matching `UNKNOWN_CALLER`), then the exact ladder (id, session id, name, id prefix).
pub fn ladder(
    ctx: &ResolveContext<'_>,
    entries: &[ThreadAddressEntry],
    text: &str,
    opts: &ResolveOptions<'_>,
) -> LadderResult {
    let target: String = if text == "self" {
        if ctx.caller_id == UNKNOWN_CALLER {
            return Err(thread_tool_failure(
                "caller_context_missing",
                "The caller's durable session id is not in the thread address book.",
                "Call thread_list (tool.thread_list() in an eval cell) and pass an explicit thread, or retry from a session with caller context.",
                None,
            ));
        }
        match resolve_self(entries, ctx.caller_id) {
            Ok(caller) => caller.thread_id.clone(),
            Err(failure) => return Err(*failure),
        }
    } else {
        text.to_owned()
    };
    let target_opts = TargetResolveOptions {
        all_scope: opts.all_scope,
        caller_workspace_root: ctx.caller_workspace_root,
        id_prefix: opts.id_prefix,
        exclude_from_prefix: opts.exclude_from_prefix,
    };
    match resolve_target(entries, &target, &target_opts) {
        Ok((entry, resolution)) => Ok(named(text, entry, resolution)),
        Err(failure) => Err(*failure),
    }
}
