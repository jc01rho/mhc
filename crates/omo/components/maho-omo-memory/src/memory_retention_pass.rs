//! Port of `components/memory/memory-retention-pass.ts` (omo #12532/#12538/#12815): the runtime
//! retention pass and the host-sweep slice that `memory_maintenance` runs when one is due.

use std::collections::BTreeSet;

use memory_core::git::GitMemoryRepo;
use memory_core::identity::layout::MemoryIdentityPaths;
use memory_core::reflection::{HostSweepOptions, HostSweepResult, sweep_host_identities};
use memory_core::retention::{
    DEFAULT_RUNTIME_MAX_BYTES, DEFAULT_RUNTIME_RETENTION, ForeignMemoryRuntimeSettings,
    LegacyRecallRunsOutcome, RETENTION_STAMP_CONFIG_KEY, RetentionContext,
    RetentionHostSweepStepOptions, RuntimeRetentionOptions, RuntimeRetentionPolicy, WarnFn,
    create_retention_host_sweep_step, run_runtime_retention, summarize_runtime_retention,
};

fn layer_number(settings: &serde_json::Value, identity: &str, key: &str) -> Option<f64> {
    settings["agents"][identity]["runtime"][key]
        .as_f64()
        .or_else(|| settings["runtime"][key].as_f64())
}

fn layer_bool(settings: &serde_json::Value, identity: &str, key: &str) -> Option<bool> {
    settings["agents"][identity]["runtime"][key]
        .as_bool()
        .or_else(|| settings["runtime"][key].as_bool())
}

/// The `memory.runtime` policy of one identity, resolved from the memory settings JSON;
/// `agents.<id>.runtime` wins field by field.
pub fn resolve_runtime_retention_policy(
    settings: &serde_json::Value,
    identity: &str,
) -> RuntimeRetentionPolicy {
    RuntimeRetentionPolicy {
        retention_days: layer_number(settings, identity, "retention_days")
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.retention_days),
        abandoned_journal_days: layer_number(settings, identity, "abandoned_journal_days")
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.abandoned_journal_days),
        completions_days: layer_number(settings, identity, "completions_days")
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.completions_days),
        pass_budget_ms: layer_number(settings, identity, "pass_budget_ms")
            .unwrap_or(DEFAULT_RUNTIME_RETENTION.pass_budget_ms as f64)
            as u64,
        discard_unreflected_journals: layer_bool(
            settings,
            identity,
            "discard_unreflected_journals",
        )
        .unwrap_or(DEFAULT_RUNTIME_RETENTION.discard_unreflected_journals),
    }
}

/// `memory.runtime.max_bytes` for one identity (`0` = unbounded): `agents.<id>.runtime` wins.
pub fn resolve_runtime_max_bytes(settings: &serde_json::Value, identity: &str) -> u64 {
    layer_number(settings, identity, "max_bytes").unwrap_or(DEFAULT_RUNTIME_MAX_BYTES as f64) as u64
}

pub type RetentionLog<'a> = &'a dyn Fn(&str);

pub struct MaintenanceRetentionInput<'a> {
    pub identity: &'a str,
    pub paths: &'a MemoryIdentityPaths,
    pub repo: &'a GitMemoryRepo,
    pub cancel: &'a dyn Fn() -> bool,
    pub now: &'a dyn Fn() -> i64,
    pub policy: Option<RuntimeRetentionPolicy>,
    pub max_bytes: Option<u64>,
    pub settings: Option<&'a serde_json::Value>,
    pub reflection_enabled: bool,
    pub is_live_session: Option<&'a dyn Fn(&str) -> bool>,
    pub owned_sidecars: Option<&'a BTreeSet<String>>,
    pub info: Option<RetentionLog<'a>>,
    pub warn: Option<RetentionLog<'a>>,
}

