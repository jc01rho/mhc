//! The approved behaviour changes of the security design (#14945, section 10), B1 to B6 and the owner's B7, as data.
//! Each carries the expectation a scenario asserts today and the approved one; `evidence_present` selects which, and
//! it is false for every entry until the change's own evidence exists. An entry whose behaviour does not exist today
//! (an SDK operation with no shipped form) has no today expectation: it refuses from its first release.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChangeId {
    B1,
    B2,
    B3,
    B4,
    B5,
    B6,
    B7,
}

impl ChangeId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::B1 => "B1",
            Self::B2 => "B2",
            Self::B3 => "B3",
            Self::B4 => "B4",
            Self::B5 => "B5",
            Self::B6 => "B6",
            Self::B7 => "B7",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExpectedOutcome {
    Allow,
    Refuse,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Expectation {
    pub outcome: ExpectedOutcome,
    pub code: Option<&'static str>,
    pub says: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exists {
    Today(Expectation),
    New { why_new: &'static str },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BehaviourChange {
    pub id: ChangeId,
    pub rows: &'static [&'static str],
    pub who: &'static str,
    pub exists: Exists,
    pub approved: Expectation,
    pub evidence_present: bool,
}

pub const BEHAVIOUR_CHANGES: &[BehaviourChange] = &[
    BehaviourChange {
        id: ChangeId::B1,
        rows: &[
            "account.credentials.handOver",
            "engine.auth.credentials",
            "engine.auth.apiKey",
            "engine.auth.bearerToken",
        ],
        who: "a program that read the value from the verbs' captured standard output",
        exists: Exists::Today(Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "omo account token|export and omo auth print-* write the value to standard output",
        }),
        approved: Expectation {
            outcome: ExpectedOutcome::Refuse,
            code: Some("unsupported"),
            says: "a typed refusal that names the protected delivery; the value is never written; the store is not read",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B2,
        rows: &[
            "account.credentials.handOver",
            "engine.auth.credentials",
            "engine.auth.apiKey",
            "engine.auth.bearerToken",
        ],
        who: "a person running the same verbs on a terminal",
        exists: Exists::Today(Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "the value is printed on the terminal",
        }),
        approved: Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "on an admitted terminal the confirmation code, then the value on a wiped screen; elsewhere a refusal naming the desktop app",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B3,
        rows: &["sync.enable"],
        who: "a person running omo sync enable",
        exists: Exists::Today(Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "the phrase is shown on any terminal that opens with no agent marker",
        }),
        approved: Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "as today on an admitted terminal; where the check refuses, a refusal naming the desktop app before any phrase exists",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B4,
        rows: &["account.signIn"],
        who: "a session calling account.signIn with replace",
        exists: Exists::Today(Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "the shipped SDK's signIn takes replace with no principal check and replaces this machine's earlier device",
        }),
        approved: Expectation {
            outcome: ExpectedOutcome::Refuse,
            code: Some("sender_not_allowed"),
            says: "a typed refusal; no device is removed",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B5,
        rows: &[
            "threads.list",
            "threads.read",
            "threads.entries",
            "threads.get",
            "threads.wait",
            "threads.follow",
            "threads.interrupt",
            "threads.close",
            "threads.setModel",
            "threads.setReasoning",
            "threads.rename",
            "threads.archive",
            "threads.handoff",
        ],
        who: "a connector root calling a thread read or control",
        exists: Exists::New {
            why_new: "no caller is a connector root today: the connectors that exist act as script principals (4.5)",
        },
        approved: Expectation {
            outcome: ExpectedOutcome::Refuse,
            code: Some("sender_not_allowed"),
            says: "a typed refusal",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B6,
        rows: &["engine.auth.check"],
        who: "a session calling engine.auth.check",
        exists: Exists::New {
            why_new: "engine.auth.check has no SDK form today; a session reaches the verb only through its shell, as a script",
        },
        approved: Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "status only, with the refresh off: the auth store is not written; whether each account is signed in, the provider and closed status codes, and no text a provider returned",
        },
        evidence_present: false,
    },
    BehaviourChange {
        id: ChangeId::B7,
        rows: &["engine.auth.check"],
        who: "a program reading omo auth check from anywhere but an admitted terminal (a pipe, a file, a tool runner, an agent pane)",
        exists: Exists::Today(Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "the command prints the provider's own text, wherever its output goes",
        }),
        approved: Expectation {
            outcome: ExpectedOutcome::Allow,
            code: None,
            says: "off an admitted terminal, the closed-code message a session gets, with no provider-returned text; on an admitted terminal the print is unchanged",
        },
        evidence_present: false,
    },
];

/// The expectation a scenario asserts now: today's until the change's evidence exists, else the approved one.
pub fn asserted_expectation(change: &BehaviourChange) -> &Expectation {
    match &change.exists {
        Exists::Today(today) if !change.evidence_present => today,
        _ => &change.approved,
    }
}

pub fn behaviour_change(id: ChangeId) -> Option<&'static BehaviourChange> {
    BEHAVIOUR_CHANGES.iter().find(|change| change.id == id)
}
