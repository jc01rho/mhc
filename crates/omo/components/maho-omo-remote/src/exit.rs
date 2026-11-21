//! Port of `remote-exit.ts`: `mhc remote`'s exit codes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteExit {
    Ok = 0,
    Failed = 1,
    Usage = 2,
    /// The device service is not there, or cannot do it.
    Unavailable = 3,
}

impl RemoteExit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Arguments the command cannot run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError(pub String);
