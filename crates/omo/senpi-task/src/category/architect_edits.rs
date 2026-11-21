//! Policy gate for omo#12239 (architect direct edits). A user `description` / `prompt_append`
//! still wins over either builtin wording.

// SAFETY-DIVERGENCE(senpi-task/src/category/anthropic-categories.ts): upstream's architect
// implements by default; mhc keeps it advisory-only unless `categories.architect.allow_edits`.

use serde_json::Value;

use crate::category::prompts;

pub const ALLOW_EDITS_KEY: &str = "allow_edits";

fn allows_edits(category_name: &str, user_config: Option<&Value>) -> bool {
    category_name == "architect"
        && user_config
            .and_then(|user| user.get(ALLOW_EDITS_KEY))
            .and_then(Value::as_bool)
            == Some(true)
}

/// The builtin description that replaces the shipped one when the category allows edits.
pub fn description(category_name: &str, user_config: Option<&Value>) -> Option<&'static str> {
    allows_edits(category_name, user_config).then_some(prompts::ARCHITECT_IMPLEMENT_DESCRIPTION)
}

/// The builtin prompt append that replaces the shipped one when the category allows edits.
pub fn prompt_append(category_name: &str, user_config: Option<&Value>) -> Option<&'static str> {
    allows_edits(category_name, user_config).then_some(prompts::ARCHITECT_IMPLEMENT_PROMPT_APPEND)
}
