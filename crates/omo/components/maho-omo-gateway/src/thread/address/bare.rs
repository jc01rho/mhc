//! A bare address over this device's book.
//!
//! Upstream counterpart: `bare.ts` (#14305). Without a remote account surface, `resolve_bare` runs the local ladder
//! and enriches any refusal with `with_addresses` (`details.kind = "thread"`).

use crate::thread::errors::thread_tool_failure;

use super::ladder::ladder;
use super::refusals::{ThreadRefusal, with_addresses};
use super::types::{Found, ResolveContext, ResolveOptions};

/// Resolves a bare address against this device's book.
pub fn resolve_bare(
    ctx: &ResolveContext<'_>,
    text: &str,
    opts: &ResolveOptions<'_>,
) -> Result<Found, ThreadRefusal> {
    if opts.fuzzy && !opts.all_scope && ctx.caller_workspace_root.trim().is_empty() {
        return Err(Box::new(thread_tool_failure(
            "caller_context_missing",
            "Scoping a fuzzy handoff needs the caller's workspace root, which this call did not provide.",
            "Set all_scope to search every workspace, or hand off from a session that knows its workspace.",
            None,
        )));
    }
    match ladder(ctx, ctx.local, text, opts) {
        Ok((entry, resolution)) => Ok(Found {
            entry,
            resolution,
            all_scope: Some(opts.all_scope),
        }),
        Err(error) => Err(with_addresses(*error, None, |_| None)),
    }
}
