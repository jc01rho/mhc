//! Engine-free host file operations confined to a scope root: a Rust port of the file and policy part of
//! senpi `packages/machine-ops` (senpi#5592), including the symlink and `..` escape refusal of
//! senpi#6755 (`machine-op/real-path.ts`) and the rooted glob pattern handling of senpi#6726.
//!
//! Every operation takes an [`OpContext`]. Relative paths resolve against the scope root, a path is
//! canonicalized through existing symlinks (a missing tail is appended to the nearest existing real
//! parent) and is refused with [`OpErrorCode::PolicyDenied`] when the result is outside the root, a
//! symlinked parent of a not-yet-existing write target included. A glob pattern that is absolute or
//! contains `..` is split into its fixed leading directory and the rest; the directory is checked like
//! any path and `..` after a wildcard is refused.
//!
//! Module layout follows senpi, one module per TypeScript file: [`errors`], [`limits`], [`types`], [`local`],
//! [`policy::filesystem`], [`policy::gate`], [`files::bounded_realpath`], [`files::realpath_without_open`],
//! [`files::glob`], [`files::grep`], [`files::local_files`], [`files::path_lock`] (the core of
//! `files/mutation-queue.ts`) and [`real_path`] (the confinement of `rpc-machine-op/real-path.ts`).
//! [`cancel`] and [`node_path`] replace `AbortSignal` and `node:path`.
//!
//! Deliberately not ported:
//! - `process/*` (exec, spawn, PTY, holder link, scopes and their sweep), `info/*` (process info, host
//!   facts, mesh messages) and `scopes`: no file or policy behavior depends on them;
//! - the realm registry, stuck-module locks and `withFileMutationQueues` of `files/mutation-queue.ts`: one
//!   Rust process holds one copy of the crate, so [`files::path_lock`] keeps only the per-canonical-path
//!   slot;
//! - shelling out to `fd`: `files.glob` always uses the built-in walker (as senpi does without `fd`), so it
//!   skips `.git` and `node_modules` and does not read `.gitignore`;
//! - the `machine_op` RPC verb itself and its wire types;
//! - the conformance harness for other implementations (the equivalent cases live in `tests/files.rs`).
//!
//! Divergences: `files.grep` takes a Rust `regex` pattern, not a JavaScript `RegExp`; `files.stat`'s
//! `access` is checked by opening the path (no `access(2)` without `unsafe`); a glob class that does not
//! compile (`[]`, `[!]`) is `invalid_pattern` instead of a JavaScript syntax error.

pub mod cancel;
pub mod errors;
pub mod files;
pub mod limits;
pub mod local;
pub mod node_path;
pub mod policy;
pub mod real_path;
pub mod types;

pub use cancel::CancellationToken;
pub use errors::{MachineOpError, OpErrorCode, io_error_code};
pub use files::automount_realpath::{
    AutomountAbort, AutomountRealpath, AutomountRealpathOptions, AutomountResolverFn,
    canonicalize_by_opening_sync, realpath_out_of_process,
};
pub use files::automount_trigger::{
    AutomountTriggerGuard, AutomountTriggerLookup, ReadMountViewFn, answer_automount_triggers_from,
    automount_trigger_path, create_automount_trigger_lookup, realpath_under_automount,
};
pub use files::bounded_realpath::{
    RESOLUTION_DEADLINE, RESOLUTION_DEADLINE_MS, ResolutionTimedOut,
    fold_path_for_case_insensitive_filesystem, is_missing_path_error, with_resolution_deadline,
};
pub use files::glob::glob_to_regex;
pub use files::local_files::{LocalFileOps, create_local_file_ops};
pub use files::mount_table::{
    MountEntry, MountView, parse_darwin_mount_table, parse_linux_mount_info, read_mount_table,
    unmounted_automount_path,
};
pub use files::path_lock::{FileMutationLock, acquire_file_mutation_lock, mutation_queue_key};
pub use files::realpath_without_open::{realpath_without_open, realpath_without_open_strict};
pub use limits::{DEFAULT_LIMITS, Limits};
pub use local::{LocalMachineOpsOptions, MachineOps, create_local_machine_ops};
pub use policy::filesystem::{
    FilesystemOperation, FilesystemPolicy, FilesystemPolicyDecision, FilesystemPolicyRequest,
    canonicalize_filesystem_path, canonicalize_filesystem_path_with, compose_filesystem_policies,
};
pub use policy::gate::{check_path, gated, resolve_op_path};
pub use real_path::spawn_path_in;
pub use types::{
    AccessMode, AuditEntry, AuditSink, AuditVerdict, BoxFuture, EditRequest, EditResult, EntryType,
    GlobEntry, GlobRequest, GlobResult, GrepMatch, GrepRequest, GrepResult, ListEntry, ListRequest,
    ListResult, OpArgs, OpContext, OpHooks, OpName, OpOutcome, PathLock, PolicyDecision,
    PolicyHooks, Principal, PrincipalKind, ReadRequest, ReadResult, Scope, StatRequest, StatResult,
    WriteRequest, WriteResult,
};
