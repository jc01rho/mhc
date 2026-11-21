//! Regular-expression search of one file or a directory tree (senpi `files/grep.ts`). The pattern is a
//! Rust `regex` expression, not a JavaScript `RegExp`: look-around and back-references are not available.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::cancel::CancellationToken;
use crate::errors::{MachineOpError, OpErrorCode};
use crate::files::glob::WALK_SKIP;
use crate::types::{GrepMatch, GrepResult};

const DEFAULT_LIMIT: usize = 100;
const MAX_LINE_CHARS: usize = 500;

fn read_limit_error(path: &Path, size: u64, limit: u64) -> MachineOpError {
    MachineOpError::new(
        OpErrorCode::LimitExceeded,
        format!(
            "{} is {size} bytes, above the {limit}-byte read limit",
            path.display()
        ),
    )
}

async fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = pending.pop_front() {
        let Ok(mut read_dir) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(dirent)) = read_dir.next_entry().await {
            let name = dirent.file_name();
            if name.to_str().is_some_and(|text| WALK_SKIP.contains(&text)) {
                continue;
            }
            let Ok(kind) = dirent.file_type().await else {
                continue;
            };
            if kind.is_dir() {
                pending.push_back(dir.join(&name));
            } else if kind.is_file() {
                files.push(dir.join(&name));
            }
        }
    }
    files
}

/// Search `root` (a file, or a tree whose symlinks are never followed) for lines matching `pattern`.
/// Files above `read_max_bytes` are skipped inside a tree and refused when `root` is that file; files
/// holding a NUL byte are skipped as binary.
pub async fn local_grep(
    root: &Path,
    pattern: &str,
    limit: Option<usize>,
    read_max_bytes: u64,
    signal: &CancellationToken,
) -> Result<GrepResult, MachineOpError> {
    let regex = Regex::new(pattern).map_err(|error| {
        MachineOpError::new(
            OpErrorCode::InvalidPattern,
            format!("invalid pattern: {error}"),
        )
    })?;
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);
    if effective_limit == 0 {
        return Err(MachineOpError::new(
            OpErrorCode::InvalidArgument,
            "limit must be a positive integer",
        ));
    }
    let info = tokio::fs::metadata(root).await?;
    let files = if info.is_file() {
        if info.len() > read_max_bytes {
            return Err(read_limit_error(root, info.len(), read_max_bytes));
        }
        vec![root.to_path_buf()]
    } else {
        collect_files(root).await
    };
    let mut matches: Vec<GrepMatch> = Vec::new();
    for file in files {
        if signal.is_cancelled() {
            return Err(MachineOpError::aborted());
        }
        if file != root && tokio::fs::metadata(&file).await?.len() > read_max_bytes {
            continue;
        }
        let content = tokio::fs::read(&file).await?;
        if content.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&content);
        for (index, line) in text.split('\n').enumerate() {
            if !regex.is_match(line) {
                continue;
            }
            if matches.len() >= effective_limit {
                return Ok(GrepResult {
                    matches,
                    truncated: true,
                });
            }
            matches.push(GrepMatch {
                path: file.clone(),
                line: index + 1,
                text: line.chars().take(MAX_LINE_CHARS).collect(),
            });
        }
    }
    Ok(GrepResult {
        matches,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: u64 = 1024;

    #[tokio::test]
    async fn matching_lines_carry_their_one_based_number() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "first\nsecond line\nthird\n").unwrap();
        let result = local_grep(&file, "second", None, LIMIT, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            result.matches,
            vec![GrepMatch {
                path: file,
                line: 2,
                text: "second line".to_owned()
            }]
        );
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn a_tree_search_skips_binary_oversized_and_vendored_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("keep.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("bin.dat"), b"needle\0needle").unwrap();
        std::fs::write(dir.path().join("node_modules/x.txt"), "needle\n").unwrap();
        std::fs::write(dir.path().join("big.txt"), "needle ".repeat(400)).unwrap();
        let result = local_grep(dir.path(), "needle", None, LIMIT, &CancellationToken::new())
            .await
            .unwrap();
        let paths: Vec<_> = result.matches.iter().map(|m| m.path.clone()).collect();
        assert_eq!(paths, vec![dir.path().join("keep.txt")]);
    }

    #[tokio::test]
    async fn limits_and_patterns_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "a\na\na\n").unwrap();
        let signal = CancellationToken::new();
        let truncated = local_grep(&file, "a", Some(2), LIMIT, &signal)
            .await
            .unwrap();
        assert_eq!(truncated.matches.len(), 2);
        assert!(truncated.truncated);
        let zero = local_grep(&file, "a", Some(0), LIMIT, &signal)
            .await
            .unwrap_err();
        assert_eq!(zero.code, OpErrorCode::InvalidArgument);
        let bad = local_grep(&file, "(", None, LIMIT, &signal)
            .await
            .unwrap_err();
        assert_eq!(bad.code, OpErrorCode::InvalidPattern);
    }

    #[tokio::test]
    async fn a_single_file_above_the_read_limit_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.txt");
        std::fs::write(&file, vec![b'a'; 65]).unwrap();
        let error = local_grep(&file, "a", None, 64, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, OpErrorCode::LimitExceeded);
    }

    #[tokio::test]
    async fn long_lines_are_cut_at_the_character_cap() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("long.txt");
        std::fs::write(&file, "\u{e9}".repeat(600)).unwrap();
        let result = local_grep(&file, "\u{e9}", None, 4096, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.matches[0].text.chars().count(), MAX_LINE_CHARS);
    }
}
