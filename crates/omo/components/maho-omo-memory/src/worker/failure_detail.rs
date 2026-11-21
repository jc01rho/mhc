//! Port of `worker/failure-detail.ts` (`childFailureCause`): reduce a memory child's failure detail
//! to the one line a reader can act on. The stored detail is the tail of the child's stderr, which
//! for a crashed child leads with a code frame, a caret, error property rows and a stack before the
//! sentence that explains the failure. Distillation applies where the detail is shown, never where
//! it is stored.

use std::sync::LazyLock;

use regex::Regex;

/// Bounds one pathological line (a minified frame, a JSON blob); notices excerpt further.
pub const FAILURE_CAUSE_MAX_CHARS: usize = 200;
const ELLIPSIS: char = '…';

static TERMINAL_CONTROL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").expect("valid terminal control pattern")
});
static WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+").expect("valid whitespace pattern"));
static MACHINERY: LazyLock<[Regex; 6]> = LazyLock::new(|| {
    [
        r"^[0-9]+\s*\|",
        r"^\^+$",
        r"^at\s+\S",
        r"(?i)^(?:path|syscall|errno|code|dest|address|port|erroredSysCall)\s*:",
        r"^Bun v[0-9]",
        r"^\[truncated to last [0-9]+ bytes\]$",
    ]
    .map(|pattern| Regex::new(pattern).expect("valid machinery pattern"))
});
/// A line that names the failure: `ENOENT: ...`, `TypeError: ...`, `error: ...`.
static CAUSE_SHAPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:[A-Z][A-Za-z]*(?:Error|Exception)|E[A-Z]{2,}|[Ee]rror|[Ww]arning|[Ff]atal)\b")
        .expect("valid cause pattern")
});

pub fn child_failure_cause(detail: Option<&str>) -> Option<String> {
    let lines: Vec<String> = detail?
        .split('\n')
        .map(|line| {
            let plain = TERMINAL_CONTROL.replace_all(line, "");
            WHITESPACE.replace_all(&plain, " ").trim().to_string()
        })
        .filter(|line| !line.is_empty() && !MACHINERY.iter().any(|pattern| pattern.is_match(line)))
        .collect();
    let cause = lines
        .iter()
        .find(|line| CAUSE_SHAPED.is_match(line))
        .or_else(|| lines.first())?;
    if cause.chars().count() <= FAILURE_CAUSE_MAX_CHARS {
        return Some(cause.clone());
    }
    let mut bounded: String = cause.chars().take(FAILURE_CAUSE_MAX_CHARS - 1).collect();
    bounded.push(ELLIPSIS);
    Some(bounded)
}

#[cfg(test)]
#[path = "failure_detail_tests.rs"]
mod tests;
