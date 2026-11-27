//! The omo SDK root's typed call table (first-party design 4.1, 4.2, 4.4): every call the root has, keyed by its
//! registry row's `sdk`, with its input schema, its answer shape and whether it can wait. This is `calls.ts` and
//! `calls/*.ts` as data; [`input_accepts`] is `validate.ts`'s `inputAccepts` over every row. The root's TypeScript type
//! (`root.ts`, `Tree`/`Member`/`Answer`/`OptionsFor`) is type-level machinery with no Rust counterpart, and the runtime
//! root itself is slice P1b's; what is carried here is the table that type is computed from.
//!
//! The five credential-bearing inputs stay a contract fact of [`crate::inputs::custody`], whose `custody_input_accepts`
//! is the authority on them; the shapes below restate their JSON form so the table is uniform, and a test checks the two
//! agree. A call whose input carries a handle or a callback has no JSON form, as that slice ruled.

use serde_json::Value;

use crate::inputs::custody::custody_input_accepts;
use crate::shape::{
    FieldSpec, Shape, accepts, all_fields_optional, array, array_range, closed, field, int_min,
    int_range, max_len, min_len, names_field, non_empty, non_empty_max, object, optional,
};

/// Which of the three answer shapes of design 4.4 a call has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerShape {
    /// `Promise<OmoValue<Result>>`.
    Value,
    /// `Promise<OmoThreadAnswer<Result>>`: the thread contract's own ok branch.
    Thread,
    /// `OmoStream<Event, Outcome>`.
    Stream,
}

/// One call of the root (`call-spec.ts`'s `CallSpec`): its input schema, answer shape and whether it takes wait options.
#[derive(Clone, Copy, Debug)]
pub struct CallSpec {
    pub key: &'static str,
    pub input: Shape,
    pub answer: AnswerShape,
    pub waits: bool,
}

const NO_FIELDS: &[FieldSpec] = &[];
const EMPTY: Shape = Shape::Object {
    fields: NO_FIELDS,
    closed: false,
};
const ANY: Shape = Shape::Any;
const ANY_OBJECT: Shape = Shape::AnyObject;
const TEXT: Shape = Shape::Text;
const NON_EMPTY: Shape = non_empty();
const BOOL: Shape = Shape::Bool;
const HANDLE: Shape = Shape::Handle;
const DEADLINE_MS: Shape = int_min(1);
const POSITIVE: Shape = int_min(1);
const TEXT_ARRAY: Shape = array(&TEXT, 0);
const NON_EMPTY_LIST: Shape = array(&NON_EMPTY, 1);

// The thread contract's own fields (`thread/contracts/fields.ts`, `params.ts`, `create-params.ts`).
const THREAD: Shape = TEXT;
const ALL_SCOPE: FieldSpec = optional("all_scope", BOOL);
const IDEMPOTENCY_KEY: FieldSpec = optional("idempotency_key", TEXT);
const MESSAGE: Shape = max_len(32768);
const SUMMARY: FieldSpec = optional("summary", max_len(200));
const EXPECTED_TURN_ID: FieldSpec = optional("expected_turn_id", TEXT);
const COLLAPSE_KEY: FieldSpec = optional("collapse_key", non_empty_max(200));
const DELIVERY: FieldSpec = optional(
    "delivery",
    Shape::OneOf(&[
        Shape::TextLiteral("auto"),
        Shape::TextLiteral("steer"),
        Shape::TextLiteral("follow_up"),
    ]),
);
const THINKING_LEVEL: Shape = Shape::OneOf(&[
    Shape::TextLiteral("off"),
    Shape::TextLiteral("minimal"),
    Shape::TextLiteral("low"),
    Shape::TextLiteral("medium"),
    Shape::TextLiteral("high"),
    Shape::TextLiteral("xhigh"),
    Shape::TextLiteral("max"),
]);
const ALIAS: Shape = non_empty_max(64);
const ALIAS_NAME: Shape = non_empty_max(64);
const AUTHOR: FieldSpec = optional(
    "author",
    closed(&[
        field("platform_user_id", NON_EMPTY),
        field("display", NON_EMPTY),
        optional("user_id", NON_EMPTY),
    ]),
);
const MIME: FieldSpec = optional("mime", NON_EMPTY);
const ATTACHMENTS: FieldSpec = optional(
    "attachments",
    array(
        &Shape::OneOf(&[
            closed(&[field("path", NON_EMPTY), MIME]),
            closed(&[field("bytes", Shape::Bytes), field("name", NON_EMPTY), MIME]),
        ]),
        1,
    ),
);
const SEND_ANCHOR: FieldSpec = optional(
    "from",
    closed(&[
        field("thread", NON_EMPTY),
        field("delivery_id", NON_EMPTY),
        optional("turn_id", NON_EMPTY),
    ]),
);
const LAUNCH_SPEC: FieldSpec = optional("launch_spec", NON_EMPTY);

