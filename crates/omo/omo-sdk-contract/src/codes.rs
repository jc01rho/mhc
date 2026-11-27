//! The code registry of design 4.4: every failure code a P1a row returns in its refusal scenarios, the namespaces
//! that may return it, and the form of its `next_action` (the SDK operation to call next, never a CLI verb). A code
//! of the source lists that no P1a row returns is in [`HELD_CODES`] with the reason.

/// One registered code. `retires_with` names the slice after which no root call returns the code any more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeEntry {
    pub namespaces: &'static [&'static str],
    pub next_action: &'static str,
    pub retires_with: Option<&'static str>,
}

pub const OMO_CODE_REGISTRY: &[(&str, CodeEntry)] = &[
    (
        "accounts_unreachable",
        CodeEntry {
            namespaces: &["account"],
            next_action: "The sign-in page did not answer; check the network or the accounts origin, then call account.signIn again.",
            retires_with: None,
        },
    ),
    (
        "already_signed_in",
        CodeEntry {
            namespaces: &["account"],
            next_action: "This device is signed in; call account.whoami, or account.signOut before signing in again.",
            retires_with: None,
        },
    ),
    (
        "device_limit",
        CodeEntry {
            namespaces: &["account"],
            next_action: "The account is at its device limit; revoke one listed in details (devices.revoke), then sign in again.",
            retires_with: None,
        },
    ),
    (
        "machine_exists",
        CodeEntry {
            namespaces: &["account"],
            next_action: "This machine already has a device on the account; sign in with replace to take it over.",
            retires_with: None,
        },
    ),
    (
        "alias_taken",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Choose another alias, or clear it from its holder (details.holder) first.",
            retires_with: None,
        },
    ),
    (
        "ambiguous_target",
        CodeEntry {
            namespaces: &["devices", "sync", "threads"],
            next_action: "Pick one candidate from details.candidates and call it again with its exact address.",
            retires_with: None,
        },
    ),
    (
        "approval_denied",
        CodeEntry {
            namespaces: &["sync"],
            next_action: "The person declined; nothing changed.",
            retires_with: None,
        },
    ),
    (
        "approval_expired",
        CodeEntry {
            namespaces: &["sync"],
            next_action: "Start again; the approval window passed.",
            retires_with: None,
        },
    ),
    (
        "caller_context_missing",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Name the thread: self is an address only a session can use.",
            retires_with: None,
        },
    ),
    (
        "cancelled",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call it again if it is still wanted.",
            retires_with: None,
        },
    ),
    (
        "chain_count_limit",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "End the chain; it is longer than the delivery chain limit.",
            retires_with: None,
        },
    ),
    (
        "credential_store_locked",
        CodeEntry {
            namespaces: &["account"],
            next_action: "Unlock the credential store, then retry.",
            retires_with: None,
        },
    ),
    (
        "credential_store_unavailable",
        CodeEntry {
            namespaces: &["account"],
            next_action: "Call account.store.status to see where credentials live, then retry.",
            retires_with: None,
        },
    ),
    (
        "cursor_invalid",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Read again without a cursor, or with the next_cursor a read answered.",
            retires_with: None,
        },
    ),
    (
        "cursor_stale",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Read again without a cursor; the transcript moved past it.",
            retires_with: None,
        },
    ),
    (
        "delivered_unconfirmed",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "It may have run or may still run; call threads.read or threads.receipt before sending again.",
            retires_with: None,
        },
    ),
    (
        "desktop_sign_in",
        CodeEntry {
            namespaces: &["account", "devices"],
            next_action: "This machine is signed in through the desktop app; act there.",
            retires_with: None,
        },
    ),
    (
        "device_offline",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call devices.list; retry once the device is online.",
            retires_with: None,
        },
    ),
    (
        "device_service_unavailable",
        CodeEntry {
            namespaces: &["deviceService"],
            next_action: "Call deviceService.status, then retry.",
            retires_with: None,
        },
    ),
    (
        "foreign_live_owner",
        CodeEntry {
            namespaces: &["daemon", "diagnostics"],
            next_action: "Call threads.locate; the thread is live under another owner, so act through that owner.",
            retires_with: None,
        },
    ),
    (
        "gateway_schema_too_new",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Update omo (install.update), restart this session, then retry.",
            retires_with: None,
        },
    ),
    (
        "host_error",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "It may have run; read the session's current state before resending.",
            retires_with: None,
        },
    ),
    (
        "host_not_answering",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call devices.list to see whether the device is online, then retry.",
            retires_with: None,
        },
    ),
    (
        "host_unavailable",
        CodeEntry {
            namespaces: &["daemon", "deviceService", "devices", "threads"],
            next_action: "Call deviceService.status or daemon.status to see why, then retry.",
            retires_with: None,
        },
    ),
    (
        "idempotency_conflict",
        CodeEntry {
            namespaces: &["daemon", "threads"],
            next_action: "Retry with a new idempotency_key, or repeat the original arguments.",
            retires_with: None,
        },
    ),
    (
        "idempotency_in_progress",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Wait, then repeat the call with the same idempotency_key for its stored result.",
            retires_with: None,
        },
    ),
    (
        "idempotency_uncertain",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Read the target's current state before repeating; it may have run.",
            retires_with: None,
        },
    ),
    (
        "internal_error",
        CodeEntry {
            namespaces: &["account", "devices", "threads"],
            next_action: "Retry; if it fails again, report it with the details.",
            retires_with: None,
        },
    ),
    (
        "invalid_arguments",
        CodeEntry {
            namespaces: &[
                "account",
                "boulder",
                "config",
                "daemon",
                "deviceService",
                "devices",
                "diagnostics",
                "engine",
                "install",
                "sync",
                "threads",
                "worktrees",
            ],
            next_action: "Fix the named input and call it again.",
            retires_with: None,
        },
    ),
    (
        "local_only",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call it on the device that holds the thread.",
            retires_with: Some("T1d"),
        },
    ),
    (
        "loop_detected",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Stop the exchange; the same delivery is circling between threads.",
            retires_with: None,
        },
    ),
    (
        "message_too_large",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Send less in one call.",
            retires_with: None,
        },
    ),
    (
        "model_ambiguous",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Pick one model from details.candidates, or pass provider.",
            retires_with: None,
        },
    ),
    (
        "model_not_found",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Pick a model from details.available and call it again.",
            retires_with: None,
        },
    ),
    (
        "name_conflict",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Choose another name, or address the thread that holds it.",
            retires_with: None,
        },
    ),
    (
        "no_transport",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call deviceService.status to see why this device has no relay transport, then retry.",
            retires_with: None,
        },
    ),
    (
        "not_found",
        CodeEntry {
            namespaces: &[
                "boulder",
                "daemon",
                "devices",
                "diagnostics",
                "engine",
                "sync",
                "threads",
                "worktrees",
            ],
            next_action: "Call threads.list or devices.list for the current ids, then call it again.",
            retires_with: None,
        },
    ),
    (
        "not_resumable",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call threads.get for the thread's state; open it again with threads.create before controlling it.",
            retires_with: None,
        },
    ),
    (
        "not_steerable",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Send with delivery auto or follow_up instead of steer.",
            retires_with: None,
        },
    ),
    (
        "overloaded",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Retry after a short wait.",
            retires_with: None,
        },
    ),
    (
        "pair_loop_guard",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "This is a send loop; send to this target again after details.retry_after_ms.",
            retires_with: None,
        },
    ),
    (
        "partial_commit",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Read details.partial for what was done, then call it again for the rest.",
            retires_with: None,
        },
    ),
    (
        "queue_full",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Wait for the target to read its queue (threads.wait), then send again.",
            retires_with: None,
        },
    ),
    (
        "relay_requires_paid_plan",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "The account's plan has no relay; upgrade it, then call deviceService.status.",
            retires_with: None,
        },
    ),
    (
        "scope_denied",
        CodeEntry {
            namespaces: &["daemon", "diagnostics", "engine", "threads"],
            next_action: "Call it again with all_scope, or with an input this caller may pass.",
            retires_with: None,
        },
    ),
    (
        "sender_not_allowed",
        CodeEntry {
            namespaces: &[
                "account",
                "boulder",
                "config",
                "daemon",
                "deviceService",
                "devices",
                "diagnostics",
                "engine",
                "install",
                "mcp",
                "sync",
                "threads",
                "toolkit",
                "worktrees",
            ],
            next_action: "This caller may not send there; send as the principal that may.",
            retires_with: None,
        },
    ),
    (
        "service_refused",
        CodeEntry {
            namespaces: &["account", "sync"],
            next_action: "Read details for the account service's reason.",
            retires_with: None,
        },
    ),
    (
        "service_unreachable",
        CodeEntry {
            namespaces: &["account", "devices", "install"],
            next_action: "Check the network, then retry.",
            retires_with: None,
        },
    ),
    (
        "session_ended",
        CodeEntry {
            namespaces: &["account", "devices"],
            next_action: "The session ended; open it again with threads.create.",
            retires_with: None,
        },
    ),
    (
        "signed_out",
        CodeEntry {
            namespaces: &["account", "deviceService", "devices", "sync"],
            next_action: "Sign in (omo login), then retry.",
            retires_with: None,
        },
    ),
    (
        "thinking_level_unsupported",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Pick a level from details.supported.",
            retires_with: None,
        },
    ),
    (
        "timeout",
        CodeEntry {
            namespaces: &["daemon"],
            next_action: "Retry the call; if it changes something, read the current state first.",
            retires_with: None,
        },
    ),
    (
        "transport_expired",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "It may have run; read the session's current state before resending.",
            retires_with: None,
        },
    ),
    (
        "turn_active",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Wait for the turn with threads.wait, or pass the input that interrupts it, then retry.",
            retires_with: None,
        },
    ),
    (
        "turn_conflict",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Call threads.get for the active turn, then retry with that expected_turn_id.",
            retires_with: None,
        },
    ),
    (
        "unconfirmed",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "It may have run; read the session's current state before resending.",
            retires_with: None,
        },
    ),
    (
        "undelivered",
        CodeEntry {
            namespaces: &["threads"],
            next_action: "Nothing ran; resend with a new idempotency_key.",
            retires_with: None,
        },
    ),
    (
        "unsupported",
        CodeEntry {
            namespaces: &[
                "account",
                "daemon",
                "deviceService",
                "engine",
                "install",
                "threads",
            ],
            next_action: "Update omo and the device service, or use the alternative details.reason names.",
            retires_with: None,
        },
    ),
];

