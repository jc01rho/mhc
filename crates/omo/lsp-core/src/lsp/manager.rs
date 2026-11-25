use crate::abort::AbortSignal;
use crate::lsp::cleanup_errors::report_best_effort_cleanup_error;
use crate::lsp::client::LspClient;
use crate::lsp::client::LspClientOptions;
use crate::lsp::client::LspDiagnosticsResult;
use crate::lsp::client_key::lsp_client_key;
use crate::lsp::client_startup::InitOutcome;
use crate::lsp::client_startup::ManagedClient;
use crate::lsp::client_startup::await_init;
use crate::lsp::client_startup::await_with_signal;
use crate::lsp::constants::CLIENT_RESPAWN_COOLDOWN_MS;
use crate::lsp::constants::CLIENT_RESPAWN_RETRY_LIMIT;
use crate::lsp::constants::IDLE_TIMEOUT_MS;
use crate::lsp::constants::INIT_TIMEOUT_MS;
use crate::lsp::constants::MAX_RESIDENT_CLIENTS;
use crate::lsp::constants::REAPER_INTERVAL_MS;
use crate::lsp::errors::LspError;
use crate::lsp::process_signal_cleanup::SignalCleanupGuard;
use crate::lsp::process_signal_cleanup::install_process_signal_cleanup;
use crate::lsp::timer_provider::RepeatHandle;
use crate::lsp::timer_provider::SharedTimerProvider;
use crate::lsp::timer_provider::real_timer_provider;
use crate::lsp::timer_provider::repeat_every;
use crate::lsp::types::ResolvedServer;
use indexmap::IndexMap;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::Weak;
use tokio::sync::watch;

pub type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The client surface the manager and directory diagnostics depend on. [`LspClient`]
/// implements it; tests inject fakes (TS used `clientFactory` plus subclass overrides).
pub trait ManagedLspClient: Send + Sync {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>>;
    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>>;
    fn stop(&self) -> ClientFuture<'_, ()>;
    fn is_alive(&self) -> bool;
    fn command(&self) -> Vec<String>;
    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>>;
    /// The concrete client for tools that need the full request surface.
    fn as_lsp_client(&self) -> Option<&LspClient> {
        None
    }
}

impl ManagedLspClient for LspClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async move { LspClient::start(self) })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(LspClient::initialize(self))
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(LspClient::stop(self))
    }

    fn is_alive(&self) -> bool {
        LspClient::is_alive(self)
    }

    fn command(&self) -> Vec<String> {
        LspClient::command(self)
    }

    fn diagnostics<'a>(
        &'a self,
        file_path: &'a str,
        signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(LspClient::diagnostics(self, file_path, signal))
    }

    fn as_lsp_client(&self) -> Option<&LspClient> {
        Some(self)
    }
}

pub type SharedClient = Arc<dyn ManagedLspClient>;
pub type ClientFactory =
    Arc<dyn Fn(&str, &ResolvedServer) -> Result<SharedClient, LspError> + Send + Sync>;
pub type ManagerNow = Arc<dyn Fn() -> f64 + Send + Sync>;

/// TS `ClientSnapshot`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientSnapshot {
    pub root: String,
    pub server_id: String,
    pub ref_count: usize,
    pub pending_waiters: usize,
    pub last_used_at: f64,
    pub is_initializing: bool,
    pub alive: bool,
    pub command: Vec<String>,
}

/// TS `LspManagerOptions`.
#[derive(Clone, Default)]
pub struct LspManagerOptions {
    pub idle_timeout_ms: Option<u64>,
    pub init_timeout_ms: Option<u64>,
    pub max_resident_clients: Option<usize>,
    pub reaper_interval_ms: Option<u64>,
    pub client_factory: Option<ClientFactory>,
    pub now: Option<ManagerNow>,
    /// Schedules the reaper.
    pub timers: Option<SharedTimerProvider>,
}

