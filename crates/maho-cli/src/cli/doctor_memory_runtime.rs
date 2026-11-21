//! Port of `omo-native/bin/lib/doctor-memory-runtime.js` (#10144/#12538): the `mhc doctor` memory
//! runtime size section. Lists the durable identities' `runtime/` size against their
//! `memory.runtime.max_bytes` cap, warns when an identity is near or over the cap, and says what the
//! next maintenance pass will evict.

use std::collections::BTreeMap;
use std::path::Path;

use memory_core::identity::layout::{build_identity_paths, resolve_memory_root};
use memory_core::retention::{
    DEFAULT_RUNTIME_MAX_BYTES, DEFAULT_RUNTIME_RETENTION, ForeignMemoryRuntimeSettings,
    MeasuredRuntimeSize, RuntimeCapExcess, RuntimeRetentionPolicy, RuntimeSizeLevel,
    describe_runtime_cap_excess, format_runtime_bytes, measure_runtime_cap_evictable_bytes,
    measure_runtime_size_within_budget, measure_unreflected_journal_bytes,
    read_last_retention_log_entry, resolve_foreign_identity_discard_unreflected,
    runtime_size_standing,
};

const LISTED_IDENTITIES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
struct IdentityEntry {
    identity: String,
    bytes: u64,
    max_bytes: u64,
    discard_unreflected_journals: bool,
    size_unknown: bool,
    last_cleanup: Option<(String, u64)>,
    /// Filled only for an over-cap identity with `discard_unreflected_journals` off.
    unreflected_bytes: Option<u64>,
    evictable_bytes: Option<u64>,
}

pub fn doctor_memory_runtime_lines(
    cwd: &Path,
    env: &BTreeMap<String, String>,
    config: &serde_json::Value,
) -> Vec<String> {
    let memory_root = resolve_memory_root(env, cwd);
    let agents_root = memory_root.join("agents");
    let Ok(names) = std::fs::read_dir(&agents_root) else {
        return Vec::new();
    };
    let mut identities: Vec<String> = names
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .filter(|name| agents_root.join(name).join("repo").exists())
        .collect();
    identities.sort();
    if identities.is_empty() {
        return Vec::new();
    }
    let settings = config
        .get("memory")
        .map(ForeignMemoryRuntimeSettings::from_json)
        .unwrap_or_default();
    let budget_ms = DEFAULT_RUNTIME_RETENTION.pass_budget_ms;
    let now_ms = || memory_core::support::time::now_millis();
    let cancel = || false;
    let mut entries = Vec::new();
    for identity in identities {
        let paths = build_identity_paths(&memory_root, &identity);
        let max_bytes = runtime_max_bytes(&settings, &identity);
        let discard = resolve_foreign_identity_discard_unreflected(Some(&settings), &identity);
        let last_cleanup = read_last_retention_log_entry(&paths.runtime)
            .ok()
            .flatten()
            .map(|entry| (entry.at, entry.total));
        let started_at = now_ms();
        let size = measure_runtime_size_within_budget(
            &paths.runtime,
            Some(budget_ms),
            Some(&cancel),
            started_at,
        );
        let measured = match size {
            Ok(MeasuredRuntimeSize::Size(size)) => size.bytes,
            Ok(MeasuredRuntimeSize::Unknown(_)) | Err(_) => {
                entries.push(IdentityEntry {
                    identity,
                    bytes: 0,
                    max_bytes,
                    discard_unreflected_journals: discard,
                    size_unknown: true,
                    last_cleanup,
                    unreflected_bytes: None,
                    evictable_bytes: None,
                });
                continue;
            }
        };
        let (unreflected_bytes, evictable_bytes) = if max_bytes > 0 && measured > max_bytes {
            let policy = memory_core::retention::RuntimeRetentionPolicy {
                discard_unreflected_journals: discard,
                ..DEFAULT_RUNTIME_RETENTION
            };
            let context = memory_core::retention::RetentionContext::new(&paths, policy, &now_ms);
            let unreflected = if discard {
                None
            } else {
                measure_unreflected_journal_bytes(&context).ok()
            };
            let evictable = measure_runtime_cap_evictable_bytes(
                &memory_core::retention::RuntimeCapEvictableOptions {
                    paths: &paths,
                    policy,
                    now: &now_ms,
                    reflection_enabled: Some(true),
                    journal_floor_ms: None,
                    cancel: Some(&cancel),
                },
            )
            .ok();
            (unreflected, evictable)
        } else {
            (None, None)
        };
        entries.push(IdentityEntry {
            identity,
            bytes: measured,
            max_bytes,
            discard_unreflected_journals: discard,
            size_unknown: false,
            last_cleanup,
            unreflected_bytes,
            evictable_bytes,
        });
    }
    format_memory_runtime_lines(&entries)
}

