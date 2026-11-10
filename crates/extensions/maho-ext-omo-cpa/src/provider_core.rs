//! Port of omo-cpa `src/provider-core.ts`. Pure functions for CPA provider metadata processing:
//! no side effects, no network calls.

pub const ANTHROPIC_BUG_PREFIX: &str = "claude-fable-5-dd-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmangleResult {
    pub unmangled: String,
    pub mangled: String,
    pub transformed: bool,
}

/// Reverse the CPA Anthropic-format id mangling.
///
/// Every id that arrives from `GET /v1/models` with `anthropic-version: 2023-06-01` carrying the
/// prefix `claude-fable-5-dd-` has its remainder split on `/`, each segment's characters reversed,
/// then the segment order reversed. The result is a canonical id present in the `/v1/models` set.
///
/// Defensive rules: prefix absent returns the id unchanged with `transformed=false`; an empty
/// remainder returns the prefix without its trailing dash with `transformed=true`.
pub fn unmangle_anthropic_id(mangled: &str) -> UnmangleResult {
    let Some(rest) = mangled.strip_prefix(ANTHROPIC_BUG_PREFIX) else {
        return UnmangleResult {
            unmangled: mangled.to_owned(),
            mangled: mangled.to_owned(),
            transformed: false,
        };
    };
    if rest.is_empty() {
        return UnmangleResult {
            unmangled: ANTHROPIC_BUG_PREFIX.trim_end_matches('-').to_owned(),
            mangled: mangled.to_owned(),
            transformed: true,
        };
    }
    let segments: Vec<String> = rest
        .split('/')
        .map(|segment| segment.chars().rev().collect())
        .collect();
    let unmangled = segments
        .iter()
        .rev()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("/");
    UnmangleResult {
        unmangled,
        mangled: mangled.to_owned(),
        transformed: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverses_the_measured_server_mangling() {
        let result = unmangle_anthropic_id("claude-fable-5-dd-0.2-noia/sbal-noia");
        assert_eq!(result.unmangled, "aion-labs/aion-2.0");
        assert_eq!(result.mangled, "claude-fable-5-dd-0.2-noia/sbal-noia");
        assert!(result.transformed);
    }

    #[test]
    fn unprefixed_ids_pass_through() {
        let result = unmangle_anthropic_id("claude-opus-5");
        assert_eq!(result.unmangled, "claude-opus-5");
        assert!(!result.transformed);
    }

    #[test]
    fn empty_remainder_yields_the_bare_prefix() {
        let result = unmangle_anthropic_id(ANTHROPIC_BUG_PREFIX);
        assert_eq!(result.unmangled, "claude-fable-5-dd");
        assert!(result.transformed);
    }
}