#[derive(Debug, Clone, PartialEq)]
struct RespawnBudget {
    dead_generations: usize,
    exhausted_at: Option<f64>,
}

#[derive(Default)]
struct ManagerState {
    clients: IndexMap<String, ManagedClient>,
    pending_stops: HashMap<String, watch::Receiver<bool>>,
    respawn_budgets: HashMap<String, RespawnBudget>,
    disposed: bool,
    next_id: u64,
    signal_cleanup: Option<SignalCleanupGuard>,
}

/// TS `LspManager`: pools one client per `(root, server id, resolved command)`, shares
/// initialization between concurrent callers, limits resident clients via idle LRU eviction,
/// enforces pending-stop tombstones to prevent duplicate client spawn races, and bounds respawn
/// attempts for crash-looping servers. The startup lifecycle contract is in
/// [`crate::lsp::client_startup`].
pub struct LspManager {
    state: Mutex<ManagerState>,
    idle_timeout_ms: f64,
    init_timeout_ms: f64,
    max_resident_clients: usize,
    client_factory: ClientFactory,
    now: ManagerNow,
    stop_reaper: RepeatHandle,
}

impl std::fmt::Debug for LspManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspManager")
            .field("clients", &self.client_count())
            .field("max_resident_clients", &self.max_resident_clients)
            .finish()
    }
}

async fn stop_client_best_effort(client: SharedClient) {
    client.stop().await;
}

fn system_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn disposed_error() -> LspError {
    LspError::other("LspManager has been disposed")
}

