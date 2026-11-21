//! The one name comparison for threads and devices.
//!
//! Upstream counterpart: `name.ts`.

use unicode_normalization::UnicodeNormalization;

/// NFKC, trim, lowercase, whitespace collapsed, so a composed and a decomposed spelling of the same name are one name.
pub fn normalize_name(value: &str) -> String {
    let folded: String = value.nfkc().collect();
    folded
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
