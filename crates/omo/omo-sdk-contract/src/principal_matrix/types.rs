//! One line of the security design's section 4 (#14945), keyed by the five principal kinds. Person and script share
//! the document's one column; a line tells them apart only with `split`, a verbatim quote from sections 3 or 4 that
//! says why.

use crate::behaviour_changes::ChangeId;
use crate::principal::PrincipalKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Allow {
        note: Option<&'static str>,
    },
    /// Answered with `code`, before any side effect, for this principal only.
    Refuse {
        code: &'static str,
        change: Option<ChangeId>,
        note: Option<&'static str>,
    },
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cells {
    pub person: Cell,
    pub script: Cell,
    pub connector: Cell,
    pub session: Cell,
    pub service: Cell,
}

impl Cells {
    pub const fn get(&self, kind: PrincipalKind) -> Cell {
        match kind {
            PrincipalKind::Person => self.person,
            PrincipalKind::Script => self.script,
            PrincipalKind::Connector => self.connector,
            PrincipalKind::Session => self.session,
            PrincipalKind::Service => self.service,
        }
    }
}

/// `capability` is the document's first column verbatim, or for a line derived from a row with an input or a split
/// the document names, what it covers, with `doc_row` naming that row verbatim. `rows` is empty only with `no_row`
/// saying why no root call exists for it. `input` is the input that reaches the cell and `root` a root option the
/// cell is about, both as JSON text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatrixLine {
    pub section: &'static str,
    pub capability: &'static str,
    pub doc_row: Option<&'static str>,
    pub slug: &'static str,
    pub rows: &'static [&'static str],
    pub no_row: Option<&'static str>,
    pub input: Option<&'static str>,
    pub root: Option<&'static str>,
    pub cells: Cells,
    pub split: Option<&'static str>,
}
