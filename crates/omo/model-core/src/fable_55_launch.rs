//! Capability row for the provisional Claude Fable 5.5 id (port of the data half of
//! `fable-55-launch.ts`).
//!
//! The launch gate itself (release flag, effort table, rung filter, rejection log, predecessor
//! default) is retired: no routing table names a `claude-fable-*` model, so nothing here is a routing
//! input. Only the capability metadata the context-limit resolver and the bundled snapshot read stays.

use std::sync::LazyLock;

use crate::model_capabilities::ModelCapabilitiesSnapshotEntry;
use crate::model_capabilities::SnapshotLimit;

/// The provisional launch id (`FABLE_55_LAUNCH.id`).
pub const FABLE_55_LAUNCH_ID: &str = "claude-fable-5-5";

pub static FABLE_55_LAUNCH: LazyLock<ModelCapabilitiesSnapshotEntry> =
    LazyLock::new(|| ModelCapabilitiesSnapshotEntry {
        id: FABLE_55_LAUNCH_ID.to_string(),
        family: Some("claude-fable".to_string()),
        reasoning: None,
        temperature: None,
        tool_call: None,
        modalities: None,
        limit: Some(SnapshotLimit {
            context: None,
            input: None,
            output: None,
        }),
    });