/// Why a source code is not in the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldCode {
    HeldRows {
        rows: &'static [&'static str],
    },
    NoProducer {
        note: &'static str,
    },
    InsideOkAnswer {
        row: &'static str,
        note: &'static str,
    },
    PrincipalOnly {
        note: &'static str,
    },
}

pub const HELD_CODES: &[(&str, HeldCode)] = &[
    (
        "approval_route_locked",
        HeldCode::NoProducer {
            note: "no producer in the source today (declared in THREAD_ERROR_CODES only); answering a dialog is attach's, held for P1a-s",
        },
    ),
    (
        "approval_unavailable",
        HeldCode::NoProducer {
            note: "no producer in the source today (declared in THREAD_ERROR_CODES only); answering a dialog is attach's, held for P1a-s",
        },
    ),
    (
        "orphaned",
        HeldCode::NoProducer {
            note: "no producer in the source today (declared in THREAD_ERROR_CODES only)",
        },
    ),
    (
        "no_active_turn",
        HeldCode::NoProducer {
            note: "no producer in the source today (declared in THREAD_ERROR_CODES only); an idle interrupt answers ok with interrupted false",
        },
    ),
    (
        "transport_closed",
        HeldCode::NoProducer {
            note: "no producer in the source today (declared in THREAD_ERROR_CODES only); a closed socket maps to host_unavailable",
        },
    ),
    (
        "prompt_not_started",
        HeldCode::InsideOkAnswer {
            row: "threads.create",
            note: "carried inside an ok answer: the thread is created and the first prompt's failure is its prompt.error",
        },
    ),
    (
        "reply_target_mismatch",
        HeldCode::PrincipalOnly {
            note: "the reply policy refuses a session answering a delivery (T1a); a script holds no reply allowance",
        },
    ),
];

pub fn code_entry(code: &str) -> Option<&'static CodeEntry> {
    OMO_CODE_REGISTRY
        .iter()
        .find(|(name, _)| *name == code)
        .map(|(_, entry)| entry)
}

pub fn held_code(code: &str) -> Option<&'static HeldCode> {
    HELD_CODES
        .iter()
        .find(|(name, _)| *name == code)
        .map(|(_, held)| held)
}
