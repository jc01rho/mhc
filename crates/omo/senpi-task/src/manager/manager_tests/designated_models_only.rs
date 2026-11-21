//! `manager/designated-models-only.test.ts` (omo #12010) under the fork policy gate
//! `task.parent_model_fallback: false`; the default path keeps the parent-model rung (see `fallback.rs`).

use std::sync::Arc;

use super::fakes::{
    FakeHandle, FakeRunner, Harness, HarnessOptions, base_spec, make_manager, started,
    wait_terminal, wait_until,
};
use crate::manager::model_refusals::MODEL_CHAIN_EXHAUSTED;
use crate::manager::types::{ChildPlanner, ManagerStartSpec, ResolvedChildPlan};
use crate::runners::RunnerFailureKind;
use crate::state::{
    ModelRefusalReason, ResolvedModelRecord, ResolvedModelSource, TaskRecord, TaskStatus,
};

const PARENT: &str = "vendor-c/parent";

fn category_record(provider: &str, model_id: &str) -> ResolvedModelRecord {
    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
}

fn designated_harness(fallbacks: Vec<ResolvedModelRecord>, runner: &Arc<FakeRunner>) -> Harness {
    let plan = ResolvedChildPlan {
        model: "vendor-a/primary-model".to_string(),
        requested_model: Some(category_record("vendor-a", "primary-model")),
        resolved_model: Some(category_record("vendor-a", "primary-model")),
        fallback_models: Some(fallbacks),
        category: Some("quick".to_string()),
        ..ResolvedChildPlan::default()
    };
    let planner: ChildPlanner = Arc::new(move |_spec: &ManagerStartSpec| Ok(plan.clone()));
    make_manager(HarnessOptions {
        planner: Some(planner),
        in_process: Some(Arc::clone(runner)),
        customize: Some(Box::new(|options| options.parent_model_fallback = false)),
        ..HarnessOptions::default()
    })
}

fn start_with_parent(harness: &Harness) -> String {
    started(harness.manager.start(&ManagerStartSpec {
        parent_model: Some(PARENT.to_string()),
        ..base_spec()
    }))
    .task_id
}

fn load(harness: &Harness, task_id: &str) -> TaskRecord {
    harness.store.load(task_id).expect("load").expect("record")
}

fn launched(runner: &FakeRunner, task_id: &str, call: usize) -> Arc<FakeHandle> {
    wait_until("launch", || runner.started_count() >= call);
    let handle = runner.wait_handle(task_id);
    wait_until("subscribed", || handle.subscribe_count() > 0);
    handle
}

fn models_run(runner: &FakeRunner) -> Vec<String> {
    runner
        .specs()
        .iter()
        .filter_map(|spec| spec.model.clone())
        .collect()
}

#[test]
fn given_the_gate_off_when_a_category_child_starts_then_the_parent_model_is_never_added_to_its_chain()
 {
    let runner = FakeRunner::new();
    let harness = designated_harness(vec![category_record("vendor-b", "rung")], &runner);
    let task_id = start_with_parent(&harness);
    let handle = launched(&runner, &task_id, 1);

    let record = load(&harness, &task_id);
    assert_eq!(record.parent_model_fallback, None);
    assert_eq!(
        record
            .fallback_models
            .unwrap_or_default()
            .iter()
            .map(|model| model.display.clone())
            .collect::<Vec<_>>(),
        vec!["vendor-b/rung"]
    );
    handle.complete("done");
    wait_terminal(&harness.manager, &task_id);
}

#[test]
fn given_the_only_designated_model_hits_its_usage_limit_when_the_chain_is_spent_then_the_task_fails_typed_and_the_parent_never_runs()
 {
    let runner = FakeRunner::new();
    let harness = designated_harness(Vec::new(), &runner);
    let task_id = start_with_parent(&harness);
    let handle = launched(&runner, &task_id, 1);

    handle.fail(
        RunnerFailureKind::ChildTurnFailed,
        "You have hit your weekly limit (resets 3:20am)",
    );

    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(
        terminal.failure_kind.as_deref(),
        Some(MODEL_CHAIN_EXHAUSTED)
    );
    let message = terminal.error_message.unwrap_or_default();
    assert!(
        message.contains("- vendor-a/primary-model: usage limit (resets 3:20am)"),
        "{message}"
    );
    assert_eq!(models_run(&runner), vec!["vendor-a/primary-model"]);
}

#[test]
fn given_two_designated_models_that_both_refuse_when_the_chain_is_spent_then_both_ran_the_parent_never_did_and_each_is_listed()
 {
    let runner = FakeRunner::new();
    let harness = designated_harness(vec![category_record("vendor-b", "rung")], &runner);
    let task_id = start_with_parent(&harness);
    let first = launched(&runner, &task_id, 1);
    first.fail(
        RunnerFailureKind::ChildTurnFailed,
        "You have hit your weekly limit (resets 3:20am)",
    );
    let second = launched(&runner, &task_id, 2);

    second.fail(RunnerFailureKind::ChildTurnFailed, "401 unauthorized");

    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(
        terminal.failure_kind.as_deref(),
        Some(MODEL_CHAIN_EXHAUSTED)
    );
    let message = terminal.error_message.clone().unwrap_or_default();
    assert!(
        message.contains("- vendor-a/primary-model: usage limit (resets 3:20am)"),
        "{message}"
    );
    assert!(
        message.contains("- vendor-b/rung: not signed in"),
        "{message}"
    );
    assert_eq!(
        terminal
            .model_refusals
            .unwrap_or_default()
            .iter()
            .map(|refusal| (refusal.model.clone(), refusal.reason))
            .collect::<Vec<_>>(),
        vec![(
            "vendor-a/primary-model".to_string(),
            ModelRefusalReason::UsageLimit
        )]
    );
    let run = models_run(&runner);
    assert_eq!(run, vec!["vendor-a/primary-model", "vendor-b/rung"]);
    assert!(!run.contains(&PARENT.to_string()));
}

#[test]
fn given_a_designated_second_model_that_works_when_the_first_hits_its_usage_limit_then_the_child_completes_on_it()
 {
    let runner = FakeRunner::new();
    let harness = designated_harness(vec![category_record("vendor-b", "rung")], &runner);
    let task_id = start_with_parent(&harness);
    let first = launched(&runner, &task_id, 1);
    first.fail(
        RunnerFailureKind::ChildTurnFailed,
        "You have hit your weekly limit (resets 3:20am)",
    );
    let second = launched(&runner, &task_id, 2);

    second.complete("done on the second model");

    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(terminal.failure_kind, None);
    assert_eq!(
        models_run(&runner),
        vec!["vendor-a/primary-model", "vendor-b/rung"]
    );
}

#[test]
fn given_an_unrecognized_failure_when_the_only_model_fails_then_the_task_fails_without_a_typed_chain_exhaustion()
 {
    let runner = FakeRunner::new();
    let harness = designated_harness(Vec::new(), &runner);
    let task_id = start_with_parent(&harness);
    let handle = launched(&runner, &task_id, 1);

    handle.fail(RunnerFailureKind::ChildTurnFailed, "something odd happened");

    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(terminal.failure_kind, None);
    assert_eq!(
        terminal.error_message.as_deref(),
        Some("something odd happened")
    );
}
