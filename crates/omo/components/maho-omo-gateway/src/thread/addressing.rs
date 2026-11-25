//! Addressing and scope resolution for the thread tools.
//!
//! Upstream counterpart: `thread/addressing.ts`, the exact ladder (`resolveTarget`) and the workspace scope; the
//! fuzzy ranking stays with `thread_handoff`'s send path. Every outcome is data: the error branch is a taxonomy failure
//! whose `next_action` names the recovery.
//!
//! Resolution ladder: an exact durable id wins at once (outside the caller's workspace it is `scope_denied`, never
//! `not_found`); a host session id names its thread; then the NFKC-normalized name over the visible entries (one hit
//! resolves, two are `ambiguous_target`); then a unique durable-id prefix of at least 8 hex characters. With no visible
//! match, a thread outside the workspace that carries the name answers `scope_denied` (#14199).
//!
//! Workspace scope: two paths share a workspace when they sit in the same git work tree (linked work trees of one
//! repository count as separate workspaces); outside git, when their canonical paths are equal.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::json;

use super::address_book::AddressEntry;
use super::contracts_results::ThreadStatus;
use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::remote::ports::{RemoteSessionRow, keyed};

pub const AMBIGUOUS_CANDIDATE_LIMIT: usize = 10;

/// NFKC-normalize, trim, lowercase and collapse internal whitespace: the one name comparison (thread-address-core).
pub fn normalize_thread_name(value: &str) -> String {
    maho_thread_address_core::normalize_name(value)
}

/// A thread as the addressing layer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadAddressEntry {
    pub thread_id: String,
    pub name: String,
    pub cwd: String,
    pub status: ThreadStatus,
    pub updated_at: Option<String>,
    /// Ephemeral session ids that name this same thread: the host routing id, when it differs from `thread_id`.
    pub session_ids: Vec<String>,
    /// The account device holding the thread when it is not this one (decision 0027: one address book across
    /// transports).
    pub device: Option<String>,
    /// Account aliases this thread holds (the registry answered them for the address being resolved).
    pub aliases: Vec<String>,
    /// The listing row a thread on another device is reached through (upstream `transport.row`).
    pub row: Option<Arc<RemoteSessionRow>>,
}

impl ThreadAddressEntry {
    pub fn from_address_entry(entry: &AddressEntry) -> Self {
        let routing = entry
            .endpoint
            .as_ref()
            .and_then(|endpoint| endpoint.routing_id.clone());
        Self {
            thread_id: entry.thread_id.clone(),
            name: entry.name.clone().unwrap_or_default(),
            cwd: entry.cwd.clone(),
            status: entry.status,
            updated_at: entry.updated_at.clone(),
            session_ids: routing
                .into_iter()
                .filter(|routing| *routing != entry.thread_id)
                .collect(),
            device: None,
            aliases: Vec::new(),
            row: None,
        }
    }
}

/// The entries of an address book, as the addressing layer reads them.
pub fn to_thread_address_entries(entries: &[AddressEntry]) -> Vec<ThreadAddressEntry> {
    entries
        .iter()
        .map(ThreadAddressEntry::from_address_entry)
        .collect()
}

/// How a hit was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetResolution {
    Id,
    Alias,
    Name,
    IdPrefix,
}

/// The candidate payload every ambiguity error carries (up to ten).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThreadCandidate {
    pub thread: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unnamed: Option<bool>,
    pub cwd: String,
    pub preview: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
    pub state: ThreadStatus,
    /// The account device that holds it, when the candidates span devices (a name matched across the account).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// What a candidate is passed back as: its thread id, or for a listed row that publishes none, its row key.
pub fn candidate_thread(entry: &ThreadAddressEntry) -> String {
    match &entry.row {
        Some(row) if entry.thread_id.is_empty() => maho_thread_address_core::row_key(&keyed(row)),
        _ => entry.thread_id.clone(),
    }
}

