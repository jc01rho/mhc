//! Translation of `directory-diagnostics-group-scan.test.ts`.

use crate::support::controlled_clock::ControlledClock;
use crate::support::controlled_clock::Deferred;
use crate::support::controlled_clock::within;
use crate::support::diagnostic;
use crate::support::typescript_projects::TypescriptProjects;
use crate::support::typescript_projects::context_for;
use crate::support::typescript_projects::path_string;
use futures::FutureExt;
use lsp_core::abort::AbortController;
use lsp_core::abort::AbortSignal;
use lsp_core::lsp::client::LspDiagnosticsResult;
use lsp_core::lsp::constants::INIT_TIMEOUT_MS;
use lsp_core::lsp::directory_diagnostics::DirectoryDiagnosticsOptions;
use lsp_core::lsp::directory_diagnostics::DirectoryDiagnosticsResult;
use lsp_core::lsp::directory_diagnostics::ListFiles;
use lsp_core::lsp::directory_diagnostics::aggregate_diagnostics_for_directory;
use lsp_core::lsp::errors::LspError;
use lsp_core::lsp::manager::ClientFactory;
use lsp_core::lsp::manager::ClientFuture;
use lsp_core::lsp::manager::LspManager;
use lsp_core::lsp::manager::LspManagerOptions;
use lsp_core::lsp::manager::ManagedLspClient;
use lsp_core::lsp::manager::SharedClient;
use lsp_core::lsp::timer_provider::SharedTimerProvider;
use lsp_core::lsp::types::Diagnostic;
use lsp_core::lsp::types::ResolvedServer;
use lsp_core::lsp::types::SeverityFilter;
use lsp_core::request_context::scope_request_context;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::Weak;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::watch;

fn server() -> ResolvedServer {
    ResolvedServer {
        id: "typescript".to_string(),
        command: vec!["fake-ts-ls".to_string()],
        extensions: vec![".ts".to_string()],
        priority: 1.0,
        env: None,
        initialization: None,
    }
}

type DiagnosticsHandler = Box<dyn Fn(&str) -> Result<Vec<Diagnostic>, LspError> + Send + Sync>;

enum StartBehavior {
    Ready,
    Never,
    Fail(&'static str),
    Gate(Deferred),
}

struct FakeGroupClient {
    start: StartBehavior,
    stop_gate: Option<Deferred>,
    on_stopped: Option<Box<dyn Fn() + Send + Sync>>,
    diagnostics: Option<DiagnosticsHandler>,
    stop_count: watch::Sender<usize>,
}

impl FakeGroupClient {
    fn new(start: StartBehavior) -> Self {
        Self {
            start,
            stop_gate: None,
            on_stopped: None,
            diagnostics: None,
            stop_count: watch::channel(0).0,
        }
    }

    fn with_diagnostics(mut self, handler: DiagnosticsHandler) -> Self {
        self.diagnostics = Some(handler);
        self
    }

    fn stop_count(&self) -> usize {
        *self.stop_count.borrow()
    }

    async fn wait_stop_count(&self, count: usize) {
        let mut stops = self.stop_count.subscribe();
        within(
            async {
                let _ = stops.wait_for(|stopped| *stopped >= count).await;
            },
            "the client was never stopped",
        )
        .await;
    }
}

impl ManagedLspClient for FakeGroupClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move {
            match &self.start {
                StartBehavior::Ready => Ok(()),
                StartBehavior::Never => std::future::pending::<Result<(), LspError>>().await,
                StartBehavior::Fail(message) => Err(LspError::other(*message)),
                StartBehavior::Gate(gate) => {
                    gate.wait().await;
                    Ok(())
                }
            }
        })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.stop_count.send_modify(|stopped| *stopped += 1);
            if let Some(gate) = &self.stop_gate {
                gate.wait().await;
            }
            if let Some(on_stopped) = &self.on_stopped {
                on_stopped();
            }
        })
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn command(&self) -> Vec<String> {
        server().command
    }

    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        _signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(async move {
            let outcome = match &self.diagnostics {
                Some(handler) => handler(file_path),
                None => Ok(vec![diagnostic(file_path)]),
            };
            tokio::task::yield_now().await;
            outcome.map(|items| LspDiagnosticsResult {
                items,
                transient_error: None,
            })
        })
    }
}

