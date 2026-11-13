//! Port of omo-cpa `src/tier-types.ts`. Shared vocabulary for CPA tiering, owned by no single
//! module so the classifier, the endpoint router and the chain generator stay independent.

/// Which pool a model belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Primary,
    Last,
}

impl Tier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Last => "last",
        }
    }
}

/// The seven families the user designated as primary workhorses.
pub const PRIMARY_FAMILIES: &[&str] =
    &["muse", "gpt", "claude", "gemini", "glm", "deepseek", "grok"];

/// CPA's three wire formats. One catalog, three shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Openai,
    Anthropic,
    Gemini,
}

/// Which list supplied `inputModalities`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModalitySource {
    Gemini,
    Codex,
}

/// One model as CPA describes it, after merging the three list formats.
///
/// Every enrichment field is nullable: CPA omits them for part of the catalog and a missing value
/// must stay missing rather than become a guess.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub owned_by: Option<String>,
    /// CPA's human-readable name. Often the ONLY reliable identity signal.
    pub display_name: Option<String>,
    pub context_length: Option<u64>,
    pub max_tokens: Option<u64>,
    pub input_modalities: Option<Vec<String>>,
    pub input_modalities_source: Option<ModalitySource>,
    pub output_modalities: Option<Vec<String>>,
    pub thinking: Option<bool>,
    /// Set only for a declared alias: the classifier judges the upstream id instead.
    pub upstream_model_id: Option<String>,
}

impl CatalogModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            owned_by: None,
            display_name: None,
            context_length: None,
            max_tokens: None,
            input_modalities: None,
            input_modalities_source: None,
            output_modalities: None,
            thinking: None,
            upstream_model_id: None,
        }
    }
}

/// Why a model landed in its tier. Shown to the user, so it must be specific.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierDecision {
    pub id: String,
    pub tier: Tier,
    /// Matched primary family, when one was matched.
    pub family: Option<String>,
    /// Human-readable justification, e.g. "family claude via displayName".
    pub reason: String,
    /// True when a user override decided this, not the automatic rule.
    pub overridden: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_families_are_the_seven_workhorses() {
        assert_eq!(
            PRIMARY_FAMILIES,
            &["muse", "gpt", "claude", "gemini", "glm", "deepseek", "grok"]
        );
    }

    #[test]
    fn tier_names_match_the_ts_spelling() {
        assert_eq!(Tier::Primary.as_str(), "primary");
        assert_eq!(Tier::Last.as_str(), "last");
    }
}