fn runtime_max_bytes(settings: &ForeignMemoryRuntimeSettings, identity: &str) -> u64 {
    let global = settings
        .runtime
        .as_ref()
        .and_then(|runtime| runtime.max_bytes)
        .unwrap_or(DEFAULT_RUNTIME_MAX_BYTES as f64);
    let override_value = settings
        .agents
        .get(identity)
        .and_then(|agent| agent.runtime.as_ref())
        .and_then(|runtime| runtime.max_bytes)
        .unwrap_or(global);
    if override_value <= 0.0 {
        0
    } else {
        override_value as u64
    }
}

fn last_cleanup_text(entry: &IdentityEntry) -> String {
    match &entry.last_cleanup {
        None => "; no cleanup recorded".to_string(),
        Some((at, total)) => format!("; last cleanup {at} removed {total}"),
    }
}

fn cap_text(max_bytes: u64) -> String {
    if max_bytes == 0 {
        "no cap".to_string()
    } else {
        format!("{} cap", format_runtime_bytes(max_bytes))
    }
}

fn format_memory_runtime_lines(identities: &[IdentityEntry]) -> Vec<String> {
    let mut by_size: Vec<&IdentityEntry> = identities.iter().collect();
    by_size.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    let total_bytes: u64 = identities.iter().map(|entry| entry.bytes).sum();
    let mut lines = Vec::new();
    for entry in by_size.iter().take(LISTED_IDENTITIES) {
        if entry.size_unknown {
            lines.push(format!(
                "INFO memory runtime: {} size unknown ({}){}",
                entry.identity,
                cap_text(entry.max_bytes),
                last_cleanup_text(entry)
            ));
            continue;
        }
        let standing = runtime_size_standing(entry.bytes, entry.max_bytes);
        let share = standing
            .percent
            .map_or_else(String::new, |percent| format!(", {percent}%"));
        lines.push(format!(
            "INFO memory runtime: {} {} ({}{}){}",
            entry.identity,
            format_runtime_bytes(entry.bytes),
            cap_text(entry.max_bytes),
            share,
            last_cleanup_text(entry)
        ));
    }
    let rest = by_size.iter().skip(LISTED_IDENTITIES).collect::<Vec<_>>();
    if !rest.is_empty() {
        let rest_bytes: u64 = rest.iter().map(|entry| entry.bytes).sum();
        lines.push(format!(
            "INFO memory runtime: {} more identities hold {}",
            rest.len(),
            format_runtime_bytes(rest_bytes)
        ));
    }
    let unknown = identities.iter().filter(|entry| entry.size_unknown).count();
    let plural = if identities.len() == 1 { "y" } else { "ies" };
    let unknown_text = if unknown == 0 {
        String::new()
    } else {
        format!(" ({unknown} not measured)")
    };
    lines.push(format!(
        "INFO memory runtime total: {} across {} identit{}{}",
        format_runtime_bytes(total_bytes),
        identities.len(),
        plural,
        unknown_text
    ));
    for entry in &by_size {
        if entry.size_unknown {
            lines.push(format!(
                "WARN memory runtime ({}): size unknown: measurement exceeded {} ms",
                entry.identity, DEFAULT_RUNTIME_RETENTION.pass_budget_ms
            ));
            continue;
        }
        let excess = describe_runtime_cap_excess(&RuntimeCapExcess {
            over_cap_bytes: entry.bytes.saturating_sub(entry.max_bytes),
            evictable_bytes: entry.evictable_bytes.unwrap_or(0),
            unreflected_bytes: entry.unreflected_bytes.unwrap_or(0),
            discard_unreflected_journals: entry.discard_unreflected_journals,
        });
        if let Some(excess) = excess {
            lines.push(format!(
                "WARN memory runtime ({}): {}; raise memory.runtime.max_bytes, or set it to 0, to keep more.",
                entry.identity, excess
            ));
            continue;
        }
        let standing = runtime_size_standing(entry.bytes, entry.max_bytes);
        if standing.level != RuntimeSizeLevel::Warn {
            continue;
        }
        lines.push(format!(
            "WARN memory runtime is near its size cap ({}): {} is {}% of the {} cap (memory.runtime.max_bytes). The next maintenance pass evicts the oldest runtime data down to the cap; raise the cap, or set it to 0, to keep more.",
            entry.identity,
            format_runtime_bytes(entry.bytes),
            standing.percent.unwrap_or(0),
            format_runtime_bytes(entry.max_bytes)
        ));
    }
    lines
}
