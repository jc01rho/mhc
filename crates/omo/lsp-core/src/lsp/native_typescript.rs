use crate::lsp::config_loader::ServerWithSource;
use crate::lsp::server_installation::NODE_PROJECT_MARKERS;
use crate::lsp::server_installation::executable_suffixes;
use crate::lsp::server_installation::probe;
use crate::lsp::server_installation::probe_with_suffixes;
use crate::request_context::canonicalize_existing_or_nearest_ancestor;
use crate::request_context::dirname;
use crate::request_context::is_path_inside;
use crate::request_context::resolve_from;
use crate::request_context::strip_verbatim_prefix;
use std::path::Path;
use std::path::PathBuf;

/// Returns the canonical starting directory inside the session for locating a project's `tsc`.
/// Targets resolving outside the real session directory fall back to the session directory so a
/// symlink inside the session never borrows an external project's compiler.
pub fn native_typescript_search_directory(
    target_path: Option<&str>,
    working_directory: &str,
) -> String {
    let Ok(session_path) = std::fs::canonicalize(working_directory) else {
        return working_directory.to_string();
    };
    let session = strip_verbatim_prefix(&session_path.to_string_lossy());
    let Some(target_path) = target_path else {
        return session;
    };
    let resolved = resolve_from(working_directory, target_path);
    let Ok(target) = canonicalize_existing_or_nearest_ancestor(&resolved) else {
        return working_directory.to_string();
    };
    if !is_path_inside(&session, &target) {
        return session;
    }
    if Path::new(&target).is_dir() {
        target
    } else {
        dirname(&target)
    }
}

/// Returns `[tsc, "--lsp", "--stdio"]` when the inherited `typescript` builtin server is backed
/// by a project-local TypeScript 7 installation (no `lib/tsserver.js`).
pub fn native_typescript_command(
    server: &ServerWithSource,
    search_directory: &str,
) -> Option<Vec<String>> {
    if server.server.id != "typescript" || !server.inherits_builtin_command {
        return None;
    }
    let found = find_project_tsc(search_directory)?;
    let speaks_lsp = !found.package_dir.join("lib").join("tsserver.js").exists();
    if speaks_lsp {
        Some(vec![found.tsc, "--lsp".to_string(), "--stdio".to_string()])
    } else {
        None
    }
}

struct ProjectTsc {
    tsc: String,
    package_dir: PathBuf,
}

fn find_project_tsc(search_directory: &str) -> Option<ProjectTsc> {
    let platform = if cfg!(windows) { "win32" } else { "linux" };
    let suffixes = executable_suffixes(platform, None);
    let mut current = PathBuf::from(search_directory);
    loop {
        if NODE_PROJECT_MARKERS
            .iter()
            .any(|marker| probe(current.join(marker)))
        {
            let bin_dir = current.join("node_modules").join(".bin");
            let tsc = probe_with_suffixes(&bin_dir, "tsc", &suffixes);
            let package_dir = current.join("node_modules").join("typescript");
            if let Some(tsc) = tsc
                && is_real_directory(&bin_dir)
                && package_dir.join("package.json").exists()
            {
                return Some(ProjectTsc { tsc, package_dir });
            }
        }
        if probe(current.join(".git")) {
            return None;
        }
        if !current.pop() {
            return None;
        }
    }
}

// Package managers may symlink entries inside `node_modules` (e.g. `.bin/tsc` or `typescript`),
// never `node_modules/.bin` itself; a symlinked `.bin` points outside the project and fails closed.
fn is_real_directory(directory: &Path) -> bool {
    let Ok(canonical) = std::fs::canonicalize(directory) else {
        return false;
    };
    strip_verbatim_prefix(&canonical.to_string_lossy()) == directory.to_string_lossy()
}
