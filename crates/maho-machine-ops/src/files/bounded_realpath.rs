//! Canonical-path resolution that is correct first and bounded second (senpi `files/bounded-realpath.ts`).
//!
//! A canonical path handed to a containment decision or used as a same-file identity key must come from
//! real `canonicalize`: only the kernel knows the on-disk spelling of a case-insensitive name and applies
//! `..` the way the following I/O will. It can also never return on a wedged mount, so the resolution is
//! raced against a deadline; the blocking call itself cannot be cancelled and finishes in the background.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::timeout;
use unicode_normalization::UnicodeNormalization;

use crate::node_path::strip_verbatim_prefix;

/// How long a canonical resolution may take before the open-free walker answers instead.
pub const RESOLUTION_DEADLINE_MS: u64 = 2000;

pub const RESOLUTION_DEADLINE: Duration = Duration::from_millis(RESOLUTION_DEADLINE_MS);

/// Returned by [`with_resolution_deadline`] when the deadline won (senpi `RESOLUTION_TIMED_OUT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionTimedOut;

/// `ENOENT` and `ENOTDIR`: the component does not exist, which resolution tolerates.
pub fn is_missing_path_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// Race `operation` against `deadline`.
pub async fn with_resolution_deadline<T>(
    operation: impl Future<Output = T>,
    deadline: Duration,
) -> Result<T, ResolutionTimedOut> {
    timeout(deadline, operation)
        .await
        .map_err(|_| ResolutionTimedOut)
}

/// The kernel's canonical path, in the plain form on Windows (no `\\?\` prefix).
pub(crate) async fn canonicalize_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = tokio::fs::canonicalize(path).await?;
    Ok(if cfg!(windows) {
        strip_verbatim_prefix(&canonical)
    } else {
        canonical
    })
}

/// Fold a path into a same-file identity key on filesystems that ignore case. Used only for
/// serialization keys, never for a path handed back to the filesystem or to a policy: folding can only
/// make two distinct files share a key, while failing to fold lets two spellings of one file run at once.
pub fn fold_path_for_case_insensitive_filesystem(file_path: &str) -> String {
    fold_path(file_path, cfg!(target_os = "linux"))
}

fn fold_path(file_path: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        file_path.to_owned()
    } else {
        file_path.nfc().collect::<String>().to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_means_enoent_or_enotdir() {
        assert!(is_missing_path_error(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        assert!(is_missing_path_error(&io::Error::from(
            io::ErrorKind::NotADirectory
        )));
        assert!(!is_missing_path_error(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn a_case_sensitive_filesystem_keeps_the_spelling() {
        assert_eq!(fold_path("/Tmp/Notes.TXT", true), "/Tmp/Notes.TXT");
    }

    #[test]
    fn a_case_insensitive_filesystem_folds_case_and_unicode_normalization() {
        assert_eq!(fold_path("/Tmp/Notes.TXT", false), "/tmp/notes.txt");
        let decomposed = "/tmp/e\u{301}.txt";
        let composed = "/tmp/\u{e9}.txt";
        assert_eq!(fold_path(decomposed, false), fold_path(composed, false));
        assert_ne!(fold_path(decomposed, true), fold_path(composed, true));
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_wins_over_an_operation_that_never_returns() {
        let outcome =
            with_resolution_deadline(std::future::pending::<()>(), RESOLUTION_DEADLINE).await;
        assert_eq!(outcome, Err(ResolutionTimedOut));
    }

    #[tokio::test(start_paused = true)]
    async fn an_operation_inside_the_deadline_keeps_its_answer() {
        let outcome = with_resolution_deadline(async { 7 }, RESOLUTION_DEADLINE).await;
        assert_eq!(outcome, Ok(7));
    }
}
