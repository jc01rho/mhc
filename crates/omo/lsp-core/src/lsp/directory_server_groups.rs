use crate::lsp::client_key::lsp_client_key;
use crate::lsp::client_wrapper::find_workspace_root;
use crate::lsp::errors::LspError;
use crate::lsp::server_installation::ResolveServerBinaryOptions;
use crate::lsp::server_resolution::find_server_for_extension_with_options;
use crate::lsp::types::FailedServerLookupResult;
use crate::lsp::types::ResolvedServer;
use crate::lsp::types::ServerLookupResult;
use crate::request_context::dirname;
use indexmap::IndexMap;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct ServedFileGroup {
    pub root: String,
    pub server: ResolvedServer,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnservedFileGroup {
    pub lookup: FailedServerLookupResult,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirectoryServerGroups {
    pub served: Vec<ServedFileGroup>,
    pub unserved: Vec<UnservedFileGroup>,
}

#[derive(Debug, Clone, Default)]
pub struct FixedDirectoryServer<'a> {
    pub server: Option<&'a ResolvedServer>,
    pub workspace_root: Option<&'a str>,
    pub binary_options: Option<&'a ResolveServerBinaryOptions>,
}

enum DirectoryResolution {
    Served {
        key: String,
        root: String,
        server: ResolvedServer,
    },
    Unserved {
        key: String,
        lookup: FailedServerLookupResult,
    },
}

pub fn group_files_by_client(
    files: &[String],
    extension: &str,
    fixed: FixedDirectoryServer<'_>,
) -> Result<DirectoryServerGroups, LspError> {
    let mut by_directory: HashMap<String, DirectoryResolution> = HashMap::new();
    let mut served: IndexMap<String, ServedFileGroup> = IndexMap::new();
    let mut unserved: IndexMap<String, UnservedFileGroup> = IndexMap::new();

    for file in files {
        let directory = dirname(file);
        if !by_directory.contains_key(&directory) {
            let resolution = resolve_directory(&directory, extension, &fixed)?;
            by_directory.insert(directory.clone(), resolution);
        }
        match &by_directory[&directory] {
            DirectoryResolution::Served { key, root, server } => {
                let group = served
                    .entry(key.clone())
                    .or_insert_with(|| ServedFileGroup {
                        root: root.clone(),
                        server: server.clone(),
                        files: Vec::new(),
                    });
                group.files.push(file.clone());
            }
            DirectoryResolution::Unserved { key, lookup } => {
                let group = unserved
                    .entry(key.clone())
                    .or_insert_with(|| UnservedFileGroup {
                        lookup: lookup.clone(),
                        files: Vec::new(),
                    });
                group.files.push(file.clone());
            }
        }
    }

    Ok(DirectoryServerGroups {
        served: served.into_values().collect(),
        unserved: unserved.into_values().collect(),
    })
}

fn resolve_directory(
    directory: &str,
    extension: &str,
    fixed: &FixedDirectoryServer<'_>,
) -> Result<DirectoryResolution, LspError> {
    let default_options = ResolveServerBinaryOptions::default();
    let binary_options = fixed.binary_options.unwrap_or(&default_options);
    let lookup = match fixed.server {
        Some(server) => ServerLookupResult::Found {
            server: server.clone(),
        },
        None => find_server_for_extension_with_options(extension, Some(directory), binary_options)?,
    };
    match lookup {
        ServerLookupResult::Found { server } => {
            let root = match fixed.workspace_root {
                Some(root) => root.to_string(),
                None => find_workspace_root(directory)?,
            };
            let key = lsp_client_key(&root, &server);
            Ok(DirectoryResolution::Served { key, root, server })
        }
        ServerLookupResult::NotConfigured {
            extension,
            available_servers,
        } => {
            let key = serde_json::json!(["not_configured", ""]).to_string();
            Ok(DirectoryResolution::Unserved {
                key,
                lookup: FailedServerLookupResult::NotConfigured {
                    extension,
                    available_servers,
                },
            })
        }
        ServerLookupResult::NotInstalled {
            server,
            install_hint,
        } => {
            let key = serde_json::json!(["not_installed", server.id]).to_string();
            Ok(DirectoryResolution::Unserved {
                key,
                lookup: FailedServerLookupResult::NotInstalled {
                    server,
                    install_hint,
                },
            })
        }
    }
}
