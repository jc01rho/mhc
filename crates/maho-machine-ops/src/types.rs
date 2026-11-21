//! Operation types, file and policy part of senpi `src/types.ts`. Paths are `PathBuf`, byte payloads
//! `Vec<u8>`, and a request field that senpi validated as a "non-negative integer" is a `u64`/`usize`, so
//! the invalid values cannot be built.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::cancel::CancellationToken;
use crate::errors::MachineOpError;
use crate::limits::Limits;
use crate::policy::filesystem::FilesystemPolicy;

/// A boxed, sendable future; the shape of every asynchronous policy callback.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Every operation name a [`OpHooks::before_op`] gate can see (the file operations; `process.*` and
/// `info.message` belong to the excluded layers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpName {
    FilesRead,
    FilesWrite,
    FilesEdit,
    FilesGlob,
    FilesGrep,
    FilesStat,
    FilesList,
}

impl OpName {
    /// The senpi spelling (`files.read`, ...).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilesRead => "files.read",
            Self::FilesWrite => "files.write",
            Self::FilesEdit => "files.edit",
            Self::FilesGlob => "files.glob",
            Self::FilesGrep => "files.grep",
            Self::FilesStat => "files.stat",
            Self::FilesList => "files.list",
        }
    }
}

impl fmt::Display for OpName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny { reason: String },
}

impl PolicyDecision {
    pub fn deny(reason: impl Into<String>) -> Self {
        Self::Deny {
            reason: reason.into(),
        }
    }

