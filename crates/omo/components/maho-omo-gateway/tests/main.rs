//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "gateway_store_tests.rs"]
mod gateway_store_tests;
#[path = "prompt_tests.rs"]
mod prompt_tests;
#[path = "remote_thread_tests.rs"]
mod remote_thread_tests;
#[path = "rules_block_tests.rs"]
mod rules_block_tests;
#[path = "store_extension_tests.rs"]
mod store_extension_tests;
#[path = "thread_control_tests.rs"]
mod thread_control_tests;
