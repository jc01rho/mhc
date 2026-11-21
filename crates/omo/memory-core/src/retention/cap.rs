//! Port of `retention/cap.ts` (omo #12538): the per-identity runtime size cap (design 3). After
//! retention, when `runtime/` still holds more than `max_bytes`, evict oldest-first through six
//! tiers, each exhausted before the next, until it fits. Every eviction goes through the retention
//! claim for its store (finalize lock + tombstone for runs, the journal lock for journals, the
//! owner lock + tombstone for sidecars), judged by the tier floor instead of the retention window,
//! so each store keeps exactly one deletion path. The run-id high water, the newest folded run,
//! reservations, live sessions, owned sidecars and contended locks are never evicted; completions
//! are not evictable at all. The abort signal is honoured between items.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

use super::guard::{
    ReflectionRetentionGuard, read_reflection_retention_guard, read_reservations, reserved_run_ids,
};
use super::journals::{
    JournalClass, Verdict, classify_journal_entry, remove_transcript_journal_under_lock,
};
use super::policy::{
    DAY_MS, RetentionContext, RetentionError, RetentionKeepReason, RuntimeRetentionPolicy, days_ms,
    list_names, lstat_or_none, newest_mtime,
};
use super::reflection_health_signal::{UnreflectedRetentionGate, read_unreflected_retention_gate};
use super::reflection_runs::{
    ClaimOutcome, RETENTION_TOMBSTONE_PREFIX, RunClassification, claim_and_remove,
    classify_reflection_run, remove_tombstone,
};
use super::sidecars::{
    KIBITZER_SIDECAR_TOMBSTONE_PREFIX, PruneKibitzerSidecarsOptions, claim_kibitzer_sidecar,
    is_kibitzer_encoded_session, kibitzer_sidecar_owner_lock_path_for, kibitzer_sidecars_root,
};
use super::size::{format_runtime_bytes, measure_runtime_size, measure_tree_bytes};
use crate::identity::layout::MemoryIdentityPaths;
use crate::journal::lock::{DefaultJournalLock, JournalLock};
use crate::reflection::run_id_high_water::{
    RunIdHighWaterPaths, reflection_run_number, scan_reflection_run_high_water,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapTier {
    FoldedRun,
    FullyReflectedTranscript,
    Sidecar,
    NeverReflectedJournal,
    PartlyReflectedTranscript,
    UnfoldedRun,
}

impl RuntimeCapTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FoldedRun => "folded-run",
            Self::FullyReflectedTranscript => "fully-reflected-transcript",
            Self::Sidecar => "sidecar",
            Self::NeverReflectedJournal => "never-reflected-journal",
            Self::PartlyReflectedTranscript => "partly-reflected-transcript",
            Self::UnfoldedRun => "unfolded-run",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapKeepReason {
    Reserved,
    ReflectionUnhealthy,
    UnreflectedRetentionOff,
    HighWater,
    Newest,
    NewestUnfolded,
    Locked,
    Recent,
    Live,
    Owned,
    ChangedDuringRemoval,
    StateUnreadable,
}

impl RuntimeCapKeepReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::ReflectionUnhealthy => "reflection-unhealthy",
            Self::UnreflectedRetentionOff => "unreflected-retention-off",
            Self::HighWater => "high-water",
            Self::Newest => "newest",
            Self::NewestUnfolded => "newest-unfolded",
            Self::Locked => "locked",
            Self::Recent => "recent",
            Self::Live => "live",
            Self::Owned => "owned",
            Self::ChangedDuringRemoval => "changed-during-removal",
            Self::StateUnreadable => "state-unreadable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapEvict {
    Removed,
    Keep(RuntimeCapKeepReason),
}

