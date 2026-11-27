//! The principal-by-capability matrix of the security design's section 4 (#14945), one line per row of its tables.

mod account;
mod host;
mod launch;
mod threads;
mod types;

pub use account::ACCOUNT_LINES;
pub use host::HOST_LINES;
pub use launch::LAUNCH_LINES;
pub use threads::THREAD_LINES;
pub use types::{Cell, Cells, MatrixLine};

use crate::behaviour_changes::ChangeId;
use crate::principal::{PRINCIPAL_KINDS, PrincipalKind};

/// Every line, in the document's order: threads, account, host, launch.
pub fn principal_matrix() -> Vec<&'static MatrixLine> {
    THREAD_LINES
        .iter()
        .chain(ACCOUNT_LINES)
        .chain(HOST_LINES)
        .chain(LAUNCH_LINES)
        .collect()
}

/// One refused cell on one registry row: answered with `code`, before any side effect, for `principal` only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub scenario: String,
    pub row: &'static str,
    pub principal: PrincipalKind,
    pub code: &'static str,
    pub section: &'static str,
    pub change: Option<ChangeId>,
    pub before_side_effect: bool,
}

pub fn refusal_scenario_id(row: &str, slug: &str, kind: PrincipalKind) -> String {
    format!("{row}/{slug}-refuses-{}", kind.as_str())
}

/// Every refused cell of the matrix on every row its line governs.
pub fn refusals() -> Vec<Refusal> {
    let mut out = Vec::new();
    for line in principal_matrix() {
        for &row in line.rows {
            for principal in PRINCIPAL_KINDS {
                if let Cell::Refuse { code, change, .. } = line.cells.get(principal) {
                    out.push(Refusal {
                        scenario: refusal_scenario_id(row, line.slug, principal),
                        row,
                        principal,
                        code,
                        section: line.section,
                        change,
                        before_side_effect: true,
                    });
                }
            }
        }
    }
    out
}