/// This device's threads first, then most recently updated first, the thread id ascending as the tiebreak, capped at
/// the candidate limit. When the candidates span devices, this device's threads say so too.
fn candidates(matches: &[&ThreadAddressEntry]) -> Vec<ThreadCandidate> {
    let spans = matches.iter().any(|entry| entry.device.is_some());
    let mut sorted = matches.to_vec();
    sorted.sort_by(|left, right| {
        match (left.row.is_none(), right.row.is_none()) {
            (true, false) => return std::cmp::Ordering::Less,
            (false, true) => return std::cmp::Ordering::Greater,
            _ => {}
        }
        match (&left.updated_at, &right.updated_at) {
            (Some(left_at), Some(right_at)) if left_at != right_at => right_at.cmp(left_at),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => left.thread_id.cmp(&right.thread_id),
        }
    });
    sorted
        .into_iter()
        .take(AMBIGUOUS_CANDIDATE_LIMIT)
        .map(|entry| {
            let shown = maho_thread_address_core::row_name(&maho_thread_address_core::NamedRow {
                name: (!entry.name.trim().is_empty()).then(|| entry.name.clone()),
                first_message: None,
                folder: entry.cwd.clone(),
            });
            ThreadCandidate {
                thread: candidate_thread(entry),
                name: shown.name,
                unnamed: shown.unnamed.then_some(true),
                cwd: entry.cwd.clone(),
                preview: String::new(),
                updated_at: entry.updated_at.clone(),
                state: entry.status,
                device: match &entry.device {
                    Some(device) => Some(device.clone()),
                    None if spans && entry.row.is_none() => Some("here".to_owned()),
                    None => None,
                },
            }
        })
        .collect()
}

fn address_error(
    code: &str,
    message: impl Into<String>,
    next_action: &str,
    matches: Option<&[&ThreadAddressEntry]>,
) -> Box<ThreadToolFailure> {
    let details = matches.map(|matches| json!({ "candidates": candidates(matches) }));
    Box::new(thread_tool_failure(code, message, next_action, details))
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The git work tree a directory sits in (`git rev-parse --show-toplevel`): the nearest ancestor holding a `.git`
/// directory or a linked work tree's `.git` file.
pub fn git_worktree_root(dir: &str) -> Option<PathBuf> {
    let start = canonical(Path::new(dir));
    start
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

/// One memoized root lookup per resolution, so a long entry list walks each distinct directory once.
#[derive(Default)]
pub struct WorkspaceJudge {
    roots: BTreeMap<String, Option<PathBuf>>,
}

impl WorkspaceJudge {
    fn root_of(&mut self, dir: &str) -> Option<PathBuf> {
        self.roots
            .entry(dir.to_owned())
            .or_insert_with(|| git_worktree_root(dir))
            .clone()
    }

    /// Whether `entry_cwd` belongs to the caller's workspace.
    pub fn same_workspace(&mut self, entry_cwd: &str, caller_root: &str) -> bool {
        match (self.root_of(entry_cwd), self.root_of(caller_root)) {
            (Some(entry_root), Some(caller_top)) => entry_root == caller_top,
            _ => canonical(Path::new(entry_cwd)) == canonical(Path::new(caller_root)),
        }
    }
}

/// Every path the caller's workspace can be spelled as: the root, its git top level, and the canonical form of each.
/// A session directory named after one of them may hold a thread the scoped ladder judges in scope.
pub fn workspace_directories(caller_workspace_root: &str) -> Vec<String> {
    let root = caller_workspace_root.trim();
    if root.is_empty() {
        return Vec::new();
    }
    let mut spelled = vec![PathBuf::from(root)];
    if let Some(top) = git_worktree_root(root) {
        spelled.push(top);
    }
    let mut directories: Vec<String> = Vec::new();
    for path in spelled {
        for form in [path.clone(), canonical(&path)] {
            let text = form.to_string_lossy().into_owned();
            if !directories.contains(&text) {
                directories.push(text);
            }
        }
    }
    directories
}

/// The entries that belong to the caller's workspace, judged exactly as the scoped ladder judges them. Without a
/// caller root nothing is in scope: listing every workspace is what `all_scope` exists to make explicit.
pub fn workspace_entries(
    entries: &[ThreadAddressEntry],
    caller_workspace_root: &str,
) -> Vec<ThreadAddressEntry> {
    let caller_root = caller_workspace_root.trim();
    if caller_root.is_empty() {
        return Vec::new();
    }
    let mut judge = WorkspaceJudge::default();
    entries
        .iter()
        .filter(|entry| judge.same_workspace(&entry.cwd, caller_root))
        .cloned()
        .collect()
}

/// How a call resolves: across every workspace or only the caller's, and whether a unique id prefix may resolve
/// (`exclude_from_prefix` leaves the caller out of the prefix candidates, as a send does).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveOptions<'a> {
    pub all_scope: bool,
    pub caller_workspace_root: &'a str,
    pub id_prefix: bool,
    pub exclude_from_prefix: Option<&'a str>,
}

fn scoped_id_hit(
    entry: &ThreadAddressEntry,
    options: &ResolveOptions<'_>,
    judge: &mut WorkspaceJudge,
    resolution: TargetResolution,
) -> Result<(ThreadAddressEntry, TargetResolution), Box<ThreadToolFailure>> {
    if options.all_scope || entry.row.is_some() {
        return Ok((entry.clone(), resolution));
    }
    let caller_root = options.caller_workspace_root.trim();
    if caller_root.is_empty() {
        return Err(address_error(
            "caller_context_missing",
            "Scoping an id hit needs the caller's workspace root, which this call did not provide.",
            "Set all_scope to address every workspace, or call thread_list (tool.thread_list() in an eval cell) from a session that knows its workspace.",
            None,
        ));
    }
    if judge.same_workspace(&entry.cwd, caller_root) {
        return Ok((entry.clone(), resolution));
    }
    Err(address_error(
        "scope_denied",
        format!("Thread {} exists outside this workspace.", entry.thread_id),
        "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to confirm the id, then retry with all_scope set, or target a thread in this workspace.",
        None,
    ))
}

fn not_found(target: &str, scope: &str, visible: &[&ThreadAddressEntry]) -> Box<ThreadToolFailure> {
    let base = format!("No visible thread matches \"{target}\" in {scope}.");
    if maho_thread_address_core::is_thread_id(target) {
        return address_error(
            "not_found",
            format!("{base} No reachable endpoint lists a thread with that thread_id."),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to check the id; a thread on a device that is offline is found once it is online.",
            None,
        );
    }
    if !maho_thread_address_core::is_session_id(target) {
        return address_error(
            "not_found",
            base,
            "Call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value or a unique name it returned.",
            None,
        );
    }
    let ids: Vec<&str> = visible
        .iter()
        .map(|entry| entry.thread_id.as_str())
        .filter(|id| !id.is_empty())
        .take(5)
        .collect();
    let listed = if ids.is_empty() {
        String::new()
    } else {
        format!(" Visible thread values: {}.", ids.join(", "))
    };
    address_error(
        "not_found",
        format!(
            "{base} \"{target}\" is a session id, not a thread value. Pass the thread value thread_list (tool.thread_list() in an eval cell) shows, or omit thread / pass self for this session.{listed}"
        ),
        "Call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value.",
        None,
    )
}

fn has_unfoldable_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| matches!(character, '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{7f}'))
}

