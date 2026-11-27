//! The answers the root's calls carry as their `value`, as data (`results/account.ts`, `results/engine.ts`,
//! `results/host.ts`, `results/opencode.ts`, `results/authority.ts`). A record here is a shape declared in the contract
//! directory itself; a call whose answer is another module's own type is not restated: a thread call answers the thread
//! contract's ok branch (`results/threads.ts` re-exports it; the thread crate carries those records), a stream answers
//! events, and a credential-bearing call answers [`crate::results::custody`]. `answer_accepts` is the same `Check` the
//! custody answers get, over the records this module carries.

use serde_json::Value;

use crate::results::custody::custody_answer_accepts;
use crate::shape::{Shape, accepts, array, closed, field, non_empty, object, optional};

const ANY_OBJECT: Shape = Shape::AnyObject;
const TEXT: Shape = Shape::Text;
const NON_EMPTY: Shape = non_empty();
const BOOL: Shape = Shape::Bool;
const TRUE: Shape = Shape::BoolLiteral(true);
const FALSE: Shape = Shape::BoolLiteral(false);
const TEXT_LIST: Shape = array(&TEXT, 0);

const DEVICE: Shape = closed(&[field("id", NON_EMPTY), field("name", NON_EMPTY)]);
const SECTION_BOOL: Shape = Shape::OneOf(&[
    closed(&[
        field("ok", TRUE),
        field("joined", BOOL),
        field("switch", Shape::Enum(&["on", "off"])),
    ]),
    closed(&[field("ok", FALSE), field("reason", TEXT)]),
]);

const ACCOUNT_ANSWERS: &[(&str, Shape)] = &[
    ("account.signInCheck", closed(&[field("ready", TRUE)])),
    (
        "account.signOut",
        closed(&[field("signedOut", TRUE), field("revoked", BOOL)]),
    ),
    ("account.forget", closed(&[field("forgotten", TRUE)])),
    (
        "account.store.status",
        closed(&[
            field("store", Shape::Enum(&["omo", "login"])),
            optional("state", TEXT),
            optional("path", TEXT),
        ]),
    ),
    (
        "account.store.enable",
        closed(&[
            field("store", Shape::Enum(&["omo", "login"])),
            optional("state", TEXT),
            optional("path", TEXT),
        ]),
    ),
    (
        "account.store.disable",
        closed(&[
            field("store", Shape::Enum(&["omo", "login"])),
            optional("state", TEXT),
            optional("path", TEXT),
        ]),
    ),
    (
        "devices.revoke",
        closed(&[field("revoked", TRUE), field("device", DEVICE)]),
    ),
    ("deviceService.enable", closed(&[field("enabled", BOOL)])),
    ("deviceService.disable", closed(&[field("enabled", BOOL)])),
    (
        "sync.status",
        closed(&[
            field("account", TEXT),
            field("deviceId", TEXT),
            field("device", SECTION_BOOL),
            field(
                "devices",
                Shape::OneOf(&[
                    closed(&[
                        field("ok", TRUE),
                        field(
                            "list",
                            array(
                                &object(&[
                                    field("id", TEXT),
                                    field("name", TEXT),
                                    optional("syncing", BOOL),
                                    optional("lastSeenAt", TEXT),
                                    field("current", BOOL),
                                ]),
                                0,
                            ),
                        ),
                    ]),
                    closed(&[field("ok", FALSE), field("reason", TEXT)]),
                ]),
            ),
            field(
                "pending",
                Shape::OneOf(&[
                    closed(&[
                        field("ok", TRUE),
                        field(
                            "list",
                            array(
                                &object(&[
                                    field("id", TEXT),
                                    field("expiresAt", TEXT),
                                    field("answered", BOOL),
                                ]),
                                0,
                            ),
                        ),
                    ]),
                    closed(&[field("ok", FALSE), field("reason", TEXT)]),
                ]),
            ),
            field(
                "engine",
                Shape::OneOf(&[
                    closed(&[field("kind", Shape::TextLiteral("no_engine"))]),
                    closed(&[field("kind", Shape::TextLiteral("running"))]),
                    closed(&[
                        field("kind", Shape::TextLiteral("unreadable")),
                        field("reason", TEXT),
                    ]),
                ]),
            ),
        ]),
    ),
    ("sync.disable", closed(&[field("enabled", FALSE)])),
    (
        "sync.approve",
        closed(&[field("approved", TRUE), field("device", TEXT)]),
    ),
    ("sync.retirePin", closed(&[field("retired", TRUE)])),
];