fn cap_outcome(outcome: ClaimOutcome) -> CapEvict {
    let reason = match outcome {
        ClaimOutcome::Removed => return CapEvict::Removed,
        ClaimOutcome::Keep(reason) => reason,
    };
    CapEvict::Keep(match reason {
        RetentionKeepReason::Reserved => RuntimeCapKeepReason::Reserved,
        RetentionKeepReason::ReflectionUnhealthy => RuntimeCapKeepReason::ReflectionUnhealthy,
        RetentionKeepReason::UnreflectedRetentionOff => {
            RuntimeCapKeepReason::UnreflectedRetentionOff
        }
        RetentionKeepReason::Locked => RuntimeCapKeepReason::Locked,
        RetentionKeepReason::Live => RuntimeCapKeepReason::Live,
        RetentionKeepReason::Owned => RuntimeCapKeepReason::Owned,
        RetentionKeepReason::ChangedDuringRemoval => RuntimeCapKeepReason::ChangedDuringRemoval,
        RetentionKeepReason::StateUnreadable => RuntimeCapKeepReason::StateUnreadable,
        _ => RuntimeCapKeepReason::Recent,
    })
}

/// The cap's own resume point: the `cap` field of the pass's cursor file (`runtime/retention.json`,
/// owned by the pass).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapCursor {
    pub tier: String,
    /// The tier's own ordering clock of the last item walked (`finishedAt` or newest mtime).
    pub clock: i64,
    pub last_name: String,
}

#[derive(Clone, Copy)]
pub struct RuntimeSizeCapOptions<'a> {
    pub context: RetentionContext<'a>,
    /// `memory.runtime.max_bytes`; `0` measures only.
    pub max_bytes: u64,
    /// Encoded sidecar directory names this process's live sessions own.
    pub owned_sidecars: Option<&'a BTreeSet<String>>,
    pub journal_lock: Option<&'a dyn JournalLock>,
    /// Idle floor for the journal tiers 2, 4 and 5. In session it stays at 24 h for tiers 4/5 (tier
    /// 2 has none); a caller that cannot see live sessions (the CLI fix pass, the host sweep)
    /// passes `retention_days`, so an idle but alive session never loses its journal to it.
    pub journal_floor_ms: Option<i64>,
    /// `pass_budget_ms` left for the cap; charged from the end of the measurement walk.
    pub budget_ms: Option<u64>,
    /// Monotonic timer for the budget in ms; separate from `context.now`, which dates the stores.
    pub clock: Option<&'a dyn Fn() -> u64>,
    /// Deadline of the measurement walk (the pass's budget and abort signal joined); firing it
    /// records `measure-over-budget`.
    pub measure_cancel: Option<&'a dyn Fn() -> bool>,
    /// Cursor of an interrupted cap pass: that tier's items up to it are skipped.
    pub resume: Option<&'a RuntimeCapCursor>,
}

