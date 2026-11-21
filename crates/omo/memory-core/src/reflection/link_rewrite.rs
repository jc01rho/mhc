//! Port of `reflection/link-rewrite.ts` (omo #12645, #12642): rewrites the body links of a note
//! that would dangle after a run removed, archived or moved their target.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::memfs::split_frontmatter;

pub const ARCHIVE_PATH: &str = "ARCHIVE.md";

/// Where a link to a path the run removed points now: the rename destination, the root archive,
/// or nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Successor {
    Moved { path: String },
    Archived,
    Removed,
}

#[derive(Debug, Clone, Default)]
pub struct LinkTargets {
    pub live: BTreeSet<String>,
    pub removed: BTreeMap<String, Successor>,
}

static FENCE_MARKER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("^ {0,3}(`{3,}|~{3,})([^\\r\\u{2028}\\u{2029}]*)$").expect("valid fence pattern")
});
static WIKI_LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]\n]+)\]\]").expect("valid wiki link pattern"));
static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[([^\]\n]*)\]\((<?[^)\n]+>?)\)").expect("valid markdown link pattern")
});
static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("valid scheme pattern"));

/// Rewrites the body links of the note at `path` that would dangle after the run: links to a removed
/// path go to its successor, and when the note itself was moved from `moved_from`, its relative
/// markdown links are re-resolved from the old folder so they keep their targets. Fenced and inline
/// code is quoted example text and stays as written.
pub fn repoint_links(
    path: &str,
    content: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let body_start = split_frontmatter(content).map_or(0, |(_, body)| content.len() - body.len());
    let mut fence: Option<(char, usize)> = None;
    let body = content[body_start..]
        .split('\n')
        .map(|line| {
            let marker = FENCE_MARKER.captures(line);
            if let Some((fence_char, fence_length)) = fence {
                if let Some(marker) = &marker {
                    let run = &marker[1];
                    if run.starts_with(fence_char)
                        && run.len() >= fence_length
                        && marker[2].trim().is_empty()
                    {
                        fence = None;
                    }
                }
                return line.to_owned();
            }
            if let Some(marker) = &marker {
                let run = &marker[1];
                fence = run.chars().next().map(|first| (first, run.len()));
                return line.to_owned();
            }
            let mut out = String::new();
            let mut last = 0;
            for (start, end) in inline_code_spans(line) {
                out.push_str(&repoint_segment(
                    path,
                    &line[last..start],
                    targets,
                    moved_from,
                ));
                out.push_str(&line[start..end]);
                last = end;
            }
            out.push_str(&repoint_segment(path, &line[last..], targets, moved_from));
            out
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}{body}", &content[..body_start])
}

/// The spans `/(`+)[^`]*?\1/g` matches, with the same backtracking: a backtick run of length `r`
/// closes at the first later backtick when that run holds at least `r` backticks; otherwise a run of
/// two or more pairs with itself (`r / 2` backticks each side), and a lone backtick matches nothing.
fn inline_code_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        if bytes[position] != b'`' {
            position += 1;
            continue;
        }
        let run = bytes[position..]
            .iter()
            .take_while(|byte| **byte == b'`')
            .count();
        let after_run = position + run;
        let closing = bytes[after_run..]
            .iter()
            .position(|byte| *byte == b'`')
            .map(|offset| after_run + offset);
        if let Some(close) = closing
            && bytes[close..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count()
                >= run
        {
            spans.push((position, close + run));
            position = close + run;
        } else if run >= 2 {
            let end = position + 2 * (run / 2);
            spans.push((position, end));
            position = end;
        } else {
            position += 1;
        }
    }
    spans
}

fn repoint_segment(
    path: &str,
    text: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let wiki = WIKI_LINK.replace_all(text, |captures: &regex::Captures<'_>| {
        repoint_wiki(&captures[0], &captures[1], targets)
    });
    let mut out = String::new();
    let mut last = 0;
    for found in MARKDOWN_LINK.captures_iter(&wiki) {
        let whole = found.get(0).expect("group 0 always matches");
        if wiki[..whole.start()].ends_with('!') {
            continue;
        }
        out.push_str(&wiki[last..whole.start()]);
        out.push_str(&repoint_markdown(
            path,
            whole.as_str(),
            &found[1],
            &found[2],
            targets,
            moved_from,
        ));
        last = whole.end();
    }
    out.push_str(&wiki[last..]);
    out
}

fn repoint_wiki(whole: &str, inner: &str, targets: &LinkTargets) -> String {
    let (target_part, alias) = match inner.split_once('|') {
        Some((target_part, alias)) => (target_part, Some(alias)),
        None => (inner, None),
    };
    let (raw_target, anchor) = match target_part.split_once('#') {
        Some((raw_target, anchor)) => (raw_target, format!("#{anchor}")),
        None => (target_part, String::new()),
    };
    let target = raw_target.trim();
    if !is_repo_relative(target) {
        return whole.to_owned();
    }
    let resolved = posix_normalize(target);
    if escapes(&resolved) || resolves(&resolved, &targets.live, true) {
        return whole.to_owned();
    }
    let exact = targets.removed.get(&resolved);
    let Some(successor) = exact.or_else(|| targets.removed.get(&format!("{resolved}.md"))) else {
        return whole.to_owned();
    };
    let suffix = alias.map(|alias| format!("|{alias}")).unwrap_or_default();
    match successor {
        Successor::Moved { path } => {
            let destination = if exact.is_none() {
                path.strip_suffix(".md").unwrap_or(path)
            } else {
                path.as_str()
            };
            format!("[[{destination}{anchor}{suffix}]]")
        }
        Successor::Archived => format!("[[{ARCHIVE_PATH}|{} (archived)]]", alias.unwrap_or(target)),
        Successor::Removed => alias.unwrap_or(target).to_owned(),
    }
}

