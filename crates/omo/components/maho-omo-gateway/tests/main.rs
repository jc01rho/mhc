//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "gateway_schema_tests.rs"]
mod gateway_schema_tests;
#[path = "gateway_store_tests.rs"]
mod gateway_store_tests;
#[path = "prompt_tests.rs"]
mod prompt_tests;
#[path = "remote_list_and_close_tests.rs"]
mod remote_list_and_close_tests;
#[path = "remote_thread_tests.rs"]
mod remote_thread_tests;
#[path = "rules_block_tests.rs"]
mod rules_block_tests;
#[path = "run_alerts_tests.rs"]
mod run_alerts_tests;
#[path = "same_device_wake_tests.rs"]
mod same_device_wake_tests;
#[path = "store_extension_tests.rs"]
mod store_extension_tests;
#[path = "thread_control_tests.rs"]
mod thread_control_tests;
#[path = "thread_create_tests.rs"]
mod thread_create_tests;
#[path = "wake_carrier_tests.rs"]
mod wake_carrier_tests;
#[path = "wake_pending_sweep_tests.rs"]
mod wake_pending_sweep_tests;
#[path = "wake_runtime_tests.rs"]
mod wake_runtime_tests;
#[path = "wake_session_waker_tests.rs"]
mod wake_session_waker_tests;
#[path = "wake_store_tests.rs"]
mod wake_store_tests;
#[path = "wake_support.rs"]
mod wake_support;