impl<'a> RuntimeSizeCapOptions<'a> {
    pub fn new(context: RetentionContext<'a>, max_bytes: u64) -> Self {
        Self {
            context,
            max_bytes,
            owned_sidecars: None,
            journal_lock: None,
            journal_floor_ms: None,
            budget_ms: None,
            clock: None,
            measure_cancel: None,
            resume: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapEviction {
    pub tier: RuntimeCapTier,
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSizeCapReport {
    pub max_bytes: u64,
    pub before_bytes: u64,
    pub after_bytes: u64,
    /// Bytes still over the cap after the pass; nonzero means nothing evictable was left.
    pub exceeded_bytes: u64,
    pub evicted: Vec<RuntimeCapEviction>,
    pub kept: BTreeMap<String, RuntimeCapKeepReason>,
    /// `reflection-scheduler-busy` when the lock was contended, so the journal tiers were skipped
    /// this pass.
    pub skipped: Vec<String>,
    /// Set when the health gate suspended tiers 4 and 5: the newest failed completion's
    /// `finishedAt`.
    pub suspended_since: Option<String>,
    /// Set when the budget ran out before the cap fit; the next pass resumes there.
    pub cursor: Option<RuntimeCapCursor>,
    /// How long the measurement walk took on `clock`.
    pub measure_ms: u64,
    /// Bytes in never/partly-reflected journals tiers 4/5 left in place (switch off or health gate
    /// closed).
    pub unreflected_held_bytes: u64,
}

const RUNTIME_CAP_SIDECAR_FLOOR_MS: i64 = 60 * 60 * 1000;
const RUNTIME_CAP_JOURNAL_FLOOR_MS: i64 = 24 * 60 * 60 * 1000;
const RUNTIME_CAP_KEPT_UNFOLDED_RUNS: usize = 10;
const TIER_COUNT: usize = 6;

fn default_clock() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[derive(Clone)]
enum EvictVia {
    FoldedRun,
    UnfoldedRun {
        cutoff: i64,
    },
    Journal {
        policy: RuntimeRetentionPolicy,
        gate: Option<UnreflectedRetentionGate>,
    },
    Sidecar,
}

#[derive(Clone)]
struct Candidate {
    tier: RuntimeCapTier,
    name: String,
    at: i64,
    /// What the eviction frees: the whole entry, or the transcript file for transcript-only tiers.
    measured_path: PathBuf,
    via: EvictVia,
}

#[derive(Default)]
struct GateState {
    suspended_since: Option<String>,
    held_bytes: u64,
}

#[derive(Clone, Default)]
struct RunCandidates {
    folded: Vec<Candidate>,
    unfolded: Vec<Candidate>,
}

#[derive(Clone, Default)]
struct JournalCandidates {
    fully: Vec<Candidate>,
    never: Vec<Candidate>,
    partly: Vec<Candidate>,
}

struct CapScan<'o, 'a> {
    options: &'o RuntimeSizeCapOptions<'a>,
    guard: Option<ReflectionRetentionGuard>,
    kept: RefCell<BTreeMap<String, RuntimeCapKeepReason>>,
    gate: RefCell<GateState>,
    runs: RunCandidates,
    journal_scan: RefCell<Option<JournalCandidates>>,
}

pub fn run_runtime_size_cap(
    options: &RuntimeSizeCapOptions<'_>,
) -> Result<RuntimeSizeCapReport, RetentionError> {
    let context = &options.context;
    context.check_abort()?;
    let default_clock_fn: fn() -> u64 = default_clock;
    let clock: &dyn Fn() -> u64 = options.clock.unwrap_or(&default_clock_fn);
    let mut evicted: Vec<RuntimeCapEviction> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    clear_tombstones(options)?;
    let measure_start = clock();
    let resume_cursor = options.resume.cloned();
    let before_bytes = match measure_runtime_size(&context.paths.runtime, options.measure_cancel) {
        Ok(size) => size.bytes,
        Err(error) => {
            context.check_abort()?;
            if !options.measure_cancel.is_some_and(|cancel| cancel()) {
                return Err(error);
            }
            skipped.push("measure-over-budget".to_string());
            return Ok(RuntimeSizeCapReport {
                max_bytes: options.max_bytes,
                before_bytes: 0,
                after_bytes: 0,
                exceeded_bytes: 0,
                evicted,
                kept: BTreeMap::new(),
                skipped,
                suspended_since: None,
                cursor: resume_cursor,
                measure_ms: clock().saturating_sub(measure_start),
                unreflected_held_bytes: 0,
            });
        }
    };
    let measure_end = clock();
    let measure_ms = measure_end.saturating_sub(measure_start);
    let unchanged = |skipped: Vec<String>, cursor: Option<RuntimeCapCursor>| RuntimeSizeCapReport {
        max_bytes: options.max_bytes,
        before_bytes,
        after_bytes: before_bytes,
        exceeded_bytes: if options.max_bytes > 0 {
            before_bytes.saturating_sub(options.max_bytes)
        } else {
            0
        },
        evicted: Vec::new(),
        kept: BTreeMap::new(),
        skipped,
        suspended_since: None,
        cursor,
        measure_ms,
        unreflected_held_bytes: 0,
    };
    if options.budget_ms.is_some_and(|budget| measure_ms > budget) {
        skipped.push("measure-over-budget".to_string());
        return Ok(unchanged(skipped, resume_cursor));
    }
    if options.max_bytes == 0 || before_bytes <= options.max_bytes {
        return Ok(unchanged(skipped, None));
    }
    let guard = read_reflection_retention_guard(context)?;
    if guard.is_none() {
        skipped.push("reflection-scheduler-busy".to_string());
    }
    let deadline = options.budget_ms.map(|budget| measure_end + budget);
    let scan = CapScan::new(options, guard)?;
    let mut bytes = before_bytes;
    let mut last: Option<Candidate> = None;
    let mut cursor: Option<RuntimeCapCursor> = None;
    'walk: for tier in 0..TIER_COUNT {
        for candidate in scan.tier(tier)? {
            if bytes <= options.max_bytes {
                break 'walk;
            }
            if is_before_cursor(&candidate, options.resume) {
                continue;
            }
            context.check_abort()?;
            if deadline.is_some_and(|deadline| clock() >= deadline) {
                cursor = match &last {
                    None => resume_cursor.clone(),
                    Some(last) => Some(RuntimeCapCursor {
                        tier: last.tier.as_str().to_string(),
                        clock: last.at,
                        last_name: last.name.clone(),
                    }),
                };
                break 'walk;
            }
            let size = measure_tree_bytes(&candidate.measured_path, None)?;
            let outcome = scan.evict(&candidate)?;
            last = Some(candidate.clone());
            match outcome {
                CapEvict::Removed => {
                    bytes = bytes.saturating_sub(size);
                    evicted.push(RuntimeCapEviction {
                        tier: candidate.tier,
                        name: candidate.name.clone(),
                        bytes: size,
                    });
                }
                CapEvict::Keep(reason) => {
                    scan.kept
                        .borrow_mut()
                        .insert(candidate.name.clone(), reason);
                }
            }
        }
    }
    let after_bytes = if evicted.is_empty() {
        before_bytes
    } else {
        measure_runtime_size(&context.paths.runtime, None)?.bytes
    };
    let gate = scan.gate.into_inner();
    Ok(RuntimeSizeCapReport {
        max_bytes: options.max_bytes,
        before_bytes,
        after_bytes,
        exceeded_bytes: after_bytes.saturating_sub(options.max_bytes),
        evicted,
        kept: scan.kept.into_inner(),
        skipped,
        suspended_since: gate.suspended_since,
        cursor,
        measure_ms,
        unreflected_held_bytes: gate.held_bytes,
    })
}

#[derive(Clone, Copy)]
pub struct RuntimeCapEvictableOptions<'a> {
    pub paths: &'a MemoryIdentityPaths,
    pub policy: RuntimeRetentionPolicy,
    pub now: &'a dyn Fn() -> i64,
    /// `memory.reflection.enabled` for the identity; when false the health gate never closes.
    pub reflection_enabled: Option<bool>,
    pub journal_floor_ms: Option<i64>,
    /// The doctors' measurement deadline: once it fires the measure fails with the abort.
    pub cancel: Option<&'a dyn Fn() -> bool>,
}

/// Bytes the next cap pass could evict now, for the doctors (#12538 review M1): every tier's
/// candidates, from a lock-free read of the run-id high water and the reservations, and nothing is
/// written. The doctors see neither live sessions nor the claims' locks, so this is an upper bound.
pub fn measure_runtime_cap_evictable_bytes(
    options: &RuntimeCapEvictableOptions<'_>,
) -> Result<u64, RetentionError> {
    let reservations = read_reservations(&options.paths.reflection);
    let guard = ReflectionRetentionGuard {
        high_water: scan_reflection_run_high_water(RunIdHighWaterPaths::from(options.paths)),
        reserved: reservations.run_ids,
        reserved_conversation_ids: reservations.conversation_ids,
    };
    let mut context = RetentionContext::new(options.paths, options.policy, options.now);
    context.reflection_enabled = options.reflection_enabled;
    context.cancel = options.cancel;
    let mut cap_options = RuntimeSizeCapOptions::new(context, 0);
    cap_options.journal_floor_ms = options.journal_floor_ms;
    let scan = CapScan::new(&cap_options, Some(guard))?;
    let mut bytes = 0;
    for tier in 0..TIER_COUNT {
        for candidate in scan.tier(tier)? {
            context.check_abort()?;
            bytes += measure_tree_bytes(&candidate.measured_path, None)?;
        }
    }
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCapExcess {
    /// Bytes over `max_bytes`; nothing is described at or under the cap.
    pub over_cap_bytes: u64,
    /// [`measure_runtime_cap_evictable_bytes`].
    pub evictable_bytes: u64,
    /// Bytes in never/partly-reflected journals.
    pub unreflected_bytes: u64,
    pub discard_unreflected_journals: bool,
}

/// The over-cap line both doctors print (`/doctor` and `omo doctor`). "Nothing evictable; held by
/// unreflected journals" is said only when no tier has anything to evict and the unreflected
/// journals the switch keeps are enough to hold the excess; otherwise the line says what the next
/// pass evicts.
pub fn describe_runtime_cap_excess(input: &RuntimeCapExcess) -> Option<String> {
    if input.over_cap_bytes == 0 {
        return None;
    }
    let over = format!(
        "cap exceeded by {}",
        format_runtime_bytes(input.over_cap_bytes)
    );
    let held = (!input.discard_unreflected_journals && input.unreflected_bytes > 0).then(|| {
        format!(
            "held by unreflected journals: {} (discard_unreflected_journals off)",
            format_runtime_bytes(input.unreflected_bytes)
        )
    });
    if input.evictable_bytes >= input.over_cap_bytes {
        return Some(format!(
            "{over}; the next maintenance pass evicts it ({} evictable)",
            format_runtime_bytes(input.evictable_bytes)
        ));
    }
    if input.evictable_bytes > 0 {
        return Some(format!(
            "{over}; the next maintenance pass evicts up to {} of it{}",
            format_runtime_bytes(input.evictable_bytes),
            held.map_or_else(String::new, |held| format!("; {held}"))
        ));
    }
    if let Some(held) = &held
        && input.over_cap_bytes <= input.unreflected_bytes
    {
        return Some(format!("{over}, nothing evictable; {held}"));
    }
    Some(format!(
        "{over}, nothing evictable: live sessions, reserved runs and the newest runs are never evicted{}",
        held.map_or_else(String::new, |held| format!("; {held}"))
    ))
}

fn is_before_cursor(candidate: &Candidate, cursor: Option<&RuntimeCapCursor>) -> bool {
    let Some(cursor) = cursor else {
        return false;
    };
    if candidate.tier.as_str() != cursor.tier {
        return false;
    }
    candidate.at < cursor.clock
        || (candidate.at == cursor.clock && candidate.name.as_str() <= cursor.last_name.as_str())
}

fn clear_tombstones(options: &RuntimeSizeCapOptions<'_>) -> Result<(), RetentionError> {
    let context = &options.context;
    let runs_dir = context.paths.reflection.join("runs");
    for name in list_names(&runs_dir)? {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            remove_tombstone(&runs_dir.join(name), context.warn);
        }
    }
    let sidecars_dir = kibitzer_sidecars_root(&context.paths.recall);
    for name in list_names(&sidecars_dir)? {
        if name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX) {
            remove_tombstone(&sidecars_dir.join(name), context.warn);
        }
    }
    Ok(())
}

fn by_age(left: &(String, i64), right: &(String, i64)) -> std::cmp::Ordering {
    left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0))
}

