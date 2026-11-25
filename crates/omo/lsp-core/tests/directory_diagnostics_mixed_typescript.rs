//! Translation of `directory-diagnostics-mixed-typescript.test.ts`.

use crate::support::diagnostic;
use crate::support::typescript_projects::TypescriptProjects;
use crate::support::typescript_projects::context_for;
use crate::support::typescript_projects::install_typescript;
use crate::support::typescript_projects::path_string;
use crate::support::typescript_projects::write_executable;
use lsp_core::abort::AbortSignal;
use lsp_core::lsp::client::LspDiagnosticsResult;
use lsp_core::lsp::directory_diagnostics::DirectoryDiagnosticsOptions;
use lsp_core::lsp::directory_diagnostics::aggregate_diagnostics_for_directory;
use lsp_core::lsp::errors::LspError;
use lsp_core::lsp::manager::ClientFactory;
use lsp_core::lsp::manager::ClientFuture;
use lsp_core::lsp::manager::LspManager;
use lsp_core::lsp::manager::LspManagerOptions;
use lsp_core::lsp::manager::ManagedLspClient;
use lsp_core::lsp::manager::SharedClient;
use lsp_core::lsp::server_installation::ResolveServerBinaryOptions;
use lsp_core::lsp::server_installation::reset_server_binary_resolution_cache_for_tests;
use lsp_core::lsp::types::ResolvedServer;
use lsp_core::lsp::types::SeverityFilter;
use lsp_core::request_context::scope_request_context;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

struct RecordingClient {
    command: Vec<String>,
    files: Mutex<Vec<String>>,
}

impl RecordingClient {
    fn files(&self) -> Vec<String> {
        let mut files = self.files.lock().expect("recorded files").clone();
        files.sort();
        files
    }
}

impl ManagedLspClient for RecordingClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async {})
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn command(&self) -> Vec<String> {
        self.command.clone()
    }

    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        _signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(async move {
            self.files
                .lock()
                .expect("recorded files")
                .push(file_path.to_string());
            let first = self.command.first().cloned().unwrap_or_default();
            Ok(LspDiagnosticsResult {
                items: vec![diagnostic(&format!("checked by {first}"))],
                transient_error: None,
            })
        })
    }
}

struct RecordingManager {
    manager: Arc<LspManager>,
    acquisitions: Arc<Mutex<Vec<Arc<RecordingClient>>>>,
}

impl RecordingManager {
    fn acquisitions(&self) -> Vec<Arc<RecordingClient>> {
        self.acquisitions.lock().expect("acquisitions").clone()
    }

    fn acquisition_for(&self, first_command: &str) -> Arc<RecordingClient> {
        self.acquisitions()
            .into_iter()
            .find(|client| client.command.first().map(String::as_str) == Some(first_command))
            .expect("acquisition for command")
    }
}

fn recording_manager() -> RecordingManager {
    let acquisitions: Arc<Mutex<Vec<Arc<RecordingClient>>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&acquisitions);
    let factory: ClientFactory = Arc::new(move |_root: &str, server: &ResolvedServer| {
        let client = Arc::new(RecordingClient {
            command: server.command.clone(),
            files: Mutex::new(Vec::new()),
        });
        recorded
            .lock()
            .expect("acquisitions")
            .push(Arc::clone(&client));
        Ok(client as SharedClient)
    });
    let manager = LspManager::new(LspManagerOptions {
        client_factory: Some(factory),
        reaper_interval_ms: Some(60_000),
        ..LspManagerOptions::default()
    });
    RecordingManager {
        manager,
        acquisitions,
    }
}

struct Monorepo {
    root: PathBuf,
    package_x: PathBuf,
    package_y: PathBuf,
}