const ENGINE_ANSWERS: &[(&str, Shape)] = &[
    (
        "engine.config.agentDir",
        closed(&[
            field("version", Shape::IntLiteral(1)),
            field("ok", TRUE),
            field("dir", TEXT),
            field("source", TEXT),
            field("rule", TEXT),
            field("brandedPrefix", TEXT),
        ]),
    ),
    (
        "engine.config.setResource",
        closed(&[
            field("scope", Shape::Enum(&["global", "project"])),
            field("source", TEXT),
            field(
                "resource",
                closed(&[field("type", TEXT), field("path", TEXT)]),
            ),
            field("enabled", BOOL),
            optional("state", Shape::Enum(&["inherit", "load", "unload"])),
        ]),
    ),
    (
        "engine.config.resources",
        closed(&[field(
            "resources",
            array(
                &closed(&[
                    field("scope", Shape::Enum(&["global", "project"])),
                    field("source", TEXT),
                    field(
                        "resource",
                        closed(&[field("type", TEXT), field("path", TEXT)]),
                    ),
                    field("enabled", BOOL),
                    optional("state", Shape::Enum(&["inherit", "load", "unload"])),
                ]),
                0,
            ),
        )]),
    ),
    (
        "engine.models.discover",
        closed(&[field("provider", TEXT), field("models", TEXT_LIST)]),
    ),
    (
        "engine.models.list",
        array(
            &closed(&[
                field("provider", TEXT),
                field("model", TEXT),
                field("context", Shape::Number),
                field("max_out", Shape::Number),
                field("thinking", BOOL),
                field("images", BOOL),
            ]),
            0,
        ),
    ),
    ("engine.models.update", closed(&[field("refreshed", TRUE)])),
    (
        "engine.packages.install",
        closed(&[
            field("source", TEXT),
            field("scope", Shape::Enum(&["user", "project"])),
        ]),
    ),
    (
        "engine.packages.remove",
        closed(&[
            field("source", TEXT),
            field("scope", Shape::Enum(&["user", "project"])),
        ]),
    ),
    (
        "engine.packages.list",
        closed(&[
            field(
                "user",
                array(&object(&[field("source", TEXT), optional("path", TEXT)]), 0),
            ),
            field(
                "project",
                array(&object(&[field("source", TEXT), optional("path", TEXT)]), 0),
            ),
        ]),
    ),
    (
        "engine.packages.update",
        closed(&[field("updated", TEXT_LIST)]),
    ),
    (
        "engine.promptSizes",
        closed(&[
            field("sections", array(&ANY_OBJECT, 0)),
            field(
                "total",
                closed(&[
                    field("bytes", Shape::Number),
                    field("estimatedTokens", Shape::Number),
                ]),
            ),
        ]),
    ),
    (
        "engine.tips",
        array(
            &object(&[
                field("id", TEXT),
                field("text", TEXT),
                optional("requiresCommand", TEXT),
            ]),
            0,
        ),
    ),
    (
        "engine.schedule.list",
        closed(&[
            field("jobs", array(&ANY_OBJECT, 0)),
            field("invalid", array(&ANY_OBJECT, 0)),
            field("runners", array(&ANY_OBJECT, 0)),
        ]),
    ),
    (
        "engine.schedule.cancel",
        closed(&[field("cancelled", TRUE), field("in_flight", BOOL)]),
    ),
    (
        "engine.schedule.run",
        closed(&[field("passes", Shape::Number)]),
    ),
    (
        "engine.sessionOfPid",
        closed(&[
            field("version", Shape::IntLiteral(1)),
            field("ok", TRUE),
            field("attested", BOOL),
            field("pid", Shape::Number),
            optional("sessionId", TEXT),
            optional("attestedPid", Shape::Number),
            optional("hostPid", Shape::Number),
            optional("chain", array(&Shape::Number, 0)),
            optional(
                "reason",
                Shape::Enum(&["process_not_found", "no_attested_ancestor"]),
            ),
        ]),
    ),
    ("engine.sessions.exportHtml", closed(&[field("path", TEXT)])),
];

