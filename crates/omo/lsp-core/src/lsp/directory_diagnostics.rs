use crate::abort::AbortSignal;
use crate::lsp::client_wrapper::format_server_lookup_error;
use crate::lsp::constants::DEFAULT_MAX_DIAGNOSTICS;
use crate::lsp::constants::DEFAULT_MAX_DIRECTORY_FILES;
use crate::lsp::directory_group_scan::GroupScanOptions;
use crate::lsp::directory_group_scan::scan_served_groups;
use crate::lsp::directory_server_groups::FixedDirectoryServer;
use crate::lsp::directory_server_groups::group_files_by_client;
use crate::lsp::effective_extension::effective_extension;
use crate::lsp::errors::LspError;
use crate::lsp::formatters::filter_diagnostics_by_severity;
use crate::lsp::formatters::format_diagnostic;
use crate::lsp::manager::LspManager;
use crate::lsp::manager::SharedClient;
use crate::lsp::manager::get_lsp_manager;
use crate::lsp::server_installation::ResolveServerBinaryOptions;
use crate::lsp::timer_provider::SharedTimerProvider;
use crate::lsp::types::Diagnostic;
use crate::lsp::types::ResolvedServer;
use crate::lsp::types::SeverityFilter;
use crate::request_context::context_cwd;
use crate::request_context::resolve_from;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

