//! Translation of `typescript-project-test-support.ts`.

use lsp_core::request_context::LspRequestCapabilities;
use lsp_core::request_context::LspRequestContext;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use tempfile::TempDir;

pub struct TypescriptProjects {
    prefix: &'static str,
    roots: Vec<TempDir>,
}

impl TypescriptProjects {
    pub fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            roots: Vec::new(),
        }
    }

    /// A fresh directory with a package.json, as a real path (the temp dir itself may be a symlink).
    pub fn make_project(&mut self) -> PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(self.prefix)
            .tempdir()
            .expect("project temp dir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical project root");
        std::fs::write(root.join("package.json"), "{}\n").expect("write package.json");
        self.roots.push(dir);
        root
    }
}

pub fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn write_executable(path: &Path) -> String {
    let parent = path.parent().expect("executable parent");
    std::fs::create_dir_all(parent).expect("create executable parent");
    std::fs::write(path, "#!/bin/sh\nexit 0\n").expect("write executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod executable");
    path_string(path)
}

/// TypeScript 5 ships lib/tsserver.js; TypeScript 7 does not and serves LSP from tsc itself.
pub fn install_typescript(node_modules: &Path, major: u8) -> String {
    let package_dir = node_modules.join("typescript");
    std::fs::create_dir_all(package_dir.join("lib")).expect("create typescript lib");
    std::fs::write(
        package_dir.join("package.json"),
        serde_json::json!({ "name": "typescript", "version": format!("{major}.0.0") }).to_string(),
    )
    .expect("write typescript package.json");
    if major == 5 {
        std::fs::write(package_dir.join("lib").join("tsserver.js"), "").expect("write tsserver");
    }
    write_executable(&node_modules.join(".bin").join("tsc"))
}

pub fn user_config_path(cwd: &Path) -> PathBuf {
    cwd.join("home-config").join("lsp-client.json")
}

pub fn context_for(cwd: &Path) -> LspRequestContext {
    LspRequestContext {
        cwd: path_string(cwd),
        project_config_paths: vec![path_string(&cwd.join(".codex").join("lsp-client.json"))],
        user_config_path: path_string(&user_config_path(cwd)),
        install_decisions_path: path_string(
            &cwd.join("home-config").join("lsp-install-decisions.json"),
        ),
        capabilities: LspRequestCapabilities {
            install_decision_tool: true,
        },
    }
}
