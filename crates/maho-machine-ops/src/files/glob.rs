//! Glob matching and the built-in walker (senpi `files/glob.ts`). senpi shells out to `fd` when it is on
//! `PATH` and walks in-process otherwise; this crate only has the walker, so results never depend on an
//! `fd` binary and `.gitignore` is not read.

use std::collections::VecDeque;
use std::path::Path;

use regex::Regex;

use crate::cancel::CancellationToken;
use crate::errors::{MachineOpError, OpErrorCode};
use crate::types::{GlobEntry, GlobResult};

const DEFAULT_LIMIT: usize = 1000;
pub(crate) const WALK_SKIP: [&str; 2] = [".git", "node_modules"];

fn push_class(source: &mut String, body: &[char]) {
    source.push('[');
    for (position, ch) in body.iter().enumerate() {
        match ch {
            '!' if position == 0 => source.push('^'),
            '\\' | '[' | '&' | '~' => {
                source.push('\\');
                source.push(*ch);
            }
            other => source.push(*other),
        }
    }
    source.push(']');
}

/// Glob syntax (`**`, `*`, `?`, `{a,b}`, `[...]`) to a whole-string regular expression over
/// `/`-separated paths. A pattern whose brace or class cannot compile is `invalid_pattern`.
pub fn glob_to_regex(glob: &str) -> Result<Regex, MachineOpError> {
    let chars: Vec<char> = glob.chars().collect();
    let mut source = String::new();
    let mut brace_depth = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        match ch {
            '*' => {
                if chars.get(index + 1) == Some(&'*') {
                    let slash_follows = chars.get(index + 2) == Some(&'/');
                    source.push_str(if slash_follows { "(?:.*/)?" } else { ".*" });
                    index += if slash_follows { 2 } else { 1 };
                } else {
                    source.push_str("[^/]*");
                }
            }
            '?' => source.push_str("[^/]"),
            '{' => {
                brace_depth += 1;
                source.push_str("(?:");
            }
            '}' if brace_depth > 0 => {
                brace_depth -= 1;
                source.push(')');
            }
            ',' if brace_depth > 0 => source.push('|'),
            '[' => match chars[index + 1..].iter().position(|c| *c == ']') {
                None => source.push_str("\\["),
                Some(offset) => {
                    let close = index + 1 + offset;
                    push_class(&mut source, &chars[index + 1..close]);
                    index = close;
                }
            },
            other => {
                let mut buffer = [0u8; 4];
                source.push_str(&regex::escape(other.encode_utf8(&mut buffer)));
            }
        }
        index += 1;
    }
    Regex::new(&format!("^{source}$")).map_err(|error| {
        MachineOpError::new(
            OpErrorCode::InvalidPattern,
            format!("invalid glob pattern: {error}"),
        )
    })
}

