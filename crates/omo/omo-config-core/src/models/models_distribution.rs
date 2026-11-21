//! Per-spawn start rotation over a category's or agent's model chain (port of
//! `models/models-distribution.ts`, omo #11905).

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, PoisonError};

use regex::Regex;
use serde_json::Value;

use crate::schema::reasoning_vocabulary::split_reasoning_suffix;

pub const MODELS_DISTRIBUTION_VALUES: [&str; 2] = ["sequential", "round_robin"];

pub const MODELS_DISTRIBUTION_ROUND_ROBIN: &str = "round_robin";

pub trait ModelsDistributionEntry {
    fn distribution_model(&self) -> &str;
}

impl ModelsDistributionEntry for Value {
    fn distribution_model(&self) -> &str {
        match self {
            Value::String(model) => model,
            Value::Object(entry) => entry
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            _ => "",
        }
    }
}

impl ModelsDistributionEntry for String {
    fn distribution_model(&self) -> &str {
        self
    }
}

static TRAILING_PARENTHETICAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^()]+\)\s*$").expect("valid parenthetical pattern"));
static TRAILING_VARIANT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+[a-z][a-z0-9_-]*$").expect("valid variant pattern"));

/// Two entries naming the same model under another reasoning suffix, parenthetical or variant word
/// share one key, so a rotation never runs the same model twice.
fn distribution_key(model: &str) -> String {
    let base = split_reasoning_suffix(model, Some(true)).base;
    let without_parenthetical = TRAILING_PARENTHETICAL.replace(&base, "");
    TRAILING_VARIANT
        .replace(&without_parenthetical, "")
        .trim()
        .to_string()
}

/// Own one distributor per spawn planner, not per resolution or model retry. Each round-robin
/// distribution of a target (`category:<name>`, `agent:<name>`) advances that target's cursor once;
/// every other policy returns the configured chain unchanged and advances nothing.
#[derive(Debug, Default)]
pub struct ModelDistributor {
    cursors: Mutex<HashMap<String, usize>>,
}

// Identity, not cursor contents: two handles are equal only when they share one planner's cursors.
impl PartialEq for ModelDistributor {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for ModelDistributor {}

impl ModelDistributor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn distribute<T: ModelsDistributionEntry + Clone>(
        &self,
        target: &str,
        models: &[T],
        distribution: Option<&str>,
    ) -> Vec<T> {
        if distribution != Some(MODELS_DISTRIBUTION_ROUND_ROBIN) || models.is_empty() {
            return models.to_vec();
        }
        let mut seen: HashSet<String> = HashSet::new();
        let unique: Vec<T> = models
            .iter()
            .filter(|entry| seen.insert(distribution_key(entry.distribution_model())))
            .cloned()
            .collect();
        let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        let start = cursors.get(target).copied().unwrap_or(0) % unique.len();
        cursors.insert(target.to_string(), (start + 1) % unique.len());
        let mut rotated = unique[start..].to_vec();
        rotated.extend_from_slice(&unique[..start]);
        rotated
    }
}
