use maho_core::credential_pool::state_store::{
    BlockReason, CredentialMaterial, CredentialSlotRepository, CredentialSlotState,
    applicable_stored_state, retire_login_slot_health, slots_started_by_login,
};
use maho_core::input_origin::{
    ConnectionOrigin, InputTurnMark, TurnOrigin, TurnOriginTracker, input_turn_mark,
    record_turn_origin, resolve_connection_turn_origin,
};
use maho_core::moved_session_cwd::{
    read_recorded_reopen_cwd_from_file, read_recorded_reopen_cwd_from_header_line,
    recorded_reopen_cwd, reopen_session_open_request,
};
use maho_ext_api::InputSource;
use serde_json::json;

#[test]
fn reopen_returns_recorded_absolute_folder_immediately_without_probing_missing_paths() {
    let unmounted = "/nonexistent/host/workspace/project-14296";
    assert_eq!(
        recorded_reopen_cwd(Some(unmounted)),
        Some(unmounted.to_owned())
    );
    assert_eq!(recorded_reopen_cwd(Some("relative/folder")), None);
    assert_eq!(recorded_reopen_cwd(Some("   ")), None);
    assert_eq!(recorded_reopen_cwd(None), None);

    let header = json!({
        "type": "session",
        "version": 3,
        "id": "019a0000-0000-7000-8000-000000014296",
        "timestamp": "2026-11-19T00:00:00.000Z",
        "cwd": unmounted
    })
    .to_string();
    assert_eq!(
        read_recorded_reopen_cwd_from_header_line(&header),
        Some(unmounted.to_owned())
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let session_file = dir.path().join("reopen.jsonl");
    std::fs::write(&session_file, format!("{header}\n")).expect("write session header");
    assert_eq!(
        read_recorded_reopen_cwd_from_file(&session_file),
        Some(unmounted.to_owned())
    );
    assert_eq!(
        reopen_session_open_request("/sessions/reopen.jsonl", Some(unmounted)),
        json!({
            "sessionPath": "/sessions/reopen.jsonl",
            "cwd": unmounted,
        })
    );
    assert_eq!(
        reopen_session_open_request("/sessions/reopen.jsonl", Some("relative/path")),
        json!({
            "sessionPath": "/sessions/reopen.jsonl",
        })
    );
}

#[tokio::test]
async fn account_keyed_blocks_survive_token_refresh_and_login_retires_slot_health() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state_path = dir
        .path()
        .join("credential-pool-state.json")
        .to_string_lossy()
        .into_owned();
    let repo = CredentialSlotRepository::new(&state_path);

    let initial = CredentialMaterial {
        key: None,
        access: Some("access-token-v1".to_owned()),
        refresh: Some("refresh-token-v1".to_owned()),
    };
    let refreshed = CredentialMaterial {
        key: None,
        access: Some("access-token-v2".to_owned()),
        refresh: Some("refresh-token-v2".to_owned()),
    };
    let rev_initial = repo
        .stored_slot_revisions("anthropic-subscription", "slot-a", &initial)
        .await
        .expect("initial revisions");
    let rev_refreshed = repo
        .stored_slot_revisions("anthropic-subscription", "slot-a", &refreshed)
        .await
        .expect("refreshed revisions");
    assert_eq!(rev_initial.account, rev_refreshed.account);
    assert_ne!(rev_initial.material, rev_refreshed.material);

    let rate_limited = CredentialSlotState {
        state_version: 1,
        blocked_until: Some(999_999),
        block_reason: Some(BlockReason::RateLimit),
        failure_count: Some(1),
        last_success_at: None,
        credential_revision: Some(rev_initial.account.clone()),
        lease: None,
    };
    assert_eq!(
        applicable_stored_state(Some(&rate_limited), &rev_refreshed),
        Some(&rate_limited)
    );

    let token_rejected = CredentialSlotState {
        state_version: 1,
        blocked_until: None,
        block_reason: Some(BlockReason::AuthError),
        failure_count: Some(1),
        last_success_at: None,
        credential_revision: Some(rev_initial.material.clone()),
        lease: None,
    };
    assert_eq!(
        applicable_stored_state(Some(&token_rejected), &rev_initial),
        Some(&token_rejected)
    );
    assert_eq!(
        applicable_stored_state(Some(&token_rejected), &rev_refreshed),
        None
    );

    let _persisted = repo
        .mutate_slot_state("anthropic-subscription", "stored", "slot-a", |_| {
            Some(rate_limited.clone())
        })
        .await
        .expect("persist block");
    let before = json!({
        "type": "oauth",
        "accounts": [
            { "name": "slot-a", "access": "access-token-v1", "refresh": "refresh-token-v1" },
            { "name": "slot-b", "access": "stay", "refresh": "stay" }
        ]
    });
    let after = json!({
        "type": "oauth",
        "accounts": [
            { "name": "slot-a", "access": "new-login-access", "refresh": "new-login-refresh" },
            { "name": "slot-b", "access": "stay", "refresh": "stay" },
            { "name": "slot-c", "access": "added", "refresh": "added" }
        ]
    });
    assert_eq!(
        slots_started_by_login(Some(&before), Some(&after)),
        vec!["slot-a".to_owned(), "slot-c".to_owned()]
    );
    retire_login_slot_health(
        &state_path,
        "anthropic-subscription",
        Some(&before),
        Some(&after),
    )
    .await;
    let remaining = repo
        .list_slots("anthropic-subscription", "stored")
        .await
        .expect("list slots");
    assert!(!remaining.contains_key("slot-a"));
}

#[test]
fn same_account_delegation_follows_host_and_record_scope_does_not_leak_across_turns() {
    assert_eq!(
        resolve_connection_turn_origin(ConnectionOrigin::Delegated, true),
        ConnectionOrigin::Local
    );
    assert_eq!(
        resolve_connection_turn_origin(ConnectionOrigin::Delegated, false),
        ConnectionOrigin::Delegated
    );
    assert_eq!(
        record_turn_origin(ConnectionOrigin::Delegated, true),
        ConnectionOrigin::Delegated
    );
    assert_eq!(
        record_turn_origin(ConnectionOrigin::Delegated, false),
        ConnectionOrigin::Local
    );

    let mut tracker = TurnOriginTracker::new();
    tracker.begin_record_scope(ConnectionOrigin::Delegated);
    assert!(tracker.record_is_delegated());
    tracker.note_run_start();
    assert!(tracker.deciding_turn_is_delegated());
    assert!(!tracker.record_is_delegated());

    tracker.note_turn_continuation(true);
    tracker.note_run_start();
    assert!(tracker.deciding_turn_is_delegated());
    tracker.end_turn_continuation();

    tracker.note_run_start();
    assert!(!tracker.deciding_turn_is_delegated());
    assert_eq!(tracker.current_origin(), TurnOrigin::Engine);

    let steer_mark = input_turn_mark(
        Some("steer"),
        Some(InputSource::Rpc),
        Some(ConnectionOrigin::Delegated),
        true,
    );
    assert_eq!(steer_mark, InputTurnMark::Join);
    tracker.note_user_message_start(Some(InputTurnMark::Delegated));
    assert!(tracker.deciding_turn_is_delegated());
    tracker.note_user_message_start(Some(steer_mark));
    assert!(tracker.deciding_turn_is_delegated());
}
