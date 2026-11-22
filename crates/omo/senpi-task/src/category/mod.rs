//! Builtin delegation categories and their model resolution (`category/` in TypeScript).

pub mod architect_edits;
mod available_names;
mod builtins;
mod chain_diagnostics;
mod coverage;
mod fallback_chains;
mod prompts;
mod resolver;
mod user_candidates;

pub use available_names::resolve_available_category_names;
pub use builtins::{
    BUILTIN_CATEGORY_DEFAULTS, BuiltinCategoryConfig, BuiltinCategoryDefinition, builtin_category,
    builtin_category_requires_model, category_caller_guidance, category_gate_models,
    is_category_chain_rung_resolvable, is_category_chain_viable, is_category_gate_satisfied,
    is_gpt6_model, resolve_deep_high_category_prompt_append,
    resolve_deep_low_category_prompt_append, resolve_ultrabrain_category_prompt_append,
    resolve_unspecified_high_category_prompt_append,
};
pub use chain_diagnostics::builtin_category_chain_candidates;
pub(crate) use chain_diagnostics::missing_chain_providers;
pub use coverage::{CategoryCoverage, UnusableCategory, resolve_category_coverage};
pub use fallback_chains::{CATEGORY_FALLBACK_CHAINS, category_fallback_chain};
pub(crate) use resolver::exclude_model_patterns;
pub use resolver::{
    CategoryModelSelection, CategoryResolutionResult, ModelSelectionSource, ModelUnavailable,
    ResolveCategoryOptions, ResolvedChildSpec, resolve_category,
};
pub(crate) use user_candidates::category_model_candidates;

#[cfg(test)]
#[path = "category_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "distribution_tests.rs"]
mod distribution_tests;

#[cfg(test)]
#[path = "gating_tests.rs"]
mod gating_tests;

#[cfg(test)]
#[path = "chains_tests.rs"]
mod chains_tests;

#[cfg(test)]
#[path = "openai_categories_tests.rs"]
mod openai_categories_tests;

#[cfg(test)]
#[path = "coverage_tests.rs"]
mod coverage_tests;

#[cfg(test)]
#[path = "gate_tests.rs"]
mod gate_tests;
