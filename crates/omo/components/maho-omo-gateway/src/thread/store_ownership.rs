//! Which incarnation of a session publishes its control endpoint.
//!
//! Upstream counterpart: `thread/gateway/store-ownership.ts`. The endpoint a session publishes is fenced by its
//! incarnation: only the incarnation that registered it clears it.

use super::store_ops::{
    SqlAccess, StoreContext, StoreError, row_opt_str, transaction, v_opt_str, v_str,
};

/// The session's published control endpoint, fenced by its incarnation.
pub fn register_incarnation(
    ctx: &StoreContext,
    durable_id: &str,
    incarnation: &str,
    endpoint_socket: Option<&str>,
    endpoint_kind: Option<&str>,
) -> Result<(), StoreError> {
    transaction(ctx, "register_incarnation", || {
        ctx.run(
            "INSERT INTO session_meta (durable_id, next_seq) VALUES (?, 1) ON CONFLICT(durable_id) DO NOTHING",
            &[v_str(durable_id)],
        )?;
        ctx.run(
            "UPDATE session_meta SET incarnation = ?, endpoint_socket = ?, endpoint_kind = ? WHERE durable_id = ?",
            &[v_str(incarnation), v_opt_str(endpoint_socket.map(str::to_owned)), v_opt_str(endpoint_kind.map(str::to_owned)), v_str(durable_id)],
        )?;
        Ok(())
    })
}

pub fn clear_endpoint(
    ctx: &StoreContext,
    durable_id: &str,
    incarnation: &str,
) -> Result<(), StoreError> {
    transaction(ctx, "clear_endpoint", || {
        ctx.run(
            "UPDATE session_meta SET endpoint_socket = NULL, endpoint_kind = NULL WHERE durable_id = ? AND incarnation = ?",
            &[v_str(durable_id), v_str(incarnation)],
        )?;
        Ok(())
    })
}

/// The endpoint that currently serves a session, when one published it.
pub fn session_endpoint(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<(String, String, String)>, StoreError> {
    Ok(ctx.one("SELECT incarnation, endpoint_socket, endpoint_kind FROM session_meta WHERE durable_id = ?", &[v_str(durable_id)])?.and_then(|row| {
        Some((row_opt_str(&row, "incarnation")?, row_opt_str(&row, "endpoint_socket")?, row_opt_str(&row, "endpoint_kind")?))
    }))
}
