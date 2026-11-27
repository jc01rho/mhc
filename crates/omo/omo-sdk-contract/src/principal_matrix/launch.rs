//! Section 4.4 of the security design (#14945): the endpoint selector, launch options and the OpenCode edition.

use super::types::{Cell, Cells, MatrixLine};

pub const LAUNCH_LINES: &[MatrixLine] = &[
    MatrixLine {
        section: "4.4",
        capability: "The root's `endpoint` selector (`socket`, `key_root`) and the endpoint options (the same `socket` and `key_root`) of `daemon.ensure`, `stop`, `handoff`",
        doc_row: None,
        slug: "endpoint",
        rows: &["daemon.ensure", "daemon.stop", "daemon.handoff"],
        no_row: None,
        input: None,
        root: Some(r#"{"endpoint":{"socket":"/run/omo-test/host.sock"}}"#),
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "the selector names paths; the transport reads the credential under key_root and never returns it",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "the selector names paths; the transport reads the credential under key_root and never returns it",
                ),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`engine.launch.interactive`, `print`, `rpc`, `hand`, `headless`",
        doc_row: None,
        slug: "launch",
        rows: &[
            "engine.launch.interactive",
            "engine.launch.print",
            "engine.launch.rpc",
            "engine.launch.hand",
            "engine.launch.headless",
        ],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`spawned_by` (ancestry) on a launch",
        doc_row: None,
        slug: "spawned-by",
        rows: &[
            "engine.launch.interactive",
            "engine.launch.print",
            "engine.launch.rpc",
            "engine.launch.hand",
            "engine.launch.headless",
        ],
        no_row: None,
        input: Some(
            r#"{"session":{"spawned_by":{"parentSessionId":"p1","rootSessionId":"r1","taskId":"t1"}}}"#,
        ),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "only from the engine's own task host, which sets it today; any other caller is refused",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "only from the engine's own task host, which sets it today; any other caller is refused",
                ),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`project_trust` on a launch or a package call",
        doc_row: None,
        slug: "project-trust",
        rows: &[
            "engine.launch.interactive",
            "engine.launch.print",
            "engine.launch.rpc",
            "engine.launch.hand",
            "engine.launch.headless",
        ],
        no_row: None,
        input: Some(r#"{"setup":{"project_trust":true}}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("passed explicitly, never defaulted to trust"),
            },
            script: Cell::Allow {
                note: Some("passed explicitly, never defaulted to trust"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`project_trust` on a package call (`trust`)",
        doc_row: Some("`project_trust` on a launch or a package call"),
        slug: "project-trust",
        rows: &[
            "engine.packages.install",
            "engine.packages.remove",
            "engine.packages.update",
        ],
        no_row: None,
        input: Some(r#"{"trust":true}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("passed explicitly, never defaulted to trust"),
            },
            script: Cell::Allow {
                note: Some("passed explicitly, never defaulted to trust"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "The presenter inputs of `engine.launch.rpc` and of the host's ensure and handoff (`bootstrap_fd`, `enforcement`)",
        doc_row: None,
        slug: "presenter",
        rows: &["engine.launch.rpc"],
        no_row: None,
        input: Some(r#"{"presenter":{"bootstrap_fd":4,"enforcement":true}}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "only from the launcher that starts a host today; unproven until the closed matrix cites it",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "only from the launcher that starts a host today; unproven until the closed matrix cites it",
                ),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "The presenter inputs of the host's ensure (`mcp_presenter_enforcement`)",
        doc_row: Some(
            "The presenter inputs of `engine.launch.rpc` and of the host's ensure and handoff (`bootstrap_fd`, `enforcement`)",
        ),
        slug: "presenter",
        rows: &["daemon.ensure"],
        no_row: None,
        input: Some(r#"{"mcp_presenter_enforcement":true}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "only from the launcher that starts a host today; unproven until the closed matrix cites it",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "only from the launcher that starts a host today; unproven until the closed matrix cites it",
                ),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`codex_autonomous` on `install.plugin`: `true` configures that platform's autonomous permissions, `false` leaves existing settings unchanged, absent takes the mode's default",
        doc_row: None,
        slug: "codex-autonomous",
        rows: &["install.plugin"],
        no_row: None,
        input: Some(r#"{"codex_autonomous":true}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`toolkit.forSession({ cwd, session_id? })`: binding the loop operations to one loop session",
        doc_row: None,
        slug: "for-session",
        rows: &["toolkit.forSession"],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("for the session it names"),
            },
            script: Cell::Allow {
                note: Some("for the session it names"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Allow {
                note: Some("only for its own session, with the id from the engine"),
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`toolkit.forSession` naming a session in its argument",
        doc_row: Some(
            "`toolkit.forSession({ cwd, session_id? })`: binding the loop operations to one loop session",
        ),
        slug: "for-session-named",
        rows: &["toolkit.forSession"],
        no_row: None,
        input: Some(r#"{"session_id":"other-session"}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("for the session it names"),
            },
            script: Cell::Allow {
                note: Some("for the session it names"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: Some("never from an argument"),
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "A provider API key as a launch input",
        doc_row: None,
        slug: "api-key",
        rows: &[],
        no_row: Some(
            "a readable handle (LaunchKeyHandle); a JSON scenario cannot carry one, so P1b's driver supplies it for the launch rows' refusals above",
        ),
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("through the carrier of 5.5"),
            },
            script: Cell::Allow {
                note: Some("through the carrier of 5.5"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`threads.runToCompletion` (OpenCode edition)",
        doc_row: None,
        slug: "run",
        rows: &["threads.runToCompletion"],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`on_complete` on `runToCompletion`: runs a command as the caller's OS user",
        doc_row: None,
        slug: "on-complete",
        rows: &["threads.runToCompletion"],
        no_row: None,
        input: Some(r#"{"on_complete":{"command":"true"}}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some("runs a command as the caller's OS user"),
            },
            script: Cell::Allow {
                note: Some("runs a command as the caller's OS user"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`install.plugin`, `install.cleanup`, `config.migrate`, `worktrees.sweep` with `apply`; `config.modelCapabilities.refresh` (it writes a cache and fetches from a URL the caller may supply)",
        doc_row: None,
        slug: "opencode-change",
        rows: &[
            "install.plugin",
            "install.cleanup",
            "config.migrate",
            "config.modelCapabilities.refresh",
        ],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`worktrees.sweep` with `apply`",
        doc_row: Some(
            "`install.plugin`, `install.cleanup`, `config.migrate`, `worktrees.sweep` with `apply`; `config.modelCapabilities.refresh` (it writes a cache and fetches from a URL the caller may supply)",
        ),
        slug: "sweep-apply",
        rows: &["worktrees.sweep"],
        no_row: None,
        input: Some(r#"{"apply":true}"#),
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`mcp.oauth.login`, `logout`; `account.keys.init`",
        doc_row: None,
        slug: "opencode-credential",
        rows: &["mcp.oauth.login", "mcp.oauth.logout", "account.keys.init"],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.4",
        capability: "`mcp.oauth.status`, `account.keys.status`, `boulder.status`, `install.localVersion`, `worktrees.sweep` as a plan (reads)",
        doc_row: None,
        slug: "opencode-read",
        rows: &[
            "mcp.oauth.status",
            "account.keys.status",
            "boulder.status",
            "install.localVersion",
            "worktrees.sweep",
        ],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Allow {
                note: Some("as an eval-cell read"),
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
        },
        split: None,
    },
];