impl<'o, 'a> CapScan<'o, 'a> {
    /// A contended scheduler lock means a reservation is being made: its journals are unknown, so
    /// the journal tiers wait for the next pass (B15). The run tiers still run on a lock-free
    /// read, because every run claim re-reads the reservations under the run's finalize lock and
    /// the high water only ever grows past the oldest candidates.
    fn new(
        options: &'o RuntimeSizeCapOptions<'a>,
        guard: Option<ReflectionRetentionGuard>,
    ) -> Result<Self, RetentionError> {
        let context = &options.context;
        let run_guard = guard.clone().unwrap_or_else(|| ReflectionRetentionGuard {
            high_water: scan_reflection_run_high_water(RunIdHighWaterPaths::from(context.paths)),
            reserved: reserved_run_ids(&context.paths.reflection),
            reserved_conversation_ids: BTreeSet::new(),
        });
        let mut kept = BTreeMap::new();
        let runs = run_candidates(options, &run_guard, &mut kept)?;
        Ok(Self {
            options,
            guard,
            kept: RefCell::new(kept),
            gate: RefCell::new(GateState::default()),
            runs,
            journal_scan: RefCell::new(None),
        })
    }

    /// Journals are classified only when the walk reaches their first tier, so a pass that fits
    /// on tier 1 reads no transcript (#12538 review r3 L1).
    fn journals(&self) -> Result<Option<JournalCandidates>, RetentionError> {
        let Some(guard) = &self.guard else {
            return Ok(None);
        };
        if let Some(scan) = self.journal_scan.borrow().as_ref() {
            return Ok(Some(scan.clone()));
        }
        let scan = journal_candidates(
            self.options,
            &guard.reserved_conversation_ids,
            &mut self.kept.borrow_mut(),
        )?;
        *self.journal_scan.borrow_mut() = Some(scan.clone());
        Ok(Some(scan))
    }