const HOST_ANSWERS: &[(&str, Shape)] = &[
    ("config.status", ANY_OBJECT),
    (
        "config.migrate",
        closed(&[
            field("dryRun", BOOL),
            optional("error", TEXT),
            field("journalResumed", BOOL),
            field("migratedFrom", TEXT_LIST),
            field(
                "migrations",
                array(&closed(&[field("from", TEXT), field("to", TEXT)]), 0),
            ),
            field("skippedConflictCount", Shape::Number),
            field("targetBackupPaths", TEXT_LIST),
        ]),
    ),
    (
        "config.modelCapabilities.refresh",
        closed(&[
            field("sourceUrl", TEXT),
            field("generatedAt", TEXT),
            field("modelCount", Shape::Number),
        ]),
    ),
    (
        "daemon.run",
        closed(&[
            field("ensure", object(&[field("action", TEXT)])),
            field("warm", ANY_OBJECT),
        ]),
    ),
    ("daemon.ensure", object(&[field("action", TEXT)])),
    (
        "daemon.status",
        closed(&[
            field("endpoints", array(&ANY_OBJECT, 0)),
            field("aggregate", ANY_OBJECT),
        ]),
    ),
    ("daemon.stop", object(&[field("action", TEXT)])),
    (
        "daemon.stopAll",
        closed(&[field(
            "endpoints",
            array(
                &closed(&[
                    field("socket", TEXT),
                    field(
                        "outcome",
                        Shape::Enum(&[
                            "requested",
                            "drained",
                            "already_drained",
                            "refused",
                            "timeout",
                        ]),
                    ),
                    optional("polls", Shape::Number),
                    optional("summary", TEXT),
                ]),
                0,
            ),
        )]),
    ),
    (
        "daemon.handoff",
        closed(&[field(
            "endpoints",
            array(&closed(&[field("socket", TEXT), field("action", TEXT)]), 0),
        )]),
    ),
    (
        "daemon.gc",
        closed(&[
            field("removed", array(&ANY_OBJECT, 0)),
            field("kept", array(&ANY_OBJECT, 0)),
            field("reaped_meta", TEXT_LIST),
            field("pruned_stores", TEXT_LIST),
        ]),
    ),
    (
        "daemon.rollbackPrepare",
        closed(&[
            field("dry_run", BOOL),
            field("stores", array(&ANY_OBJECT, 0)),
            field(
                "coverage",
                closed(&[
                    field("discovered", Shape::Number),
                    field("from_index", Shape::Number),
                    field("from_sidecars", Shape::Number),
                    field("from_store", Shape::Number),
                    field("missing", TEXT_LIST),
                    field("endpoints_without_store_map", TEXT_LIST),
                ]),
            ),
        ]),
    ),
    ("daemon.shardPath", closed(&[field("path", TEXT)])),
    (
        "diagnostics.run",
        closed(&[field(
            "findings",
            array(
                &closed(&[
                    field("level", Shape::Enum(&["ok", "notice", "warn", "fail"])),
                    field("check", TEXT),
                    field("message", TEXT),
                ]),
                0,
            ),
        )]),
    ),
    (
        "diagnostics.models",
        closed(&[field(
            "findings",
            array(
                &closed(&[
                    field("level", Shape::Enum(&["ok", "notice", "warn", "fail"])),
                    field("check", TEXT),
                    field("message", TEXT),
                ]),
                0,
            ),
        )]),
    ),
    (
        "diagnostics.fixReflectionLock",
        closed(&[
            field("dry_run", BOOL),
            field(
                "findings",
                array(
                    &closed(&[
                        field("level", Shape::Enum(&["ok", "notice", "warn", "fail"])),
                        field("check", TEXT),
                        field("message", TEXT),
                    ]),
                    0,
                ),
            ),
        ]),
    ),
    (
        "diagnostics.reapStaleEngines",
        closed(&[field(
            "engines",
            array(
                &closed(&[
                    field("pid", Shape::Number),
                    field("outcome", Shape::Enum(&["reaped", "refused", "not_found"])),
                    optional("reason", TEXT),
                ]),
                0,
            ),
        )]),
    ),
    (
        "install.setup.detect",
        closed(&[field(
            "harnesses",
            array(
                &object(&[
                    field("id", TEXT),
                    field("installed", BOOL),
                    field("providers", TEXT_LIST),
                    field("credentialTypes", TEXT_LIST),
                    field("modelHint", TEXT),
                    field("notices", TEXT_LIST),
                    field("importableCount", Shape::Number),
                ]),
                0,
            ),
        )]),
    ),
    (
        "install.setup.plan",
        closed(&[
            field(
                "inventory",
                closed(&[field(
                    "harnesses",
                    array(
                        &object(&[
                            field("id", TEXT),
                            field("installed", BOOL),
                            field("providers", TEXT_LIST),
                            field("credentialTypes", TEXT_LIST),
                            field("modelHint", TEXT),
                            field("notices", TEXT_LIST),
                            field("importableCount", Shape::Number),
                        ]),
                        0,
                    ),
                )]),
            ),
            field(
                "stages",
                array(
                    &object(&[field("id", NON_EMPTY), field("description", TEXT)]),
                    0,
                ),
            ),
        ]),
    ),
    (
        "install.setup.apply",
        closed(&[field("applied", TEXT_LIST), field("skipped", TEXT_LIST)]),
    ),
    (
        "install.update",
        closed(&[
            field("version", TEXT),
            field("command", TEXT_LIST),
            field("ran", BOOL),
        ]),
    ),
    ("install.version", closed(&[field("version", TEXT)])),
    ("install.sdkRoot", closed(&[field("path", TEXT)])),
];