struct SessionWithPackages {
    _projects: TypescriptProjects,
    session: PathBuf,
    files: BTreeMap<String, Vec<String>>,
    order: Vec<String>,
}

impl SessionWithPackages {
    fn package(&self, name: &str) -> String {
        path_string(&self.session.join(name))
    }

    fn files_of(&self, name: &str) -> Vec<String> {
        self.files
            .get(&self.package(name))
            .cloned()
            .unwrap_or_default()
    }

    fn all_files(&self) -> Vec<String> {
        self.order
            .iter()
            .flat_map(|root| self.files.get(root).cloned().unwrap_or_default())
            .collect()
    }
}

/// One package directory per name under a fresh session root; each package is its own workspace root.
fn session_with_packages(names: &[&str], files_per_package: usize) -> SessionWithPackages {
    let mut projects = TypescriptProjects::new("lsp-dir-group-scan-");
    let session = projects.make_project();
    let mut files = BTreeMap::new();
    let mut order = Vec::new();
    for name in names {
        let package_dir = session.join(name);
        std::fs::create_dir_all(&package_dir).expect("create package");
        std::fs::write(package_dir.join("package.json"), "{}\n").expect("write package.json");
        let package_files: Vec<String> = (0..files_per_package)
            .map(|index| path_string(&package_dir.join(format!("f{index}.ts"))))
            .collect();
        for file in &package_files {
            std::fs::write(file, "export {}\n").expect("write package file");
        }
        order.push(path_string(&package_dir));
        files.insert(path_string(&package_dir), package_files);
    }
    SessionWithPackages {
        _projects: projects,
        session,
        files,
        order,
    }
}

fn new_manager(
    factory: impl Fn(&str) -> Arc<FakeGroupClient> + Send + Sync + 'static,
) -> Arc<LspManager> {
    let client_factory: ClientFactory =
        Arc::new(move |root: &str, _server: &ResolvedServer| Ok(factory(root) as SharedClient));
    LspManager::new(LspManagerOptions {
        client_factory: Some(client_factory),
        reaper_interval_ms: Some(3_600_000),
        ..LspManagerOptions::default()
    })
}

fn referenced_clients(manager: &LspManager) -> usize {
    manager
        .get_snapshot()
        .iter()
        .map(|client| client.ref_count)
        .sum()
}

fn roots_and_refs(manager: &LspManager) -> Vec<(String, usize)> {
    manager
        .get_snapshot()
        .into_iter()
        .map(|client| (client.root, client.ref_count))
        .collect()
}

fn fixed_files(files: Vec<String>) -> ListFiles {
    Arc::new(move |_, _, _| files.clone())
}

fn scan_options(
    manager: &Arc<LspManager>,
    files: Vec<String>,
    timers: Option<SharedTimerProvider>,
    signal: Option<AbortSignal>,
) -> DirectoryDiagnosticsOptions {
    DirectoryDiagnosticsOptions {
        list_files: Some(fixed_files(files)),
        manager: Some(Arc::clone(manager)),
        server: Some(server()),
        timers,
        signal,
        ..DirectoryDiagnosticsOptions::default()
    }
}

async fn scan(
    session: &Path,
    options: DirectoryDiagnosticsOptions,
) -> Result<DirectoryDiagnosticsResult, LspError> {
    let directory = path_string(session);
    scope_request_context(
        context_for(session),
        aggregate_diagnostics_for_directory(
            &directory,
            ".ts",
            Some(SeverityFilter::All),
            Some(50),
            options,
        ),
    )
    .await
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values
}

fn timeout_message() -> String {
    format!("typescript did not start within {INIT_TIMEOUT_MS}ms")
}

