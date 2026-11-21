//! Port of `reflection/link-removals.ts` (omo #12645): which paths a run removed and where their
//! links go now.

use std::collections::{BTreeMap, BTreeSet};

use super::completion_repair::Run;
use super::link_rewrite::{ARCHIVE_PATH, Successor, posix_basename};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub regular: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removals {
    pub removed: BTreeMap<String, Successor>,
    /// rename destination -> its source, so a moved note's own relative links are resolved from
    /// where they were written
    pub moved_from: BTreeMap<String, String>,
}

pub(crate) fn list_tree(run: &Run<'_>, revision: &str) -> Result<Vec<TreeEntry>, String> {
    let out = run.must(&["ls-tree", "-r", "-z", "--full-tree", revision])?;
    Ok(out
        .split('\0')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (meta, path) = line.split_once('\t').unwrap_or((line, ""));
            let mode = meta.split(' ').next().unwrap_or_default();
            TreeEntry {
                path: path.to_owned(),
                regular: mode == "100644" || mode == "100755",
            }
        })
        .collect())
}

/// Every path `base..tip` removed, with where its links go: the rename destination, the archive,
/// or nowhere.
pub(crate) fn removed_paths(
    run: &Run<'_>,
    base_sha: &str,
    tip_sha: &str,
    live: &BTreeSet<String>,
) -> Result<Removals, String> {
    let range = format!("{base_sha}..{tip_sha}");
    let out = run.must(&["diff", "--name-status", "-M", "-z", range.as_str(), "--"])?;
    let parts: Vec<&str> = out.split('\0').collect();
    let mut deleted: Vec<String> = Vec::new();
    let mut removals = Removals::default();
    let mut index = 0;
    while index < parts.len() {
        let status = parts[index];
        if status.is_empty() {
            break;
        }
        if status.starts_with('R') || status.starts_with('C') {
            if status.starts_with('R')
                && let (Some(from), Some(to)) = (parts.get(index + 1), parts.get(index + 2))
                && !live.contains(*from)
            {
                removals.removed.insert(
                    (*from).to_owned(),
                    Successor::Moved {
                        path: (*to).to_owned(),
                    },
                );
                removals
                    .moved_from
                    .insert((*to).to_owned(), (*from).to_owned());
            }
            index += 3;
            continue;
        }
        if status == "D"
            && let Some(path) = parts.get(index + 1)
            && !live.contains(*path)
        {
            deleted.push((*path).to_owned());
        }
        index += 2;
    }
    let archived = if live.contains(ARCHIVE_PATH) {
        archive_additions(run, base_sha, tip_sha)?
    } else {
        String::new()
    };
    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for path in live.iter().chain(deleted.iter()) {
        *names.entry(posix_basename(path)).or_default() += 1;
    }
    for path in &deleted {
        let unique = names.get(posix_basename(path)) == Some(&1);
        let successor = if recorded_in(&archived, path, unique) {
            Successor::Archived
        } else {
            Successor::Removed
        };
        removals.removed.insert(path.clone(), successor);
    }
    Ok(removals)
}

fn archive_additions(run: &Run<'_>, base_sha: &str, tip_sha: &str) -> Result<String, String> {
    let out = run.must(&[
        "diff",
        "-U0",
        "--no-color",
        base_sha,
        tip_sha,
        "--",
        ARCHIVE_PATH,
    ])?;
    Ok(out
        .split('\n')
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Whether the lines this run added to `ARCHIVE.md` name `path`: its repository path (with or
/// without `.md`), or its bare file name with `.md` when no other note shares that name. The name
/// must end there, so `x.mdx` or `x.txt` never counts as naming `x.md`.
pub(crate) fn recorded_in(archived: &str, path: &str, unique_name: bool) -> bool {
    let stem = path.strip_suffix(".md").unwrap_or(path);
    if occurs_bounded(archived, stem, true) {
        return true;
    }
    unique_name && path.ends_with(".md") && occurs_bounded(archived, posix_basename(path), false)
}

/// `(^|[^\w./-])<needle>(\.md)?(?![\w/-]|\.\w)` over every start position, where `\w` is ASCII.
fn occurs_bounded(text: &str, needle: &str, optional_md: bool) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(offset) = text[from..].find(needle) {
        let start = from + offset;
        let starts_clean = text[..start]
            .chars()
            .next_back()
            .is_none_or(|previous| !is_word(previous) && !matches!(previous, '.' | '/' | '-'));
        let end = start + needle.len();
        let ends_clean =
            (optional_md && text[end..].starts_with(".md") && ends_cleanly(&text[end + 3..]))
                || ends_cleanly(&text[end..]);
        if starts_clean && ends_clean {
            return true;
        }
        from = start + text[start..].chars().next().map_or(1, char::len_utf8);
    }
    false
}

fn ends_cleanly(rest: &str) -> bool {
    let mut chars = rest.chars();
    match chars.next() {
        None => true,
        Some(next) if is_word(next) || matches!(next, '/' | '-') => false,
        Some('.') => !chars.next().is_some_and(is_word),
        Some(_) => true,
    }
}

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

#[cfg(test)]
#[path = "link_removals_tests.rs"]
mod tests;