const SKIP_DIRECTORIES: [&str; 6] = ["node_modules", ".git", "dist", "build", ".next", "out"];
const DIRECTORY_DIAGNOSTICS_MAX_CONCURRENCY: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryDiagnosticsFileFailure {
    pub file: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryDiagnosticsResult {
    pub output: String,
    pub total_diagnostics: usize,
    pub file_failures: Vec<DirectoryDiagnosticsFileFailure>,
}

pub type ListFiles = Arc<dyn Fn(&str, &str, usize) -> Vec<String> + Send + Sync>;

type FileOutcome = (String, Result<Vec<Diagnostic>, String>);

/// TS `DirectoryDiagnosticsOptions`.
#[derive(Clone, Default)]
pub struct DirectoryDiagnosticsOptions {
    pub list_files: Option<ListFiles>,
    pub manager: Option<Arc<LspManager>>,
    pub max_concurrency: Option<usize>,
    pub workspace_root: Option<String>,
    pub server: Option<ResolvedServer>,
    pub signal: Option<AbortSignal>,
    pub timers: Option<SharedTimerProvider>,
    /// Binary-resolution overrides for the per-file server lookup. Upstream reads the process
    /// environment; mhc threads the same knobs so a test never mutates it.
    pub binary_options: Option<ResolveServerBinaryOptions>,
}

/// TS `collectFilesWithExtension`: depth-first, skips symlinks and build directories.
pub fn collect_files_with_extension(dir: &str, extension: &str, max_files: usize) -> Vec<String> {
    fn walk(current: &Path, extension: &str, max_files: usize, files: &mut Vec<String>) {
        if files.len() >= max_files {
            return;
        }
        let Ok(entries) = std::fs::read_dir(current) else {
            return;
        };
        let mut names: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        names.sort();
        for name in names {
            if files.len() >= max_files {
                return;
            }
            let full_path = current.join(&name);
            let Ok(metadata) = std::fs::symlink_metadata(&full_path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let full = full_path.to_string_lossy().into_owned();
            if metadata.is_dir() {
                if !SKIP_DIRECTORIES.contains(&name.to_string_lossy().as_ref()) {
                    walk(&full_path, extension, max_files, files);
                }
            } else if metadata.is_file() && effective_extension(&full) == extension {
                files.push(full);
            }
        }
    }
    let mut files = Vec::new();
    walk(Path::new(dir), extension, max_files, &mut files);
    files
}

/// TS `aggregateDiagnosticsForDirectory`.
pub async fn aggregate_diagnostics_for_directory(
    directory: &str,
    extension: &str,
    severity: Option<SeverityFilter>,
    max_files: Option<usize>,
    options: DirectoryDiagnosticsOptions,
) -> Result<DirectoryDiagnosticsResult, LspError> {
    let max_files = max_files.unwrap_or(DEFAULT_MAX_DIRECTORY_FILES);
    if !extension.starts_with('.') {
        return Err(LspError::InvalidPath(format!(
            "Extension must start with a dot (e.g., \".ts\", not \"{extension}\"). Use \".{extension}\" instead."
        )));
    }
    let base = match &options.workspace_root {
        Some(root) => root.clone(),
        None => context_cwd()?,
    };
    let abs_dir = resolve_from(&base, directory);
    if !Path::new(&abs_dir).exists() {
        return Err(LspError::InvalidPath(format!(
            "Directory does not exist: {abs_dir}"
        )));
    }
    let all_files = match &options.list_files {
        Some(list_files) => list_files(&abs_dir, extension, max_files + 1),
        None => {
            let (dir, ext) = (abs_dir.clone(), extension.to_string());
            tokio::task::spawn_blocking(move || {
                collect_files_with_extension(&dir, &ext, max_files + 1)
            })
            .await
            .map_err(|error| LspError::other(error.to_string()))?
        }
    };
    let was_capped = all_files.len() > max_files;
    let files_to_process: Vec<String> = all_files.into_iter().take(max_files).collect();

    if files_to_process.is_empty() {
        let output = [
            format!("Directory: {abs_dir}"),
            format!("Extension: {extension}"),
            "Files scanned: 0".to_string(),
            format!("No files found with extension \"{extension}\"."),
        ]
        .join("\n");
        return Ok(DirectoryDiagnosticsResult {
            output,
            total_diagnostics: 0,
            file_failures: Vec::new(),
        });
    }

    // Each file goes to the client a single-file request for it would use; one scan at a repository
    // root can span packages served by different servers. File cap and concurrency stay per request.
    let groups = group_files_by_client(
        &files_to_process,
        extension,
        FixedDirectoryServer {
            server: options.server.as_ref(),
            workspace_root: options.workspace_root.as_deref(),
            binary_options: options.binary_options.as_ref(),
        },
    )?;
    if groups.served.is_empty()
        && let Some(first) = groups.unserved.first()
    {
        return Err(LspError::ServerLookup {
            message: format_server_lookup_error(&first.lookup)?,
            lookup: Some(first.lookup.clone()),
        });
    }

    let manager = options.manager.clone().unwrap_or_else(get_lsp_manager);
    let max_concurrency = options
        .max_concurrency
        .unwrap_or(DIRECTORY_DIAGNOSTICS_MAX_CONCURRENCY)
        .max(1);
    let signal = options.signal.clone();

    let outcomes: Arc<Mutex<Vec<FileOutcome>>> = Arc::new(Mutex::new(Vec::new()));
    let file_errors: Vec<DirectoryDiagnosticsFileFailure> = groups
        .unserved
        .iter()
        .flat_map(|group| {
            let first_line = format_server_lookup_error(&group.lookup)
                .unwrap_or_else(|_| "No LSP server".to_string())
                .lines()
                .next()
                .unwrap_or("No LSP server")
                .to_string();
            group
                .files
                .iter()
                .map(move |file| DirectoryDiagnosticsFileFailure {
                    file: file.clone(),
                    error: first_line.clone(),
                })
        })
        .collect();

    if let Some(signal) = options.signal.as_ref() {
        signal.throw_if_aborted()?;
    }

    let collected = Arc::clone(&outcomes);
    let check_signal = signal.clone();
    let check_file = Arc::new(move |client: SharedClient, file: String| {
        let collected = Arc::clone(&collected);
        let check_signal = check_signal.clone();
        Box::pin(async move {
            let outcome = match client.diagnostics(&file, check_signal.as_ref()).await {
                Ok(result) => Ok(result.items),
                Err(error) => Err(error.to_string()),
            };
            collected
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((file, outcome));
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    });
    let unavailable_errors: Arc<Mutex<Vec<DirectoryDiagnosticsFileFailure>>> =
        Arc::new(Mutex::new(Vec::new()));
    let unavailable = Arc::clone(&unavailable_errors);
    let on_group_unavailable = Arc::new(
        move |group: &crate::lsp::directory_server_groups::ServedFileGroup, error: String| {
            let mut errors = unavailable.lock().unwrap_or_else(PoisonError::into_inner);
            errors.extend(
                group
                    .files
                    .iter()
                    .map(|file| DirectoryDiagnosticsFileFailure {
                        file: file.clone(),
                        error: error.clone(),
                    }),
            );
        },
    );

    scan_served_groups(GroupScanOptions {
        manager: &manager,
        groups: &groups.served,
        max_concurrency,
        signal,
        timers: options.timers.clone(),
        check_file,
        on_group_unavailable,
    })
    .await?;

    let mut diagnostics: Vec<(String, Diagnostic)> = Vec::new();
    let mut errors = file_errors;
    errors.extend(std::mem::take(
        &mut *unavailable_errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    ));
    let checked = std::mem::take(&mut *outcomes.lock().unwrap_or_else(PoisonError::into_inner));
    for (file, outcome) in checked {
        match outcome {
            Ok(items) => {
                let filtered = filter_diagnostics_by_severity(items, severity);
                diagnostics.extend(
                    filtered
                        .into_iter()
                        .map(|diagnostic| (file.clone(), diagnostic)),
                );
            }
            Err(error) => errors.push(DirectoryDiagnosticsFileFailure { file, error }),
        }
    }

    let capped_note = if was_capped {
        format!(" (capped at {max_files})")
    } else {
        String::new()
    };
    let mut lines = vec![
        format!("Directory: {abs_dir}"),
        format!("Extension: {extension}"),
        format!("Files scanned: {}{capped_note}", files_to_process.len()),
        format!("Files with errors: {}", errors.len()),
        format!("Total diagnostics: {}", diagnostics.len()),
    ];
    if !errors.is_empty() {
        lines.push(String::new());
        lines.push("File processing errors:".to_string());
        lines.extend(
            errors
                .iter()
                .map(|failure| format!("  {}: {}", failure.file, failure.error)),
        );
    }
    if !diagnostics.is_empty() {
        lines.push(String::new());
        lines.extend(
            diagnostics
                .iter()
                .take(DEFAULT_MAX_DIAGNOSTICS)
                .map(|(file, diagnostic)| format!("{file}: {}", format_diagnostic(diagnostic))),
        );
        if diagnostics.len() > DEFAULT_MAX_DIAGNOSTICS {
            lines.push(String::new());
            lines.push(format!(
                "... ({} more diagnostics not shown)",
                diagnostics.len() - DEFAULT_MAX_DIAGNOSTICS
            ));
        }
    }
    Ok(DirectoryDiagnosticsResult {
        output: lines.join("\n"),
        total_diagnostics: diagnostics.len(),
        file_failures: errors,
    })
}

#[cfg(test)]
#[path = "directory_diagnostics_tests.rs"]
mod tests;
