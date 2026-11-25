//! Translation of `manager-test-support.ts`.

use crate::support::controlled_clock::ControlledClock;
use crate::support::controlled_clock::Deferred;
use crate::support::controlled_clock::within;
use lsp_core::abort::AbortSignal;
use lsp_core::lsp::client::LspDiagnosticsResult;
use lsp_core::lsp::errors::LspError;
use lsp_core::lsp::manager::ClientFactory;
use lsp_core::lsp::manager::ClientFuture;
use lsp_core::lsp::manager::LspManager;
use lsp_core::lsp::manager::LspManagerOptions;
use lsp_core::lsp::manager::ManagedLspClient;
use lsp_core::lsp::manager::SharedClient;
use lsp_core::lsp::types::ResolvedServer;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::watch;

pub const IDLE_TIMEOUT_MS: u64 = 1_000;
pub const INIT_TIMEOUT_MS: u64 = 5_000;
pub const REAPER_INTERVAL_MS: u64 = 100;
pub const MICROTASK_SWEEP: std::ops::Range<usize> = 0..12;

pub fn server() -> ResolvedServer {
    ResolvedServer {
        id: "typescript".to_string(),
        command: vec!["fake-ts-ls".to_string()],
        extensions: vec![".ts".to_string()],
        priority: 1.0,
        env: None,
        initialization: None,
    }
}

/// A client whose start finishes when `started` resolves and whose stop finishes when `stopped`
/// resolves.
pub struct GatedClient {
    pub started: Deferred,
    pub stopped: Deferred,
    pub initialized: Deferred,
    stop_count: watch::Sender<usize>,
    alive: AtomicBool,
}

impl GatedClient {
    fn new(hold_start: bool, hold_stop: bool) -> Self {
        let client = Self {
            started: Deferred::new(),
            stopped: Deferred::new(),
            initialized: Deferred::new(),
            stop_count: watch::channel(0).0,
            alive: AtomicBool::new(true),
        };
        if !hold_start {
            client.started.resolve();
        }
        if !hold_stop {
            client.stopped.resolve();
        }
        client
    }

    pub fn stop_count(&self) -> usize {
        *self.stop_count.borrow()
    }

    /// The manager stops clients on spawned tasks; this awaits the stop call itself.
    pub async fn wait_stop_count(&self, count: usize) {
        let mut stops = self.stop_count.subscribe();
        within(
            async {
                let _ = stops.wait_for(|stopped| *stopped >= count).await;
            },
            "the client was never stopped",
        )
        .await;
    }

    pub fn running(&self) -> bool {
        self.alive.load(Ordering::SeqCst) && self.stop_count() == 0
    }
}

impl ManagedLspClient for GatedClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move {
            self.started.wait().await;
            Ok(())
        })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move {
            self.initialized.resolve();
            Ok(())
        })
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async move {
            self.stop_count.send_modify(|stopped| *stopped += 1);
            self.stopped.wait().await;
            self.alive.store(false, Ordering::SeqCst);
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn command(&self) -> Vec<String> {
        server().command
    }

    fn diagnostics<'a>(
        &'a self,
        _file_path: &'a str,
        _signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(async {
            Ok(LspDiagnosticsResult {
                items: Vec::new(),
                transient_error: None,
            })
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GatedManagerOptions {
    pub max_resident_clients: Option<usize>,
    /// Whether the start of the first spawned client waits for its gate.
    pub hold_first_start: bool,
    pub hold_stops: bool,
}

pub struct GatedManager {
    pub manager: Arc<LspManager>,
    pub clock: Arc<ControlledClock>,
    clients: Arc<Mutex<Vec<Arc<GatedClient>>>>,
    spawned: watch::Sender<usize>,
}

/// A real LspManager over gated fake clients, on a virtual clock that also drives its reaper.
pub fn gated_manager(options: GatedManagerOptions) -> GatedManager {
    let clock = ControlledClock::new();
    let clients: Arc<Mutex<Vec<Arc<GatedClient>>>> = Arc::new(Mutex::new(Vec::new()));
    let spawned = watch::channel(0_usize).0;
    let factory: ClientFactory = {
        let clients = Arc::clone(&clients);
        let spawned = spawned.clone();
        Arc::new(move |_root: &str, _server: &ResolvedServer| {
            let client = {
                let mut list = clients.lock().expect("gated clients");
                let hold_start = options.hold_first_start && list.is_empty();
                let client = Arc::new(GatedClient::new(hold_start, options.hold_stops));
                list.push(Arc::clone(&client));
                client
            };
            spawned.send_modify(|count| *count += 1);
            Ok(client as SharedClient)
        })
    };
    let manager = LspManager::new(LspManagerOptions {
        max_resident_clients: Some(options.max_resident_clients.unwrap_or(6)),
        idle_timeout_ms: Some(IDLE_TIMEOUT_MS),
        init_timeout_ms: Some(INIT_TIMEOUT_MS),
        reaper_interval_ms: Some(REAPER_INTERVAL_MS),
        now: Some(clock.manager_now()),
        timers: Some(clock.timers()),
        client_factory: Some(factory),
    });
    GatedManager {
        manager,
        clock,
        clients,
        spawned,
    }
}

impl GatedManager {
    pub fn client(&self, index: usize) -> Arc<GatedClient> {
        Arc::clone(&self.clients.lock().expect("gated clients")[index])
    }

    pub fn shared_client(&self, index: usize) -> SharedClient {
        self.client(index) as SharedClient
    }

    pub fn open_all_stops(&self) {
        for client in self.clients.lock().expect("gated clients").iter() {
            client.stopped.resolve();
        }
    }

    pub fn client_total(&self) -> usize {
        self.clients.lock().expect("gated clients").len()
    }

    pub async fn wait_for_clients(&self, count: usize) {
        let mut spawned = self.spawned.subscribe();
        within(
            async {
                let _ = spawned.wait_for(|total| *total >= count).await;
            },
            "the manager never spawned the awaited client",
        )
        .await;
    }

    /// Per pooled key: root, references, counted waiters, still starting.
    pub fn pool(&self) -> Vec<(String, usize, usize, bool)> {
        pool(&self.manager)
    }
}

pub fn pool(manager: &LspManager) -> Vec<(String, usize, usize, bool)> {
    manager
        .get_snapshot()
        .into_iter()
        .map(|entry| {
            (
                entry.root,
                entry.ref_count,
                entry.pending_waiters,
                entry.is_initializing,
            )
        })
        .collect()
}

pub fn entry(
    root: &str,
    refs: usize,
    waiters: usize,
    initializing: bool,
) -> (String, usize, usize, bool) {
    (root.to_string(), refs, waiters, initializing)
}

pub fn same_client(client: &SharedClient, gated: &Arc<GatedClient>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(client), Arc::as_ptr(gated))
}