fn repoint_markdown(
    path: &str,
    whole: &str,
    label: &str,
    raw: &str,
    targets: &LinkTargets,
    moved_from: Option<&str>,
) -> String {
    let destination = markdown_destination(raw);
    let (destination_path, anchor) = match destination.split_once('#') {
        Some((destination_path, anchor)) => (destination_path, format!("#{anchor}")),
        None => (destination, String::new()),
    };
    if !is_repo_relative(destination_path) {
        return whole.to_owned();
    }
    // The link's own target is where it resolved when written: from the note's old folder when the
    // note moved.
    let original = posix_normalize(&posix_join(
        &posix_dirname(moved_from.unwrap_or(path)),
        destination_path,
    ));
    if escapes(&original) {
        return whole.to_owned();
    }
    let successor = targets.removed.get(&original);
    if successor == Some(&Successor::Removed) {
        return label.to_owned();
    }
    let next_target = match successor {
        None => resolves(&original, &targets.live, false).then(|| original.clone()),
        Some(Successor::Moved { path }) => Some(path.clone()),
        Some(Successor::Archived | Successor::Removed) => Some(ARCHIVE_PATH.to_owned()),
    };
    let Some(next_target) = next_target else {
        return whole.to_owned();
    };
    if next_target == posix_normalize(&posix_join(&posix_dirname(path), destination_path)) {
        return whole.to_owned();
    }
    let relative = posix_relative(&posix_dirname(path), &next_target);
    let next = if successor == Some(&Successor::Archived) {
        relative
    } else {
        format!("{relative}{anchor}")
    };
    let replaced = if next.chars().any(char::is_whitespace) && !raw.trim().starts_with('<') {
        format!("<{next}>")
    } else {
        next
    };
    format!("[{label}]({})", raw.replacen(destination, &replaced, 1))
}

fn is_repo_relative(target: &str) -> bool {
    !target.is_empty() && !URL_SCHEME.is_match(target) && !target.starts_with('/')
}

fn escapes(resolved: &str) -> bool {
    resolved == ".." || resolved.starts_with("../")
}

/// The audit's resolution rule: an existing file, a wiki link's implied `.md`, or a directory prefix.
fn resolves(resolved: &str, live: &BTreeSet<String>, wiki: bool) -> bool {
    if live.contains(resolved) || (wiki && live.contains(&format!("{resolved}.md"))) {
        return true;
    }
    let prefix = if resolved.ends_with('/') {
        resolved.to_owned()
    } else {
        format!("{resolved}/")
    };
    live.iter().any(|file| file.starts_with(&prefix))
}

/// The destination of a markdown link: `<a b.md>` keeps its spaces, otherwise a space ends it (a
/// title follows).
fn markdown_destination(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('<') {
        return rest.find('>').map_or(rest, |end| &rest[..end]);
    }
    trimmed.split_whitespace().next().unwrap_or_default()
}

/// `path.posix.normalize`.
pub(crate) fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if !absolute {
                    segments.push("..");
                }
            }
            other => segments.push(other),
        }
    }
    let joined = segments.join("/");
    if joined.is_empty() {
        return if absolute {
            "/".to_owned()
        } else if trailing {
            "./".to_owned()
        } else {
            ".".to_owned()
        };
    }
    let joined = if trailing {
        format!("{joined}/")
    } else {
        joined
    };
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `path.posix.join` of two segments.
pub(crate) fn posix_join(base: &str, tail: &str) -> String {
    match (base.is_empty(), tail.is_empty()) {
        (true, true) => ".".to_owned(),
        (true, false) => posix_normalize(tail),
        (false, true) => posix_normalize(base),
        (false, false) => posix_normalize(&format!("{base}/{tail}")),
    }
}

/// `path.posix.dirname`.
pub(crate) fn posix_dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".to_owned();
    }
    let rooted = bytes[0] == b'/';
    let mut end = None;
    let mut matched_slash = true;
    for index in (1..bytes.len()).rev() {
        if bytes[index] == b'/' {
            if !matched_slash {
                end = Some(index);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        None if rooted => "/".to_owned(),
        None => ".".to_owned(),
        Some(1) if rooted => "//".to_owned(),
        Some(end) => path[..end].to_owned(),
    }
}

/// `path.posix.basename`.
pub(crate) fn posix_basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or(trimmed)
}

/// `path.posix.relative` between two repository-relative paths.
pub(crate) fn posix_relative(from: &str, to: &str) -> String {
    let from = posix_normalize(from);
    let to = posix_normalize(to);
    let parts = |path: &str| -> Vec<String> {
        path.split('/')
            .filter(|segment| !segment.is_empty() && *segment != ".")
            .map(str::to_owned)
            .collect()
    };
    let from = parts(&from);
    let to = parts(&to);
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative: Vec<&str> = vec![".."; from.len() - common];
    relative.extend(to[common..].iter().map(String::as_str));
    relative.join("/")
}

#[cfg(test)]
#[path = "link_rewrite_tests.rs"]
mod tests;
