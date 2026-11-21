//! Containment and gating for one operation (senpi `policy/gate.ts`).

use std::future::Future;
use std::path::{Path, PathBuf};

use crate::errors::MachineOpError;
use crate::node_path::resolve;
use crate::policy::filesystem::{
    FilesystemOperation, FilesystemPolicyDecision, FilesystemPolicyRequest,
    canonicalize_filesystem_path,
};
use crate::types::{
    AuditEntry, AuditVerdict, OpArgs, OpContext, OpName, OpOutcome, PolicyDecision,
};

/// Resolve a request path against the scope root (or the process cwd when the scope has none).
pub fn resolve_op_path(path: &Path, ctx: &OpContext) -> PathBuf {
    match &ctx.scope.root {
        Some(root) => resolve(root, path),
        None => resolve(Path::new(""), path),
    }
}

fn contains(root: &Path, target: &Path) -> bool {
    target.starts_with(root)
}

fn record_audit(ctx: &OpContext, op: OpName, target: String, verdict: AuditVerdict) {
    if let Some(audit) = &ctx.audit {
        audit(AuditEntry {
            op,
            principal: ctx.principal.clone(),
            target,
            verdict,
        });
    }
}

/// Containment and filesystem policy for one path, before any effect. A path (or a symlink in it)
/// resolving outside the scope root is `policy_denied`. With neither a root nor a filesystem policy the
/// path is not canonicalized at all.
pub async fn check_path(
    absolute_path: &Path,
    operation: FilesystemOperation,
    op: OpName,
    ctx: &OpContext,
) -> Result<(), MachineOpError> {
    let root = ctx.scope.root.as_deref();
    let filesystem = ctx.policy.filesystem.as_ref();
    if root.is_none() && filesystem.is_none() {
        return Ok(());
    }
    let canonical_path = canonicalize_filesystem_path(absolute_path).await?;
    if let Some(root) = root {
        let canonical_root = canonicalize_filesystem_path(root).await?;
        if !contains(&canonical_root, &canonical_path) {
            record_audit(
                ctx,
                op,
                canonical_path.display().to_string(),
                AuditVerdict::Denied,
            );
            return Err(MachineOpError::policy_denied(format!(
                "{} is outside the scope root",
                absolute_path.display()
            )));
        }
    }
    if let Some(filesystem) = filesystem {
        let request = FilesystemPolicyRequest {
            operation,
            canonical_path: canonical_path.clone(),
            tool_name: op,
        };
        if let FilesystemPolicyDecision::Deny { reason } = filesystem.check(&request).await {
            record_audit(
                ctx,
                op,
                canonical_path.display().to_string(),
                AuditVerdict::Denied,
            );
            return Err(MachineOpError::policy_denied(reason));
        }
    }
    Ok(())
}

/// Run `before_op`, the effect, then `after_op`. A denial returns `policy_denied` before the effect is
/// polled (a Rust future does nothing until awaited, so `effect` has not started).
pub async fn gated<T>(
    op: OpName,
    args: OpArgs<'_>,
    ctx: &OpContext,
    effect: impl Future<Output = Result<T, MachineOpError>>,
) -> Result<T, MachineOpError> {
    if ctx.signal.is_cancelled() {
        return Err(MachineOpError::aborted());
    }
    let decision = match &ctx.policy.hooks {
        Some(hooks) => hooks.before_op(op, args, ctx).await,
        None => PolicyDecision::Allow,
    };
    if let PolicyDecision::Deny { reason } = decision {
        record_audit(ctx, op, args.describe_target(), AuditVerdict::Denied);
        return Err(MachineOpError::policy_denied(reason));
    }
    record_audit(ctx, op, args.describe_target(), AuditVerdict::Allowed);
    let result = effect.await;
    if let Some(hooks) = &ctx.policy.hooks {
        let outcome = match &result {
            Ok(_) => OpOutcome::Ok,
            Err(error) => OpOutcome::Err(error),
        };
        hooks.after_op(op, args, outcome, ctx).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Principal, PrincipalKind, Scope};

    fn ctx(root: Option<&str>) -> OpContext {
        let scope = match root {
            Some(root) => Scope::rooted("s", root),
            None => Scope::unrooted("s"),
        };
        OpContext::new(
            scope,
            Principal {
                kind: PrincipalKind::LocalSession,
                id: "p".to_owned(),
            },
        )
    }

    #[test]
    fn containment_is_component_wise() {
        assert!(contains(Path::new("/r"), Path::new("/r")));
        assert!(contains(Path::new("/r"), Path::new("/r/a/b")));
        assert!(!contains(Path::new("/r"), Path::new("/r-sibling/a")));
        assert!(!contains(Path::new("/r/a"), Path::new("/r")));
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_path_is_resolved_against_the_scope_root() {
        let scoped = ctx(Some("/root"));
        assert_eq!(
            resolve_op_path(Path::new("dir/../a.txt"), &scoped),
            PathBuf::from("/root/a.txt")
        );
        assert_eq!(
            resolve_op_path(Path::new("../outside"), &scoped),
            PathBuf::from("/outside")
        );
    }

    #[tokio::test]
    async fn an_aborted_signal_stops_the_gate_before_the_effect() {
        let aborted = ctx(None);
        aborted.signal.cancel();
        let request = crate::types::ListRequest::new(".");
        let outcome: Result<(), MachineOpError> =
            gated(OpName::FilesList, OpArgs::List(&request), &aborted, async {
                Ok(())
            })
            .await;
        assert_eq!(outcome, Err(MachineOpError::aborted()));
    }
}
