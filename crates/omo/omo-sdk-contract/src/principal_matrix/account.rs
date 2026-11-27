//! Section 4.2 of the security design (#14945): devices, account, device service, sync.

use super::types::{Cell, Cells, MatrixLine};
use crate::behaviour_changes::ChangeId;

pub const ACCOUNT_LINES: &[MatrixLine] = &[
    MatrixLine {
        section: "4.2",
        capability: "`devices.list`, `account.whoami`, `sync.status` (reads)",
        doc_row: Some(
            "`devices.list`, `account.whoami`, `deviceService.status`, `control()`, `wake()`, `sync.status` (reads)",
        ),
        slug: "account-read",
        rows: &["devices.list", "account.whoami", "sync.status"],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: Some("new"),
            },
            session: Cell::Allow {
                note: Some(
                    "through the SDK in an eval cell; sync.status once its output is shown to hold no credential",
                ),
            },
            service: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: Some("the service column allows the service reads only"),
            },
        },
        split: None,
    },
    MatrixLine {
        section: "4.2",
        capability: "`deviceService.status`, `control()`, `wake()` (reads)",
        doc_row: Some(
            "`devices.list`, `account.whoami`, `deviceService.status`, `control()`, `wake()`, `sync.status` (reads)",
        ),
        slug: "service-read",
        rows: &[
            "deviceService.status",
            "deviceService.info",
            "deviceService.control",
            "deviceService.wake",
        ],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow { note: None },
            script: Cell::Allow { note: None },
            connector: Cell::Allow { note: None },
            session: Cell::Allow {
                note: Some("once its output is shown to hold no credential"),
            },
            service: Cell::Allow { note: None },
        },
        split: None,
    },
    MatrixLine {
        section: "4.2",
        capability: "`deviceService.setEnabled`, `setControl`, `setWake`",
        doc_row: None,
        slug: "service-switch",
        rows: &[
            "deviceService.enable",
            "deviceService.disable",
            "deviceService.setControl",
            "deviceService.setWake",
        ],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "a script through the person client (section 3); R/client.ts refuses a cli sender on the wire",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "a script through the person client (section 3); R/client.ts refuses a cli sender on the wire",
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
                note: Some("not a person"),
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
        section: "4.2",
        capability: "`deviceService.bootstrap`",
        doc_row: None,
        slug: "bootstrap",
        rows: &["deviceService.bootstrap"],
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
            service: Cell::Allow { note: None },
        },
        split: None,
    },
    MatrixLine {
        section: "4.2",
        capability: "`devices.rename`",
        doc_row: None,
        slug: "rename",
        rows: &["devices.rename"],
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
                note: Some("as shipped"),
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
        section: "4.2",
        capability: "`devices.revoke` (never this device; an ambiguous name or prefix revokes nothing; the name as confirmation)",
        doc_row: None,
        slug: "revoke",
        rows: &["devices.revoke"],
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
        section: "4.2",
        capability: "`account.signIn`",
        doc_row: None,
        slug: "sign-in",
        rows: &["account.signIn"],
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
                note: Some(
                    "where the shipped SDK allows it: the codes reach a callback a person reads",
                ),
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
        section: "4.2",
        capability: "`account.signIn` with `replace`",
        doc_row: Some("`account.signIn`"),
        slug: "sign-in-replace",
        rows: &["account.signIn"],
        no_row: None,
        input: Some(r#"{"replace":true}"#),
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
                change: Some(ChangeId::B4),
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
        section: "4.2",
        capability: "The account service origin on any account call",
        doc_row: None,
        slug: "origin",
        rows: &["account.signIn", "account.whoami"],
        no_row: None,
        input: None,
        root: Some(r#"{"api":"https://api.example.test"}"#),
        cells: Cells {
            person: Cell::Allow {
                note: Some("as today: the caller supplies it"),
            },
            script: Cell::Allow {
                note: Some("as today: the caller supplies it"),
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: Some("a session's call uses the configured origin"),
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
        section: "4.2",
        capability: "`account.signOut`, `account.forget`",
        doc_row: None,
        slug: "sign-out",
        rows: &["account.signOut", "account.forget"],
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
        section: "4.2",
        capability: "`account.adopt`, `account.credentials.handOver`, `account.store.*`, `account.keys.*` except `account.keys.status` (4.4: it reports state and no key material)",
        doc_row: None,
        slug: "custody",
        rows: &[
            "account.adopt",
            "account.credentials.handOver",
            "account.store.status",
            "account.store.enable",
            "account.store.disable",
            "account.keys.init",
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
        section: "4.2",
        capability: "`sync.enable`, `sync.recover`",
        doc_row: None,
        slug: "phrase",
        rows: &["sync.enable", "sync.recover"],
        no_row: None,
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Allow {
                note: Some(
                    "only where the terminal check of 5.2 admits the terminal; otherwise refused before anything secret exists",
                ),
            },
            script: Cell::Allow {
                note: Some(
                    "only where the terminal check of 5.2 admits the terminal; otherwise refused before anything secret exists",
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
        section: "4.2",
        capability: "`sync.approve`, `sync.disable`, `sync.retirePin`",
        doc_row: None,
        slug: "sync-change",
        rows: &["sync.approve", "sync.disable", "sync.retirePin"],
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
        section: "4.2",
        capability: "`account.audit.list` (8.2)",
        doc_row: None,
        slug: "audit-list",
        rows: &[],
        no_row: Some(
            "the registry has no row for it yet; the slice that adds the custody log adds the row and the call",
        ),
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
        section: "4.2",
        capability: "Changing the peer allowlist or the named principals for credential return",
        doc_row: None,
        slug: "peer-allowlist",
        rows: &[],
        no_row: Some(
            "no call exists and none may: the list ships with the build and no caller enrols a peer",
        ),
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            script: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
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
        section: "4.2",
        capability: "Any account call under a desktop launch",
        doc_row: None,
        slug: "desktop-launch",
        rows: &[],
        no_row: Some(
            "a launch condition, not a principal: every account call answers desktop_sign_in under a desktop launch",
        ),
        input: None,
        root: None,
        cells: Cells {
            person: Cell::Refuse {
                code: "desktop_sign_in",
                change: None,
                note: None,
            },
            script: Cell::Refuse {
                code: "desktop_sign_in",
                change: None,
                note: None,
            },
            connector: Cell::Refuse {
                code: "sender_not_allowed",
                change: None,
                note: None,
            },
            session: Cell::Refuse {
                code: "desktop_sign_in",
                change: None,
                note: None,
            },
            service: Cell::NotApplicable,
        },
        split: None,
    },
];
