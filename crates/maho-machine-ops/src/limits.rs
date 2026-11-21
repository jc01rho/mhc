//! Limits that apply to file operations (senpi `src/limits.ts`; the exec and PTY limits belong to the
//! excluded process layer).

const MIB: u64 = 1024 * 1024;

/// Host limits an [`crate::OpContext`] carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest file `read`, `edit` and `grep` read. `u64::MAX` means unbounded and skips the size
    /// check entirely, like senpi's `Infinity` on the tool path.
    pub read_max_bytes: u64,
}

/// Machine-path defaults: a 10 MiB read cap.
pub const DEFAULT_LIMITS: Limits = Limits {
    read_max_bytes: 10 * MIB,
};

impl Default for Limits {
    fn default() -> Self {
        DEFAULT_LIMITS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_read_cap_is_ten_mib() {
        assert_eq!(Limits::default().read_max_bytes, 10 * 1024 * 1024);
    }
}
