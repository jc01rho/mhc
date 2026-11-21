//! Port of `retention/budget.ts`: one retention pass per identity is bounded by `pass_budget_ms`
//! and by the abort signal. Stores are walked in a fixed order and each in sorted name order, so
//! the last item a pass finished is a complete resume point: the next pass skips every earlier
//! store and every name up to and including `last_name` in the cursor's store. An item in flight
//! always finishes, and the first removal item a pass reaches always runs, so a store whose
//! classification alone outlasts the budget still moves forward. Classification (`proceed`) is a
//! metadata read: only the abort signal stops it.

use std::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionStore {
    Runs,
    Completions,
    ReflectionSessions,
    Journals,
    Sidecars,
    Ledgers,
    RecallRuns,
}

impl RetentionStore {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Runs => "runs",
            Self::Completions => "completions",
            Self::ReflectionSessions => "reflection-sessions",
            Self::Journals => "journals",
            Self::Sidecars => "sidecars",
            Self::Ledgers => "ledgers",
            Self::RecallRuns => "recall-runs",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "runs" => Self::Runs,
            "completions" => Self::Completions,
            "reflection-sessions" => Self::ReflectionSessions,
            "journals" => Self::Journals,
            "sidecars" => Self::Sidecars,
            "ledgers" => Self::Ledgers,
            "recall-runs" => Self::RecallRuns,
            _ => return None,
        })
    }

    fn index(self) -> usize {
        match self {
            Self::Runs => 0,
            Self::Completions => 1,
            Self::ReflectionSessions => 2,
            Self::Journals => 3,
            Self::Sidecars => 4,
            Self::Ledgers => 5,
            Self::RecallRuns => 6,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionCursor {
    pub store: String,
    pub last_name: Option<String>,
}

#[derive(Default)]
struct BudgetState {
    last: Option<RetentionCursor>,
    stopped: Option<RetentionCursor>,
    aborted: bool,
    admitted_any: bool,
}

pub struct PassBudget<'a> {
    now: &'a dyn Fn() -> i64,
    started_at: i64,
    budget_ms: u64,
    cancel: Option<&'a dyn Fn() -> bool>,
    resume: Option<RetentionCursor>,
    state: RefCell<BudgetState>,
}

impl<'a> PassBudget<'a> {
    pub fn new(
        now: &'a dyn Fn() -> i64,
        budget_ms: u64,
        cancel: Option<&'a dyn Fn() -> bool>,
        resume: Option<RetentionCursor>,
    ) -> Self {
        Self {
            started_at: now(),
            now,
            budget_ms,
            cancel,
            resume,
            state: RefCell::new(BudgetState::default()),
        }
    }

    /// The cursor the pass stopped at, when it stopped early.
    pub fn stopped(&self) -> Option<RetentionCursor> {
        self.state.borrow().stopped.clone()
    }

    /// True when the pass stopped because the abort signal fired.
    pub fn aborted(&self) -> bool {
        self.state.borrow().aborted
    }

    /// True when an earlier pass already finished this item (or its whole store).
    pub fn skip(&self, store: RetentionStore, name: &str) -> bool {
        let Some(resume) = &self.resume else {
            return false;
        };
        let Some(resume_at) = RetentionStore::from_name(&resume.store).map(RetentionStore::index)
        else {
            return false;
        };
        let here = store.index();
        if here > resume_at {
            return false;
        }
        if here < resume_at {
            return true;
        }
        resume
            .last_name
            .as_deref()
            .is_some_and(|last_name| name <= last_name)
    }

    /// False once the budget is spent or the pass was aborted; the cursor then points at the last
    /// finished item.
    pub fn admit(&self, store: RetentionStore) -> bool {
        if !self.proceed(store) {
            return false;
        }
        let spent = {
            let state = self.state.borrow();
            state.admitted_any && (self.now)() - self.started_at >= self.budget_ms as i64
        };
        if spent {
            self.stop(store);
            return false;
        }
        self.state.borrow_mut().admitted_any = true;
        true
    }

    /// False once the pass was aborted (or already stopped); the time budget never stops a
    /// classification read.
    pub fn proceed(&self, store: RetentionStore) -> bool {
        if self.state.borrow().stopped.is_some() {
            return false;
        }
        if !self.cancel.is_some_and(|cancel| cancel()) {
            return true;
        }
        self.state.borrow_mut().aborted = true;
        self.stop(store);
        false
    }

    fn stop(&self, store: RetentionStore) {
        let mut state = self.state.borrow_mut();
        state.stopped = if let Some(last) = state.last.clone() {
            Some(last)
        } else if let Some(resume) = self.resume.as_ref().filter(|resume| {
            RetentionStore::from_name(&resume.store)
                .map(RetentionStore::index)
                .is_some_and(|resume_at| resume_at >= store.index())
        }) {
            Some(resume.clone())
        } else {
            Some(RetentionCursor {
                store: store.as_str().to_string(),
                last_name: None,
            })
        };
    }

    /// What is left of the budget; the size cap, which runs last, spends only this.
    pub fn remaining_ms(&self) -> u64 {
        let spent = ((self.now)() - self.started_at).max(0) as u64;
        self.budget_ms.saturating_sub(spent)
    }

    pub fn done(&self, store: RetentionStore, name: &str) {
        self.state.borrow_mut().last = Some(RetentionCursor {
            store: store.as_str().to_string(),
            last_name: Some(name.to_string()),
        });
    }
}
