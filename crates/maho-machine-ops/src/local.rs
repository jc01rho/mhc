//! The local machine operations (senpi `src/local.ts`, file part).

use crate::files::local_files::{LocalFileOps, create_local_file_ops};
use crate::limits::{DEFAULT_LIMITS, Limits};

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalMachineOpsOptions {
    /// Host defaults; every operation reads its own limits from `OpContext::limits`. Default
    /// [`DEFAULT_LIMITS`].
    pub limits: Option<Limits>,
}

/// The local implementation of every file operation, one instance per host runtime. No concurrency cap:
/// only one canonical path's mutations are ordered.
#[derive(Debug, Clone, Copy)]
pub struct MachineOps {
    pub files: LocalFileOps,
    /// The host defaults this instance was created with; callers build `OpContext::limits` from them.
    pub limits: Limits,
}

pub fn create_local_machine_ops(options: LocalMachineOpsOptions) -> MachineOps {
    MachineOps {
        files: create_local_file_ops(),
        limits: options.limits.unwrap_or(DEFAULT_LIMITS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_default_to_the_machine_path_defaults() {
        let ops = create_local_machine_ops(LocalMachineOpsOptions::default());
        assert_eq!(ops.limits, DEFAULT_LIMITS);
        let custom = Limits { read_max_bytes: 5 };
        let ops = create_local_machine_ops(LocalMachineOpsOptions {
            limits: Some(custom),
        });
        assert_eq!(ops.limits.read_max_bytes, 5);
    }
}