// The launch inputs (`inputs/launch.ts`), shared by the five launch calls and engine.models.list/promptSizes.
const LAUNCH_SESSION: FieldSpec = optional(
    "session",
    closed(&[
        optional("continue", Shape::BoolLiteral(true)),
        optional("path", NON_EMPTY),
        optional("id", NON_EMPTY),
        optional("fork", NON_EMPTY),
        optional("rebind", NON_EMPTY),
        optional("ephemeral", Shape::BoolLiteral(true)),
        optional("dir", NON_EMPTY),
        optional(
            "spawned_by",
            closed(&[
                field("parentSessionId", NON_EMPTY),
                field("rootSessionId", NON_EMPTY),
                field("taskId", NON_EMPTY),
            ]),
        ),
        optional("name", NON_EMPTY),
    ]),
);
const LAUNCH_SETUP: FieldSpec = optional(
    "setup",
    closed(&[
        optional("model", NON_EMPTY),
        optional("provider", NON_EMPTY),
        optional("model_patterns", NON_EMPTY_LIST),
        optional("reasoning", NON_EMPTY),
        optional("system_prompt", NON_EMPTY),
        optional("append_system_prompt", NON_EMPTY),
        optional(
            "tools",
            closed(&[
                optional("allow", NON_EMPTY_LIST),
                optional("deny", NON_EMPTY_LIST),
                optional("none", BOOL),
                optional("no_builtin", BOOL),
            ]),
        ),
        optional(
            "extensions",
            closed(&[optional("paths", NON_EMPTY_LIST), optional("none", BOOL)]),
        ),
        optional(
            "skills",
            closed(&[optional("names", NON_EMPTY_LIST), optional("none", BOOL)]),
        ),
        optional(
            "prompt_templates",
            closed(&[optional("paths", NON_EMPTY_LIST), optional("none", BOOL)]),
        ),
        optional(
            "themes",
            closed(&[
                optional("paths", NON_EMPTY_LIST),
                optional("none", BOOL),
                optional("use", NON_EMPTY),
            ]),
        ),
        optional("context_files", BOOL),
        optional("project_trust", BOOL),
        optional("offline", BOOL),
        optional("session_runtime", Shape::Enum(&["in-process", "worker"])),
        optional("tool_runtime", Shape::Enum(&["in-process", "child"])),
        optional("auto_title", BOOL),
    ]),
);
const LAUNCH_INPUT: Shape = closed(&[
    optional("messages", array(&TEXT, 0)),
    optional("files", NON_EMPTY_LIST),
    optional("stdin", BOOL),
]);
const GATED: FieldSpec = optional("gated", closed(&[optional("grok_neo", BOOL)]));
const PRESENTER: FieldSpec = optional(
    "presenter",
    closed(&[
        field("bootstrap_fd", Shape::IntLiteral(4)),
        optional("enforcement", BOOL),
    ]),
);
const API_KEY: FieldSpec = optional("api_key", HANDLE);

// The delegated loop's session binding and manifest fields (`inputs/loop.ts`).
const SESSION_ID: FieldSpec = optional("session_id", NON_EMPTY);
const GOAL: Shape = TEXT;
const EVIDENCE: Shape = TEXT;
const USER_MODEL: Shape = Shape::Enum(&["happy", "edge", "regression", "adversarial"]);