const OPENCODE_ANSWERS: &[(&str, Shape)] = &[
    (
        "install.plugin",
        closed(&[field(
            "platforms",
            array(
                &closed(&[field("platform", TEXT), field("wrote", TEXT_LIST)]),
                0,
            ),
        )]),
    ),
    (
        "install.cleanup",
        closed(&[
            field("codexHome", TEXT),
            field("configPath", TEXT),
            field("configChanged", BOOL),
            optional("configBackupPath", TEXT),
            field("removedPaths", TEXT_LIST),
            field(
                "skippedPaths",
                array(&closed(&[field("path", TEXT), field("reason", TEXT)]), 0),
            ),
            field("removedAgentLinks", TEXT_LIST),
            field("skippedAgentLinks", TEXT_LIST),
            optional(
                "projectCleanup",
                closed(&[
                    field("changed", BOOL),
                    field("configPath", TEXT),
                    field("artifacts", array(&closed(&[field("path", TEXT)]), 0)),
                ]),
            ),
        ]),
    ),
    (
        "install.localVersion",
        closed(&[
            field("currentVersion", Shape::OneOf(&[TEXT, Shape::Null])),
            field("latestVersion", Shape::OneOf(&[TEXT, Shape::Null])),
            field("isUpToDate", BOOL),
            field("isLocalDev", BOOL),
            field("isPinned", BOOL),
            field("pinnedVersion", Shape::OneOf(&[TEXT, Shape::Null])),
            field(
                "status",
                Shape::Enum(&[
                    "up-to-date",
                    "outdated",
                    "local-dev",
                    "dev",
                    "pinned",
                    "pinned-mismatch",
                    "error",
                    "unknown",
                ]),
            ),
        ]),
    ),
    (
        "boulder.status",
        closed(&[field(
            "works",
            array(
                &closed(&[
                    field("work_id", TEXT),
                    field("plan_name", TEXT),
                    field("active_plan", TEXT),
                    optional("worktree_path", TEXT),
                    field("status", TEXT),
                    field("started_at", TEXT),
                    optional("ended_at", TEXT),
                    optional("elapsed_ms", Shape::Number),
                    optional("elapsed_human", TEXT),
                    field("total_tasks", Shape::Number),
                    field("completed_tasks", Shape::Number),
                    field("remaining_tasks", Shape::Number),
                    field("percentage", Shape::Number),
                    field("session_count", Shape::Number),
                    optional(
                        "current_task",
                        closed(&[
                            field("task_key", TEXT),
                            field("task_title", TEXT),
                            optional("elapsed_human", TEXT),
                        ]),
                    ),
                ]),
                0,
            ),
        )]),
    ),
    (
        "worktrees.sweep",
        closed(&[
            field("apply", BOOL),
            field("olderThanDays", Shape::Number),
            field(
                "repos",
                array(
                    &closed(&[
                        field("repo", TEXT),
                        field("defaultBranch", TEXT),
                        field(
                            "classifications",
                            array(
                                &closed(&[
                                    field("path", TEXT),
                                    field("action", Shape::Enum(&["SWEEP", "KEEP", "PRUNE"])),
                                    field("reason", TEXT),
                                ]),
                                0,
                            ),
                        ),
                        field("removed", TEXT_LIST),
                        field(
                            "failed",
                            array(&closed(&[field("path", TEXT), field("error", TEXT)]), 0),
                        ),
                    ]),
                    0,
                ),
            ),
            field("sweepCount", Shape::Number),
            field("keepCount", Shape::Number),
            field("pruneCount", Shape::Number),
        ]),
    ),
    ("toolkit.help", closed(&[field("text", TEXT)])),
];

