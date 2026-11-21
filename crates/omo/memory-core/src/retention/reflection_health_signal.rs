//! Port of `retention/reflection-health-signal.ts`: the reflection-health gate for unreflected
//! input (design 2.3, N4). Deleting a never- or partly-reflected journal discards input that was
//! never folded into memory, which is acceptable only while reflection works on the identity. The
//! gate is CLOSED when reflection is enabled and the identity is parked or its newest terminal
//! event is not a success; it has no time window, so an identity that failed 20 days ago and never
//! succeeded since stays closed.

use std::path::Path;

use serde_json::Value;

use super::policy::{RetentionError, list_names};
use crate::fs::resilient::read_to_string;
use crate::support::time::{format_rfc3339_millis, parse_rfc3339};

const SUCCESS_OUTCOMES: [&str; 2] = ["merged", "no_changes"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflectionTerminalKind {
    Failed,
    Abandoned,
    Quarantined,
    Parked,
}

impl ReflectionTerminalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
            Self::Quarantined => "quarantined",
            Self::Parked => "parked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreflectedGateEvent {
    pub kind: ReflectionTerminalKind,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreflectedRetentionGate {
    pub open: bool,
    /// The event that decided a closed gate, for the doctor line.
    pub newest: Option<UnreflectedGateEvent>,
}

impl UnreflectedRetentionGate {
    pub fn open() -> Self {
        Self {
            open: true,
            newest: None,
        }
    }
}

/// `admit` is asked before each record is read; once it answers false the walk stops and the gate
/// is `None` (unknown), so no unreflected input is judged on a partial read.
pub fn read_unreflected_retention_gate(
    reflection_dir: &Path,
    reflection_enabled: bool,
    admit: &dyn Fn() -> bool,
) -> Result<Option<UnreflectedRetentionGate>, RetentionError> {
    if !reflection_enabled {
        return Ok(Some(UnreflectedRetentionGate::open()));
    }
    if let Some(parked_at) = string_field(&read_json(&reflection_dir.join("park.json")), "parkedAt")
    {
        return Ok(Some(UnreflectedRetentionGate {
            open: false,
            newest: Some(UnreflectedGateEvent {
                kind: ReflectionTerminalKind::Parked,
                at: parked_at,
            }),
        }));
    }
    let mut last_success: Option<i64> = None;
    let mut last_failure: Option<(ReflectionTerminalKind, i64)> = None;
    let mut failure = |kind: ReflectionTerminalKind, at: i64| {
        if last_failure.is_none_or(|(_, newest)| at > newest) {
            last_failure = Some((kind, at));
        }
    };
    let completions = reflection_dir.join("completions");
    for name in list_names(&completions)? {
        if !name.ends_with(".json") {
            continue;
        }
        if !admit() {
            return Ok(None);
        }
        let record = read_json(&completions.join(&name));
        let outcome = string_field(&record, "outcome");
        let at = parse(string_field(&record, "finishedAt"));
        let (Some(outcome), Some(at)) = (outcome, at) else {
            continue;
        };
        if SUCCESS_OUTCOMES.contains(&outcome.as_str()) {
            last_success = Some(last_success.map_or(at, |newest| newest.max(at)));
        } else {
            failure(ReflectionTerminalKind::Failed, at);
        }
    }
    let runs = reflection_dir.join("runs");
    for name in list_names(&runs)? {
        if !admit() {
            return Ok(None);
        }
        let abandoned = parse(string_field(
            &read_json(&runs.join(&name).join("abandoned.json")),
            "generation",
        ));
        if let Some(at) = abandoned {
            failure(ReflectionTerminalKind::Abandoned, at);
        }
        let quarantined = parse(string_field(
            &read_json(&runs.join(&name).join("quarantined.json")),
            "generation",
        ));
        if let Some(at) = quarantined {
            failure(ReflectionTerminalKind::Quarantined, at);
        }
    }
    let gate = match last_failure {
        Some((kind, at)) if last_success.is_none_or(|success| success < at) => {
            UnreflectedRetentionGate {
                open: false,
                newest: Some(UnreflectedGateEvent {
                    kind,
                    at: format_rfc3339_millis(at),
                }),
            }
        }
        _ => UnreflectedRetentionGate::open(),
    };
    Ok(Some(gate))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&read_to_string(path).ok()?).ok()
}

fn string_field(value: &Option<Value>, key: &str) -> Option<String> {
    value
        .as_ref()?
        .as_object()?
        .get(key)?
        .as_str()
        .filter(|field| !field.is_empty())
        .map(str::to_string)
}

fn parse(value: Option<String>) -> Option<i64> {
    parse_rfc3339(&value?)
}