#[tokio::test]
async fn given_a_group_whose_client_never_finishes_starting_before_a_healthy_group_when_scanning_then_the_healthy_group_is_diagnosed_first_and_only_the_slow_groups_files_time_out()
 {
    // given: the slow group has more files than there are workers and comes first
    let packages = session_with_packages(&["slow", "fast"], 5);
    let slow_root = packages.package("slow");
    let fast_root = packages.package("fast");
    let fast_checked = Deferred::new();
    let fast_count = Arc::new(AtomicUsize::new(0));
    let clients: Arc<Mutex<BTreeMap<String, Arc<FakeGroupClient>>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let manager = {
        let slow_root = slow_root.clone();
        let fast_checked = fast_checked.clone();
        let fast_count = Arc::clone(&fast_count);
        let clients = Arc::clone(&clients);
        new_manager(move |root| {
            let client = if root == slow_root {
                FakeGroupClient::new(StartBehavior::Never)
            } else {
                let fast_checked = fast_checked.clone();
                let fast_count = Arc::clone(&fast_count);
                FakeGroupClient::new(StartBehavior::Ready).with_diagnostics(Box::new(move |file| {
                    if fast_count.fetch_add(1, Ordering::SeqCst) + 1 == 5 {
                        fast_checked.resolve();
                    }
                    Ok(vec![diagnostic(file)])
                }))
            };
            let client = Arc::new(client);
            clients
                .lock()
                .expect("clients")
                .insert(root.to_string(), Arc::clone(&client));
            client
        })
    };
    let clock = ControlledClock::new();
    let mut files = packages.files_of("slow");
    files.extend(packages.files_of("fast"));

    // when
    let scanning = scan(
        &packages.session,
        scan_options(&manager, files, Some(clock.timers()), None),
    );
    let driver = async {
        within(
            fast_checked.wait(),
            "the healthy group was never diagnosed while the slow group was starting",
        )
        .await;
        let fast_done_at = clock.now();
        clock.wait_for_timer(INIT_TIMEOUT_MS).await;
        clock.advance_by(INIT_TIMEOUT_MS);
        fast_done_at
    };
    let (result, fast_done_at) = within(
        async { tokio::join!(scanning, driver) },
        "the scan never finished",
    )
    .await;
    let result = result.expect("scan result");
    let slow_client = Arc::clone(
        clients
            .lock()
            .expect("clients")
            .get(&slow_root)
            .expect("slow client"),
    );
    slow_client.wait_stop_count(1).await;

    // then
    assert_eq!(fast_done_at, 0);
    assert_eq!(result.total_diagnostics, 5);
    assert_eq!(
        sorted(
            result
                .file_failures
                .iter()
                .map(|failure| failure.file.clone())
                .collect()
        ),
        packages.files_of("slow")
    );
    let within_deadline = format!("within {INIT_TIMEOUT_MS}ms");
    assert!(
        result
            .file_failures
            .iter()
            .all(|failure| failure.error.contains(&within_deadline))
    );
    assert_eq!(roots_and_refs(&manager), vec![(fast_root, 0)]);
    assert_eq!(slow_client.stop_count(), 1);
    manager.stop_all().await;
}

