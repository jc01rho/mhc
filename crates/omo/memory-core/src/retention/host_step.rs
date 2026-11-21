//! Port of `retention/host-step.ts`: retention as a host-sweep step (design 6b): the same pass a
//! bound session runs, applied to an identity this process does not host. Foreign identities get
//! the conservative inputs: no live sessions are known (none are hosted here), reflection counts
//! as enabled for the health gate, the owned-sidecar set is empty, and the unreflected-input
//! switch comes from the GLOBAL config only. The size cap runs too, with its journal floor raised
//! to `retention_days` because liveness is unknown.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::cap::{
    RuntimeCapEvictableOptions, RuntimeCapExcess, describe_runtime_cap_excess,
    measure_runtime_cap_evictable_bytes,
};
use super::legacy::LegacyRecallRunsOutcome;
use super::pass::{RuntimeRetentionOptions, run_runtime_retention, summarize_runtime_retention};
use super::past_retention::{
    count_runtime_past_retention, count_runtime_past_retention_other_stores,
    measure_unreflected_journal_bytes,
};
use super::policy::{DEFAULT_RUNTIME_RETENTION, RetentionContext, RuntimeRetentionPolicy, days_ms};
use super::size::{
    DEFAULT_RUNTIME_MAX_BYTES, MeasuredRuntimeSize, measure_runtime_size_within_budget,
};
use crate::identity::layout::build_identity_paths;
use crate::reflection::host_sweep::{HostSweepIdentity, HostSweepStep};
use crate::support::time::now_millis;

/// The memory settings shape this decision reads; plain data so the CLI can pass parsed JSON.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForeignRuntimeLayer {
    pub retention_days: Option<f64>,
    pub abandoned_journal_days: Option<f64>,
    pub completions_days: Option<f64>,
    pub pass_budget_ms: Option<f64>,
    pub max_bytes: Option<f64>,
    pub discard_unreflected_journals: Option<bool>,
}