const AUTHORITY_ANSWERS: &[(&str, Shape)] = &[
    (
        "engine.launch.interactive",
        closed(&[field("exit_code", Shape::Number), optional("session", TEXT)]),
    ),
    (
        "engine.launch.print",
        closed(&[field("exit_code", Shape::Number), optional("session", TEXT)]),
    ),
    (
        "threads.runToCompletion",
        closed(&[
            field("session_id", TEXT),
            field("success", BOOL),
            field("duration_ms", Shape::Number),
            field("message_count", Shape::Number),
            field("summary", TEXT),
        ]),
    ),
    (
        "threads.extensions.sessionOps",
        closed(&[field(
            "extensions",
            array(
                &closed(&[
                    field("name", TEXT),
                    field(
                        "ops",
                        array(
                            &closed(&[field("op", TEXT), field("parameters", ANY_OBJECT)]),
                            0,
                        ),
                    ),
                ]),
                0,
            ),
        )]),
    ),
    (
        "threads.extensions.register",
        closed(&[field("version", Shape::Number)]),
    ),
    (
        "toolkit.forSession",
        closed(&[field("session_id", TEXT), field("cwd", TEXT)]),
    ),
    ("toolkit.addGoal", ANY_OBJECT),
    ("toolkit.checkpoint", ANY_OBJECT),
    ("toolkit.completeGoals", ANY_OBJECT),
    ("toolkit.createGoals", ANY_OBJECT),
    ("toolkit.criteria", ANY_OBJECT),
    ("toolkit.recordEvidence", ANY_OBJECT),
    ("toolkit.recordReviewBlockers", ANY_OBJECT),
    ("toolkit.status", ANY_OBJECT),
    ("toolkit.steer", ANY_OBJECT),
];

/// The answer records this module carries, keyed by call.
const ANSWER_TABLES: &[&[(&str, Shape)]] = &[
    ACCOUNT_ANSWERS,
    ENGINE_ANSWERS,
    HOST_ANSWERS,
    OPENCODE_ANSWERS,
    AUTHORITY_ANSWERS,
];

/// The shape of the answer `key` carries here, if it is one of the records this module restates.
pub fn answer_spec(key: &str) -> Option<&'static Shape> {
    ANSWER_TABLES
        .iter()
        .flat_map(|table| table.iter())
        .find(|(call, _)| *call == key)
        .map(|(_, shape)| shape)
}

/// Whether `value` is a valid answer of `key`: the custody answers' own check for a credential-bearing call, the
/// restated record for a call whose answer this module carries, and `true` for a call whose answer is another module's
/// type (a thread result, a stream event, a handle), which this crate does not restate.
pub fn answer_accepts(key: &str, value: &Value) -> bool {
    if crate::results::custody::is_custody_answer(key) {
        return custody_answer_accepts(key, value);
    }
    match answer_spec(key) {
        Some(shape) => accepts(shape, value),
        None => crate::calls::is_root_call(key),
    }
}