    fn tier(&self, index: usize) -> Result<Vec<Candidate>, RetentionError> {
        match index {
            0 => Ok(self.runs.folded.clone()),
            1 => Ok(self
                .journals()?
                .map(|journals| journals.fully)
                .unwrap_or_default()),
            2 => sidecar_candidates(self.options, &mut self.kept.borrow_mut()),
            3 => {
                let entries = self
                    .journals()?
                    .map(|journals| journals.never)
                    .unwrap_or_default();
                self.unfolded(entries)
            }
            4 => {
                let entries = self
                    .journals()?
                    .map(|journals| journals.partly)
                    .unwrap_or_default();
                self.unfolded(entries)
            }
            _ => Ok(self.runs.unfolded.clone()),
        }
    }

    fn unfolded(&self, entries: Vec<Candidate>) -> Result<Vec<Candidate>, RetentionError> {
        if entries.is_empty() {
            return Ok(entries);
        }
        let context = &self.options.context;
        // `discard_unreflected_journals` (default off) gates every rule that deletes unreflected
        // input.
        if !context.policy.discard_unreflected_journals {
            return self.hold(&entries, RuntimeCapKeepReason::UnreflectedRetentionOff);
        }
        let health = read_unreflected_retention_gate(
            &context.paths.reflection,
            context.reflection_enabled(),
            &|| true,
        )?;
        let Some(health) = health else {
            return self.hold(&entries, RuntimeCapKeepReason::ReflectionUnhealthy);
        };
        if health.open {
            return Ok(entries);
        }
        if let Some(newest) = &health.newest {
            self.gate.borrow_mut().suspended_since = Some(newest.at.clone());
        }
        self.hold(&entries, RuntimeCapKeepReason::ReflectionUnhealthy)
    }

