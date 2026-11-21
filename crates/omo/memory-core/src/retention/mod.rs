//! Port of `packages/memory-core/src/retention/` (omo #12532/#12538/#12783, v5.1.79): bounded
//! runtime retention and the per-identity runtime size cap.
//!
//! A pass walks the identity's `runtime/` stores in a fixed order (folded reflection runs, consumed
//! completions, legacy reflection sessions, transcript journals, kibitzer sidecars, recall ledgers,
//! legacy `recall/runs`) inside `pass_budget_ms`, records where it stopped in
//! `runtime/retention.json`, and then lets the size cap evict oldest-first until `runtime/` fits
//! `memory.runtime.max_bytes`. Nothing a live session owns, no locked or unfinished work and no run
//! id reflection still needs is touched, and journals reflection never folded into memory are kept
//! unless `discard_unreflected_journals` is on and reflection is healthy for the identity.

pub mod budget;
pub mod cap;
pub mod completions;
pub mod guard;
pub mod host_step;
pub mod journals;
pub mod legacy;
pub mod log;
pub mod pass;
pub mod past_retention;
pub mod policy;
pub mod recall_ledgers;
pub mod reflection_health_signal;
pub mod reflection_runs;
pub mod sidecars;
pub mod size;
pub mod stamp;

pub use budget::{PassBudget, RetentionCursor, RetentionStore};
pub use cap::{
    RuntimeCapCursor, RuntimeCapEvictableOptions, RuntimeCapEviction, RuntimeCapExcess,
    RuntimeCapKeepReason, RuntimeCapTier, RuntimeSizeCapOptions, RuntimeSizeCapReport,
    describe_runtime_cap_excess, measure_runtime_cap_evictable_bytes, run_runtime_size_cap,
};
pub use completions::{COMPLETIONS_KEEP_NEWEST, Completion, prune_completions, read_completion};
pub use guard::{
    LazyRetentionGuard, ReflectionRetentionGuard, Reservations, read_reflection_retention_guard,
    read_reservations, reserved_run_ids,
};
pub use host_step::{
    ForeignAgentSettings, ForeignMemoryRuntimeSettings, ForeignRuntimeLayer,
    RetentionHostSweepStepOptions, create_retention_host_sweep_step,
    resolve_foreign_identity_discard_unreflected,
};
pub use journals::{
    JournalClass, Verdict, classify_journal_entry, prune_transcript_journals,
    remove_transcript_journal_under_lock,
};
pub use legacy::{
    LegacyRecallRunsOutcome, prune_legacy_recall_runs, prune_legacy_reflection_sessions,
};
pub use log::{
    RETENTION_LOG_FILE, RETENTION_LOG_PREVIOUS_FILE, RetentionLogEntry, RetentionLogInput,
    RetentionLogLimits, append_retention_log, read_last_retention_log_entry,
};
pub use pass::{
    ReportCursor, RetentionCapSummary, RetentionSummary, RuntimeRetentionOptions,
    RuntimeRetentionReport, run_runtime_retention, summarize_runtime_retention,
};
pub use past_retention::{
    RuntimePastRetention, RuntimePastRetentionOtherStores, count_runtime_past_retention,
    count_runtime_past_retention_other_stores, measure_unreflected_journal_bytes,
};
pub use policy::{
    DAY_MS, DEFAULT_RUNTIME_RETENTION, RetentionContext, RetentionError, RetentionKeepReason,
    RetentionStoreResult, RuntimeRetentionPolicy, StoreTally, WarnFn, days_ms,
};
pub use recall_ledgers::prune_recall_ledgers;
pub use reflection_health_signal::{
    ReflectionTerminalKind, UnreflectedGateEvent, UnreflectedRetentionGate,
    read_unreflected_retention_gate,
};
pub use reflection_runs::{
    ClaimOutcome, FoldedRun, RETENTION_TOMBSTONE_PREFIX, RunClassification, claim_and_remove,
    classify_reflection_run, prune_reflection_runs, remove_tombstone,
};
pub use sidecars::{
    KIBITZER_SIDECAR_OWNER_LOCK_PURPOSE, KIBITZER_SIDECAR_RETENTION_MS,
    KIBITZER_SIDECAR_TOMBSTONE_PREFIX, PruneKibitzerSidecarsOptions, PruneKibitzerSidecarsResult,
    is_kibitzer_encoded_session, kibitzer_sidecar_owner_lock_path_for, kibitzer_sidecars_root,
    prune_kibitzer_sidecars,
};
pub use size::{
    DEFAULT_RUNTIME_MAX_BYTES, MeasuredRuntimeSize, RuntimeSize, RuntimeSizeLevel,
    RuntimeSizeStanding, RuntimeSizeUnknown, RuntimeStoreSize, format_runtime_bytes,
    measure_runtime_size, measure_runtime_size_within_budget, measure_tree_bytes,
    runtime_size_standing,
};
pub use stamp::{
    RETENTION_STAMP_CONFIG_KEY, RetentionResumePoints, clear_retention_cursor,
    read_retention_resume_points, retention_cursor_path, retention_resume_pending,
    write_retention_resume_points,
};
