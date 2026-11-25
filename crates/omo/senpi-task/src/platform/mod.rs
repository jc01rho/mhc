//! Operating-system process inspection primitives (`platform/` in TypeScript).

pub mod native_process;

pub use native_process::{read_process_cpu_seconds, read_process_start_identity, scale_mach_ticks};
