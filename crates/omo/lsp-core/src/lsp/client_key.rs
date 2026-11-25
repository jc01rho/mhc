//! Port of TS `client-key.ts`.

use crate::lsp::types::ResolvedServer;

/// What tells two resident clients apart: the server id and the exact command it was resolved to
/// (TS `ClientIdentity = Pick<ResolvedServer, "id" | "command">`; the Rust callers pass the whole
/// resolved server, which carries both).
///
/// The resolved command is part of the resident key because one server id can resolve to different
/// binaries under one workspace root: in a monorepo, a TypeScript 7 package is served by its own
/// `tsc --lsp` while a TypeScript 5 sibling is served by typescript-language-server. The same root
/// and command still share one client.
pub fn lsp_client_key(root: &str, server: &ResolvedServer) -> String {
    serde_json::json!([root, server.id, server.command]).to_string()
}