/// Resolves a thread address (durable id, session id, or unique name, then a unique id prefix) against the entries the
/// host knows, restricted to the caller's workspace unless `all_scope` widens it.
pub fn resolve_target(
    entries: &[ThreadAddressEntry],
    target: &str,
    options: &ResolveOptions<'_>,
) -> Result<(ThreadAddressEntry, TargetResolution), Box<ThreadToolFailure>> {
    if has_unfoldable_control(target) {
        return Err(address_error(
            "invalid_arguments",
            "The thread address contains control characters.",
            "Remove the control characters and retry, or pass an address from thread_list (tool.thread_list() in an eval cell).",
            None,
        ));
    }
    let normalized = normalize_thread_name(target);
    if normalized.is_empty() {
        return Err(address_error(
            "invalid_arguments",
            "The thread address is empty.",
            "Pass a thread value or a unique name from thread_list (tool.thread_list() in an eval cell) as the thread address.",
            None,
        ));
    }
    let mut judge = WorkspaceJudge::default();
    if let Some(hit) = entries.iter().find(|entry| entry.thread_id == target) {
        return scoped_id_hit(hit, options, &mut judge, TargetResolution::Id);
    }
    let session_hits: Vec<&ThreadAddressEntry> = entries
        .iter()
        .filter(|entry| entry.session_ids.iter().any(|id| id.as_str() == target))
        .collect();
    if let [hit] = session_hits.as_slice() {
        return scoped_id_hit(hit, options, &mut judge, TargetResolution::Id).map_err(|error| {
            if error.code.as_str() == "scope_denied" {
                address_error(
                    "scope_denied",
                    format!(
                        "Thread {} (session id \"{target}\") exists outside this workspace.",
                        hit.thread_id
                    ),
                    "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) and pass that thread, or target a thread in this workspace.",
                    None,
                )
            } else {
                error
            }
        });
    }
    if session_hits.len() > 1 {
        return Err(address_error(
            "ambiguous_target",
            format!(
                "{} visible threads share the session id \"{target}\".",
                session_hits.len()
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass the exact thread value of the intended thread.",
            Some(&session_hits),
        ));
    }
    if let Some(hit) = entries.iter().find(|entry| {
        entry
            .aliases
            .iter()
            .any(|alias| normalize_thread_name(alias) == normalized)
    }) {
        return Ok((hit.clone(), TargetResolution::Alias));
    }
    let caller_root = options.caller_workspace_root.trim();
    let visible: Vec<&ThreadAddressEntry> = if options.all_scope {
        entries.iter().collect()
    } else {
        if caller_root.is_empty() {
            return Err(address_error(
                "caller_context_missing",
                "Scoped address resolution needs the caller's workspace root, which this call did not provide.",
                "Set all_scope to address every workspace, or call thread_list (tool.thread_list() in an eval cell) from a session that knows its workspace.",
                None,
            ));
        }
        entries
            .iter()
            .filter(|entry| entry.row.is_some() || judge.same_workspace(&entry.cwd, caller_root))
            .collect()
    };
    let named: Vec<&ThreadAddressEntry> = visible
        .iter()
        .copied()
        .filter(|entry| normalize_thread_name(&entry.name) == normalized)
        .collect();
    if let [hit] = named.as_slice() {
        return Ok(((*hit).clone(), TargetResolution::Name));
    }
    if named.len() > 1 {
        return Err(address_error(
            "ambiguous_target",
            format!(
                "{} visible threads share the name \"{target}\".",
                named.len()
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass the exact thread value of the intended thread.",
            Some(&named),
        ));
    }
    if options.id_prefix && maho_thread_address_core::is_id_prefix(target) {
        let prefix = target.to_lowercase();
        let mut prefixed: Vec<&ThreadAddressEntry> = Vec::new();
        for entry in entries {
            if Some(entry.thread_id.as_str()) != options.exclude_from_prefix
                && entry.thread_id.to_lowercase().starts_with(&prefix)
                && !prefixed
                    .iter()
                    .any(|seen| seen.thread_id == entry.thread_id)
            {
                prefixed.push(entry);
            }
        }
        if let [hit] = prefixed.as_slice() {
            return scoped_id_hit(hit, options, &mut judge, TargetResolution::IdPrefix);
        }
        if prefixed.len() > 1 {
            return Err(address_error(
                "ambiguous_target",
                format!(
                    "{} threads have an id starting with \"{target}\".",
                    prefixed.len()
                ),
                "Pass a longer prefix or the exact thread value from the candidates.",
                Some(&prefixed),
            ));
        }
    }
    if !options.all_scope {
        let out_of_scope: Vec<&ThreadAddressEntry> = entries
            .iter()
            .filter(|entry| {
                entry.row.is_none()
                    && normalize_thread_name(&entry.name) == normalized
                    && !judge.same_workspace(&entry.cwd, caller_root)
            })
            .collect();
        if !out_of_scope.is_empty() {
            return Err(address_error(
                "scope_denied",
                if out_of_scope.len() == 1 {
                    format!(
                        "A thread named \"{target}\" exists on this device outside this workspace."
                    )
                } else {
                    format!(
                        "{} threads named \"{target}\" exist on this device outside this workspace.",
                        out_of_scope.len()
                    )
                },
                "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to see it, then retry with all_scope set, or target a thread in this workspace.",
                Some(&out_of_scope),
            ));
        }
    }
    let scope = if options.all_scope {
        "the daemon-wide scope"
    } else {
        "this workspace"
    };
    Err(not_found(target, scope, &visible))
}

/// A new name must not be one a visible thread already carries (`checkNameConflict`): the collision is data, so the
/// caller picks a new name or reuses the thread it names by id.
pub fn check_name_conflict(
    entries: &[ThreadAddressEntry],
    name: &str,
) -> Result<(), Box<ThreadToolFailure>> {
    if has_unfoldable_control(name) {
        return Err(address_error(
            "invalid_arguments",
            "The thread name contains control characters.",
            "Remove the control characters and retry, or pass an address from thread_list (tool.thread_list() in an eval cell).",
            None,
        ));
    }
    let normalized = normalize_thread_name(name);
    if normalized.is_empty() {
        return Err(address_error(
            "invalid_arguments",
            "The thread name is empty.",
            "Pass a thread value or a unique name from thread_list (tool.thread_list() in an eval cell) as the thread name.",
            None,
        ));
    }
    let Some(conflict) = entries
        .iter()
        .find(|entry| normalize_thread_name(&entry.name) == normalized)
    else {
        return Ok(());
    };
    Err(address_error(
        "name_conflict",
        format!(
            "A thread named \"{}\" ({}) already exists.",
            conflict.name, conflict.thread_id
        ),
        "Call thread_list (tool.thread_list() in an eval cell) to reuse that thread by id, or pass a different name for the new thread.",
        Some(&[conflict]),
    ))
}

/// `self` is the caller's own entry, found by its durable id or a session id the host gave it; the caller's identity is
/// never taken from an argument.
pub fn resolve_self<'a>(
    entries: &'a [ThreadAddressEntry],
    caller_id: &str,
) -> Result<&'a ThreadAddressEntry, Box<ThreadToolFailure>> {
    entries
        .iter()
        .find(|entry| entry.thread_id == caller_id)
        .or_else(|| {
            entries
                .iter()
                .find(|entry| entry.session_ids.iter().any(|id| id.as_str() == caller_id))
        })
        .filter(|_| !caller_id.is_empty())
        .ok_or_else(|| {
            address_error(
                "caller_context_missing",
                "The caller's durable session id is not in the thread address book.",
                "Call thread_list (tool.thread_list() in an eval cell) and pass an explicit thread, or retry from a session with caller context.",
                None,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(thread_id: &str, name: &str, cwd: &str) -> ThreadAddressEntry {
        ThreadAddressEntry {
            thread_id: thread_id.into(),
            name: name.into(),
            cwd: cwd.into(),
            status: ThreadStatus::Live,
            updated_at: None,
            session_ids: Vec::new(),
            device: None,
            aliases: Vec::new(),
            row: None,
        }
    }

    fn scoped(root: &str) -> ResolveOptions<'_> {
        ResolveOptions {
            all_scope: false,
            caller_workspace_root: root,
            id_prefix: true,
            exclude_from_prefix: None,
        }
    }

    fn code(
        result: Result<(ThreadAddressEntry, TargetResolution), Box<ThreadToolFailure>>,
    ) -> String {
        match result {
            Ok((entry, _)) => entry.thread_id,
            Err(error) => error.code.as_str().to_owned(),
        }
    }

    #[test]
    fn an_exact_id_wins_and_is_scoped_to_the_workspace() {
        let entries = [
            entry(
                "0aa00001-0000-4000-8000-000000000001",
                "builder",
                "/nonexistent/a",
            ),
            entry(
                "0bb00001-0000-4000-8000-000000000001",
                "0aa00001-0000-4000-8000-000000000001",
                "/nonexistent/a",
            ),
        ];
        assert_eq!(
            code(resolve_target(
                &entries,
                "0aa00001-0000-4000-8000-000000000001",
                &scoped("/nonexistent/a")
            )),
            "0aa00001-0000-4000-8000-000000000001"
        );
        assert_eq!(
            code(resolve_target(
                &entries,
                "0aa00001-0000-4000-8000-000000000001",
                &scoped("/nonexistent/b")
            )),
            "scope_denied"
        );
    }

    #[test]
    fn a_name_matches_after_normalization_and_two_matches_are_ambiguous() {
        let entries = [
            entry("t-1", "Ｂuilder", "/nonexistent/a"),
            entry("t-2", "reviewer", "/nonexistent/a"),
            entry("t-3", "reviewer", "/nonexistent/a"),
        ];
        assert_eq!(
            code(resolve_target(
                &entries,
                " builder ",
                &scoped("/nonexistent/a")
            )),
            "t-1"
        );
        let ambiguous = resolve_target(&entries, "Reviewer", &scoped("/nonexistent/a"))
            .expect_err("two reviewers");
        assert_eq!(ambiguous.code.as_str(), "ambiguous_target");
        assert_eq!(
            ambiguous
                .details
                .as_ref()
                .map(|details| details["candidates"].as_array().map_or(0, Vec::len)),
            Some(2)
        );
    }

    #[test]
    fn a_unique_id_prefix_resolves_and_a_shared_one_is_ambiguous() {
        let entries = [
            entry(
                "0aa00001-0000-4000-8000-000000000001",
                "one",
                "/nonexistent/a",
            ),
            entry(
                "0bb00001-0000-4000-8000-000000000001",
                "two",
                "/nonexistent/a",
            ),
            entry(
                "0bb00001-0000-4000-8000-000000000002",
                "three",
                "/nonexistent/a",
            ),
        ];
        assert_eq!(
            code(resolve_target(
                &entries,
                "0aa00001",
                &scoped("/nonexistent/a")
            )),
            "0aa00001-0000-4000-8000-000000000001"
        );
        assert_eq!(
            code(resolve_target(
                &entries,
                "0bb00001",
                &scoped("/nonexistent/a")
            )),
            "ambiguous_target"
        );
        let mut excluding = scoped("/nonexistent/a");
        excluding.exclude_from_prefix = Some("0bb00001-0000-4000-8000-000000000002");
        assert_eq!(
            code(resolve_target(&entries, "0bb00001", &excluding)),
            "0bb00001-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn a_name_outside_the_workspace_is_scope_denied_never_not_found() {
        let entries = [entry("t-1", "builder", "/nonexistent/other")];
        assert_eq!(
            code(resolve_target(
                &entries,
                "builder",
                &scoped("/nonexistent/a")
            )),
            "scope_denied"
        );
        assert_eq!(
            code(resolve_target(
                &entries,
                "nobody",
                &scoped("/nonexistent/a")
            )),
            "not_found"
        );
        let everywhere = ResolveOptions {
            all_scope: true,
            ..scoped("")
        };
        assert_eq!(
            code(resolve_target(&entries, "builder", &everywhere)),
            "t-1"
        );
        assert_eq!(
            code(resolve_target(&entries, "builder", &scoped(""))),
            "caller_context_missing"
        );
    }

    #[test]
    fn a_session_id_names_its_thread_and_self_is_the_caller() {
        let mut live = entry("d-1", "builder", "/nonexistent/a");
        live.session_ids = vec!["rpc-4".into()];
        let entries = [live];
        assert_eq!(
            code(resolve_target(&entries, "rpc-4", &scoped("/nonexistent/a"))),
            "d-1"
        );
        assert_eq!(
            resolve_self(&entries, "rpc-4").map(|hit| hit.thread_id.clone()),
            Ok("d-1".to_owned())
        );
        assert_eq!(
            resolve_self(&entries, "someone-else").map_err(|error| error.code.as_str().to_owned()),
            Err("caller_context_missing".to_owned())
        );
        assert_eq!(
            code(resolve_target(
                &entries,
                "a\u{1}b",
                &scoped("/nonexistent/a")
            )),
            "invalid_arguments"
        );
    }

    #[test]
    fn unnamed_candidates_show_folder_name_and_unnamed_flag() {
        let mut first = entry("d-1", "", "/work/alpha-app");
        first.session_ids = vec!["rpc-1".into()];
        let mut second = entry("d-2", "Named", "/work/beta-app");
        second.session_ids = vec!["rpc-1".into()];
        let entries = [first, second];
        let err = resolve_target(&entries, "rpc-1", &scoped("/work/alpha-app"))
            .expect_err("ambiguous session id");
        assert_eq!(err.code.as_str(), "ambiguous_target");
        let list = err
            .details
            .as_ref()
            .and_then(|d| d.get("candidates"))
            .and_then(|c| c.as_array())
            .expect("candidates");
        let anon = list
            .iter()
            .find(|c| c["thread"] == "d-1")
            .expect("d-1 candidate");
        assert_eq!(anon["name"], "alpha-app");
        assert_eq!(anon["unnamed"], true);
        let named = list
            .iter()
            .find(|c| c["thread"] == "d-2")
            .expect("d-2 candidate");
        assert_eq!(named["name"], "Named");
        assert!(named.get("unnamed").is_none());
    }
}