    pub const fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// How an operation ended, as `after_op` sees it (senpi's `OpOutcome` without the value).
#[derive(Debug, Clone, Copy)]
pub enum OpOutcome<'a> {
    Ok,
    Err(&'a MachineOpError),
}

/// The request an operation was called with, borrowed for the hooks.
#[derive(Debug, Clone, Copy)]
pub enum OpArgs<'a> {
    Read(&'a ReadRequest),
    Write(&'a WriteRequest),
    Edit(&'a EditRequest),
    Glob(&'a GlobRequest),
    Grep(&'a GrepRequest),
    Stat(&'a StatRequest),
    List(&'a ListRequest),
}

impl OpArgs<'_> {
    /// The audit target: the request path, empty for a glob (senpi `describeTarget`); never a payload.
    pub fn describe_target(&self) -> String {
        let path = match self {
            Self::Read(request) => &request.path,
            Self::Write(request) => &request.path,
            Self::Edit(request) => &request.path,
            Self::Grep(request) => &request.path,
            Self::Stat(request) => &request.path,
            Self::List(request) => &request.path,
            Self::Glob(_) => return String::new(),
        };
        path.display().to_string()
    }
}

/// The gate both callers run before any effect and the notification after it.
pub trait OpHooks: Send + Sync {
    /// May this operation run? A denial throws `policy_denied` before the effect.
    fn before_op<'a>(
        &'a self,
        _op: OpName,
        _args: OpArgs<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, PolicyDecision> {
        Box::pin(async { PolicyDecision::Allow })
    }

    fn after_op<'a>(
        &'a self,
        _op: OpName,
        _args: OpArgs<'a>,
        _outcome: OpOutcome<'a>,
        _ctx: &'a OpContext,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// `filesystem` sees the canonical target (deny wins); `hooks` gate and observe each operation.
#[derive(Clone, Default)]
pub struct PolicyHooks {
    pub filesystem: Option<Arc<dyn FilesystemPolicy>>,
    pub hooks: Option<Arc<dyn OpHooks>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditVerdict {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub op: OpName,
    pub principal: Principal,
    pub target: String,
    pub verdict: AuditVerdict,
}

pub type AuditSink = Arc<dyn Fn(AuditEntry) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub id: String,
    /// When set, contains every path an operation touches: a path or symlink resolving outside it is a
    /// policy denial. Absent means no containment beyond `PolicyHooks::filesystem`.
    pub root: Option<PathBuf>,
}

impl Scope {
    pub fn rooted(id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root: Some(root.into()),
        }
    }

    pub fn unrooted(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            root: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalKind {
    LocalSession,
    RemoteDevice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub kind: PrincipalKind,
    pub id: String,
}

/// Set by a caller that already holds the per-path mutation slot of every path the operation mutates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathLock {
    Caller,
}

/// Everything an operation needs besides its request.
#[derive(Clone)]
pub struct OpContext {
    pub scope: Scope,
    pub principal: Principal,
    pub signal: CancellationToken,
    pub limits: Limits,
    pub policy: PolicyHooks,
    pub audit: Option<AuditSink>,
    /// `Some(PathLock::Caller)`: a mutating operation does not take the path slot itself.
    pub path_lock: Option<PathLock>,
}

impl OpContext {
    pub fn new(scope: Scope, principal: Principal) -> Self {
        Self {
            scope,
            principal,
            signal: CancellationToken::new(),
            limits: Limits::default(),
            policy: PolicyHooks::default(),
            audit: None,
            path_lock: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadRequest {
    pub path: PathBuf,
    pub offset: Option<u64>,
    pub limit_bytes: Option<u64>,
}

impl ReadRequest {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResult {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// Size of the whole file.
    pub size: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteRequest {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub create_parent_dirs: bool,
    /// Write `bytes` at this byte offset of an existing or new file instead of replacing the file.
    pub offset: Option<u64>,
    /// With `offset`: the final file size, so ranges written in any order leave the same file.
    pub total: Option<u64>,
}

impl WriteRequest {
    pub fn new(path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteResult {
    pub path: PathBuf,
    pub written_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditRequest {
    pub path: PathBuf,
    pub exact: String,
    pub replacement: String,
}

impl EditRequest {
    pub fn new(
        path: impl Into<PathBuf>,
        exact: impl Into<String>,
        replacement: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            exact: exact.into(),
            replacement: replacement.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditResult {
    pub path: PathBuf,
    pub content: String,
    pub replacements: usize,
    pub write_result: WriteResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobRequest {
    pub pattern: String,
    pub cwd: Option<PathBuf>,
    pub limit: Option<usize>,
}

impl GlobRequest {
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            ..Self::default()
        }
    }

    pub fn in_dir(pattern: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            pattern: pattern.into(),
            cwd: Some(cwd.into()),
            limit: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobEntry {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobResult {
    /// Matching paths as the search printed them (absolute under the search directory).
    pub entries: Vec<GlobEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrepRequest {
    pub path: PathBuf,
    pub pattern: String,
    pub limit: Option<usize>,
}

impl GrepRequest {
    pub fn new(path: impl Into<PathBuf>, pattern: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            pattern: pattern.into(),
            limit: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepMatch {
    pub path: PathBuf,
    /// One-based line number.
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatRequest {
    pub path: PathBuf,
    /// Also require these permissions; the access error is the operation's error.
    pub access: Option<AccessMode>,
}

impl StatRequest {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            access: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatResult {
    pub path: PathBuf,
    pub entry_type: EntryType,
    pub size: u64,
    pub mtime_ms: f64,
    pub mode: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListRequest {
    pub path: PathBuf,
}

impl ListRequest {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub name: String,
    pub entry_type: EntryType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListResult {
    pub path: PathBuf,
    /// Directory entries in the order the filesystem returned them.
    pub entries: Vec<ListEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_have_the_senpi_spelling() {
        assert_eq!(OpName::FilesRead.to_string(), "files.read");
        assert_eq!(OpName::FilesList.as_str(), "files.list");
    }

    #[test]
    fn the_audit_target_is_a_path_and_never_a_payload() {
        let write = WriteRequest::new("a/b.txt", b"secret payload".to_vec());
        assert_eq!(OpArgs::Write(&write).describe_target(), "a/b.txt");
        let glob = GlobRequest::new("*.rs");
        assert_eq!(OpArgs::Glob(&glob).describe_target(), "");
    }

    #[test]
    fn a_policy_decision_knows_whether_it_allows() {
        assert!(PolicyDecision::Allow.is_allowed());
        assert!(!PolicyDecision::deny("no").is_allowed());
    }
}