    fn hold(
        &self,
        entries: &[Candidate],
        reason: RuntimeCapKeepReason,
    ) -> Result<Vec<Candidate>, RetentionError> {
        for entry in entries {
            self.kept.borrow_mut().insert(entry.name.clone(), reason);
            let bytes = measure_tree_bytes(&entry.measured_path, None)?;
            self.gate.borrow_mut().held_bytes += bytes;
        }
        Ok(Vec::new())
    }

    fn evict(&self, candidate: &Candidate) -> Result<CapEvict, RetentionError> {
        let context = &self.options.context;
        match &candidate.via {
            EvictVia::FoldedRun => {
                let runs_dir = context.paths.reflection.join("runs");
                Ok(cap_outcome(claim_and_remove(
                    context,
                    &runs_dir,
                    &candidate.name,
                    i64::MAX,
                )?))
            }
            EvictVia::UnfoldedRun { cutoff } => {
                let runs_dir = context.paths.reflection.join("runs");
                Ok(cap_outcome(claim_and_remove(
                    context,
                    &runs_dir,
                    &candidate.name,
                    *cutoff,
                )?))
            }
            EvictVia::Journal { policy, gate } => {
                let journal_context = RetentionContext {
                    policy: *policy,
                    ..*context
                };
                let default_lock = DefaultJournalLock;
                let lock: &dyn JournalLock = match self.options.journal_lock {
                    Some(lock) => lock,
                    None => &default_lock,
                };
                let dir = context.paths.transcripts.join(&candidate.name);
                match remove_transcript_journal_under_lock(
                    &journal_context,
                    &dir,
                    lock,
                    gate.as_ref(),
                ) {
                    Ok(outcome) => Ok(cap_outcome(outcome)),
                    Err(error) => {
                        if context.aborted() {
                            Err(error)
                        } else {
                            Ok(CapEvict::Keep(RuntimeCapKeepReason::StateUnreadable))
                        }
                    }
                }
            }
            EvictVia::Sidecar => {
                let root = kibitzer_sidecars_root(&context.paths.recall);
                let mut sidecar_options =
                    PruneKibitzerSidecarsOptions::new(&context.paths.recall, &context.paths.locks);
                sidecar_options.warn = context.warn;
                Ok(cap_outcome(claim_kibitzer_sidecar(
                    &root.join(&candidate.name),
                    &kibitzer_sidecar_owner_lock_path_for(&context.paths.locks, &candidate.name),
                    context.now_ms(),
                    RUNTIME_CAP_SIDECAR_FLOOR_MS,
                    &sidecar_options,
                )))
            }
        }
    }
}

