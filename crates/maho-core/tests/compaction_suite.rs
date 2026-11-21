use maho_core::compaction::compaction::{
    cjk_extra_chars, estimate_cut_point_tokens, estimate_tokens, find_cut_point,
};
use maho_core::compaction::compaction_execution::{
    failed_compaction_latches_admission, is_non_latching_rejection_cause,
    should_exclude_from_provider_fallback,
};
use maho_core::compaction::lifecycle::{
    BeginCompactionOperation, CompactionFinishStatus, CompactionLifecycleState, CompactionStage,
    FinishCompactionOperation, begin_compaction_operation, finish_compaction_operation,
    initial_compaction_lifecycle_state,
};
use maho_core::compaction::utils::truncate_utf8_at_char_boundary;
use serde_json::{Value, json};

fn message_entry(id: &str, parent_id: Option<&str>, role: &str, text: &str) -> Value {
    let content = if role == "assistant" {
        json!([{ "type": "text", "text": text }])
    } else {
        json!(text)
    };
    json!({
        "type": "message",
        "id": id,
        "parentId": parent_id,
        "timestamp": "2026-11-19T00:00:00.000Z",
        "message": {
            "role": role,
            "content": content,
            "timestamp": 1
        }
    })
}

#[test]
fn cjk_and_korean_cut_point_budget_weights_dense_text_and_cuts_recent_turn() {
    let korean_turn = "가나다라마바사아자차카타파하".repeat(20);
    let msg = json!({
        "role": "user",
        "content": korean_turn,
    });
    let base = estimate_tokens(&msg);
    let weighted = estimate_cut_point_tokens(&msg);
    assert!(
        cjk_extra_chars(&korean_turn) >= korean_turn.chars().count() * 2,
        "each Korean syllable adds 2 extra UTF-16 char weights"
    );
    assert!(
        weighted > base * 2,
        "CJK cut-point token estimate ({weighted}) must exceed 2x base chars/4 estimate ({base})"
    );

    let entries = vec![
        message_entry("e1", None, "user", "first turn question"),
        message_entry("e2", Some("e1"), "assistant", "first turn answer"),
        message_entry("e3", Some("e2"), "user", &korean_turn),
        message_entry("e4", Some("e3"), "assistant", &korean_turn),
    ];
    let budget = (base as i64) * 3;
    let cut = find_cut_point(&entries, 0, entries.len(), budget);
    assert!(
        cut.first_kept_entry_index >= 2,
        "CJK turn must fill the keep budget and advance cut index past 0 (got {})",
        cut.first_kept_entry_index
    );
}

#[test]
fn truncate_utf8_at_char_boundary_never_splits_korean_cjk_or_emoji() {
    let sample = "한글テスト😀끝";
    for limit in 0..=sample.len() + 4 {
        let slice = truncate_utf8_at_char_boundary(sample, limit);
        assert!(slice.len() <= limit);
        assert!(sample.starts_with(slice));
    }
    assert_eq!(truncate_utf8_at_char_boundary("가나다", 0), "");
    assert_eq!(truncate_utf8_at_char_boundary("가나다", 1), "");
    assert_eq!(truncate_utf8_at_char_boundary("가나다", 2), "");
    assert_eq!(truncate_utf8_at_char_boundary("가나다", 3), "가");
    assert_eq!(truncate_utf8_at_char_boundary("😀", 3), "");
    assert_eq!(truncate_utf8_at_char_boundary("😀", 4), "😀");
}

#[test]
fn transient_failure_and_circuit_breaker_do_not_latch_compaction_admission() {
    let running = begin_compaction_operation(
        &initial_compaction_lifecycle_state(),
        BeginCompactionOperation {
            operation_id: "op-1".to_owned(),
            stage: CompactionStage::Execution,
            reason: "threshold".to_owned(),
            model: None,
            started_revision: 1,
        },
    );
    let finish_with = |cause: Option<&str>| -> CompactionLifecycleState {
        finish_compaction_operation(
            &running,
            &FinishCompactionOperation {
                operation_id: "op-1".to_owned(),
                status: CompactionFinishStatus::Failed,
                ended_revision: 1,
                rejection_cause: cause.map(str::to_owned),
                error_message: Some("failed".to_owned()),
            },
        )
    };

    assert!(is_non_latching_rejection_cause("transient-failure"));
    assert!(is_non_latching_rejection_cause("circuit-breaker"));
    assert!(is_non_latching_rejection_cause("external-owner"));
    assert!(!is_non_latching_rejection_cause("would-overflow"));

    assert!(!failed_compaction_latches_admission(
        &finish_with(Some("transient-failure")),
        true
    ));
    assert!(!failed_compaction_latches_admission(
        &finish_with(Some("circuit-breaker")),
        true
    ));
    assert!(!failed_compaction_latches_admission(
        &finish_with(Some("external-owner")),
        true
    ));
    assert!(failed_compaction_latches_admission(
        &finish_with(Some("would-overflow")),
        true
    ));
    assert!(failed_compaction_latches_admission(
        &finish_with(None),
        true
    ));
    assert!(!failed_compaction_latches_admission(
        &finish_with(Some("would-overflow")),
        false
    ));

    assert!(should_exclude_from_provider_fallback(false, true, false));
    assert!(!should_exclude_from_provider_fallback(false, false, false));
}