impl LspManager {
    /// Creates a manager and starts its reaper when a Tokio runtime is available.
    pub fn new(options: LspManagerOptions) -> Arc<Self> {
        let idle_timeout_ms = options.idle_timeout_ms.unwrap_or(IDLE_TIMEOUT_MS) as f64;
        let init_timeout_ms = options.init_timeout_ms.unwrap_or(INIT_TIMEOUT_MS) as f64;
        let reaper_interval_ms = options.reaper_interval_ms.unwrap_or(REAPER_INTERVAL_MS);
        let timers = options.timers.clone().unwrap_or_else(real_timer_provider);
        let client_factory = options.client_factory.unwrap_or_else(|| {
            Arc::new(|root: &str, server: &ResolvedServer| {
                Ok(Arc::new(LspClient::new(
                    root,
                    server.clone(),
                    LspClientOptions::default(),
                )) as SharedClient)
            })
        });
        let now: ManagerNow = options.now.unwrap_or_else(|| Arc::new(system_now_ms));

        let manager = Arc::new_cyclic(|weak| {
            let weak: Weak<LspManager> = weak.clone();
            let tick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Some(manager) = weak.upgrade() {
                    manager.reap_stale();
                }
            });
            let stop_reaper = repeat_every(timers, reaper_interval_ms.max(1), tick);
            Self {
                state: Mutex::new(ManagerState {
                    next_id: 1,
                    ..ManagerState::default()
                }),
                idle_timeout_ms,
                init_timeout_ms,
                max_resident_clients: options.max_resident_clients.unwrap_or(MAX_RESIDENT_CLIENTS),
                client_factory,
                now,
                stop_reaper,
            }
        });

        let weak = Arc::downgrade(&manager);
        let signal_cleanup = install_process_signal_cleanup(move || {
            let manager = weak.upgrade();
            async move {
                if let Some(manager) = manager {
                    manager.stop_all().await;
                }
            }
        });
        manager.lock().signal_cleanup = Some(signal_cleanup);
        manager
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ManagerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Deletes-then-stops with a tombstone: the pending stop is recorded so a concurrent getClient
    /// for the same key awaits it instead of spawning a duplicate client while the old one lingers
    /// (production stops reach ~21s). The tombstone removes itself once the stop settles.
    fn tombstone_stop(self: &Arc<Self>, key: &str, client: SharedClient) -> watch::Receiver<bool> {
        let (tx, rx) = watch::channel(false);
        {
            let mut state = self.lock();
            state.pending_stops.insert(key.to_string(), rx.clone());
        }

        let weak = Arc::downgrade(self);
        let key_str = key.to_string();
        tokio::spawn(async move {
            stop_client_best_effort(client).await;
            let _ = tx.send(true);
            if let Some(mgr) = weak.upgrade() {
                let mut state = mgr.lock();
                state.pending_stops.remove(&key_str);
            }
        });

        rx
    }

    fn record_dead_generation(&self, key: &str) {
        let mut state = self.lock();
        let budget = state
            .respawn_budgets
            .entry(key.to_string())
            .or_insert_with(|| RespawnBudget {
                dead_generations: 0,
                exhausted_at: None,
            });
        budget.dead_generations += 1;
    }

    fn mark_healthy_generation(state: &mut ManagerState, key: &str) {
        state.respawn_budgets.remove(key);
    }

    /// Throws while the respawn budget is spent and the cooldown has not elapsed.
    fn ensure_respawn_allowed(
        &self,
        key: &str,
        root: &str,
        server_id: &str,
    ) -> Result<(), LspError> {
        let mut state = self.lock();
        let Some(budget) = state.respawn_budgets.get_mut(key) else {
            return Ok(());
        };
        if budget.dead_generations < CLIENT_RESPAWN_RETRY_LIMIT {
            return Ok(());
        }
        let now = (self.now)();
        match budget.exhausted_at {
            None => {
                budget.exhausted_at = Some(now);
                Err(LspError::ClientRespawnBudgetExceeded {
                    server_id: server_id.to_string(),
                    root: root.to_string(),
                    retry_limit: CLIENT_RESPAWN_RETRY_LIMIT,
                })
            }
            Some(exhausted_at) => {
                if now - exhausted_at < CLIENT_RESPAWN_COOLDOWN_MS as f64 {
                    Err(LspError::ClientRespawnBudgetExceeded {
                        server_id: server_id.to_string(),
                        root: root.to_string(),
                        retry_limit: CLIENT_RESPAWN_RETRY_LIMIT,
                    })
                } else {
                    budget.dead_generations = 0;
                    budget.exhausted_at = None;
                    Ok(())
                }
            }
        }
    }

    /// Frees capacity for one new client by stopping the least recently used idle client.
    ///
    /// Only clients with no in-flight work are eligible, so a resident server is never torn down
    /// underneath a live request; when every resident client is busy the cap is exceeded instead.
    fn evict_for_admission(self: &Arc<Self>) {
        loop {
            let victim = {
                let mut state = self.lock();
                if state.clients.len() < self.max_resident_clients {
                    break;
                }
                let mut candidate_key: Option<String> = None;
                let mut candidate_last_used = f64::MAX;
                for (key, managed) in state.clients.iter() {
                    if managed.ref_count > 0
                        || managed.pending_waiters > 0
                        || managed.is_initializing
                    {
                        continue;
                    }
                    if candidate_key.is_none() || managed.last_used_at < candidate_last_used {
                        candidate_key = Some(key.clone());
                        candidate_last_used = managed.last_used_at;
                    }
                }
                candidate_key.and_then(|key| state.clients.shift_remove(&key))
            };

            match victim {
                Some(managed) => {
                    let key = managed.key.clone();
                    self.tombstone_stop(&key, managed.client);
                }
                None => break,
            }
        }
    }

    fn reap_stale(self: &Arc<Self>) {
        let now = (self.now)();
        let stale: Vec<(String, SharedClient)> = {
            let mut state = self.lock();
            let expired: Vec<String> = state
                .clients
                .iter()
                .filter(|(_, managed)| {
                    let idle = !managed.is_initializing
                        && managed.ref_count == 0
                        && managed.pending_waiters == 0
                        && now - managed.last_used_at > self.idle_timeout_ms;
                    idle || managed.startup_expired(now, self.init_timeout_ms)
                })
                .map(|(key, _)| key.clone())
                .collect();
            expired
                .into_iter()
                .filter_map(|key| {
                    state
                        .clients
                        .shift_remove(&key)
                        .map(|managed| (key, managed.client))
                })
                .collect()
        };
        for (key, client) in stale {
            self.tombstone_stop(&key, client);
        }
    }

    /// Drops `managed` from the pool and stops it as a tracked tombstone, once; nothing here awaits
    /// the stop.
    fn discard_if_same(self: &Arc<Self>, key: &str, id: u64) {
        let removed = {
            let mut state = self.lock();
            let matches = state
                .clients
                .get(key)
                .is_some_and(|managed| managed.id == id);
            if matches {
                state.clients.shift_remove(key)
            } else {
                None
            }
        };
        if let Some(managed) = removed {
            self.tombstone_stop(key, managed.client);
        }
    }

    fn count_waiter(&self, key: &str, id: u64) {
        let mut state = self.lock();
        if let Some(managed) = state
            .clients
            .get_mut(key)
            .filter(|managed| managed.id == id)
        {
            managed.pending_waiters += 1;
        }
    }

    fn uncount_waiter(&self, key: &str, id: u64) {
        let mut state = self.lock();
        if let Some(managed) = state
            .clients
            .get_mut(key)
            .filter(|managed| managed.id == id)
            && managed.pending_waiters > 0
        {
            managed.pending_waiters -= 1;
        }
    }

    /// TS `ClientStartups.detach`: the last waiter of a waiter-held startup that holds no reference
    /// makes the manager drop and stop it.
    fn detach_waiter(self: &Arc<Self>, key: &str, id: u64) {
        let orphans = {
            let mut state = self.lock();
            let mut orphans = None;
            if let Some(managed) = state
                .clients
                .get_mut(key)
                .filter(|managed| managed.id == id)
            {
                managed.pending_waiters = managed.pending_waiters.saturating_sub(1);
                if managed.pending_waiters == 0 && managed.ref_count == 0 && managed.held_by_waiters
                {
                    orphans = state.clients.shift_remove(key);
                }
            }
            orphans
        };
        if let Some(managed) = orphans {
            self.tombstone_stop(key, managed.client);
        }
    }

    fn finish_startup(&self, key: &str, id: u64) {
        let now = (self.now)();
        let mut state = self.lock();
        if let Some(managed) = state
            .clients
            .get_mut(key)
            .filter(|managed| managed.id == id)
        {
            managed.is_initializing = false;
            managed.initializing_since = None;
            managed.init = None;
            managed.last_used_at = now;
        }
    }

    fn start_client(
        self: &Arc<Self>,
        key: &str,
        root: &str,
        server: &ResolvedServer,
        held_by_waiters: bool,
    ) -> Result<(), LspError> {
        let client = (self.client_factory)(root, server)?;
        let started_at = (self.now)();
        let (sender, receiver) = watch::channel(None::<InitOutcome>);
        let id = {
            let mut state = self.lock();
            let id = state.next_id;
            state.next_id += 1;
            state.clients.insert(
                key.to_string(),
                ManagedClient {
                    id,
                    key: key.to_string(),
                    root: root.to_string(),
                    server_id: server.id.clone(),
                    client: Arc::clone(&client),
                    held_by_waiters,
                    ref_count: 0,
                    pending_waiters: 0,
                    last_used_at: started_at,
                    init: Some(receiver),
                    is_initializing: true,
                    initializing_since: Some(started_at),
                },
            );
            id
        };

        let weak = Arc::downgrade(self);
        let task_key = key.to_string();
        tokio::spawn(async move {
            let outcome = match client.start().await {
                Ok(()) => client.initialize().await,
                Err(error) => Err(error),
            };
            if let Some(manager) = weak.upgrade() {
                match &outcome {
                    Ok(()) => manager.finish_startup(&task_key, id),
                    Err(_) => manager.discard_if_same(&task_key, id),
                }
            }
            sender.send_replace(Some(outcome));
        });
        Ok(())
    }

    /// TS `getClient`: returns a started, initialized client and takes one reference. Every await
    /// happens before the acquisition counts or while it counts as a waiter (client-startup.ts).
    pub async fn get_client(
        self: &Arc<Self>,
        root: &str,
        server: &ResolvedServer,
        signal: Option<&AbortSignal>,
    ) -> Result<SharedClient, LspError> {
        let key = lsp_client_key(root, server);
        loop {
            if self.lock().disposed {
                return Err(disposed_error());
            }
            if let Some(signal) = signal {
                signal.throw_if_aborted()?;
            }

            let pending_stop = {
                let state = self.lock();
                state.pending_stops.get(&key).cloned()
            };
            if let Some(mut rx) = pending_stop {
                await_with_signal(rx.wait_for(|settled| *settled), signal)
                    .await?
                    .map_err(|error| LspError::other(error.to_string()))?;
                continue;
            }

            let expired_id = {
                let state = self.lock();
                let now = (self.now)();
                state
                    .clients
                    .get(&key)
                    .filter(|managed| managed.startup_expired(now, self.init_timeout_ms))
                    .map(|managed| managed.id)
            };
            if let Some(id) = expired_id {
                self.discard_if_same(&key, id);
                continue;
            }

            if !self.lock().clients.contains_key(&key) {
                self.evict_for_admission();
                self.ensure_respawn_allowed(&key, root, &server.id)?;
                self.start_client(&key, root, server, true)?;
            }

            let (id, client, init) = {
                let state = self.lock();
                match state.clients.get(&key) {
                    Some(managed) => (
                        managed.id,
                        Arc::clone(&managed.client),
                        managed.init.clone(),
                    ),
                    None => continue,
                }
            };

            if let Some(init) = init {
                self.count_waiter(&key, id);
                match await_init(init, signal).await {
                    Ok(()) => {}
                    Err(error) => {
                        self.detach_waiter(&key, id);
                        return Err(error);
                    }
                }
                if let Some(signal) = signal.filter(|signal| signal.aborted()) {
                    self.detach_waiter(&key, id);
                    signal.throw_if_aborted()?;
                }
                self.uncount_waiter(&key, id);
            }

            let now = (self.now)();
            let outcome = {
                let mut state = self.lock();
                match state
                    .clients
                    .get(&key)
                    .map(|managed| (managed.id, managed.client.is_alive()))
                {
                    Some((current_id, alive)) if current_id == id => {
                        if alive {
                            Self::mark_healthy_generation(&mut state, &key);
                            if let Some(managed) = state.clients.get_mut(&key) {
                                managed.ref_count += 1;
                                managed.last_used_at = now;
                            }
                            Some(true)
                        } else {
                            Some(false)
                        }
                    }
                    _ => None,
                }
            };
            match outcome {
                None => continue,
                Some(true) => return Ok(client),
                Some(false) => {
                    self.record_dead_generation(&key);
                    self.discard_if_same(&key, id);
                    continue;
                }
            }
        }
    }

    /// Gives back the reference on `client`; a client no longer pooled for the key has none left to
    /// give back.
    pub fn release_client(
        &self,
        root: &str,
        server: &ResolvedServer,
        client: Option<&SharedClient>,
    ) {
        let key = lsp_client_key(root, server);
        let now = (self.now)();
        let mut state = self.lock();
        if let Some(managed) = state.clients.get_mut(&key) {
            if let Some(client) = client
                && !Arc::ptr_eq(&managed.client, client)
            {
                return;
            }
            if managed.ref_count > 0 {
                managed.ref_count -= 1;
                managed.last_used_at = now;
            }
        }
    }

    /// TS `invalidateClient`: drops the pooled client (only if it is `client`, when given).
    pub fn invalidate_client(
        self: &Arc<Self>,
        root: &str,
        server: &ResolvedServer,
        client: Option<&SharedClient>,
    ) {
        let key = lsp_client_key(root, server);
        let removed = {
            let mut state = self.lock();
            let matches = state.clients.get(&key).is_some_and(|managed| {
                client.is_none_or(|client| Arc::ptr_eq(&managed.client, client))
            });
            if matches {
                state.clients.shift_remove(&key)
            } else {
                None
            }
        };
        if let Some(managed) = removed {
            self.tombstone_stop(&key, managed.client);
        }
    }

    /// TS `warmupClient`: starts initialization in the background without taking a reference.
    pub fn warmup_client(self: &Arc<Self>, root: &str, server: &ResolvedServer) {
        let key = lsp_client_key(root, server);
        {
            let state = self.lock();
            if state.disposed
                || state.clients.contains_key(&key)
                || state.pending_stops.contains_key(&key)
            {
                return;
            }
        }

        self.evict_for_admission();
        if self.ensure_respawn_allowed(&key, root, &server.id).is_err() {
            return;
        }

        let _ = self.start_client(&key, root, server, false);
    }

    pub fn is_server_initializing(&self, root: &str, server: &ResolvedServer) -> bool {
        self.lock()
            .clients
            .get(&lsp_client_key(root, server))
            .is_some_and(|managed| managed.is_initializing)
    }

    pub fn get_snapshot(&self) -> Vec<ClientSnapshot> {
        self.lock()
            .clients
            .values()
            .map(|managed| ClientSnapshot {
                root: managed.root.clone(),
                server_id: managed.server_id.clone(),
                ref_count: managed.ref_count,
                pending_waiters: managed.pending_waiters,
                last_used_at: managed.last_used_at,
                is_initializing: managed.is_initializing,
                alive: managed.client.is_alive(),
                command: managed.client.command(),
            })
            .collect()
    }

    pub fn has_client(&self, root: &str, server: &ResolvedServer) -> bool {
        self.lock()
            .clients
            .contains_key(&lsp_client_key(root, server))
    }

    pub fn client_count(&self) -> usize {
        self.lock().clients.len()
    }

    /// TS `stopAll`: disposes the manager and stops every pooled client.
    pub async fn stop_all(&self) {
        let (clients, pending_stops): (Vec<SharedClient>, Vec<watch::Receiver<bool>>) = {
            let mut state = self.lock();
            state.disposed = true;
            self.stop_reaper.stop();
            state.signal_cleanup = None;
            state.respawn_budgets.clear();
            let clients = state
                .clients
                .drain(..)
                .map(|(_, managed)| managed.client)
                .collect();
            let pending_stops = state.pending_stops.values().cloned().collect();
            (clients, pending_stops)
        };

        for client in clients {
            stop_client_best_effort(client).await;
        }
        for mut rx in pending_stops {
            let _ = rx.wait_for(|settled| *settled).await;
        }
        self.lock().pending_stops.clear();
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.stop_reaper.stop();
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if !state.clients.is_empty() {
            report_best_effort_cleanup_error(
                "manager drop",
                &format!(
                    "{} LSP client(s) dropped without stopAll",
                    state.clients.len()
                ),
            );
        }
    }
}

static DEFAULT_MANAGER: OnceLock<Mutex<Option<Arc<LspManager>>>> = OnceLock::new();

fn default_slot() -> &'static Mutex<Option<Arc<LspManager>>> {
    DEFAULT_MANAGER.get_or_init(|| Mutex::new(None))
}

/// TS `getLspManager`: the process-wide default manager.
pub fn get_lsp_manager() -> Arc<LspManager> {
    default_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(|| LspManager::new(LspManagerOptions::default()))
        .clone()
}

/// TS `disposeDefaultLspManager`.
pub async fn dispose_default_lsp_manager() {
    let manager = default_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(manager) = manager {
        manager.stop_all().await;
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
