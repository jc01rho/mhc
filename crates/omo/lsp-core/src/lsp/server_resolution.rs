//! Port of TS `server-resolution.ts`.

use crate::lsp::config_loader::get_merged_servers;
use crate::lsp::config_loader::get_merged_servers_and_disabled_ids;
use crate::lsp::native_typescript::native_typescript_command;
use crate::lsp::native_typescript::native_typescript_search_directory;
use crate::lsp::server_definitions::builtin_server;
use crate::lsp::server_definitions::install_hint;
use crate::lsp::server_installation::{
    ResolveServerBinaryOptions, is_server_installed_with_options, resolve_server_binary,
};
use crate::lsp::types::ResolvedServer;
use crate::lsp::types::ServerLookupInfo;
use crate::lsp::types::ServerLookupResult;
use crate::request_context::LspRequestContextUnavailableError;
use crate::request_context::context_cwd;

/// TS `findServerForExtension`.
pub fn find_server_for_extension(
    ext: &str,
) -> Result<ServerLookupResult, LspRequestContextUnavailableError> {
    find_server_for_extension_for_target(ext, None)
}

pub fn find_server_for_extension_for_target(
    ext: &str,
    target_path: Option<&str>,
) -> Result<ServerLookupResult, LspRequestContextUnavailableError> {
    find_server_for_extension_with_options(ext, target_path, &ResolveServerBinaryOptions::default())
}

pub fn find_server_for_extension_with_options(
    ext: &str,
    target_path: Option<&str>,
    binary_options: &ResolveServerBinaryOptions,
) -> Result<ServerLookupResult, LspRequestContextUnavailableError> {
    let servers = get_merged_servers()?;
    let working_directory = context_cwd()?;
    let search_directory = native_typescript_search_directory(target_path, &working_directory);
    let has_ext = |server: &ResolvedServer| server.extensions.iter().any(|e| e == ext);

    for entry in servers.iter().filter(|entry| has_ext(&entry.server)) {
        if let Some(native_ts) = native_typescript_command(entry, &search_directory) {
            // The builtin initialization is typescript-language-server's option set; tsc gets only
            // what a config file supplied.
            return Ok(ServerLookupResult::Found {
                server: ResolvedServer {
                    id: entry.server.id.clone(),
                    command: native_ts,
                    extensions: entry.server.extensions.clone(),
                    priority: entry.server.priority,
                    env: entry.server.env.clone(),
                    initialization: entry.configured_initialization.clone(),
                },
            });
        }
        if let Some(binary) = resolve_server_binary(
            &entry.server.command,
            Some(&working_directory),
            binary_options,
        ) {
            let mut server = entry.server.clone();
            server.command[0] = binary;
            return Ok(ServerLookupResult::Found { server });
        }
    }

    if let Some(configured) = servers.iter().find(|s| has_ext(&s.server)) {
        let server = &configured.server;
        let hint = install_hint(&server.id).map_or_else(
            || {
                let cmd = server.command.first().map_or("undefined", String::as_str);
                format!("Install '{cmd}' and ensure it's in your PATH")
            },
            str::to_string,
        );
        return Ok(ServerLookupResult::NotInstalled {
            server: ServerLookupInfo {
                id: server.id.clone(),
                command: server.command.clone(),
                extensions: server.extensions.clone(),
            },
            install_hint: hint,
        });
    }

    let mut available: Vec<String> = Vec::new();
    for server in &servers {
        if !available.contains(&server.server.id) {
            available.push(server.server.id.clone());
        }
    }
    Ok(ServerLookupResult::NotConfigured {
        extension: ext.to_string(),
        available_servers: available,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerStatus {
    pub id: String,
    pub installed: bool,
    pub extensions: Vec<String>,
    pub disabled: bool,
    pub source: String,
    pub priority: f64,
}

/// TS `getAllServers`.
pub fn get_all_servers() -> Result<Vec<ServerStatus>, LspRequestContextUnavailableError> {
    get_all_servers_with_options(&ResolveServerBinaryOptions::default())
}

pub fn get_all_servers_with_options(
    binary_options: &ResolveServerBinaryOptions,
) -> Result<Vec<ServerStatus>, LspRequestContextUnavailableError> {
    // PERF-DIVERGENCE(omo lsp config-loader): upstream reads the LSP config files twice per call (merged servers, then disabled ids); this port loads them once. Output is identical.
    let (servers, disabled) = get_merged_servers_and_disabled_ids()?;
    let working_directory = context_cwd()?;
    let native_search_directory = native_typescript_search_directory(None, &working_directory);
    let mut result: Vec<ServerStatus> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for entry in servers {
        if seen.contains(&entry.server.id) {
            continue;
        }
        seen.push(entry.server.id.clone());
        let installed = is_server_installed_with_options(
            &entry.server.command,
            Some(&working_directory),
            binary_options,
        ) || native_typescript_command(&entry, &native_search_directory).is_some();
        result.push(ServerStatus {
            installed,
            id: entry.server.id,
            extensions: entry.server.extensions,
            disabled: false,
            source: entry.source.as_str().to_string(),
            priority: entry.server.priority,
        });
    }
    for id in disabled {
        if seen.contains(&id) {
            continue;
        }
        let builtin = builtin_server(&id);
        result.push(ServerStatus {
            installed: builtin.is_some_and(|b| {
                is_server_installed_with_options(
                    &b.command_vec(),
                    Some(&working_directory),
                    binary_options,
                )
            }),
            extensions: builtin.map(|b| b.extensions_vec()).unwrap_or_default(),
            id,
            disabled: true,
            source: "disabled".to_string(),
            priority: 0.0,
        });
    }
    Ok(result)
}
