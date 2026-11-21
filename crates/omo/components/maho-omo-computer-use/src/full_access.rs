//! Port of `full-access.ts`: the permission preset the permission system enforces for a session.

use maho_ext_permission_system::preset_registry::read_permission_preset;
use maho_ext_permission_system::types::PermissionPresetName;

/// True only when the permission system enforces Full access for `session` (#10022). A host without
/// the permission system, or a session it has not bound yet, never counts as full access, so desktop
/// input keeps waiting for a grant.
pub fn is_full_access(session: &str) -> bool {
    read_permission_preset(session) == Some(PermissionPresetName::FullAccess)
}
