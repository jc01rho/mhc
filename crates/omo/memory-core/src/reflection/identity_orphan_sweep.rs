//! Port of `reflection/identity-orphan-sweep.ts` (omo #12517): the orphan worktree/branch sweep for
//! one identity from outside a session (`doctor --fix`, DESIGN.r2 6b). It needs the identity's git
//! repo; `dry_run` lists what would go and why the rest stays.

use std::collections::BTreeSet;
use std::path::Path;

use crate::git::GitMemoryRepo;

use super::orphan_sweep::{
    REFLECTION_ORPHAN_GRACE_MS, ReflectionOrphanSelection, ReflectionOrphanSweepOptions,
    list_reflection_leftovers, select_reflection_orphans, sweep_reflection_orphans,
};

const TERMINAL_SENTINELS: [&str; 3] = ["final.json", "abandoned.json", "quarantined.json"];

fn reserved_run_id(path: &Path) -> Option<String> {
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    parsed.get("runId")?.as_str().map(str::to_string)
}

/// The runs a worktree may still belong to, read the way session reconcile reads them: the reserved
/// runs, plus every run dir with a ledger or prelaunch file and no terminal sentinel (a quarantined
/// run is terminal, REVIEW-1 M2).
fn collect_reflection_live_run_ids(reflection_dir: &Path) -> BTreeSet<String> {
    let mut live = BTreeSet::new();
    for name in ["active.lock", "pending.json"] {
        if let Some(run_id) = reserved_run_id(&reflection_dir.join(name)) {
            live.insert(run_id);
        }
    }
    let runs_dir = reflection_dir.join("runs");
    let Ok(entries) = std::fs::read_dir(&runs_dir) else {
        return live;
    };
    for entry in entries.filter_map(Result::ok) {
        let run_id = entry.file_name().to_string_lossy().into_owned();
        if run_id.starts_with('.') {
            continue;
        }
        let run_dir = runs_dir.join(&run_id);
        if TERMINAL_SENTINELS
            .iter()
            .any(|file| run_dir.join(file).exists())
        {
            continue;
        }
        if run_dir.join("ledger.json").exists() || run_dir.join("prelaunch.json").exists() {
            live.insert(run_id);
        }
    }
    live
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityOrphanKept {
    pub target: String,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityOrphanFailed {
    pub target: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentityOrphanSweepReport {
    /// `Some("no-repo")` when the identity has no git repo to sweep.
    pub skipped: Option<&'static str>,
    pub removed: Vec<String>,
    pub kept: Vec<IdentityOrphanKept>,
    pub failed: Vec<IdentityOrphanFailed>,
}

pub struct IdentityOrphanSweepOptions {
    pub now_ms: f64,
    pub dry_run: bool,
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn sweep_identity_orphan_worktrees(
    identity_root: &Path,
    options: &IdentityOrphanSweepOptions,
) -> Result<IdentityOrphanSweepReport, String> {
    let repo_dir = identity_root.join("repo");
    if !repo_dir.join(".git").exists() {
        return Ok(IdentityOrphanSweepReport {
            skipped: Some("no-repo"),
            ..Default::default()
        });
    }
    let runtime_dir = identity_root.join("runtime");
    let worktrees_dir = runtime_dir.join("worktrees");
    let agent_id = identity_root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let repo = GitMemoryRepo::open(&repo_dir, agent_id).map_err(|error| error.to_string())?;
    let live_run_ids = collect_reflection_live_run_ids(&runtime_dir.join("reflection"));
    let selection = ReflectionOrphanSelection {
        live_run_ids: live_run_ids.clone(),
        now_ms: options.now_ms,
        grace_ms: Some(REFLECTION_ORPHAN_GRACE_MS),
    };
    let exec = repo.exec();
    let leftovers = list_reflection_leftovers(&repo, &worktrees_dir, exec.as_ref())?;
    let orphans = select_reflection_orphans(&leftovers, &selection);
    let orphan_targets: BTreeSet<String> = orphans
        .registered_worktrees
        .iter()
        .map(|worktree| text(&worktree.dir))
        .chain(orphans.stray_dirs.iter().map(|dir| text(dir)))
        .chain(orphans.branches.iter().cloned())
        .collect();
    let kept_reason = |target: &str| -> &'static str {
        let name = target.rsplit(['/', '\\']).next().unwrap_or(target);
        if live_run_ids
            .iter()
            .any(|run_id| name.ends_with(run_id.as_str()))
        {
            "live"
        } else {
            "grace"
        }
    };
    let kept: Vec<IdentityOrphanKept> = leftovers
        .registered_worktrees
        .iter()
        .map(|worktree| text(&worktree.dir))
        .chain(leftovers.stray_dirs.iter().map(|dir| text(dir)))
        .chain(leftovers.branches.iter().cloned())
        .filter(|target| !orphan_targets.contains(target))
        .map(|target| IdentityOrphanKept {
            reason: kept_reason(&target),
            target,
        })
        .collect();
    if options.dry_run {
        return Ok(IdentityOrphanSweepReport {
            skipped: None,
            removed: orphan_targets.into_iter().collect(),
            kept,
            failed: Vec::new(),
        });
    }
    let receipts = sweep_reflection_orphans(
        &repo,
        &worktrees_dir,
        &ReflectionOrphanSweepOptions {
            live_run_ids,
            now_ms: options.now_ms,
            grace_ms: Some(REFLECTION_ORPHAN_GRACE_MS),
            exec: None,
        },
    )?;
    Ok(IdentityOrphanSweepReport {
        skipped: None,
        removed: receipts
            .iter()
            .filter(|receipt| receipt.removed)
            .map(|receipt| receipt.target.clone())
            .collect(),
        kept,
        failed: receipts
            .iter()
            .filter(|receipt| !receipt.removed)
            .map(|receipt| IdentityOrphanFailed {
                target: receipt.target.clone(),
                detail: receipt
                    .detail
                    .clone()
                    .unwrap_or_else(|| "not removed".to_string()),
            })
            .collect(),
    })
}

#[cfg(test)]
#[path = "identity_orphan_sweep_tests.rs"]
mod tests;
