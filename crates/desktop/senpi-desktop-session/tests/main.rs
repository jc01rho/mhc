//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "bounded_holds.rs"]
mod bounded_holds;
#[path = "capture_budget.rs"]
mod capture_budget;
#[path = "control_grant.rs"]
mod control_grant;
#[path = "display_targets.rs"]
mod display_targets;
#[path = "menus.rs"]
mod menus;
#[path = "session.rs"]
mod session;
#[path = "session_options.rs"]
mod session_options;