fn monorepo(projects: &mut TypescriptProjects) -> Monorepo {
    let root = projects.make_project();
    std::fs::create_dir(root.join(".git")).expect("create .git");
    let package_x = root.join("packages").join("x");
    let package_y = root.join("packages").join("y");
    for package_dir in [&package_x, &package_y] {
        std::fs::create_dir_all(package_dir.join("src")).expect("create package src");
        std::fs::write(package_dir.join("package.json"), "{}\n").expect("write package.json");
        std::fs::write(package_dir.join("src").join("a.ts"), "export const a = 1\n")
            .expect("write a.ts");
        std::fs::write(package_dir.join("src").join("b.ts"), "export const b = 1\n")
            .expect("write b.ts");
    }
    Monorepo {
        root,
        package_x,
        package_y,
    }
}

fn package_files(package_dir: &Path) -> Vec<String> {
    vec![
        path_string(&package_dir.join("src").join("a.ts")),
        path_string(&package_dir.join("src").join("b.ts")),
    ]
}

async fn scan_root(
    root: &Path,
    manager: &Arc<LspManager>,
) -> lsp_core::lsp::directory_diagnostics::DirectoryDiagnosticsResult {
    let options = DirectoryDiagnosticsOptions {
        manager: Some(Arc::clone(manager)),
        binary_options: Some(ResolveServerBinaryOptions {
            path_env: Some(path_string(&root.join("definitely-empty"))),
            ..ResolveServerBinaryOptions::default()
        }),
        ..DirectoryDiagnosticsOptions::default()
    };
    let directory = path_string(root);
    scope_request_context(
        context_for(root),
        aggregate_diagnostics_for_directory(
            &directory,
            ".ts",
            Some(SeverityFilter::All),
            Some(50),
            options,
        ),
    )
    .await
    .expect("directory diagnostics")
}

#[tokio::test]
async fn given_packages_on_typescript_7_and_typescript_5_when_scanning_the_repository_root_then_each_packages_files_go_to_its_own_server()
 {
    // given
    reset_server_binary_resolution_cache_for_tests();
    let mut projects = TypescriptProjects::new("lsp-dir-mixed-ts-");
    let Monorepo {
        root,
        package_x,
        package_y,
    } = monorepo(&mut projects);
    let language_server = write_executable(
        &root
            .join("node_modules")
            .join(".bin")
            .join("typescript-language-server"),
    );
    let tsc_x = install_typescript(&package_x.join("node_modules"), 7);
    install_typescript(&package_y.join("node_modules"), 5);
    let recording = recording_manager();

    // when
    let result = scan_root(&root, &recording.manager).await;

    // then
    assert_eq!(result.file_failures, Vec::new());
    assert_eq!(recording.acquisitions().len(), 2);
    assert_eq!(
        recording.acquisition_for(&tsc_x).files(),
        package_files(&package_x)
    );
    assert_eq!(
        recording.acquisition_for(&language_server).files(),
        package_files(&package_y)
    );
    let snapshot = recording.manager.get_snapshot();
    assert_eq!(snapshot.len(), 2);
    assert!(snapshot.iter().all(|client| client.ref_count == 0));
    recording.manager.stop_all().await;
}

#[tokio::test]
async fn given_a_root_without_a_typescript_server_but_a_typescript_7_package_when_scanning_the_root_then_the_package_is_diagnosed_and_only_the_unserved_files_report_the_missing_server()
 {
    // given
    reset_server_binary_resolution_cache_for_tests();
    let mut projects = TypescriptProjects::new("lsp-dir-mixed-ts-");
    let Monorepo {
        root,
        package_x,
        package_y,
    } = monorepo(&mut projects);
    let tsc_x = install_typescript(&package_x.join("node_modules"), 7);
    let recording = recording_manager();

    // when
    let result = scan_root(&root, &recording.manager).await;

    // then
    let commands: Vec<String> = recording
        .acquisitions()
        .iter()
        .filter_map(|client| client.command.first().cloned())
        .collect();
    assert_eq!(commands, vec![tsc_x]);
    assert_eq!(result.total_diagnostics, 2);
    let mut failed: Vec<String> = result
        .file_failures
        .iter()
        .map(|failure| failure.file.clone())
        .collect();
    failed.sort();
    assert_eq!(failed, package_files(&package_y));
    assert!(
        result
            .file_failures
            .iter()
            .all(|failure| failure.error.contains("NOT INSTALLED"))
    );
    recording.manager.stop_all().await;
}
