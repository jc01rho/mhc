//! `MachineOpError` and its codes (senpi `src/errors.ts`).

use std::fmt;
use std::io;

/// The code carried by every error an operation raises or classifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpErrorCode {
    PolicyDenied,
    NotFound,
    LimitExceeded,
    InvalidPattern,
    InvalidArgument,
    PermissionDenied,
    Aborted,
    IoError,
}

impl OpErrorCode {
    /// The wire spelling (`policy_denied`, `not_found`, ...), identical to senpi's `OpErrorCode`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyDenied => "policy_denied",
            Self::NotFound => "not_found",
            Self::LimitExceeded => "limit_exceeded",
            Self::InvalidPattern => "invalid_pattern",
            Self::InvalidArgument => "invalid_argument",
            Self::PermissionDenied => "permission_denied",
            Self::Aborted => "aborted",
            Self::IoError => "io_error",
        }
    }
}

impl fmt::Display for OpErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error raised by the operation layer itself. Filesystem errors are classified into the same
/// type by [`MachineOpError::from`] (`ENOENT`/`ENOTDIR` -> `not_found`, `EACCES`/`EPERM` ->
/// `permission_denied`, anything else `io_error`) and keep the operating system's message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MachineOpError {
    pub code: OpErrorCode,
    pub message: String,
}

impl MachineOpError {
    pub fn new(code: OpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// The error of an operation whose [`crate::CancellationToken`] was cancelled.
    pub fn aborted() -> Self {
        Self::new(OpErrorCode::Aborted, "Operation aborted")
    }

    pub fn policy_denied(message: impl Into<String>) -> Self {
        Self::new(OpErrorCode::PolicyDenied, message)
    }
}

/// Classify an I/O error into one wire code (senpi `opErrorCode`).
pub fn io_error_code(error: &io::Error) -> OpErrorCode {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => OpErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => OpErrorCode::PermissionDenied,
        _ => OpErrorCode::IoError,
    }
}

impl From<io::Error> for MachineOpError {
    fn from(error: io::Error) -> Self {
        Self {
            code: io_error_code(&error),
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_errors_are_classified_like_senpi_errno_codes() {
        let code = |kind| io_error_code(&io::Error::from(kind));
        assert_eq!(code(io::ErrorKind::NotFound), OpErrorCode::NotFound);
        assert_eq!(code(io::ErrorKind::NotADirectory), OpErrorCode::NotFound);
        assert_eq!(
            code(io::ErrorKind::PermissionDenied),
            OpErrorCode::PermissionDenied
        );
        assert_eq!(code(io::ErrorKind::TimedOut), OpErrorCode::IoError);
    }

    #[test]
    fn codes_have_the_wire_spelling() {
        assert_eq!(OpErrorCode::PolicyDenied.to_string(), "policy_denied");
        assert_eq!(OpErrorCode::InvalidPattern.as_str(), "invalid_pattern");
        let error = MachineOpError::from(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(error.code, OpErrorCode::NotFound);
    }
}
