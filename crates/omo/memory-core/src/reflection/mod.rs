//! Reflection scheduling, isolated worktrees, state machine, and persona assets.

pub mod abandoned;
pub mod assets;
pub mod completion_links;
pub mod completion_repair;
pub mod completion_secret_scan;
pub mod completion_validation;
pub mod description_preflight;
pub mod host_sweep;
pub mod identity_orphan_sweep;
pub mod link_removals;
pub mod link_rewrite;
pub mod machine;
pub mod orphan_sweep;
pub mod quarantine;
pub mod release_without_promotion;
pub mod reservation;
pub mod run_id_high_water;
pub mod run_ledger;
pub mod run_process;
pub mod stale_holder;
pub mod stale_lock_reaper;
pub mod terminal_claim;
pub mod worktree;
pub mod worktree_integration;

pub use abandoned::{AbandonedLauncher, AbandonedReason, AbandonedRecord, write_abandoned};
pub use assets::{
    DREAM_PERSONA_MARKDOWN, REFLECTION_PERSONA_MARKDOWN, ReflectionPersona,
    ReflectionPersonaSection, load_dream_persona, load_reflection_persona, parse_sections,
};
pub use completion_links::{
    LinkRepair, RecognizedLinkRepair, apply_link_repair, recognize_link_repair,
};
pub use completion_repair::{
    DROPPED_TRAILER, DroppedEntry, STABLE_LOG_OUTPUT, commit_count, describe_dropped_entries,
    squash_without_paths,
};
pub use completion_secret_scan::{
    SecretLikeFailure, find_secret_like_failure, find_secret_like_failures,
};
pub use completion_validation::{
    CompletionValidation, frontmatter_block, list_reflection_changed_paths, paths_outside_target,
    read_dropped_trailers, validate_completion,
};
pub use description_preflight::{
    DescriptionPreflightWarn, TrimmedDescription, preflight_reflection_descriptions,
};
pub use host_sweep::{
    HOST_SWEEP_LOCK_FILE, HOST_SWEEP_STATE_FILE, HostSweepIdentity, HostSweepOptions,
    HostSweepResult, HostSweepState, HostSweepStep, read_host_sweep_state, sweep_host_identities,
};
pub use identity_orphan_sweep::{
    IdentityOrphanSweepOptions, IdentityOrphanSweepReport, sweep_identity_orphan_worktrees,
};
pub use machine::{
    CapturedConversation, CompleteOptions, CompleteTransition, DreamOrigin, EvaluationAction,
    EvaluationResult, JournalSnapshot, MachineState, ReflectionEvent, ReflectionOutcome,
    ReflectionRequest, ReflectionTrigger, ReservationState, ReservedRun, TriggerConfig,
    complete_transition, evaluate_transitions, reserve_transition,
};
pub use orphan_sweep::{
    OrphanKind, REFLECTION_ORPHAN_GRACE_MS, ReflectionLeftovers, ReflectionOrphanReceipt,
    ReflectionOrphanSelection, ReflectionOrphanSweepOptions, RegisteredReflectionWorktree,
    list_reflection_leftovers, select_reflection_orphans, sweep_reflection_orphans,
};
pub use quarantine::{
    QUARANTINE_FILENAME, QuarantineReason, QuarantineRecord, RESERVATION_EVIDENCE_FILENAME,
    write_quarantine,
};
pub use release_without_promotion::{
    ReleaseError, ReleaseWithoutPromotionOptions, ReleaseWithoutPromotionResult,
    release_reservation_without_promotion,
};
pub use reservation::{
    CompletionResult, ReflectionLauncherIdentity, ReflectionReservationStore,
    ReflectionReservationStoreOptions, ReservationError, ReservationResult,
};
pub use reservation::{is_reserved_run, move_torn_reservation_aside};
pub use run_id_high_water::{
    REFLECTION_RUN_ID_PREFIX, RunIdHighWaterPaths, read_reflection_run_id_mark,
    record_reflection_run_id_mark, reflection_run_number, scan_reflection_run_high_water,
};
pub use run_ledger::{ReservationLedgerError, ReservationRunLedger, parse_reservation_run_ledger};
pub use run_process::{
    RecordedProcessSeams, RecordedProcessVerdict, RecordedStart, classify_recorded_process,
    classify_recorded_process_lenient,
};
pub use stale_holder::{
    LockReadError, ReflectionLockProbe, ReflectionLockVerdict, SystemReflectionLockProbe,
    classify_reflection_lock_holder,
};
pub use stale_lock_reaper::{
    DeadOwnerLockReport, QuarantineStrandedRunOptions, REFLECTION_LAUNCH_WINDOW_MS,
    STRANDED_RUN_GRACE_MS, StaleLockDecision, StaleLockProbe, StaleLockReapAction,
    StaleLockReaperAction, StaleLockReaperOptions, StaleLockReaperReport, StrandedRunAction,
    StrandedRunDecision, StrandedRunReport, TORN_LOCK_MIN_AGE_MS, TombstoneReport,
    decide_stale_reflection_lock, decide_stranded_run, format_stale_lock_fix_lines,
    is_settled_under_dead_launcher, quarantine_stranded_run, reclaim_dead_owner_locks,
    run_stale_lock_reaper, stranded_since,
};
pub use terminal_claim::{
    RunTerminalClaim, RunTerminalClaimKind, RunTerminalPublication, TerminalClaimError,
    claim_run_terminal, read_run_terminal_claim, read_run_terminal_publication,
    run_terminal_claim_matches,
};
pub use worktree::{
    ReflectionCleanupReceipt, ReflectionFinalizeMode, ReflectionFinalizeResult, ReflectionWorktree,
    ReflectionWorktreeIdentity, WorktreeError, create_reflection_worktree,
    discard_reflection_worktree, finalize_reflection_worktree,
    finalize_reflection_worktree_with_warn,
};
pub use worktree_integration::{
    IntegrateValidatedReflectionInput, LegacyAutoRunReceiptProbe, ReflectionIntegrationMode,
    ReflectionIntegrationProbe, ReflectionIntegrationResult, ValidatedReflectionTip,
    cleanup_reflection_worktree, integrate_validated_reflection, probe_legacy_auto_run_receipt,
    probe_reflection_integration,
};

pub mod stale_holder_proof;
