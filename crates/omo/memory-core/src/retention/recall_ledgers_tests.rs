use super::*;

use crate::identity::layout::MemoryIdentityPaths;
use crate::retention::policy::RuntimeRetentionPolicy;
use crate::retention::test_support::{NOW, age_tree, days_ago, exists, identity, write_json};

fn pruned(
    paths: &MemoryIdentityPaths,
    live: Option<&dyn Fn(&str) -> bool>,
) -> RetentionStoreResult {
    let now = || NOW;
    let context = RetentionContext {
        is_live_session: live,
        ..RetentionContext::new(paths, RuntimeRetentionPolicy::default(), &now)
    };
    prune_recall_ledgers(&context).expect("prune recall ledgers")
}

#[test]
fn an_old_recall_ledger_for_an_idle_and_for_a_live_session_removes_only_the_idle_one() {
    let id = identity();
    let paths = &id.paths;
    write_json(
        &paths.recall_ledger.join("idle.json"),
        &serde_json::json!({ "surfaced": [] }),
    );
    write_json(
        &paths.recall_ledger.join("live.json"),
        &serde_json::json!({ "surfaced": [] }),
    );
    age_tree(&paths.recall_ledger, days_ago(31.0));

    let live = |session_id: &str| session_id == "live";
    let report = pruned(paths, Some(&live));

    assert_eq!(report.removed, vec!["idle".to_string()]);
    assert_eq!(report.kept["live"].as_str(), "live");
    assert!(exists(&paths.recall_ledger.join("live.json")));
    assert!(!exists(&paths.recall_ledger.join("idle.json")));
}

#[test]
fn a_recent_ledger_and_a_non_json_entry_are_left_alone() {
    let id = identity();
    let paths = &id.paths;
    write_json(
        &paths.recall_ledger.join("fresh.json"),
        &serde_json::json!({ "surfaced": [] }),
    );
    write_json(
        &paths.recall_ledger.join("readme.txt"),
        &serde_json::json!({ "surfaced": [] }),
    );
    age_tree(&paths.recall_ledger, days_ago(2.0));

    let report = pruned(paths, None);

    assert_eq!(report.kept["fresh"].as_str(), "recent");
    assert!(report.removed.is_empty());
    assert!(exists(&paths.recall_ledger.join("readme.txt")));
}