#[tokio::test]
async fn given_many_groups_with_a_failing_start_and_a_failing_file_when_scanning_then_referenced_clients_stay_within_the_worker_limit_and_all_are_released()
 {
    // given
    let names: Vec<String> = (0..10).map(|index| format!("p{index}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let packages = session_with_packages(&name_refs, 2);
    let max_referenced = Arc::new(AtomicUsize::new(0));
    let slot: Arc<OnceLock<Weak<LspManager>>> = Arc::new(OnceLock::new());
    let failing_file = path_string(&packages.session.join("p5").join("f0.ts"));
    let manager = {
        let slot = Arc::clone(&slot);
        let max_referenced = Arc::clone(&max_referenced);
        new_manager(move |root| {
            let start = if root.ends_with("p3") {
                StartBehavior::Fail("p3 cannot start")
            } else {
                StartBehavior::Ready
            };
            let slot = Arc::clone(&slot);
            let max_referenced = Arc::clone(&max_referenced);
            let failing_file = failing_file.clone();
            Arc::new(
                FakeGroupClient::new(start).with_diagnostics(Box::new(move |file| {
                    if let Some(manager) = slot.get().and_then(Weak::upgrade) {
                        max_referenced.fetch_max(referenced_clients(&manager), Ordering::SeqCst);
                    }
                    if file == failing_file {
                        Err(LspError::other("p5 file failed"))
                    } else {
                        Ok(vec![diagnostic(file)])
                    }
                })),
            )
        })
    };
    slot.set(Arc::downgrade(&manager))
        .expect("manager slot is set once");

    // when
    let result = within(
        scan(
            &packages.session,
            scan_options(&manager, packages.all_files(), None, None),
        ),
        "the scan never finished",
    )
    .await
    .expect("scan result");

    // then
    let max_referenced = max_referenced.load(Ordering::SeqCst);
    assert!(max_referenced > 0);
    assert!(max_referenced <= 4, "{max_referenced}");
    assert_eq!(referenced_clients(&manager), 0);
    assert_eq!(result.total_diagnostics, 17);
    assert_eq!(
        sorted(
            result
                .file_failures
                .iter()
                .map(|failure| failure.error.clone())
                .collect()
        ),
        vec![
            "p3 cannot start".to_string(),
            "p3 cannot start".to_string(),
            "p5 file failed".to_string()
        ]
    );
    manager.stop_all().await;
}

#[tokio::test]
async fn given_four_groups_whose_clients_never_start_ahead_of_a_healthy_group_when_the_start_deadline_passes_then_the_scan_finishes_at_the_deadline_while_the_stalled_clients_shutdown_stays_tracked()
 {
    // given: stopping a stalled client takes until `shutdown` resolves; a second start of a root succeeds
    let packages = session_with_packages(&["s0", "s1", "s2", "s3", "fast"], 1);
    let fast_root = packages.package("fast");
    let shutdown = Deferred::new();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let manager = {
        let fast_root = fast_root.clone();
        let shutdown = shutdown.clone();
        let log = Arc::clone(&log);
        new_manager(move |root| {
            let name = Path::new(root)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let generation = {
                let mut entries = log.lock().expect("log");
                let prefix = format!("spawn:{name}#");
                let generation = entries
                    .iter()
                    .filter(|event| event.starts_with(&prefix))
                    .count()
                    + 1;
                entries.push(format!("spawn:{name}#{generation}"));
                generation
            };
            if root == fast_root || generation > 1 {
                return Arc::new(FakeGroupClient::new(StartBehavior::Ready));
            }
            let stopped_log = Arc::clone(&log);
            let mut client = FakeGroupClient::new(StartBehavior::Never);
            client.stop_gate = Some(shutdown.clone());
            client.on_stopped = Some(Box::new(move || {
                stopped_log
                    .lock()
                    .expect("log")
                    .push(format!("stopped:{name}"));
            }));
            Arc::new(client)
        })
    };
    let clock = ControlledClock::new();
    let scanning = scan(
        &packages.session,
        scan_options(&manager, packages.all_files(), Some(clock.timers()), None),
    );

    // when
    let driver = async {
        clock.wait_for_scheduled(4).await;
        clock.advance_by(INIT_TIMEOUT_MS);
    };
    let (result, ()) = within(
        async { tokio::join!(scanning, driver) },
        "the scan did not finish at the start deadline while the stalled clients were still stopping",
    )
    .await;
    let result = result.expect("scan result");

    // then: timeouts reported and the healthy group diagnosed at the deadline, no stop finished yet
    let stopped = |log: &Arc<Mutex<Vec<String>>>| -> Vec<String> {
        log.lock()
            .expect("log")
            .iter()
            .filter(|event| event.starts_with("stopped:"))
            .cloned()
            .collect()
    };
    assert_eq!(clock.now(), INIT_TIMEOUT_MS);
    assert_eq!(result.total_diagnostics, 1);
    assert_eq!(
        result
            .file_failures
            .iter()
            .map(|failure| failure.error.clone())
            .collect::<Vec<_>>(),
        vec![timeout_message(); 4]
    );
    assert_eq!(stopped(&log), Vec::<String>::new());
    assert_eq!(roots_and_refs(&manager), vec![(fast_root, 0)]);

    // then: the manager still tracks each stop, so a stalled root restarts only after it finished
    let s0 = packages.package("s0");
    let server = server();
    let reacquired = manager.get_client(&s0, &server, None);
    shutdown.resolve();
    let client = within(reacquired, "the stalled root never restarted")
        .await
        .expect("reacquired client");
    manager.release_client(&s0, &server, Some(&client));
    let position = |event: &str| {
        log.lock()
            .expect("log")
            .iter()
            .position(|entry| entry == event)
    };
    assert!(
        position("stopped:s0").expect("s0 stopped") < position("spawn:s0#2").expect("s0 respawned")
    );
    manager.stop_all().await;
    assert_eq!(
        sorted(stopped(&log)),
        vec!["stopped:s0", "stopped:s1", "stopped:s2", "stopped:s3"]
    );
    assert_eq!(referenced_clients(&manager), 0);
}

struct SharedColdStart {
    packages: SessionWithPackages,
    root: String,
    started: Deferred,
    spawned: Deferred,
    clients: Arc<Mutex<Vec<Arc<FakeGroupClient>>>>,
    manager: Arc<LspManager>,
    clock: Arc<ControlledClock>,
}

impl SharedColdStart {
    fn client(&self, index: usize) -> Arc<FakeGroupClient> {
        Arc::clone(&self.clients.lock().expect("clients")[index])
    }

    fn client_total(&self) -> usize {
        self.clients.lock().expect("clients").len()
    }
}

/// One package whose client starts when `started` resolves; the scan is the first waiter on that start.
fn cold_start_shared_with_scan() -> SharedColdStart {
    let packages = session_with_packages(&["shared"], 1);
    let root = packages.package("shared");
    let started = Deferred::new();
    let spawned = Deferred::new();
    let clients: Arc<Mutex<Vec<Arc<FakeGroupClient>>>> = Arc::new(Mutex::new(Vec::new()));
    let manager = {
        let started = started.clone();
        let spawned = spawned.clone();
        let clients = Arc::clone(&clients);
        new_manager(move |_root| {
            let client = Arc::new(FakeGroupClient::new(StartBehavior::Gate(started.clone())));
            clients.lock().expect("clients").push(Arc::clone(&client));
            spawned.resolve();
            client
        })
    };
    SharedColdStart {
        packages,
        root,
        started,
        spawned,
        clients,
        manager,
        clock: ControlledClock::new(),
    }
}

#[tokio::test]
async fn given_a_scan_and_another_caller_waiting_on_one_cold_start_when_the_scans_start_deadline_passes_then_the_other_caller_still_gets_the_started_client()
 {
    // given
    let shared = cold_start_shared_with_scan();
    let server = server();
    let scanning = scan(
        &shared.packages.session,
        scan_options(
            &shared.manager,
            shared.packages.files_of("shared"),
            Some(shared.clock.timers()),
            None,
        ),
    );
    let other = shared.manager.get_client(&shared.root, &server, None);
    tokio::pin!(other);

    // when
    let driver = async {
        within(
            shared.spawned.wait(),
            "the scan never started the shared client",
        )
        .await;
        assert!((&mut other).now_or_never().is_none());
        shared.clock.wait_for_timer(INIT_TIMEOUT_MS).await;
        shared.clock.advance_by(INIT_TIMEOUT_MS);
    };
    let (scan_result, ()) = within(
        async { tokio::join!(scanning, driver) },
        "the scan never passed its start deadline",
    )
    .await;
    shared.started.resolve();
    let client = within(other, "the other caller never got the client")
        .await
        .expect("other caller acquisition");

    // then
    let scan_error = scan_result.expect_err("the scan fails without a started client");
    assert!(
        scan_error
            .to_string()
            .contains(&format!("did not start within {INIT_TIMEOUT_MS}ms")),
        "{scan_error}"
    );
    assert_eq!(shared.client_total(), 1);
    let first = shared.client(0);
    assert!(std::ptr::addr_eq(Arc::as_ptr(&client), Arc::as_ptr(&first)));
    assert_eq!(first.stop_count(), 0);
    let snapshot: Vec<(String, usize, bool)> = shared
        .manager
        .get_snapshot()
        .into_iter()
        .map(|entry| (entry.root, entry.ref_count, entry.is_initializing))
        .collect();
    assert_eq!(snapshot, vec![(shared.root.clone(), 1, false)]);
    shared
        .manager
        .release_client(&shared.root, &server, Some(&client));
    shared.manager.stop_all().await;
}

#[tokio::test]
async fn given_a_scan_and_another_caller_waiting_on_one_cold_start_when_the_deadline_passes_and_then_the_other_caller_aborts_then_the_startup_is_stopped_only_once_its_last_waiter_left()
 {
    // given
    let shared = cold_start_shared_with_scan();
    let server = server();
    let controller = AbortController::new();
    let signal = controller.signal();
    let scanning = scan(
        &shared.packages.session,
        scan_options(
            &shared.manager,
            shared.packages.files_of("shared"),
            Some(shared.clock.timers()),
            None,
        ),
    );
    let other = shared
        .manager
        .get_client(&shared.root, &server, Some(&signal));
    tokio::pin!(other);

    // when: the scan's waiter leaves first
    let driver = async {
        within(
            shared.spawned.wait(),
            "the scan never started the shared client",
        )
        .await;
        assert!((&mut other).now_or_never().is_none());
        shared.clock.wait_for_timer(INIT_TIMEOUT_MS).await;
        shared.clock.advance_by(INIT_TIMEOUT_MS);
    };
    let (scan_result, ()) = within(
        async { tokio::join!(scanning, driver) },
        "the scan never passed its start deadline",
    )
    .await;
    assert!(scan_result.is_err());

    // then
    let first = shared.client(0);
    assert_eq!(first.stop_count(), 0);
    assert_eq!(shared.manager.client_count(), 1);

    // when: the last waiter leaves
    controller.abort();

    // then
    let outcome = within(other, "the aborted caller never settled").await;
    let error = outcome.err().expect("the aborted caller rejects");
    assert_eq!(error.name(), "AbortError");
    first.wait_stop_count(1).await;
    assert_eq!(first.stop_count(), 1);
    assert_eq!(shared.manager.client_count(), 0);
    shared.manager.stop_all().await;
}

#[tokio::test]
async fn given_many_groups_when_the_scan_is_aborted_while_files_are_being_checked_then_it_throws_the_abort_after_every_client_is_released()
 {
    // given
    let names: Vec<String> = (0..10).map(|index| format!("p{index}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let packages = session_with_packages(&name_refs, 2);
    let controller = AbortController::new();
    let max_referenced = Arc::new(AtomicUsize::new(0));
    let checked = Arc::new(AtomicUsize::new(0));
    let slot: Arc<OnceLock<Weak<LspManager>>> = Arc::new(OnceLock::new());
    let manager = {
        let slot = Arc::clone(&slot);
        let max_referenced = Arc::clone(&max_referenced);
        let checked = Arc::clone(&checked);
        let controller = controller.clone();
        new_manager(move |_root| {
            let slot = Arc::clone(&slot);
            let max_referenced = Arc::clone(&max_referenced);
            let checked = Arc::clone(&checked);
            let controller = controller.clone();
            Arc::new(
                FakeGroupClient::new(StartBehavior::Ready).with_diagnostics(Box::new(
                    move |file| {
                        if let Some(manager) = slot.get().and_then(Weak::upgrade) {
                            max_referenced
                                .fetch_max(referenced_clients(&manager), Ordering::SeqCst);
                        }
                        if checked.fetch_add(1, Ordering::SeqCst) + 1 == 6 {
                            controller.abort();
                        }
                        Ok(vec![diagnostic(file)])
                    },
                )),
            )
        })
    };
    slot.set(Arc::downgrade(&manager))
        .expect("manager slot is set once");

    // when
    let outcome = within(
        scan(
            &packages.session,
            scan_options(
                &manager,
                packages.all_files(),
                None,
                Some(controller.signal()),
            ),
        ),
        "the aborted scan never settled",
    )
    .await;

    // then
    let error = outcome.err().expect("the scan rethrows the abort");
    assert!(matches!(error, LspError::Aborted), "{error}");
    assert_eq!(error.name(), "AbortError");
    assert!(checked.load(Ordering::SeqCst) < 20);
    assert!(max_referenced.load(Ordering::SeqCst) <= 4);
    assert_eq!(referenced_clients(&manager), 0);
    manager.stop_all().await;
}
