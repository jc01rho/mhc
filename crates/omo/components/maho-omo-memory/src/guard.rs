use crate::{
    context::MemoryIdentityContext,
    guard_foreign_roots::{
        ForeignRootScan, ReadAreaEntries, read_area_entries, scan_foreign_roots,
    },
    policy_guard::{canonicalize_from_nearest_existing, resolve_path, stable_roots, within},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
/// Per session; past it an ancestor call is denied rather than left unchecked (omo #12476).
const MAX_PENDING_ANCESTOR_CHECKS: usize = 256;
#[derive(Debug, PartialEq, Eq)]
pub enum MemoryGuardDecision {
    Allow,
    Block {
        reason: String,
    },
    AdviseBash,
    /// A recursive tool walks into the memory area from above and no foreign identity is there now;
    /// its result must be re-checked, because an identity created during the walk would be read.
    AllowAncestor(PendingAncestorCheck),
}
/// What an ancestor enumeration was cleared against, re-listed when its result arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAncestorCheck {
    /// Each physical memory area once, by its canonical form.
    pub areas: Vec<PathBuf>,
    /// Own roots, including the lexical own root re-rooted under its canonical area.
    pub own_roots: Vec<PathBuf>,
    pub tool_name: String,
    pub raw_path: String,
}
/// Per session, the ancestor enumerations allowed at tool_call whose results are still outstanding.
#[derive(Default)]
pub struct AncestorChecks {
    pending: BTreeMap<String, BTreeMap<String, PendingAncestorCheck>>,
}
impl AncestorChecks {
    /// A call whose result cannot be matched (no tool call id) or that would pass the bound is denied.
    pub fn admit(
        &mut self,
        session_id: &str,
        tool_call_id: &str,
        check: PendingAncestorCheck,
    ) -> MemoryGuardDecision {
        let outstanding = self.pending.get(session_id).map_or(0, BTreeMap::len);
        if tool_call_id.is_empty() || outstanding >= MAX_PENDING_ANCESTOR_CHECKS {
            let area = check
                .areas
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from(&check.raw_path));
            return MemoryGuardDecision::Block {
                reason: ancestor_deny_reason(
                    &check.tool_name,
                    &check.raw_path,
                    &ForeignRootScan::Unlisted { area },
                )
                .unwrap_or_default(),
            };
        }
        self.pending
            .entry(session_id.to_string())
            .or_default()
            .insert(tool_call_id.to_string(), check);
        MemoryGuardDecision::Allow
    }
    /// Re-lists the areas once the result exists; the deny reason replaces the whole result.
    pub fn recheck(
        &mut self,
        session_id: &str,
        tool_call_id: &str,
        read_entries: ReadAreaEntries<'_>,
    ) -> Option<String> {
        let check = self.take(session_id, tool_call_id)?;
        let scan = scan_foreign_roots(&check.areas, &check.own_roots, read_entries);
        ancestor_deny_reason(&check.tool_name, &check.raw_path, &scan)
    }
    /// A call blocked by a later handler, or aborted before it ran, ends without a tool_result.
    pub fn release(&mut self, session_id: &str, tool_call_id: &str) {
        self.take(session_id, tool_call_id);
    }
    pub fn forget_session(&mut self, session_id: &str) {
        self.pending.remove(session_id);
    }
    fn take(&mut self, session_id: &str, tool_call_id: &str) -> Option<PendingAncestorCheck> {
        let pending = self.pending.get_mut(session_id)?;
        let check = pending.remove(tool_call_id);
        if pending.is_empty() {
            self.pending.remove(session_id);
        }
        check
    }
}
/// Names only the memory area, never the identity found in it; `None` when the scan is clear.
fn ancestor_deny_reason(tool_name: &str, raw_path: &str, scan: &ForeignRootScan) -> Option<String> {
    let (verdict, area) = match scan {
        ForeignRootScan::Clear => return None,
        ForeignRootScan::Foreign { area } => {
            ("would enumerate another memory identity found under", area)
        }
        ForeignRootScan::Unlisted { area } => (
            "could not be checked for another memory identity under",
            area,
        ),
    };
    Some(format!(
        "cross-identity memory access denied: {tool_name} to {raw_path} {verdict} {}",
        area.display()
    ))
}
#[derive(Default)]
pub struct MemoryGuard {
    warned_bash_sessions: BTreeSet<String>,
    ancestor_checks: AncestorChecks,
}
pub type GuardWarning = std::sync::Arc<dyn Fn(&str) + Send + Sync>;
pub fn register_memory_guard(
    api: &mut maho_ext_api::ExtensionApi,
    resolve: crate::prompt::PromptContextResolver,
    resolve_cwd: std::sync::Arc<dyn Fn() -> std::path::PathBuf + Send + Sync>,
    captured_tools: std::sync::Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    warn: GuardWarning,
) {
    let guard = std::sync::Arc::new(std::sync::Mutex::new(MemoryGuard::default()));
    // ONE tool_result registration for the whole guard: the host runs a PostToolUse hook per
    // handler. The ancestor re-check (enumeration tools) and the receipt notice (read tools) never
    // apply to the same call, and a replaced result is final.
    let results = guard.clone();
    api.on(maho_ext_api::EventKind::ToolResult, std::sync::Arc::new(move |event, context| {
        let session = context.session_manager.session_id().to_string();
        let results = results.clone();
        Box::pin(async move {
            let maho_ext_api::ExtensionEvent::ToolResult(result) = event else {
                return Ok(maho_ext_api::EventResult::None);
            };
            let denied = results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ancestor_checks
                .recheck(&session, &result.tool_call_id, &read_area_entries);
            if let Some(reason) = denied {
                return Ok(maho_ext_api::EventResult::ToolResult(maho_ext_api::ToolResultEventResult {
                    content: Some(vec![maho_ext_api::ToolContent::text(reason)]),
                    details: Some(serde_json::json!({})),
                    is_error: Some(true),
                    usage: None,
                }));
            }
            let payload = serde_json::json!({"type":"tool_result", "toolName":result.tool_name, "isError":result.is_error, "content":result.content});
            if crate::receipt_read_guard::unverified_receipt_notice(&payload).is_some() {
                let mut content = result.content.clone();
                content.push(maho_ext_api::ToolContent::text(crate::receipt_read_guard::UNVERIFIED_RECEIPT_NOTICE));
                return Ok(maho_ext_api::EventResult::ToolResult(maho_ext_api::ToolResultEventResult {
                    content: Some(content),
                    ..Default::default()
                }));
            }
            Ok(maho_ext_api::EventResult::None)
        })
    }));
    let calls = guard.clone();
    api.on(maho_ext_api::EventKind::ToolCall,std::sync::Arc::new(move |event,context| {
        let result=if let maho_ext_api::ExtensionEvent::ToolCall(call)=event {
            let id=context.session_manager.session_id();
            let mut guard=calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let decision=guard.check(resolve(id).as_ref(),&resolve_cwd(),&call.tool_name,&call.input,Some(id),&captured_tools());
            decision.map(|decision| match decision {
                MemoryGuardDecision::AllowAncestor(check)=>guard.ancestor_checks.admit(id,&call.tool_call_id,check),
                other=>other,
            })
        } else {Ok(MemoryGuardDecision::Allow)};
        let warn=warn.clone();
        Box::pin(async move {
            Ok(match result.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))? {
                MemoryGuardDecision::Allow|MemoryGuardDecision::AllowAncestor(_)=>maho_ext_api::EventResult::None,
                MemoryGuardDecision::Block{reason}=>maho_ext_api::EventResult::ToolCall(maho_ext_api::ToolCallEventResult{block:Some(true),reason:Some(reason),terminate:None}),
                MemoryGuardDecision::AdviseBash=>{warn("omo-senpi memory guard advisory: bash command references another memory identity; shell text is not blocked because this soft guard is not a security boundary");maho_ext_api::EventResult::None},
            })
        })
    }));
    // Pending checks are never released on agent_end: a nested walk from a background cell can
    // outlive a run. They end with their result, their execution, or the session.
    let ends = guard.clone();
    api.on(
        maho_ext_api::EventKind::ToolExecutionEnd,
        std::sync::Arc::new(move |event, context| {
            if let maho_ext_api::ExtensionEvent::ToolExecutionEnd { tool_call_id, .. } = event {
                ends.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .ancestor_checks
                    .release(context.session_manager.session_id(), tool_call_id);
            }
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }),
    );
    api.on(
        maho_ext_api::EventKind::SessionShutdown,
        std::sync::Arc::new(move |_event, context| {
            guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ancestor_checks
                .forget_session(context.session_manager.session_id());
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }),
    );
}
fn normalized(name: &str) -> String {
    name.trim().to_lowercase().replace('-', "_")
}
fn matches_tool(name: &str, expected: &str) -> bool {
    let name = normalized(name);
    name == expected
        || ["_", ":", "/"]
            .iter()
            .any(|separator| name.ends_with(&format!("{separator}{expected}")))
}
fn target_paths(input: &serde_json::Value, patch: bool) -> Vec<String> {
    let mut paths = Vec::new();
    for key in ["path", "filePath", "file_path", "target"] {
        if let Some(value) = input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
        {
            paths.push(value.to_owned());
        }
    }
    if let Some(values) = input.get("paths").and_then(serde_json::Value::as_array) {
        paths.extend(
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if patch && let Some(patch) = input.get("input").and_then(serde_json::Value::as_str) {
        for prefixes in [
            &["*** Add File:", "*** Update File:", "*** Delete File:"][..],
            &["*** Move to:"][..],
        ] {
            for line in patch.lines() {
                for prefix in prefixes {
                    if let Some(path) = line
                        .strip_prefix(prefix)
                        .map(str::trim)
                        .filter(|path| !path.is_empty())
                    {
                        paths.push(path.to_owned());
                    }
                }
            }
        }
    }
    paths
}
impl MemoryGuard {
    pub fn check(
        &mut self,
        context: Option<&MemoryIdentityContext>,
        cwd: &Path,
        tool_name: &str,
        input: &serde_json::Value,
        session_id: Option<&str>,
        captured_tools: &[String],
    ) -> std::io::Result<MemoryGuardDecision> {
        let Some(context) = context else {
            return Ok(MemoryGuardDecision::Allow);
        };
        if !input.is_object() {
            return Ok(MemoryGuardDecision::Allow);
        }
        // Structural, not enumerated (issue #7765): the verdict is "inside a memory area but outside
        // your own root", so no directory is listed and identities not yet on disk are denied too.
        // Both areas are denied because a transient run's own root lives under `transient-runs/`.
        let own = resolve_path(&context.identity_paths.root)?;
        let durable_agents = resolve_path(&context.durable_root)?
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let memory_root = durable_agents
            .parent()
            .unwrap_or(Path::new("/"))
            .to_path_buf();
        let mut denied = Vec::new();
        for root in [
            own.parent().unwrap_or(Path::new("/")).to_path_buf(),
            durable_agents,
            memory_root.join(crate::transient_identity::TRANSIENT_DIRNAME),
        ] {
            if !denied.contains(&root) {
                denied.push(root);
            }
        }
        if matches_tool(tool_name, "bash") {
            if let Some(command) = input.get("command").and_then(serde_json::Value::as_str) {
                let session = session_id
                    .filter(|id| !id.is_empty())
                    .unwrap_or("unknown-session");
                if denied
                    .iter()
                    .any(|root| command.contains(root.to_string_lossy().as_ref()))
                    && self.warned_bash_sessions.insert(session.into())
                {
                    return Ok(MemoryGuardDecision::AdviseBash);
                }
            }
            return Ok(MemoryGuardDecision::Allow);
        }
        let operation = ["read", "write", "edit", "ls", "find", "grep", "glob"]
            .into_iter()
            .find(|name| matches_tool(tool_name, name));
        let registered_patch = captured_tools.iter().any(|name| {
            name.eq_ignore_ascii_case(tool_name)
                && normalized(name)
                    .split(|character: char| !character.is_ascii_alphanumeric())
                    .any(|part| part == "patch")
        });
        let patch =
            operation.is_none() && (matches_tool(tool_name, "apply_patch") || registered_patch);
        if operation.is_none() && !patch {
            return Ok(MemoryGuardDecision::Allow);
        }
        let recursive = matches!(operation, Some("ls" | "find" | "grep" | "glob"));
        let denied_roots = stable_roots(&denied);
        let own_roots = stable_roots(std::slice::from_ref(&own));
        let mut ancestor: Option<PendingAncestorCheck> = None;
        for raw in target_paths(input, patch) {
            if raw.contains('\0') {
                continue;
            }
            // Built-ins open resolved targets; suffix-classified external tools may open raw strings.
            let name = normalized(tool_name);
            let builtin = Some(name.as_str()) == operation || name == "apply_patch";
            let target = if builtin {
                resolve_path(&cwd.join(&raw))?
            } else if Path::new(&raw).is_absolute() {
                PathBuf::from(&raw)
            } else {
                PathBuf::from(format!("{}/{}", cwd.display(), raw))
            };
            if operation == Some("read")
                && crate::project_memory_grants::is_granted_project_memory_read(context, &target)
            {
                continue;
            }
            let lexical = resolve_path(&target)?;
            let mut candidates = vec![lexical];
            if let Some(canonical) = canonicalize_from_nearest_existing(&target)
                && !candidates.contains(&canonical)
            {
                candidates.push(canonical);
            }
            let mut walks_from_above = false;
            for candidate in candidates {
                if own_roots.iter().any(|root| within(root, &candidate)) {
                    continue;
                }
                // Also denies a direct read of a stray file such as agents/.DS_Store, which the
                // ancestor scan skips; the structural rule over-approximates there, harmlessly.
                if denied_roots.iter().any(|root| within(root, &candidate)) {
                    return Ok(MemoryGuardDecision::Block {
                        reason: format!(
                            "cross-identity memory access denied: {tool_name} to {raw} belongs to another memory identity"
                        ),
                    });
                }
                if recursive && denied_roots.iter().any(|root| within(&candidate, root)) {
                    walks_from_above = true;
                }
            }
            if !walks_from_above {
                continue;
            }
            // A walk from above the memory area is denied only while another identity's directory
            // (durable, repo-less, or a live run) exists there (omo #12476, #12301).
            let (areas, scan_own_roots) = ancestor_scan_inputs(&denied, &own, &own_roots);
            let scan = scan_foreign_roots(&areas, &scan_own_roots, &read_area_entries);
            if let Some(reason) = ancestor_deny_reason(tool_name, &raw, &scan) {
                return Ok(MemoryGuardDecision::Block { reason });
            }
            ancestor = Some(PendingAncestorCheck {
                areas,
                own_roots: scan_own_roots,
                tool_name: tool_name.to_string(),
                raw_path: raw,
            });
        }
        Ok(ancestor.map_or(
            MemoryGuardDecision::Allow,
            MemoryGuardDecision::AllowAncestor,
        ))
    }
}
/// The verdict must not depend on which path form reached a directory: each physical area is listed
/// once by its canonical form, against the own roots plus the lexical own root re-rooted under the
/// canonical area (a case-variant own directory under a symlinked home lists as
/// `<canonical>/agents/Global`).
fn ancestor_scan_inputs(
    denied: &[PathBuf],
    own: &Path,
    own_roots: &[PathBuf],
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut areas: Vec<PathBuf> = Vec::new();
    let mut roots = own_roots.to_vec();
    for lexical in denied {
        let canonical =
            canonicalize_from_nearest_existing(lexical).unwrap_or_else(|| lexical.clone());
        if let Ok(relative) = own.strip_prefix(lexical) {
            let rerooted = canonical.join(relative);
            if !roots.contains(&rerooted) {
                roots.push(rerooted);
            }
        }
        if !areas.contains(&canonical) {
            areas.push(canonical);
        }
    }
    (areas, roots)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn fixture() -> (tempfile::TempDir, MemoryIdentityContext, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
        let foreign = paths.root.parent().unwrap().join("foreign/repo");
        std::fs::create_dir_all(&foreign).unwrap();
        let context = MemoryIdentityContext::new(
            "own".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "own".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        (dir, context, foreign)
    }
    #[test]
    fn granted_project_notes_allow_only_reads() {
        let (dir, context, foreign) = fixture();
        let note = foreign.join("notes/fixture.md");
        std::fs::create_dir_all(note.parent().unwrap()).unwrap();
        std::fs::write(&note, "fixture").unwrap();
        crate::project_memory_grants::grant_project_memory_read(&context, &foreign);
        let mut guard = MemoryGuard::default();
        assert_eq!(
            guard
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":note}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
        for tool in ["write", "edit", "grep", "glob", "ls"] {
            assert!(matches!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        tool,
                        &serde_json::json!({"path":note}),
                        None,
                        &[]
                    )
                    .unwrap(),
                MemoryGuardDecision::Block { .. }
            ));
        }
        crate::project_memory_grants::revoke_project_memory_reads(&context);
    }
    #[test]
    fn foreign_read_has_stable_reason() {
        let (dir, context, foreign) = fixture();
        let raw = foreign.join("persona.md");
        assert_eq!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":raw}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block {
                reason: format!(
                    "cross-identity memory access denied: read to {} belongs to another memory identity",
                    raw.display()
                )
            }
        );
    }
    #[test]
    fn own_worktree_edit_is_allowed() {
        let (dir, context, _) = fixture();
        assert_eq!(MemoryGuard::default().check(Some(&context),dir.path(),"edit",&serde_json::json!({"file_path":context.identity_paths.worktrees.join("reflection/notes.md")}),None,&[]).unwrap(),MemoryGuardDecision::Allow);
    }
    #[test]
    fn unbound_foreign_read_is_allowed() {
        let (dir, _, foreign) = fixture();
        assert_eq!(
            MemoryGuard::default()
                .check(
                    None,
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":foreign}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
    }
    #[test]
    fn relative_traversal_from_workspace_is_blocked() {
        let (dir, context, foreign) = fixture();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let relative = format!("../{}", foreign.strip_prefix(dir.path()).unwrap().display());
        assert!(matches!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    &workspace,
                    "vendor_READ",
                    &serde_json::json!({"target":relative}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
    }
    #[test]
    fn agents_root_and_ancestors_deny_enumeration() {
        let (dir, context, _) = fixture();
        let agents = context.identity_paths.root.parent().unwrap();
        for tool in ["ls", "find", "grep", "glob"] {
            for path in [agents, dir.path()] {
                assert!(matches!(
                    MemoryGuard::default()
                        .check(
                            Some(&context),
                            dir.path(),
                            tool,
                            &serde_json::json!({"path":path}),
                            None,
                            &[]
                        )
                        .unwrap(),
                    MemoryGuardDecision::Block { .. }
                ));
            }
        }
    }
    #[test]
    fn captured_patch_tool_blocks_foreign_move() {
        let (dir, context, foreign) = fixture();
        let tool = "custom_patch_tool";
        assert!(matches!(MemoryGuard::default().check(Some(&context),dir.path(),tool,&serde_json::json!({"input":format!("*** Begin Patch\n*** Update File: own.md\n*** Move to: {}/new.md\n*** End Patch",foreign.display())}),None,&[tool.into()]).unwrap(),MemoryGuardDecision::Block{..}));
    }
    #[test]
    fn file_paths_and_patch_directives_block() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        for (tool, input) in [
            (
                "vendor_READ",
                serde_json::json!({"target":foreign.join("a.md")}),
            ),
            (
                "glob",
                serde_json::json!({"paths":[context.identity_paths.repo,foreign]}),
            ),
            (
                "vendor_APPLY_PATCH",
                serde_json::json!({"input":format!("*** Begin Patch\n*** Move to: {}/new.md\n*** End Patch",foreign.display())}),
            ),
        ] {
            assert!(matches!(
                guard
                    .check(Some(&context), dir.path(), tool, &input, None, &[])
                    .unwrap(),
                MemoryGuardDecision::Block { .. }
            ));
        }
    }
    #[test]
    fn own_unbound_unknown_and_no_path_allowed() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        for (bound, tool, input) in [
            (
                Some(&context),
                "write",
                serde_json::json!({"path":context.identity_paths.repo}),
            ),
            (None, "read", serde_json::json!({"path":foreign})),
            (Some(&context), "query", serde_json::json!({"path":foreign})),
            (Some(&context), "read", serde_json::json!({"offset":1})),
        ] {
            assert_eq!(
                guard
                    .check(bound, dir.path(), tool, &input, None, &[])
                    .unwrap(),
                MemoryGuardDecision::Allow
            );
        }
    }
    #[test]
    fn enumeration_ancestors_block_but_reads_allowed() {
        let (dir, context, _) = fixture();
        let mut guard = MemoryGuard::default();
        let input = serde_json::json!({"path":dir.path()});
        assert!(matches!(
            guard
                .check(Some(&context), dir.path(), "grep", &input, None, &[])
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
        assert_eq!(
            guard
                .check(Some(&context), dir.path(), "read", &input, None, &[])
                .unwrap(),
            MemoryGuardDecision::Allow
        );
    }
    #[test]
    fn bash_advisory_once_per_session() {
        let (dir, context, foreign) = fixture();
        let mut guard = MemoryGuard::default();
        let input = serde_json::json!({"command":format!("ls {}",foreign.display())});
        for (session, result) in [
            ("a", MemoryGuardDecision::AdviseBash),
            ("a", MemoryGuardDecision::Allow),
            ("b", MemoryGuardDecision::AdviseBash),
        ] {
            assert_eq!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        "bash",
                        &input,
                        Some(session),
                        &[]
                    )
                    .unwrap(),
                result
            );
        }
    }
    #[test]
    fn an_identity_not_on_disk_is_blocked_without_enumerating_the_agents_root() {
        let (dir, context, _) = fixture();
        let ghost = context
            .identity_paths
            .root
            .parent()
            .unwrap()
            .join("never-created/repo/system/persona.md");
        assert_eq!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "read",
                    &serde_json::json!({"path":ghost}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block {
                reason: format!(
                    "cross-identity memory access denied: read to {} belongs to another memory identity",
                    ghost.display()
                )
            }
        );
    }
    #[test]
    fn a_transient_run_reaches_only_its_own_transient_root() {
        let dir = tempfile::tempdir().unwrap();
        let durable = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
        let foreign = memory_core::identity::layout::build_identity_paths(dir.path(), "foreign");
        let run = memory_core::identity::layout::build_identity_paths(
            &dir.path()
                .join(crate::transient_identity::TRANSIENT_DIRNAME)
                .join("token-1"),
            "own",
        );
        let other = memory_core::identity::layout::build_identity_paths(
            &dir.path()
                .join(crate::transient_identity::TRANSIENT_DIRNAME)
                .join("token-2"),
            "other",
        );
        let durable_foreign_file = foreign.repo.join("system/persona.md");
        let other_run_file = other.transcripts.join("stream/transcript.jsonl");
        for file in [
            &durable_foreign_file,
            &other_run_file,
            &run.repo.join("system/persona.md"),
        ] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "fixture").unwrap();
        }
        let context = MemoryIdentityContext::new(
            "own".into(),
            run.clone(),
            crate::binding::MemorySessionBinding {
                identity: "own".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        )
        .with_durable_root(durable.root);
        let mut guard = MemoryGuard::default();
        assert_eq!(
            guard
                .check(
                    Some(&context),
                    dir.path(),
                    "write",
                    &serde_json::json!({"path":run.transcripts.join("stream/transcript.jsonl")}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Allow
        );
        for file in [durable_foreign_file, other_run_file] {
            assert_eq!(
                guard
                    .check(
                        Some(&context),
                        dir.path(),
                        "read",
                        &serde_json::json!({"path":file}),
                        None,
                        &[]
                    )
                    .unwrap(),
                MemoryGuardDecision::Block {
                    reason: format!(
                        "cross-identity memory access denied: read to {} belongs to another memory identity",
                        file.display()
                    )
                }
            );
        }
    }
    fn single_identity() -> (tempfile::TempDir, MemoryIdentityContext) {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "own");
        std::fs::create_dir_all(&paths.repo).unwrap();
        std::fs::write(paths.root.parent().unwrap().join(".DS_Store"), "").unwrap();
        let context = MemoryIdentityContext::new(
            "own".into(),
            paths,
            crate::binding::MemorySessionBinding {
                identity: "own".into(),
                repo_path_hash: "hash".into(),
                bound_at: 1.0,
            },
        );
        (dir, context)
    }
    fn cleared_check(dir: &Path, context: &MemoryIdentityContext) -> PendingAncestorCheck {
        match MemoryGuard::default()
            .check(
                Some(context),
                dir,
                "ls",
                &serde_json::json!({"path":dir}),
                None,
                &[],
            )
            .unwrap()
        {
            MemoryGuardDecision::AllowAncestor(check) => check,
            other => panic!("expected an ancestor check, got {other:?}"),
        }
    }
    #[test]
    fn a_single_identity_may_enumerate_the_memory_root() {
        let (dir, context) = single_identity();
        for tool in ["ls", "find", "grep", "glob"] {
            let decision = MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    tool,
                    &serde_json::json!({"path":dir.path()}),
                    None,
                    &[],
                )
                .unwrap();
            assert!(
                matches!(decision, MemoryGuardDecision::AllowAncestor(_)),
                "{tool}: {decision:?}"
            );
        }
    }
    #[test]
    fn a_foreign_identity_denies_enumerating_from_above_without_naming_it() {
        let (dir, context, _) = fixture();
        let decision = MemoryGuard::default()
            .check(
                Some(&context),
                dir.path(),
                "grep",
                &serde_json::json!({"path":dir.path()}),
                None,
                &[],
            )
            .unwrap();
        let MemoryGuardDecision::Block { reason } = decision else {
            panic!("expected a deny, got {decision:?}");
        };
        assert!(reason.contains("would enumerate another memory identity found under"));
        assert!(!reason.contains("foreign"), "{reason}");
    }
    #[test]
    fn an_unlistable_memory_area_denies() {
        let (dir, context) = single_identity();
        std::fs::write(
            dir.path()
                .join(crate::transient_identity::TRANSIENT_DIRNAME),
            "not a directory",
        )
        .unwrap();
        let decision = MemoryGuard::default()
            .check(
                Some(&context),
                dir.path(),
                "ls",
                &serde_json::json!({"path":dir.path()}),
                None,
                &[],
            )
            .unwrap();
        assert!(
            matches!(decision, MemoryGuardDecision::Block { ref reason } if reason.contains("could not be checked for another memory identity under")),
            "{decision:?}"
        );
    }
    #[cfg(unix)]
    #[test]
    fn a_symlink_named_like_a_finder_file_still_denies() {
        let (dir, context) = single_identity();
        let agents = context.identity_paths.root.parent().unwrap().to_path_buf();
        std::fs::remove_file(agents.join(".DS_Store")).unwrap();
        std::os::unix::fs::symlink(&context.identity_paths.root, agents.join(".DS_Store")).unwrap();
        let decision = MemoryGuard::default()
            .check(
                Some(&context),
                dir.path(),
                "find",
                &serde_json::json!({"path":dir.path()}),
                None,
                &[],
            )
            .unwrap();
        assert!(
            matches!(decision, MemoryGuardDecision::Block { ref reason } if reason.contains("would enumerate another memory identity")),
            "{decision:?}"
        );
    }
    #[test]
    fn an_identity_created_during_the_walk_replaces_the_result() {
        let (dir, context) = single_identity();
        let mut checks = AncestorChecks::default();
        assert_eq!(
            checks.admit("s", "call-1", cleared_check(dir.path(), &context)),
            MemoryGuardDecision::Allow
        );
        assert_eq!(
            checks.admit("s", "call-2", cleared_check(dir.path(), &context)),
            MemoryGuardDecision::Allow
        );
        assert_eq!(checks.recheck("s", "call-2", &read_area_entries), None);
        std::fs::create_dir_all(context.identity_paths.root.parent().unwrap().join("other"))
            .unwrap();
        let reason = checks.recheck("s", "call-1", &read_area_entries).unwrap();
        assert!(reason.contains("would enumerate another memory identity found under"));
        assert_eq!(checks.recheck("s", "call-1", &read_area_entries), None);
    }
    #[test]
    fn the_pending_bound_denies_until_a_slot_is_released() {
        let (dir, context) = single_identity();
        let check = cleared_check(dir.path(), &context);
        let mut checks = AncestorChecks::default();
        for index in 0..MAX_PENDING_ANCESTOR_CHECKS {
            assert_eq!(
                checks.admit("s", &format!("call-{index}"), check.clone()),
                MemoryGuardDecision::Allow
            );
        }
        assert!(matches!(
            checks.admit("s", "overflow", check.clone()),
            MemoryGuardDecision::Block { ref reason } if reason.contains("could not be checked")
        ));
        assert_eq!(
            checks.admit("other-session", "call-0", check.clone()),
            MemoryGuardDecision::Allow
        );
        checks.release("s", "call-0");
        assert_eq!(
            checks.admit("s", "overflow", check),
            MemoryGuardDecision::Allow
        );
        checks.forget_session("s");
        assert_eq!(checks.recheck("s", "overflow", &read_area_entries), None);
    }
    #[test]
    fn a_call_without_an_id_cannot_be_rechecked_and_is_denied() {
        let (dir, context) = single_identity();
        assert!(matches!(
            AncestorChecks::default().admit("s", "", cleared_check(dir.path(), &context)),
            MemoryGuardDecision::Block { .. }
        ));
    }
    #[cfg(unix)]
    #[test]
    fn missing_symlink_descendant_blocked() {
        let (dir, context, foreign) = fixture();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(foreign, &alias).unwrap();
        assert!(matches!(
            MemoryGuard::default()
                .check(
                    Some(&context),
                    dir.path(),
                    "write",
                    &serde_json::json!({"path":alias.join("new/file.md")}),
                    None,
                    &[]
                )
                .unwrap(),
            MemoryGuardDecision::Block { .. }
        ));
    }
}
