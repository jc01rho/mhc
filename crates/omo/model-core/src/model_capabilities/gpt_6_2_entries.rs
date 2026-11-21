//! GPT-6.2 Astra and Sol capability rows (port of `model-capabilities/gpt-6-2-entries.ts`).

use std::sync::LazyLock;

use indexmap::IndexMap;

use super::types::ModelCapabilitiesSnapshotEntry;
use super::types::SnapshotLimit;
use super::types::SnapshotModalities;

// GPT-6.2 Astra and Sol (developers.openai.com/api/docs/models/gpt-6.2-astra, gpt-6.2-sol, 2026-11-17):
// 1,050,000 context, 922,000 max input, 128,000 max output, text and image input.
fn gpt_6_2_entry(id: &str) -> ModelCapabilitiesSnapshotEntry {
    ModelCapabilitiesSnapshotEntry {
        id: id.to_string(),
        family: Some("gpt".to_string()),
        reasoning: Some(true),
        temperature: Some(false),
        tool_call: Some(true),
        modalities: Some(SnapshotModalities {
            input: Some(vec!["text".to_string(), "image".to_string()]),
            output: Some(vec!["text".to_string()]),
        }),
        limit: Some(SnapshotLimit {
            context: Some(1_050_000),
            input: Some(922_000),
            output: Some(128_000),
        }),
    }
}

pub static GPT_6_2_MODEL_CAPABILITIES: LazyLock<IndexMap<String, ModelCapabilitiesSnapshotEntry>> =
    LazyLock::new(|| {
        [
            "gpt-6.2-astra",
            "gpt-6.2-astra-fast",
            "gpt-6.2-sol",
            "gpt-6.2-sol-fast",
        ]
        .into_iter()
        .map(|id| (id.to_string(), gpt_6_2_entry(id)))
        .collect()
    });
