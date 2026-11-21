//! omo-senpi `components/call-budget`: the per-tool-call time budget a cross-device command waits
//! inside (omo#14130).

pub mod call_deadline;

pub use call_deadline::{
    READ_RESERVE_MS, TOOL_CALL_BUDGET_MS, command_wait_ms, now_ms, outside_call_deadline,
    outside_call_deadline_async, wait_within_call_budget, with_call_deadline, within_call_budget,
};