/// Every call of the root (`ROOT_CALLS`), in registry order: threads, account/devices, host, engine, authority.
pub const ROOT_CALLS: &[CallSpec] = &[
    // threads: reading (design 6.1).
    CallSpec {
        key: "threads.list",
        input: object(&[
            ALL_SCOPE,
            optional(
                "state",
                array(
                    &Shape::OneOf(&[
                        Shape::TextLiteral("opening"),
                        Shape::TextLiteral("running"),
                        Shape::TextLiteral("idle"),
                        Shape::TextLiteral("inactive"),
                        Shape::TextLiteral("archived"),
                    ]),
                    1,
                ),
            ),
            IDEMPOTENCY_KEY,
            optional("workspace", NON_EMPTY),
            optional("device", NON_EMPTY),
            optional("project", NON_EMPTY),
            optional("name", NON_EMPTY),
            optional("archived", BOOL),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.resolve",
        input: object(&[
            field("address", NON_EMPTY),
            ALL_SCOPE,
            optional("workspace", NON_EMPTY),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.read",
        input: object(&[
            field("thread", THREAD),
            optional("cursor", TEXT),
            optional("max_bytes", int_range(1, 1_048_576)),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
            optional("limit", POSITIVE),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.readAfterWait",
        input: object(&[
            field("thread", THREAD),
            optional("cursor", TEXT),
            optional("max_bytes", int_range(1, 1_048_576)),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
            field("wait_ms", int_range(1, 600_000)),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.receipt",
        input: object(&[
            field("thread", THREAD),
            ALL_SCOPE,
            field("delivery_id", non_empty_max(128)),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.entries",
        input: object(&[
            field("thread", THREAD),
            optional("since", NON_EMPTY),
            optional("before", NON_EMPTY),
            optional("tail", POSITIVE),
            optional("limit", POSITIVE),
            optional("max_bytes", POSITIVE),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.messages",
        input: object(&[field("thread", THREAD)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.get",
        input: object(&[field("thread", THREAD)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.wait",
        input: object(&[
            field("thread", THREAD),
            SEND_ANCHOR,
            optional(
                "until",
                Shape::OneOf(&[Shape::TextLiteral("idle"), Shape::TextLiteral("done")]),
            ),
            optional("deadline_ms", DEADLINE_MS),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.follow",
        input: object(&[field("thread", THREAD), SEND_ANCHOR]),
        answer: AnswerShape::Stream,
        waits: true,
    },
    CallSpec {
        key: "threads.locate",
        input: object(&[field("thread", THREAD), ALL_SCOPE]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    // threads: writing and control (design 6.2).
    CallSpec {
        key: "threads.send",
        input: object(&[
            field("thread", THREAD),
            field("message", MESSAGE),
            DELIVERY,
            EXPECTED_TURN_ID,
            SUMMARY,
            IDEMPOTENCY_KEY,
            COLLAPSE_KEY,
            ALL_SCOPE,
            ATTACHMENTS,
            AUTHOR,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.sendAndFollow",
        input: object(&[
            field("thread", THREAD),
            field("message", MESSAGE),
            DELIVERY,
            EXPECTED_TURN_ID,
            SUMMARY,
            IDEMPOTENCY_KEY,
            COLLAPSE_KEY,
            ALL_SCOPE,
            ATTACHMENTS,
            AUTHOR,
            optional("deadline_ms", DEADLINE_MS),
        ]),
        answer: AnswerShape::Stream,
        waits: true,
    },
    CallSpec {
        key: "threads.create",
        input: object(&[
            optional("thread", NON_EMPTY),
            optional("device", NON_EMPTY),
            optional("alias", ALIAS),
            optional("name", TEXT),
            optional("cwd", TEXT),
            optional(
                "worktree",
                closed(&[
                    optional("branch", non_empty_max(200)),
                    optional("base", non_empty_max(200)),
                ]),
            ),
            optional("fork_from", TEXT),
            optional("skills", array_range(&non_empty_max(64), 1, 16)),
            optional("model", min_len(3)),
            optional("reasoning", THINKING_LEVEL),
            optional(
                "permission_preset",
                Shape::Enum(&[
                    "full-access",
                    "workspace",
                    "accept-edits",
                    "auto",
                    "read-only",
                    "ask",
                ]),
            ),
            optional(
                "prompt",
                Shape::OneOf(&[
                    non_empty_max(32768),
                    object(&[
                        field("message", MESSAGE),
                        DELIVERY,
                        EXPECTED_TURN_ID,
                        SUMMARY,
                        ATTACHMENTS,
                        AUTHOR,
                    ]),
                ]),
            ),
            DELIVERY,
            IDEMPOTENCY_KEY,
            LAUNCH_SPEC,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.interrupt",
        input: object(&[
            field("thread", THREAD),
            optional("turn_id", TEXT),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.close",
        input: object(&[
            optional("thread", NON_EMPTY),
            IDEMPOTENCY_KEY,
            ALL_SCOPE,
            optional("force", BOOL),
            optional("graceful", BOOL),
            optional("wait_ms", int_range(1, 600_000)),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.unload",
        input: object(&[
            field("thread", THREAD),
            optional("interrupt", BOOL),
            optional("force", BOOL),
            optional("clear_queue", BOOL),
            optional(
                "children",
                Shape::OneOf(&[Shape::TextLiteral("refuse"), Shape::TextLiteral("detach")]),
            ),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.release",
        input: object(&[
            field("thread", THREAD),
            optional("interrupt", BOOL),
            optional("force", BOOL),
            optional(
                "move",
                closed(&[field("id", NON_EMPTY), field("target", NON_EMPTY)]),
            ),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "threads.adopt",
        input: object(&[
            field("thread", THREAD),
            optional("interrupt", BOOL),
            optional("force", BOOL),
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.setModel",
        input: object(&[
            field("thread", THREAD),
            field("model", NON_EMPTY),
            optional("provider", TEXT),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.setReasoning",
        input: object(&[
            field("thread", THREAD),
            field("level", THINKING_LEVEL),
            optional(
                "scope",
                Shape::OneOf(&[Shape::TextLiteral("session"), Shape::TextLiteral("turn")]),
            ),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.rename",
        input: object(&[
            optional("thread", NON_EMPTY),
            optional("name", non_empty_max(200)),
            optional("alias", ALIAS),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.aliases.resolve",
        input: object(&[field("alias", ALIAS_NAME)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.aliases.set",
        input: object(&[optional("thread", NON_EMPTY), field("alias", ALIAS_NAME)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.aliases.clear",
        input: object(&[field("thread", NON_EMPTY)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.archive",
        input: object(&[
            field("thread", THREAD),
            optional("archived", BOOL),
            ALL_SCOPE,
            IDEMPOTENCY_KEY,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.handoff",
        input: object(&[
            field("thread", TEXT),
            optional(
                "match",
                Shape::OneOf(&[Shape::TextLiteral("exact"), Shape::TextLiteral("fuzzy")]),
            ),
            field("message", MESSAGE),
            DELIVERY,
            EXPECTED_TURN_ID,
            SUMMARY,
            IDEMPOTENCY_KEY,
            ALL_SCOPE,
        ]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.extensions.register",
        input: object(&[
            field("name", NON_EMPTY),
            field("migrations", array(&TEXT_ARRAY, 0)),
            field("moduleUrl", NON_EMPTY),
            optional("sessionCallable", SESSION_CALLABLE),
            optional("wakeDir", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "threads.extensions.call",
        input: object(&[
            field("name", NON_EMPTY),
            field("op", NON_EMPTY),
            field("args", ANY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    // threads: authority (security design 4.1, 4.4).
    CallSpec {
        key: "threads.attach",
        input: object(&[field("thread", THREAD)]),
        answer: AnswerShape::Thread,
        waits: true,
    },
    CallSpec {
        key: "threads.extensions.sessionOps",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "threads.extensions.sessionCall",
        input: object(&[
            field("name", NON_EMPTY),
            field("op", NON_EMPTY),
            field("args", ANY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "threads.runToCompletion",
        input: object(&[
            field("message", NON_EMPTY),
            optional("agent", NON_EMPTY),
            optional("model", NON_EMPTY),
            optional("directory", NON_EMPTY),
            optional("session_id", NON_EMPTY),
            optional(
                "endpoint",
                Shape::OneOf(&[
                    closed(&[field("port", int_range(1, 65535))]),
                    closed(&[field("attach_url", NON_EMPTY)]),
                ]),
            ),
            optional("on_complete", closed(&[field("command", NON_EMPTY)])),
            optional("signal", HANDLE),
        ]),
        answer: AnswerShape::Stream,
        waits: true,
    },
    // account, devices, deviceService and sync (design 6.3 to 6.6).
    CallSpec {
        key: "account.whoami",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.show",
        input: object(&[optional("account", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.signInCheck",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "account.signOut",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.forget",
        input: object(&[optional("device", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.store.status",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.store.enable",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.store.disable",
        input: object(&[optional("discard", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "devices.list",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "devices.rename",
        input: object(&[field("id", NON_EMPTY), field("name", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "devices.resolve",
        input: object(&[field("device", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "devices.revoke",
        input: object(&[field("device", NON_EMPTY), field("confirm_name", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.status",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.info",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.enable",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.disable",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.control",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.setControl",
        input: object(&[field("on", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.wake",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.setWake",
        input: object(&[field("on", Shape::BoolOrNull)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "deviceService.bootstrap",
        input: object(&[
            optional("account", ANY_OBJECT),
            optional("install", BOOL),
            optional("adopt", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.status",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.disable",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.approve",
        input: object(&[optional("id", NON_EMPTY), field("confirmCode", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.retirePin",
        input: object(&[field("account", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    // The credential-bearing calls (security design 4.2, 4.3, 5.0 to 5.7); `custody_input_accepts` is their authority.
    CallSpec {
        key: "account.signIn",
        input: object(&[
            field("method", Shape::Enum(&["device_code", "browser"])),
            optional("open_browser", BOOL),
            field("onUserCode", HANDLE),
            optional("onOpenUrl", HANDLE),
            optional("deviceName", NON_EMPTY),
            optional("replace", BOOL),
            optional("signal", HANDLE),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.adopt",
        input: object(&[field("credential", HANDLE), optional("replace", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.credentials.handOver",
        input: object(&[
            field("what", Shape::Enum(&["access_token", "stored_credential"])),
            field("destination", DESTINATION),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.keys.init",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "account.keys.status",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.enable",
        input: object(&[
            field("onCode", HANDLE),
            field("confirm", HANDLE),
            optional("signal", HANDLE),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "sync.recover",
        input: object(&[field("confirm", HANDLE), optional("signal", HANDLE)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.auth.check",
        input: object(&[
            optional("provider", NON_EMPTY),
            optional("model", NON_EMPTY),
            optional("refresh", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.auth.credentials",
        input: object(&[
            optional("provider", NON_EMPTY),
            optional("model", NON_EMPTY),
            optional("refresh", BOOL),
            field("destination", DESTINATION),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.auth.apiKey",
        input: object(&[
            optional("provider", NON_EMPTY),
            optional("model", NON_EMPTY),
            field("destination", DESTINATION),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.auth.bearerToken",
        input: object(&[
            optional("provider", NON_EMPTY),
            optional("model", NON_EMPTY),
            optional("min_expiry_ms", int_min(0)),
            field("destination", DESTINATION),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.trust.ensure",
        input: object(&[optional("wait", BOOL), optional("lease_pid", int_min(1))]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "mcp.oauth.login",
        input: object(&[
            field("server", NON_EMPTY),
            optional("server_url", NON_EMPTY),
            optional("client_id", NON_EMPTY),
            optional("scopes", NON_EMPTY_LIST),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "mcp.oauth.logout",
        input: object(&[
            field("server", NON_EMPTY),
            optional("server_url", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "mcp.oauth.status",
        input: object(&[optional("server", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    // The five launch operations (design 6.9; security design 4.4, 5.5).
    CallSpec {
        key: "engine.launch.interactive",
        input: object(&[
            LAUNCH_SESSION,
            LAUNCH_SETUP,
            optional("input", LAUNCH_INPUT),
            GATED,
            optional("tui_mode", Shape::Enum(&["regular", "fullscreen"])),
            optional("verbose", BOOL),
            optional("attach", closed(&[field("socket", NON_EMPTY)])),
            PRESENTER,
            API_KEY,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.launch.print",
        input: object(&[
            LAUNCH_SESSION,
            LAUNCH_SETUP,
            field("input", LAUNCH_INPUT),
            GATED,
            field("mode", Shape::Enum(&["text", "json"])),
            PRESENTER,
            API_KEY,
        ]),
        answer: AnswerShape::Stream,
        waits: true,
    },
    CallSpec {
        key: "engine.launch.rpc",
        input: object(&[
            LAUNCH_SESSION,
            LAUNCH_SETUP,
            GATED,
            optional("listen", NON_EMPTY),
            optional("multi_session", BOOL),
            PRESENTER,
            optional("input", LAUNCH_INPUT),
            API_KEY,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.launch.hand",
        input: object(&[
            LAUNCH_SESSION,
            LAUNCH_SETUP,
            optional("input", LAUNCH_INPUT),
            PRESENTER,
            GATED,
            API_KEY,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.launch.headless",
        input: object(&[
            LAUNCH_SESSION,
            LAUNCH_SETUP,
            optional("listen", NON_EMPTY),
            optional("input", LAUNCH_INPUT),
            PRESENTER,
            GATED,
            API_KEY,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    // config, daemon, diagnostics, install, boulder and worktrees (design 6.7, 6.11).
    CallSpec {
        key: "config.status",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "config.migrate",
        input: object(&[optional("dry_run", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "config.modelCapabilities.refresh",
        input: object(&[
            optional("directory", NON_EMPTY),
            optional("source_url", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.run",
        input: object(&[optional("policy", POLICY), optional("pinned", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.ensure",
        input: object(&[
            optional("policy", POLICY),
            optional("pinned", BOOL),
            LAUNCH_SPEC,
            optional("mcp_presenter_enforcement", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.warm",
        input: object(&[
            optional("cwd", NON_EMPTY),
            optional("kind", NON_EMPTY),
            optional("context", ANY_OBJECT),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.status",
        input: object(&[optional("all", BOOL), optional("include_workers", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.stop",
        input: object(&[
            optional("drain", BOOL),
            optional("instance", NON_EMPTY),
            optional("wait_ms", POSITIVE),
            optional("force", BOOL),
            optional(
                "expected_generation",
                closed(&[
                    field("instance_id", NON_EMPTY),
                    field("generation", int_min(0)),
                    field("pid", POSITIVE),
                    field("process_start_time", NON_EMPTY),
                ]),
            ),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.stopAll",
        input: object(&[
            optional("drain", BOOL),
            optional("wait", BOOL),
            optional("deadline_ms", POSITIVE),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.handoff",
        input: object(&[
            optional("policy", POLICY),
            LAUNCH_SPEC,
            optional("allow_downgrade", BOOL),
            optional(
                "when",
                closed(&[field(
                    "idle",
                    closed(&[
                        field("operation", NON_EMPTY),
                        field("if_instance", NON_EMPTY),
                        field("if_generation", int_min(0)),
                        field("target_build", NON_EMPTY),
                    ]),
                )]),
            ),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.gc",
        input: object(&[
            optional("prune_store_index", BOOL),
            optional("agent_dir", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.rollbackPrepare",
        input: object(&[
            optional("stores", NON_EMPTY_LIST),
            optional("allow_missing_index", BOOL),
            optional("dry_run", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "daemon.shardPath",
        input: object(&[
            field(
                "kind",
                Shape::OneOf(&[Shape::TextLiteral("p"), Shape::TextLiteral("i")]),
            ),
            field("owner", NON_EMPTY),
            optional("root", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "diagnostics.run",
        input: object(&[
            optional("status", BOOL),
            optional("verbose", BOOL),
            optional(
                "platform",
                Shape::OneOf(&[Shape::TextLiteral("opencode"), Shape::TextLiteral("codex")]),
            ),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "diagnostics.models",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "diagnostics.fixReflectionLock",
        input: object(&[optional("dry_run", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "diagnostics.reapStaleEngines",
        input: object(&[field("pids", array(&POSITIVE, 1))]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.setup.detect",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.setup.plan",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.setup.apply",
        input: object(&[
            field("plan", SETUP_PLAN),
            field(
                "consent",
                Shape::OneOf(&[Shape::TextLiteral("all"), Shape::TextLiteral("each")]),
            ),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.update",
        input: object(&[optional("dry_run", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.version",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "install.sdkRoot",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "install.plugin",
        input: object(&[
            field("interactive", BOOL),
            optional(
                "platform",
                Shape::Enum(&["opencode", "codex", "both", "native", "native-dev"]),
            ),
            optional("claude", Shape::Enum(&["no", "yes", "max20"])),
            optional("openai", YES_NO),
            optional("gemini", YES_NO),
            optional("copilot", YES_NO),
            optional("opencodeZen", YES_NO),
            optional("zaiCodingPlan", YES_NO),
            optional("kimiForCoding", YES_NO),
            optional("opencodeGo", YES_NO),
            optional("bailianCodingPlan", YES_NO),
            optional("minimaxCnCodingPlan", YES_NO),
            optional("minimaxCodingPlan", YES_NO),
            optional("vercelAiGateway", YES_NO),
            optional("codex_autonomous", BOOL),
            field("skip_auth", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.cleanup",
        input: object(&[
            optional(
                "platform",
                Shape::Enum(&["opencode", "codex", "both", "native", "native-dev"]),
            ),
            optional("codex_home", NON_EMPTY),
            optional("project", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "install.localVersion",
        input: object(&[optional("directory", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "boulder.status",
        input: object(&[
            optional("directory", NON_EMPTY),
            optional("work_id", NON_EMPTY),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "worktrees.sweep",
        input: object(&[
            optional("apply", BOOL),
            optional("older_than_days", int_min(0)),
            optional("repos", NON_EMPTY_LIST),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.help",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    // The engine namespace (design 6.8), without the credential and launch rows above.
    CallSpec {
        key: "engine.config.agentDir",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "engine.config.resources",
        input: object(&[optional("local", BOOL), optional("trust", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.config.setResource",
        input: Shape::OneOf(&[
            closed(&[
                optional("trust", BOOL),
                field("source", NON_EMPTY),
                field("resource", RESOURCE),
                field("enabled", BOOL),
            ]),
            closed(&[
                field("local", Shape::BoolLiteral(true)),
                optional("trust", BOOL),
                field("source", NON_EMPTY),
                field("resource", RESOURCE),
                field("state", Shape::Enum(&["inherit", "load", "unload"])),
            ]),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.models.discover",
        input: object(&[field("provider", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.models.list",
        input: object(&[optional("pattern", NON_EMPTY), LAUNCH_SETUP]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.models.update",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.packages.install",
        input: object(&[
            field("source", NON_EMPTY),
            optional("local", BOOL),
            optional("trust", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.packages.remove",
        input: object(&[
            field("source", NON_EMPTY),
            optional("local", BOOL),
            optional("trust", BOOL),
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.packages.list",
        input: object(&[optional("trust", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.packages.update",
        input: object(&[optional("source", NON_EMPTY), optional("trust", BOOL)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.promptSizes",
        input: object(&[LAUNCH_SETUP]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.tips",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: false,
    },
    CallSpec {
        key: "engine.schedule.list",
        input: EMPTY,
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.schedule.cancel",
        input: object(&[field("id", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.schedule.run",
        input: object(&[
            optional("watch", BOOL),
            optional("exec", NON_EMPTY),
            optional("poll_seconds", POSITIVE),
            optional("timeout_seconds", POSITIVE),
            optional("concurrency", POSITIVE),
            optional("signal", HANDLE),
        ]),
        answer: AnswerShape::Stream,
        waits: true,
    },
    CallSpec {
        key: "engine.sessionOfPid",
        input: object(&[field("pid", POSITIVE)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "engine.sessions.exportHtml",
        input: object(&[field("session", NON_EMPTY), optional("out", NON_EMPTY)]),
        answer: AnswerShape::Value,
        waits: true,
    },
    // The delegated loop (design 6.11).
    CallSpec {
        key: "toolkit.forSession",
        input: object(&[field("cwd", NON_EMPTY), SESSION_ID]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.addGoal",
        input: object(&[
            field("title", GOAL),
            field("objective", GOAL),
            optional(
                "successCriteria",
                array(
                    &object(&[
                        field("scenario", TEXT),
                        field("expectedEvidence", TEXT),
                        optional("userModel", USER_MODEL),
                        optional("essential", BOOL),
                    ]),
                    0,
                ),
            ),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.checkpoint",
        input: object(&[
            field("goalId", GOAL),
            field("status", Shape::Enum(&["complete", "failed", "blocked"])),
            field("evidence", EVIDENCE),
            optional("codexGoalJson", TEXT),
            optional("qualityGateJson", TEXT),
            optional("printTemplate", Shape::BoolLiteral(true)),
            optional("advance", BOOL),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.completeGoals",
        input: object(&[optional("retryFailed", BOOL), SESSION_ID]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.createGoals",
        input: object(&[
            field("brief", TEXT),
            optional("codexGoalMode", Shape::Enum(&["aggregate", "per_story"])),
            optional("force", BOOL),
            optional("validationBatchesJson", TEXT),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.criteria",
        input: object(&[field("goalId", GOAL), SESSION_ID]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.recordEvidence",
        input: object(&[
            field("goalId", GOAL),
            field("criterionId", TEXT),
            field("status", Shape::Enum(&["pass", "fail", "blocked"])),
            field("evidence", EVIDENCE),
            optional("notes", TEXT),
            optional("artifacts", array(&TEXT, 0)),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.recordReviewBlockers",
        input: object(&[
            field("goalId", GOAL),
            field("title", TEXT),
            field("objective", TEXT),
            field("evidence", EVIDENCE),
            optional("codexGoalJson", TEXT),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.status",
        input: object(&[SESSION_ID]),
        answer: AnswerShape::Value,
        waits: true,
    },
    CallSpec {
        key: "toolkit.steer",
        input: object(&[
            field(
                "kind",
                Shape::Enum(&[
                    "add_subgoal",
                    "split_subgoal",
                    "reorder_pending",
                    "revise_pending_wording",
                    "revise_criterion",
                    "annotate_ledger",
                    "mark_blocked_superseded",
                ]),
            ),
            field(
                "source",
                Shape::Enum(&["finding", "user_prompt_submit", "cli"]),
            ),
            field("evidence", EVIDENCE),
            field("rationale", TEXT),
            optional("goalId", TEXT),
            optional("criterionId", TEXT),
            optional("scenario", TEXT),
            optional("expectedEvidence", TEXT),
            optional("userModel", USER_MODEL),
            optional("title", TEXT),
            optional("objective", TEXT),
            optional(
                "childGoals",
                array(
                    &object(&[field("title", TEXT), field("objective", TEXT)]),
                    0,
                ),
            ),
            optional("revisedTitle", TEXT),
            optional("revisedObjective", TEXT),
            optional("pendingOrder", array(&TEXT, 0)),
            optional("blockedReason", TEXT),
            optional("idempotencyKey", TEXT),
            optional("proposals", ANY),
            SESSION_ID,
        ]),
        answer: AnswerShape::Value,
        waits: true,
    },
];

const POLICY: Shape = Shape::OneOf(&[
    Shape::TextLiteral("upgrade"),
    Shape::TextLiteral("fallback"),
    Shape::TextLiteral("never"),
]);
const YES_NO: Shape = Shape::Enum(&["no", "yes"]);
const RESOURCE: Shape = closed(&[field("type", NON_EMPTY), field("path", NON_EMPTY)]);
const DESTINATION: Shape = Shape::OneOf(&[
    closed(&[field("kind", Shape::TextLiteral("terminal"))]),
    closed(&[
        field("kind", Shape::TextLiteral("peer")),
        field("pid", int_min(1)),
        field("started_at", NON_EMPTY),
    ]),
]);
const SESSION_CALLABLE: Shape = array(
    &Shape::OneOf(&[
        closed(&[
            field("op", NON_EMPTY),
            field("parameters", ANY_OBJECT),
            optional("targetArg", NON_EMPTY),
            field("toolName", Shape::ToolName),
            field("description", NON_EMPTY),
            optional(
                "await",
                closed(&[
                    field("statusOp", NON_EMPTY),
                    field("expireOp", NON_EMPTY),
                    field("timeoutMs", POSITIVE),
                ]),
            ),
            optional("wake", NON_EMPTY),
        ]),
        closed(&[
            field("op", NON_EMPTY),
            field("parameters", ANY_OBJECT),
            optional("targetArg", NON_EMPTY),
            field("internal", Shape::BoolLiteral(true)),
            optional("description", NON_EMPTY),
        ]),
    ]),
    0,
);
const SETUP_PLAN: Shape = object(&[
    field(
        "inventory",
        object(&[field(
            "harnesses",
            array(
                &object(&[
                    field("id", TEXT),
                    field("installed", BOOL),
                    field("providers", array(&TEXT, 0)),
                    field("credentialTypes", array(&TEXT, 0)),
                    field("modelHint", TEXT),
                    field("notices", array(&TEXT, 0)),
                    field("importableCount", int_min(0)),
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
]);

/// The call `key`, or `None` when the root has no such call.
pub fn call(key: &str) -> Option<&'static CallSpec> {
    ROOT_CALLS.iter().find(|call| call.key == key)
}

/// Whether the root has the call `key` (`validate.ts`'s `isCall`).
pub fn is_root_call(key: &str) -> bool {
    call(key).is_some()
}

/// Whether `input` is inside the input schema of the call `key`; an unknown key accepts nothing (`inputAccepts`).
pub fn input_accepts(key: &str, input: &Value) -> bool {
    call(key).is_some_and(|call| {
        if crate::inputs::custody::is_custody_input(call.key) {
            custody_input_accepts(call.key, input)
        } else {
            accepts(&call.input, input)
        }
    })
}

/// The answer shape of the call `key`.
pub fn answer_shape(key: &str) -> Option<AnswerShape> {
    call(key).map(|call| call.answer)
}

/// Whether the call `key` can wait, so it takes `CallOptions` at all.
pub fn can_wait(key: &str) -> bool {
    call(key).is_some_and(|call| call.waits)
}

/// Whether the call's input may be omitted entirely (`Partial<Input> extends Input`).
pub fn input_omittable(key: &str) -> bool {
    call(key).is_some_and(|call| all_fields_optional(&call.input))
}

/// Whether the call takes the `deadline_ms` wait option: a call that can wait and whose input does not already name
/// `deadline_ms` (`OptionsFor` in `root.ts`).
pub fn takes_deadline_option(key: &str) -> bool {
    call(key).is_some_and(|call| call.waits && !names_field(&call.input, "deadline_ms"))
}

/// The namespace a key belongs to: its first dotted segment.
pub fn namespace_of(key: &str) -> &str {
    key.split('.').next().unwrap_or(key)
}

/// The keys of the calls in `namespace`, in table order.
pub fn calls_of(namespace: &str) -> Vec<&'static str> {
    ROOT_CALLS
        .iter()
        .filter(|call| namespace_of(call.key) == namespace)
        .map(|call| call.key)
        .collect()
}