/// One identity's retention pass; the completion stamp is written after the work, so an interrupted
/// pass resumes at the next bind.
pub fn run_maintenance_retention(input: &MaintenanceRetentionInput<'_>) -> Result<(), String> {
    let policy = input.policy.unwrap_or_else(|| match input.settings {
        Some(settings) => resolve_runtime_retention_policy(settings, input.identity),
        None => DEFAULT_RUNTIME_RETENTION,
    });
    let max_bytes = input.max_bytes.unwrap_or_else(|| match input.settings {
        Some(settings) => resolve_runtime_max_bytes(settings, input.identity),
        None => DEFAULT_RUNTIME_MAX_BYTES,
    });
    let mut context = RetentionContext::new(input.paths, policy, input.now);
    context.reflection_enabled = Some(input.reflection_enabled);
    context.cancel = Some(input.cancel);
    context.is_live_session = input.is_live_session;
    let identity = input.identity.to_string();
    let warn = input.warn;
    let fields: WarnFn<'_> = &|message, values| {
        if let Some(warn) = warn {
            let detail = values
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            warn(&format!("{message} identity={identity} {detail}"));
        }
    };
    context.warn = Some(fields);
    let mut options = RuntimeRetentionOptions::new(context);
    options.max_bytes = Some(max_bytes);
    options.owned_sidecars = input.owned_sidecars;
    let report = run_runtime_retention(&options).map_err(|error| error.to_string())?;
    let summary = summarize_runtime_retention(&report);
    if (summary.removed > 0 || !report.skipped.is_empty() || report.cursor.is_some())
        && let Some(info) = input.info
    {
        info(&format!(
            "omo-senpi memory runtime retention identity={} removed={} runs={} completions={} journals={} sidecars={} ledgers={} legacy={}{}",
            input.identity,
            summary.removed,
            summary.runs,
            summary.completions,
            summary.journals,
            summary.sidecars,
            summary.ledgers,
            summary.reflection_sessions,
            if summary.recall_runs == LegacyRecallRunsOutcome::Removed {
                " + recall/runs"
            } else {
                ""
            }
        ));
    }
    // A pass whose stores finished and whose cap could not even measure is stamped too: its cap
    // resume point is kept for the next due pass, which comes at the interval (#12538 review r5 L1).
    let cap_stalled = report.cap.as_ref().is_some_and(|cap| {
        cap.skipped
            .iter()
            .any(|reason| reason == "measure-over-budget")
    });
    if report.cursor.is_none() || cap_stalled {
        input
            .repo
            .config_set(RETENTION_STAMP_CONFIG_KEY, &(input.now)().to_string())
            .map_err(|error| error.to_string())?;
    }
    if let Some(cap) = &report.cap
        && cap.exceeded_bytes > 0
    {
        // "Nothing evictable" only when the cap walked every tier: a budget stop (cursor), a
        // measurement past the budget and skipped journal tiers all leave evictable data for the
        // next pass.
        let unfinished = cap.cursor.is_some() || !cap.skipped.is_empty();
        if let Some(warn) = input.warn {
            warn(&format!(
                "omo-senpi memory runtime cap exceeded, {} identity={} overCapBytes={} kept={:?}",
                if unfinished {
                    "eviction continues next pass"
                } else {
                    "nothing evictable"
                },
                input.identity,
                cap.exceeded_bytes,
                cap.kept
            ));
        }
    }
    Ok(())
}

const HOST_SWEEP_IDENTITIES_PER_PASS: usize = 25;
const HOST_SWEEP_SLICE_MS: u64 = 30_000;

pub struct MaintenanceHostSweepInput<'a> {
    pub memory_root: &'a std::path::Path,
    pub settings: Option<ForeignMemoryRuntimeSettings>,
    pub is_hosted_identity: &'a dyn Fn(&str) -> bool,
    pub cancel: &'a dyn Fn() -> bool,
    pub now: &'a dyn Fn() -> i64,
    pub info: Option<RetentionLog<'a>>,
}

/// One bounded slice of the host-wide sweep: every identity a session in this process did not bind.
pub fn run_maintenance_host_sweep(
    input: MaintenanceHostSweepInput<'_>,
) -> Result<HostSweepResult, String> {
    let now = input.now;
    let deadline = now() + HOST_SWEEP_SLICE_MS as i64;
    let step_options = RetentionHostSweepStepOptions {
        settings: input.settings,
        is_hosted_identity: Some(input.is_hosted_identity),
        now: Some(now),
        cancel: Some(input.cancel),
        deadline: Some(deadline),
        dry_run: false,
    };
    let mut options = HostSweepOptions::new(input.memory_root);
    options.steps = vec![create_retention_host_sweep_step(step_options)];
    options.max_identities = Some(HOST_SWEEP_IDENTITIES_PER_PASS);
    options.budget_ms = Some(HOST_SWEEP_SLICE_MS);
    let now_unsigned = || now().max(0) as u64;
    options.now = Some(&now_unsigned);
    options.aborted = Some(input.cancel);
    let result = sweep_host_identities(options)?;
    if let HostSweepResult::Swept { visited, lines, .. } = &result
        && !lines.is_empty()
        && let Some(info) = input.info
    {
        info(&format!(
            "omo-senpi memory host sweep visited={} lines={}",
            visited.len(),
            lines.join(" | ")
        ));
    }
    Ok(result)
}
