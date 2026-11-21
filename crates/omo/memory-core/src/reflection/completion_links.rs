//! Port of `reflection/completion-links.ts` (omo #12645, #12642): a run that deletes, archives or
//! moves a note must not leave other notes linking to the old path. On the screened run tip, every
//! link to a path the run removed is rewritten in one extra commit, so the fix lands with the run
//! that broke it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::git::exec::GitExec;
use crate::git::repo::{COMMIT_TIMEOUT_MS, NO_AUTO_MAINTENANCE};
use crate::memfs::{describe_frontmatter_violation, is_memory_content_path, split_frontmatter};
use crate::sync::redact::scan_secret_like_material;
use crate::tools::claim_frontmatter::restamp_content;

use super::completion_repair::{Run, STABLE_LOG_OUTPUT, read_dropped_trailers};
use super::link_removals::{list_tree, removed_paths};
use super::link_rewrite::{LinkTargets, posix_basename, repoint_links};

const LINK_REPAIR_SUBJECT: &str = "chore(memory): repoint links to removed notes";

/// The gate's link-repair commit sitting on a run tip, recognised by recomputing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognizedLinkRepair {
    pub tip_sha: String,
    pub run_tip: String,
    pub writes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRepair {
    pub tip_sha: String,
    /// Paths only the repair wrote: notes the run itself never changed.
    pub link_repairs: Vec<String>,
}

/// Whether `tip_sha` is the gate's own link repair of its parent. Nothing the run writes (a
/// trailer, a subject, an author) is trusted: the repair is recomputed from the parent, and the tip
/// counts only when it changes exactly the notes that recomputation rewrites, to exactly that
/// content.
pub fn recognize_link_repair(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    tip_sha: &str,
) -> Result<Option<RecognizedLinkRepair>, String> {
    let run = Run::literal_pathspec(exec, dir);
    let listed = run.must(&["rev-list", "--parents", "-n", "1", tip_sha])?;
    let parents: Vec<&str> = listed.split_whitespace().skip(1).collect();
    if parents.len() != 1 {
        return Ok(None);
    }
    let run_tip = parents[0];
    if run_tip == base_sha {
        return Ok(None);
    }
    if run
        .call(&["merge-base", "--is-ancestor", base_sha, run_tip])?
        .code
        != 0
    {
        return Ok(None);
    }
    let changed_out = run.must(&[
        "diff",
        "--name-status",
        "--no-renames",
        "-z",
        run_tip,
        tip_sha,
        "--",
    ])?;
    let changed: Vec<&str> = changed_out
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .collect();
    let paths: Vec<&str> = changed.iter().skip(1).step_by(2).copied().collect();
    if paths.is_empty() || changed.iter().step_by(2).any(|status| *status != "M") {
        return Ok(None);
    }
    let skip: BTreeSet<String> = read_dropped_trailers(exec, dir, base_sha, run_tip)?
        .into_iter()
        .map(|entry| entry.path)
        .collect();
    let writes = plan_link_repair(&run, base_sha, run_tip, &skip)?;
    if writes.len() != paths.len() {
        return Ok(None);
    }
    for path in paths {
        let Some(planned) = writes.get(path) else {
            return Ok(None);
        };
        if blob(&run, tip_sha, path)? != *planned {
            return Ok(None);
        }
    }
    Ok(Some(RecognizedLinkRepair {
        tip_sha: tip_sha.to_owned(),
        run_tip: run_tip.to_owned(),
        writes,
    }))
}

/// Rewrites, on the screened run tip (HEAD, clean), every link to a path the run removed, in one
/// extra commit. The content is screened before it is written: a note whose rewrite would be
/// refused (secret-like text it already held, a header the screen rejects) is left as it is, so the
/// completion screen never runs on the repair. `prior` is a repair already recognised on this same
/// tip; when the plan is unchanged that commit is restored instead of recreated, so validating
/// twice gives one result.
pub fn apply_link_repair(
    exec: &dyn GitExec,
    dir: &Path,
    base_sha: &str,
    run_tip: &str,
    dropped: &[String],
    prior: Option<&RecognizedLinkRepair>,
) -> Result<Option<LinkRepair>, String> {
    let run = Run::literal_pathspec(exec, dir);
    let skip: BTreeSet<String> = dropped.iter().cloned().collect();
    let writes = plan_link_repair(&run, base_sha, run_tip, &skip)?;
    if writes.is_empty() {
        return Ok(None);
    }
    let tip_sha = match prior.filter(|prior| prior.run_tip == run_tip && prior.writes == writes) {
        Some(prior) => {
            run.must(&["reset", "-q", "--hard", prior.tip_sha.as_str()])?;
            prior.tip_sha.clone()
        }
        None => commit_link_repair(&run, dir, run_tip, &writes)?,
    };
    let range = format!("{base_sha}..{run_tip}");
    let by_run: BTreeSet<String> = run
        .must(&["diff", "--name-only", "-z", range.as_str(), "--"])?
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect();
    Ok(Some(LinkRepair {
        tip_sha,
        link_repairs: writes
            .keys()
            .filter(|path| !by_run.contains(*path))
            .cloned()
            .collect(),
    }))
}