fn relative_slash_path(base: &Path, full: &Path) -> String {
    let relative = full.strip_prefix(base).unwrap_or(full);
    relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Walk `search_path` breadth first and collect entries whose name (pattern without `/`) or
/// `/`-separated relative path matches `pattern`. Skips `.git` and `node_modules`, does not follow
/// symlinked directories, and stops at `limit` (default 1000).
pub async fn local_glob(
    pattern: &str,
    search_path: &Path,
    limit: Option<usize>,
    signal: &CancellationToken,
) -> Result<GlobResult, MachineOpError> {
    let effective_limit = limit.unwrap_or(DEFAULT_LIMIT);
    let matcher = glob_to_regex(pattern.strip_prefix("./").unwrap_or(pattern))?;
    let match_basename = !pattern.contains('/');
    let mut entries: Vec<GlobEntry> = Vec::new();
    let mut pending = VecDeque::from([search_path.to_path_buf()]);
    tokio::fs::metadata(search_path).await?;
    'walk: while entries.len() < effective_limit {
        let Some(dir) = pending.pop_front() else {
            break;
        };
        if signal.is_cancelled() {
            return Err(MachineOpError::aborted());
        }
        let Ok(mut read_dir) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(dirent)) = read_dir.next_entry().await {
            let name = dirent.file_name();
            if name.to_str().is_some_and(|text| WALK_SKIP.contains(&text)) {
                continue;
            }
            let full = dir.join(&name);
            let candidate = if match_basename {
                name.to_string_lossy().into_owned()
            } else {
                relative_slash_path(search_path, &full)
            };
            if matcher.is_match(&candidate) {
                entries.push(GlobEntry { path: full.clone() });
                if entries.len() >= effective_limit {
                    break 'walk;
                }
            }
            if dirent.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                pending.push_back(full);
            }
        }
    }
    let truncated = entries.len() >= effective_limit;
    Ok(GlobResult { entries, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(glob: &str, text: &str) -> bool {
        glob_to_regex(glob).unwrap().is_match(text)
    }

    #[test]
    fn star_stays_inside_one_segment_and_double_star_crosses_them() {
        assert!(matches("*.ts", "a.ts"));
        assert!(!matches("*.ts", "dir/a.ts"));
        assert!(matches("**/*.ts", "a.ts"));
        assert!(matches("**/*.ts", "x/y/a.ts"));
        assert!(matches("src/**", "src/a/b.rs"));
        assert!(matches("src/**/mod.rs", "src/mod.rs"));
        assert!(matches("src/**/mod.rs", "src/a/b/mod.rs"));
    }

    #[test]
    fn question_mark_is_one_non_separator_character() {
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "a/c"));
        assert!(!matches("a?c", "ac"));
    }

    #[test]
    fn braces_alternate_and_may_nest() {
        assert!(matches("*.{ts,tsx}", "a.tsx"));
        assert!(matches("*.{ts,tsx}", "a.ts"));
        assert!(!matches("*.{ts,tsx}", "a.js"));
        assert!(matches("{a,b{1,2}}.x", "b2.x"));
    }

    #[test]
    fn character_classes_support_ranges_and_negation() {
        assert!(matches("[a-c]x", "bx"));
        assert!(!matches("[a-c]x", "dx"));
        assert!(matches("[!a-c]x", "dx"));
        assert!(!matches("[!a-c]x", "ax"));
    }

    #[test]
    fn regex_metacharacters_in_a_glob_are_literal() {
        assert!(matches("a.b", "a.b"));
        assert!(!matches("a.b", "axb"));
        assert!(matches("a+b(1)|c", "a+b(1)|c"));
        assert!(matches("x}", "x}"));
        assert!(matches("a,b", "a,b"));
        assert!(matches("[", "["));
    }

    #[test]
    fn matching_is_whole_string() {
        assert!(!matches("a", "ab"));
        assert!(!matches("a", "ba"));
        assert!(!matches("b", "a\nb"));
    }

    #[test]
    fn a_brace_that_never_closes_is_an_invalid_pattern() {
        let error = glob_to_regex("{a,b").unwrap_err();
        assert_eq!(error.code, OpErrorCode::InvalidPattern);
    }

    #[tokio::test]
    async fn the_walker_matches_basenames_without_a_slash_and_paths_with_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join("a.rs"), "a").unwrap();
        std::fs::write(dir.path().join("src/b.rs"), "b").unwrap();
        std::fs::write(dir.path().join("src/deep/c.rs"), "c").unwrap();
        std::fs::write(dir.path().join("node_modules/pkg/d.rs"), "d").unwrap();
        let signal = CancellationToken::new();

        let mut by_name = local_glob("*.rs", dir.path(), None, &signal)
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();
        by_name.sort();
        assert_eq!(
            by_name,
            vec![
                dir.path().join("a.rs"),
                dir.path().join("src/b.rs"),
                dir.path().join("src/deep/c.rs"),
            ]
        );

        let by_path = local_glob("src/*.rs", dir.path(), None, &signal)
            .await
            .unwrap();
        assert_eq!(
            by_path
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            vec![dir.path().join("src/b.rs")]
        );
    }

    #[tokio::test]
    async fn the_walker_truncates_at_the_limit_and_reports_a_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(dir.path().join(name), name).unwrap();
        }
        let signal = CancellationToken::new();
        let limited = local_glob("*.txt", dir.path(), Some(2), &signal)
            .await
            .unwrap();
        assert_eq!(limited.entries.len(), 2);
        assert!(limited.truncated);
        let missing = local_glob("*", &dir.path().join("nope"), None, &signal)
            .await
            .unwrap_err();
        assert_eq!(missing.code, OpErrorCode::NotFound);
    }

    #[tokio::test]
    async fn a_cancelled_signal_aborts_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let signal = CancellationToken::new();
        signal.cancel();
        let error = local_glob("*", dir.path(), None, &signal)
            .await
            .unwrap_err();
        assert_eq!(error.code, OpErrorCode::Aborted);
    }
}
