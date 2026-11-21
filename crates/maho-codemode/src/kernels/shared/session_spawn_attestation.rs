//! Port of senpi `packages/senpi-codemode/src/kernels/shared/session-spawn-attestation.ts` (6d4426a28; senpi#6978).
use crate::kernels::session_env::SessionEnvironment;
use maho_tools::session_spawn_attestation::{
    SessionSpawnAttestation, attest_session_spawn_in_store,
};

/// Attests a kernel process this host spawned to the session it runs for (senpi#6972), so every descendant
/// (a `Bun.$` command, a `Bun.spawn` child) resolves to that session. The session id is the host's own
/// session environment, never anything the kernel can set. No session, no record.
pub fn attest_kernel_spawn(
    session_env: Option<&SessionEnvironment>,
    pid: Option<u32>,
) -> Option<SessionSpawnAttestation> {
    let session_id = session_env?.get("PI_SESSION_ID")?;
    attest_session_spawn_in_store(session_id, pid?)
}
