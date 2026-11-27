//! Operating-system process inspection primitives (`platform/` in TypeScript).

pub mod native_process;
pub mod process_tree_cpu;

pub use native_process::{
    read_parent_pids, read_process_cpu_seconds, read_process_start_identity,
    read_process_start_time, scale_mach_ticks,
};
pub use process_tree_cpu::{
    ProcessTable, read_process_tree_cpu_seconds, sum_process_tree_cpu_seconds,
};
