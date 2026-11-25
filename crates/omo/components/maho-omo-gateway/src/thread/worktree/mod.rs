//! The folder and worktree side of `thread_create`.
//!
//! Upstream counterpart: `packages/omo-senpi/src/components/thread/worktree/`. `failures.ts` maps to `failures.rs`
//! and the request type and `checkFolder` of `thread-worktree.ts` to `thread_worktree.rs`. Creating the git worktree
//! itself (`createThreadWorktree`, `discard-worktree.ts`, `remote-worktree.ts`, `worktree-rpc*.ts`) is not ported: a
//! create that asks for one is refused `unsupported` before anything is opened (`docs/omo-sync/v5.1.83/thread-control.md`).

pub mod failures;
pub mod thread_worktree;
