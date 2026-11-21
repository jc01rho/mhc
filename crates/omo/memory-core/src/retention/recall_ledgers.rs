//! Port of `retention/recall-ledgers.ts`: per-session recall ledgers (design 2.5). A resumed session
//! starts with an empty surfaced set.

use super::budget::RetentionStore;
use super::policy::{
    RetentionContext, RetentionError, RetentionKeepReason, RetentionStoreResult, StoreTally,
    days_ms, list_names, lstat_or_none, mtime_ms,
};
use crate::fs::resilient::remove_file;

pub fn prune_recall_ledgers(
    context: &RetentionContext<'_>,
) -> Result<RetentionStoreResult, RetentionError> {
    let mut tally = StoreTally::default();
    let cutoff = context.now_ms() - days_ms(context.policy.completions_days);
    for name in list_names(&context.paths.recall_ledger)? {
        let Some(session_id) = name.strip_suffix(".json") else {
            continue;
        };
        if context
            .budget
            .is_some_and(|budget| budget.skip(RetentionStore::Ledgers, &name))
        {
            continue;
        }
        if context
            .budget
            .is_some_and(|budget| !budget.admit(RetentionStore::Ledgers))
        {
            break;
        }
        let path = context.paths.recall_ledger.join(&name);
        let Some(stats) = lstat_or_none(&path).filter(|stats| stats.is_file()) else {
            continue;
        };
        if context.is_live(session_id) {
            tally.keep(session_id, RetentionKeepReason::Live);
        } else if mtime_ms(&stats) > cutoff {
            tally.keep(session_id, RetentionKeepReason::Recent);
        } else {
            match remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(RetentionError::Io(error));
                }
                _ => tally.removed.push(session_id.to_string()),
            }
        }
        if let Some(budget) = context.budget {
            budget.done(RetentionStore::Ledgers, &name);
        }
    }
    Ok(tally.result())
}

#[cfg(test)]
#[path = "recall_ledgers_tests.rs"]
mod tests;