fn commit_link_repair(
    run: &Run<'_>,
    dir: &Path,
    run_tip: &str,
    writes: &BTreeMap<String, String>,
) -> Result<String, String> {
    for (path, content) in writes {
        std::fs::write(dir.join(path), content)
            .map_err(|error| format!("cannot write {path}: {error}"))?;
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(writes.keys().map(String::as_str));
    run.must(&add)?;
    let mut identity_argv: Vec<&str> = STABLE_LOG_OUTPUT.to_vec();
    identity_argv.extend(["log", "-1", "--format=%an%n%ae", run_tip]);
    let identity_out = run.must(&identity_argv)?;
    let mut identity = identity_out.split('\n');
    let name = identity.next().unwrap_or("reflection");
    let email = identity.next().unwrap_or("reflection@omo.local");
    let name_arg = format!("user.name={name}");
    let email_arg = format!("user.email={email}");
    // SAFETY-DIVERGENCE(reflection/completion-links.ts): upstream runs this commit with neither
    // flag; like every other memory commit here (`completion_repair.rs`, `repo.rs`) it gets the
    // memory commit budget and must not end in auto maintenance.
    let mut commit_argv: Vec<&str> = NO_AUTO_MAINTENANCE.to_vec();
    commit_argv.extend([
        "-c",
        name_arg.as_str(),
        "-c",
        email_arg.as_str(),
        "commit",
        "-q",
        "--no-verify",
        "-m",
        LINK_REPAIR_SUBJECT,
    ]);
    run.must_with_timeout(&commit_argv, COMMIT_TIMEOUT_MS)?;
    Ok(run
        .must(&["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_owned())
}

fn plan_link_repair(
    run: &Run<'_>,
    base_sha: &str,
    run_tip: &str,
    skip: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let tree = list_tree(run, run_tip)?;
    let live: BTreeSet<String> = tree.iter().map(|entry| entry.path.clone()).collect();
    let removals = removed_paths(run, base_sha, run_tip, &live)?;
    let mut writes = BTreeMap::new();
    if removals.removed.is_empty() {
        return Ok(writes);
    }
    let notes: BTreeSet<&str> = tree
        .iter()
        .filter(|entry| entry.regular && entry.path.ends_with(".md") && !skip.contains(&entry.path))
        .map(|entry| entry.path.as_str())
        .collect();
    let removed: Vec<String> = removals.removed.keys().cloned().collect();
    let mut candidates: BTreeSet<String> = removals.moved_from.keys().cloned().collect();
    candidates.extend(notes_naming(run, run_tip, &removed)?);
    let targets = LinkTargets {
        live,
        removed: removals.removed,
    };
    for path in candidates
        .iter()
        .filter(|candidate| notes.contains(candidate.as_str()))
    {
        let Some(current) = note_text(run, run_tip, path)? else {
            continue;
        };
        let next = repoint_links(
            path,
            &current,
            &targets,
            removals.moved_from.get(path).map(String::as_str),
        );
        if next == current {
            continue;
        }
        let stamped = restamped(&current, &next);
        if screen_accepts(path, &current, &stamped) {
            writes.insert(path.clone(), stamped);
        }
    }
    Ok(writes)
}

/// Notes whose text holds a removed note's file name; a link to it cannot be written without that
/// name.
fn notes_naming(run: &Run<'_>, revision: &str, removed: &[String]) -> Result<Vec<String>, String> {
    let stems: BTreeSet<&str> = removed
        .iter()
        .map(|path| {
            let base = posix_basename(path);
            base.strip_suffix(".md").unwrap_or(base)
        })
        .filter(|stem| !stem.is_empty())
        .collect();
    if stems.is_empty() {
        return Ok(Vec::new());
    }
    let mut argv: Vec<&str> = vec!["grep", "-l", "-z", "-F"];
    for stem in &stems {
        argv.extend(["-e", *stem]);
    }
    argv.push(revision);
    let result = run.call(&argv)?;
    if result.code == 1 {
        return Ok(Vec::new());
    }
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            "git grep failed".to_owned()
        } else {
            stderr.to_owned()
        });
    }
    let prefix = format!("{revision}:");
    Ok(result
        .stdout
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| entry.strip_prefix(&prefix).unwrap_or(entry).to_owned())
        .collect())
}

fn screen_accepts(path: &str, before: &str, after: &str) -> bool {
    if !scan_secret_like_material(after).is_empty() {
        return false;
    }
    if !is_memory_content_path(path) || describe_frontmatter_violation(after).is_none() {
        return true;
    }
    split_frontmatter(after).map(|(header, _)| header)
        == split_frontmatter(before).map(|(header, _)| header)
}

/// The same provenance rule every writer and the commit hook apply; a note whose header does not
/// parse keeps its bytes.
fn restamped(current: &str, next: &str) -> String {
    if split_frontmatter(current).is_none() {
        return next.to_owned();
    }
    restamp_content(Some(current), next, None).unwrap_or_else(|_| next.to_owned())
}

fn blob(run: &Run<'_>, revision: &str, path: &str) -> Result<String, String> {
    run.must(&["cat-file", "blob", &format!("{revision}:{path}")])
}

/// A note's text, or `None` when it is not valid UTF-8: decoding it would rewrite bytes outside
/// its links.
fn note_text(run: &Run<'_>, revision: &str, path: &str) -> Result<Option<String>, String> {
    let spec = format!("{revision}:{path}");
    let result = run.call_bytes(&["cat-file", "blob", &spec])?;
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(if stderr.is_empty() {
            format!("git cat-file blob {spec} failed")
        } else {
            stderr.to_owned()
        });
    }
    Ok(String::from_utf8(result.stdout).ok())
}

#[cfg(test)]
#[path = "completion_links_tests.rs"]
mod tests;
