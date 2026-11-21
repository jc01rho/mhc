use std::cell::Cell;

use super::*;

fn cursor(store: &str, last_name: Option<&str>) -> RetentionCursor {
    RetentionCursor {
        store: store.to_string(),
        last_name: last_name.map(str::to_string),
    }
}

#[test]
fn a_resumed_pass_aborted_before_its_first_item_keeps_the_resumed_last_name() {
    let now = || 0_i64;
    let aborted = Cell::new(false);
    let cancel = || aborted.get();
    let budget = PassBudget::new(
        &now,
        10,
        Some(&cancel),
        Some(cursor("journals", Some("s-200"))),
    );
    aborted.set(true);

    assert!(!budget.admit(RetentionStore::Journals));
    assert!(budget.aborted());
    assert_eq!(budget.stopped(), Some(cursor("journals", Some("s-200"))));
}

#[test]
fn a_resumed_pass_aborted_in_a_later_store_names_that_store() {
    let now = || 0_i64;
    let aborted = Cell::new(false);
    let cancel = || aborted.get();
    let budget = PassBudget::new(
        &now,
        10,
        Some(&cancel),
        Some(cursor("journals", Some("s-200"))),
    );
    aborted.set(true);

    assert!(!budget.admit(RetentionStore::Sidecars));
    assert_eq!(budget.stopped(), Some(cursor("sidecars", None)));
}

#[test]
fn a_spent_budget_stops_at_the_last_finished_item() {
    let clock = Cell::new(0_i64);
    let now = || clock.get();
    let budget = PassBudget::new(&now, 10, None, Some(cursor("journals", Some("s-200"))));

    assert!(budget.admit(RetentionStore::Journals));
    budget.done(RetentionStore::Journals, "s-300");
    clock.set(11);

    assert!(!budget.admit(RetentionStore::Journals));
    assert_eq!(budget.stopped(), Some(cursor("journals", Some("s-300"))));
}

#[test]
fn a_pass_resumed_at_journals_that_stops_in_an_earlier_store_keeps_the_journals_resume_point() {
    let now = || 0_i64;
    let aborted = Cell::new(false);
    let cancel = || aborted.get();
    let budget = PassBudget::new(
        &now,
        10,
        Some(&cancel),
        Some(cursor("journals", Some("s-05"))),
    );
    aborted.set(true);

    assert!(!budget.admit(RetentionStore::Runs));
    assert_eq!(budget.stopped(), Some(cursor("journals", Some("s-05"))));
}

#[test]
fn only_the_abort_signal_stops_classification() {
    let clock = Cell::new(0_i64);
    let now = || clock.get();
    let aborted = Cell::new(false);
    let cancel = || aborted.get();
    let budget = PassBudget::new(
        &now,
        10,
        Some(&cancel),
        Some(cursor("journals", Some("s-05"))),
    );
    clock.set(11);

    assert!(budget.proceed(RetentionStore::Runs));
    assert_eq!(budget.stopped(), None);

    aborted.set(true);
    assert!(!budget.proceed(RetentionStore::Runs));
    assert!(budget.aborted());
    assert_eq!(budget.stopped(), Some(cursor("journals", Some("s-05"))));
}

#[test]
fn the_first_removal_item_runs_even_past_the_budget_and_the_next_one_stops_at_it() {
    let clock = Cell::new(0_i64);
    let now = || clock.get();
    let budget = PassBudget::new(&now, 10, None, None);
    clock.set(11);

    assert!(budget.admit(RetentionStore::Runs));
    budget.done(RetentionStore::Runs, "reflection-run-1");
    assert!(!budget.admit(RetentionStore::Runs));
    assert_eq!(
        budget.stopped(),
        Some(cursor("runs", Some("reflection-run-1")))
    );
}