impl ForeignRuntimeLayer {
    /// Reads the `memory.runtime` object of parsed settings; a field of the wrong type is absent.
    pub fn from_json(value: &Value) -> Self {
        let number = |key: &str| value.get(key).and_then(Value::as_f64);
        Self {
            retention_days: number("retention_days"),
            abandoned_journal_days: number("abandoned_journal_days"),
            completions_days: number("completions_days"),
            pass_budget_ms: number("pass_budget_ms"),
            max_bytes: number("max_bytes"),
            discard_unreflected_journals: value
                .get("discard_unreflected_journals")
                .and_then(Value::as_bool),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForeignAgentSettings {
    pub runtime: Option<ForeignRuntimeLayer>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ForeignMemoryRuntimeSettings {
    pub runtime: Option<ForeignRuntimeLayer>,
    pub agents: BTreeMap<String, ForeignAgentSettings>,
}

impl ForeignMemoryRuntimeSettings {
    /// Reads `{ runtime, agents: { <id>: { runtime } } }` from parsed `memory` settings.
    pub fn from_json(value: &Value) -> Self {
        let agents = value
            .get("agents")
            .and_then(Value::as_object)
            .map(|agents| {
                agents
                    .iter()
                    .map(|(name, agent)| {
                        (
                            name.clone(),
                            ForeignAgentSettings {
                                runtime: agent.get("runtime").map(ForeignRuntimeLayer::from_json),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            runtime: value.get("runtime").map(ForeignRuntimeLayer::from_json),
            agents,
        }
    }
}

/// `discard_unreflected_journals` for an identity this process does not host (`omo doctor --fix`,
/// the host sweep's foreign identities; REVIEW-4 M-b). Only the global config is read, never
/// another project's layer, so the switch counts as on only when the global value is on AND the
/// identity's own global override does not turn it off. No readable config means off.
pub fn resolve_foreign_identity_discard_unreflected(
    settings: Option<&ForeignMemoryRuntimeSettings>,
    identity: &str,
) -> bool {
    let Some(settings) = settings else {
        return false;
    };
    if settings
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.discard_unreflected_journals)
        != Some(true)
    {
        return false;
    }
    settings
        .agents
        .get(identity)
        .and_then(|agent| agent.runtime.as_ref())
        .and_then(|runtime| runtime.discard_unreflected_journals)
        != Some(false)
}

/// A non-negative finite number from the agent override, else the global value, else the default.
fn window(
    settings: Option<&ForeignMemoryRuntimeSettings>,
    identity: &str,
    pick: impl Fn(&ForeignRuntimeLayer) -> Option<f64>,
    fallback: f64,
) -> f64 {
    let agent = settings
        .and_then(|settings| settings.agents.get(identity))
        .and_then(|agent| agent.runtime.as_ref())
        .and_then(&pick);
    let global = settings
        .and_then(|settings| settings.runtime.as_ref())
        .and_then(&pick);
    [agent, global]
        .into_iter()
        .flatten()
        .find(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(fallback)
}

/// The retention policy for an identity the host sweep visits (REVIEW-5 M1): the same windows the
/// session resolves, read from the global layer and its `agents.<id>.runtime` override.
fn resolve_foreign_identity_retention_policy(
    settings: Option<&ForeignMemoryRuntimeSettings>,
    identity: &str,
) -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        retention_days: window(
            settings,
            identity,
            |layer| layer.retention_days,
            DEFAULT_RUNTIME_RETENTION.retention_days,
        ),
        abandoned_journal_days: window(
            settings,
            identity,
            |layer| layer.abandoned_journal_days,
            DEFAULT_RUNTIME_RETENTION.abandoned_journal_days,
        ),
        completions_days: window(
            settings,
            identity,
            |layer| layer.completions_days,
            DEFAULT_RUNTIME_RETENTION.completions_days,
        ),
        pass_budget_ms: window(
            settings,
            identity,
            |layer| layer.pass_budget_ms,
            DEFAULT_RUNTIME_RETENTION.pass_budget_ms as f64,
        ) as u64,
        discard_unreflected_journals: resolve_foreign_identity_discard_unreflected(
            settings, identity,
        ),
    }
}

/// `max_bytes` for a foreign identity: the agent override, else the global value, else the shipped
/// default.
fn resolve_foreign_identity_max_bytes(
    settings: Option<&ForeignMemoryRuntimeSettings>,
    identity: &str,
) -> u64 {
    window(
        settings,
        identity,
        |layer| layer.max_bytes,
        DEFAULT_RUNTIME_MAX_BYTES as f64,
    ) as u64
}

#[derive(Default)]
pub struct RetentionHostSweepStepOptions<'a> {
    pub settings: Option<ForeignMemoryRuntimeSettings>,
    /// Identities this process hosts (REVIEW-5 M2). Their own pass already ran with live
    /// sessions, owned sidecars and the project layer, so the host sweep must not visit them
    /// again with less context.
    pub is_hosted_identity: Option<&'a dyn Fn(&str) -> bool>,
    pub now: Option<&'a dyn Fn() -> i64>,
    pub cancel: Option<&'a dyn Fn() -> bool>,
    /// End of the driver's slice (epoch ms): each identity's budget is `min(pass budget, slice
    /// remaining)`.
    pub deadline: Option<i64>,
    /// `--fix --dry-run`: retention changes nothing and prints what the pass would remove
    /// (#12932).
    pub dry_run: bool,
}

/// The dry-run preview for one identity: read-only counts of what is past its window in every
/// store the pass prunes, by the pass's own rules and in its order (a completion whose run the
/// pass removes is counted), and over the cap what the cap could evict. Locks, reservations, live
/// sessions and live recall ledgers are decided only by the real pass, which can keep some of what
/// the preview counts.
fn preview_retention(
    identity: &str,
    root: &Path,
    policy: RuntimeRetentionPolicy,
    max_bytes: u64,
    now: &dyn Fn() -> i64,
    cancel: &dyn Fn() -> bool,
) -> Result<Vec<String>, String> {
    let memory_root = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("no memory root above {}", root.display()))?;
    let paths = build_identity_paths(memory_root, identity);
    let mut context = RetentionContext::new(&paths, policy, now);
    context.reflection_enabled = Some(true);
    context.cancel = Some(cancel);
    let past = count_runtime_past_retention(&context).map_err(|error| error.to_string())?;
    let other =
        count_runtime_past_retention_other_stores(&context).map_err(|error| error.to_string())?;
    let total = past.runs
        + past.journals
        + past.sidecars
        + other.completions
        + other.ledgers
        + other.legacy;
    let counts = format!(
        "runs {}, completions {}, journals {}, sidecars {}, ledgers {}, legacy {}",
        past.runs, other.completions, past.journals, past.sidecars, other.ledgers, other.legacy
    );
    let mut lines = if total == 0 {
        Vec::new()
    } else {
        vec![format!(
            "WOULD FIX {identity} retention would remove {total} ({counts})"
        )]
    };
    if max_bytes == 0 {
        return Ok(lines);
    }
    let measured = measure_runtime_size_within_budget(
        &paths.runtime,
        Some(policy.pass_budget_ms),
        Some(cancel),
        now(),
    )
    .map_err(|error| error.to_string())?;
    let MeasuredRuntimeSize::Size(size) = measured else {
        return Ok(lines);
    };
    if size.bytes <= max_bytes {
        return Ok(lines);
    }
    let evictable_bytes = measure_runtime_cap_evictable_bytes(&RuntimeCapEvictableOptions {
        paths: &paths,
        policy,
        now,
        reflection_enabled: Some(true),
        journal_floor_ms: Some(days_ms(policy.retention_days)),
        cancel: Some(cancel),
    })
    .map_err(|error| error.to_string())?;
    let unreflected_bytes = if policy.discard_unreflected_journals {
        0
    } else {
        measure_unreflected_journal_bytes(&context).map_err(|error| error.to_string())?
    };
    let excess = describe_runtime_cap_excess(&RuntimeCapExcess {
        over_cap_bytes: size.bytes - max_bytes,
        evictable_bytes,
        unreflected_bytes,
        discard_unreflected_journals: policy.discard_unreflected_journals,
    });
    if let Some(excess) = excess {
        lines.push(format!("WOULD FIX {identity} size cap: {excess}"));
    }
    Ok(lines)
}

pub fn create_retention_host_sweep_step<'a>(
    options: RetentionHostSweepStepOptions<'a>,
) -> HostSweepStep<'a> {
    Box::new(move |identity: &HostSweepIdentity| {
        let default_now: fn() -> i64 = now_millis;
        let now: &dyn Fn() -> i64 = options.now.unwrap_or(&default_now);
        if options
            .is_hosted_identity
            .is_some_and(|is_hosted| is_hosted(&identity.name))
        {
            return Ok(Vec::new());
        }
        let policy =
            resolve_foreign_identity_retention_policy(options.settings.as_ref(), &identity.name);
        let max_bytes =
            resolve_foreign_identity_max_bytes(options.settings.as_ref(), &identity.name);
        let caller_cancel = options.cancel;
        let aborted = || caller_cancel.is_some_and(|cancel| cancel());
        if options.dry_run {
            let deadline = Instant::now() + Duration::from_millis(policy.pass_budget_ms);
            let cancel = || aborted() || Instant::now() >= deadline;
            return match preview_retention(
                &identity.name,
                &identity.root,
                policy,
                max_bytes,
                now,
                &cancel,
            ) {
                Ok(lines) => Ok(lines),
                Err(message) if aborted() => Err(message),
                Err(message) => Ok(vec![format!(
                    "WOULD FIX {} retention preview unavailable: {message}",
                    identity.name
                )]),
            };
        }
        let remaining = options
            .deadline
            .map_or(policy.pass_budget_ms as i64, |deadline| {
                (deadline - now()).max(1)
            });
        let memory_root = identity
            .root
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| format!("no memory root above {}", identity.root.display()))?;
        let paths = build_identity_paths(memory_root, &identity.name);
        let run_policy = RuntimeRetentionPolicy {
            pass_budget_ms: policy.pass_budget_ms.min(remaining.max(0) as u64),
            ..policy
        };
        let mut context = RetentionContext::new(&paths, run_policy, now);
        context.reflection_enabled = Some(true);
        context.cancel = options.cancel;
        let mut retention_options = RuntimeRetentionOptions::new(context);
        retention_options.max_bytes = Some(max_bytes);
        retention_options.journal_floor_ms = Some(days_ms(policy.retention_days));
        let report = match run_runtime_retention(&retention_options) {
            Ok(report) => report,
            Err(error) if aborted() => return Err(error.to_string()),
            Err(error) => {
                return Ok(vec![format!(
                    "FAIL {} retention failed: {error}",
                    identity.name
                )]);
            }
        };
        let summary = summarize_runtime_retention(&report);
        if summary.removed == 0 && report.cursor.is_none() {
            return Ok(Vec::new());
        }
        let recall_runs = if report.recall_runs == LegacyRecallRunsOutcome::Removed {
            " + recall/runs"
        } else {
            ""
        };
        let size_cap = match &report.cap {
            Some(cap) if !cap.evicted.is_empty() => format!(", size cap {}", cap.evicted.len()),
            _ => String::new(),
        };
        let counts = format!(
            "runs {}, completions {}, journals {}, sidecars {}, ledgers {}, legacy {}{recall_runs}{size_cap}",
            summary.runs,
            summary.completions,
            summary.journals,
            summary.sidecars,
            summary.ledgers,
            summary.reflection_sessions
        );
        let resumes = report.cursor.as_ref().map_or_else(String::new, |cursor| {
            format!("; resumes at {}", cursor.store())
        });
        Ok(vec![format!(
            "FIXED {} retention removed {} ({counts}){resumes}",
            identity.name, summary.removed
        )])
    })
}

#[cfg(test)]
#[path = "host_step_tests.rs"]
mod tests;
