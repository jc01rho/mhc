//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "argument_source_cases.rs"]
mod argument_source_cases;
#[path = "image_placeholder.rs"]
mod image_placeholder;
#[path = "image_service.rs"]
mod image_service;
#[path = "model_auth.rs"]
mod model_auth;
#[path = "native.rs"]
mod native;
#[path = "tool_result_description.rs"]
mod tool_result_description;