fn run_candidates(
    options: &RuntimeSizeCapOptions<'_>,
    guard: &ReflectionRetentionGuard,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> Result<RunCandidates, RetentionError> {
    let context = &options.context;
    let runs_dir = context.paths.reflection.join("runs");
    let mut folded: Vec<(String, i64)> = Vec::new();
    let mut unfolded: Vec<(String, i64)> = Vec::new();
    for name in list_names(&runs_dir)? {
        if name.starts_with(RETENTION_TOMBSTONE_PREFIX) {
            continue;
        }
        match classify_reflection_run(&runs_dir.join(&name), &name) {
            RunClassification::Folded(run) => folded.push((name, run.finished_at)),
            RunClassification::Keep(RetentionKeepReason::UnfoldedTerminal) => {
                let at = newest_mtime(&runs_dir.join(&name))?.unwrap_or(0);
                unfolded.push((name, at));
            }
            RunClassification::Keep(_) => {}
        }
    }
    folded.sort_by(by_age);
    unfolded.sort_by(by_age);
    let exempt = |name: &str| -> Option<RuntimeCapKeepReason> {
        if reflection_run_number(name) == guard.high_water {
            Some(RuntimeCapKeepReason::HighWater)
        } else if guard.reserved.contains(name) {
            Some(RuntimeCapKeepReason::Reserved)
        } else {
            None
        }
    };
    let newest_folded = folded.last().map(|(name, _)| name.clone());
    let mut folded_candidates = Vec::new();
    for (name, at) in &folded {
        let reason = exempt(name).or_else(|| {
            (newest_folded.as_deref() == Some(name.as_str()))
                .then_some(RuntimeCapKeepReason::Newest)
        });
        if let Some(reason) = reason {
            kept.insert(name.clone(), reason);
            continue;
        }
        folded_candidates.push(Candidate {
            tier: RuntimeCapTier::FoldedRun,
            name: name.clone(),
            at: *at,
            measured_path: runs_dir.join(name),
            via: EvictVia::FoldedRun,
        });
    }
    let retention_cutoff = context.now_ms() - days_ms(context.policy.retention_days);
    let kept_unfolded: BTreeSet<&String> = unfolded
        .iter()
        .rev()
        .take(RUNTIME_CAP_KEPT_UNFOLDED_RUNS)
        .map(|(name, _)| name)
        .collect();
    let mut unfolded_candidates = Vec::new();
    for (name, at) in &unfolded {
        let reason = exempt(name)
            .or_else(|| {
                kept_unfolded
                    .contains(name)
                    .then_some(RuntimeCapKeepReason::NewestUnfolded)
            })
            .or_else(|| (*at > retention_cutoff).then_some(RuntimeCapKeepReason::Recent));
        if let Some(reason) = reason {
            kept.insert(name.clone(), reason);
            continue;
        }
        unfolded_candidates.push(Candidate {
            tier: RuntimeCapTier::UnfoldedRun,
            name: name.clone(),
            at: *at,
            measured_path: runs_dir.join(name),
            via: EvictVia::UnfoldedRun {
                cutoff: retention_cutoff,
            },
        });
    }
    Ok(RunCandidates {
        folded: folded_candidates,
        unfolded: unfolded_candidates,
    })
}

fn journal_candidates(
    options: &RuntimeSizeCapOptions<'_>,
    reserved: &BTreeSet<String>,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> Result<JournalCandidates, RetentionError> {
    let context = &options.context;
    // The tier floors stand in for the retention windows: fully reflected at any age, the rest
    // idle 24 h, unless the caller raised every journal floor (`journal_floor_ms`).
    let floor_days = options
        .journal_floor_ms
        .unwrap_or(RUNTIME_CAP_JOURNAL_FLOOR_MS) as f64
        / DAY_MS as f64;
    let floor_policy = RuntimeRetentionPolicy {
        retention_days: if options.journal_floor_ms.is_none() {
            0.0
        } else {
            floor_days
        },
        abandoned_journal_days: floor_days,
        ..context.policy
    };
    // Classes come from an open gate with the switch on; the switch and the health gate are
    // applied per tier below (with the held bytes reported), and the locked claim re-checks the
    // real gate.
    let classify_context = RetentionContext {
        policy: RuntimeRetentionPolicy {
            discard_unreflected_journals: true,
            ..floor_policy
        },
        ..*context
    };
    let real_gate = read_unreflected_retention_gate(
        &context.paths.reflection,
        context.reflection_enabled(),
        &|| true,
    )?;
    let open_gate = UnreflectedRetentionGate::open();
    let mut fully: Vec<(String, i64)> = Vec::new();
    let mut never: Vec<(String, i64)> = Vec::new();
    let mut partly: Vec<(String, i64)> = Vec::new();
    for session_id in list_names(&context.paths.transcripts)? {
        let dir = context.paths.transcripts.join(&session_id);
        if context.is_live(&session_id) {
            kept.insert(session_id, RuntimeCapKeepReason::Live);
            continue;
        }
        if reserved.contains(&session_id) {
            kept.insert(session_id, RuntimeCapKeepReason::Reserved);
            continue;
        }
        let verdict = classify_journal_entry(&classify_context, &dir, &open_gate)?;
        match verdict {
            Some(Verdict::Keep(RetentionKeepReason::StateUnreadable)) => {
                kept.insert(session_id, RuntimeCapKeepReason::StateUnreadable);
            }
            Some(Verdict::Class(kind)) => {
                let at = newest_mtime(&dir)?.unwrap_or(0);
                match kind {
                    JournalClass::Fully => fully.push((session_id, at)),
                    JournalClass::Never => never.push((session_id, at)),
                    JournalClass::Partly => partly.push((session_id, at)),
                }
            }
            Some(Verdict::Keep(_)) | None => {}
        }
    }
    let to_candidates = |mut entries: Vec<(String, i64)>,
                         tier: RuntimeCapTier,
                         transcript_only: bool|
     -> Vec<Candidate> {
        entries.sort_by(by_age);
        entries
            .into_iter()
            .map(|(name, at)| {
                let dir = context.paths.transcripts.join(&name);
                Candidate {
                    tier,
                    at,
                    measured_path: if transcript_only {
                        dir.join("transcript.jsonl")
                    } else {
                        dir
                    },
                    via: EvictVia::Journal {
                        policy: floor_policy,
                        gate: real_gate.clone(),
                    },
                    name,
                }
            })
            .collect()
    };
    Ok(JournalCandidates {
        fully: to_candidates(fully, RuntimeCapTier::FullyReflectedTranscript, true),
        never: to_candidates(never, RuntimeCapTier::NeverReflectedJournal, false),
        partly: to_candidates(partly, RuntimeCapTier::PartlyReflectedTranscript, true),
    })
}

fn sidecar_candidates(
    options: &RuntimeSizeCapOptions<'_>,
    kept: &mut BTreeMap<String, RuntimeCapKeepReason>,
) -> Result<Vec<Candidate>, RetentionError> {
    let context = &options.context;
    let root = kibitzer_sidecars_root(&context.paths.recall);
    let mut entries: Vec<(String, i64)> = Vec::new();
    for name in list_names(&root)? {
        if name.starts_with(KIBITZER_SIDECAR_TOMBSTONE_PREFIX)
            || !is_kibitzer_encoded_session(&name)
        {
            continue;
        }
        if options
            .owned_sidecars
            .is_some_and(|owned| owned.contains(&name))
        {
            kept.insert(name, RuntimeCapKeepReason::Owned);
            continue;
        }
        let path = root.join(&name);
        if !lstat_or_none(&path).is_some_and(|stats| stats.is_dir()) {
            continue;
        }
        let at = newest_mtime(&path)?.unwrap_or(0);
        if context.now_ms() - at < RUNTIME_CAP_SIDECAR_FLOOR_MS {
            kept.insert(name, RuntimeCapKeepReason::Recent);
            continue;
        }
        entries.push((name, at));
    }
    entries.sort_by(by_age);
    Ok(entries
        .into_iter()
        .map(|(name, at)| Candidate {
            tier: RuntimeCapTier::Sidecar,
            at,
            measured_path: root.join(&name),
            via: EvictVia::Sidecar,
            name,
        })
        .collect())
}
