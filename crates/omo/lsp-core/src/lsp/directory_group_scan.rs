use crate::abort::AbortController;
use crate::abort::AbortListenerId;
use crate::abort::AbortSignal;
use crate::lsp::constants::INIT_TIMEOUT_MS;
use crate::lsp::directory_server_groups::ServedFileGroup;
use crate::lsp::errors::LspError;
use crate::lsp::manager::LspManager;
use crate::lsp::manager::SharedClient;
use crate::lsp::timer_provider::SharedTimerProvider;
use crate::lsp::timer_provider::TimerHandle;
use crate::lsp::timer_provider::real_timer_provider;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::Semaphore;

pub type GroupCheckFile =
    Arc<dyn Fn(SharedClient, String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;
pub type GroupUnavailableHook = Arc<dyn Fn(&ServedFileGroup, String) + Send + Sync>;

pub struct GroupScanOptions<'a> {
    pub manager: &'a Arc<LspManager>,
    pub groups: &'a [ServedFileGroup],
    pub max_concurrency: usize,
    pub signal: Option<AbortSignal>,
    pub timers: Option<SharedTimerProvider>,
    pub check_file: GroupCheckFile,
    pub on_group_unavailable: GroupUnavailableHook,
}

struct ReleaseClientGuard<'a> {
    manager: &'a Arc<LspManager>,
    group: &'a ServedFileGroup,
    client: SharedClient,
}

impl Drop for ReleaseClientGuard<'_> {
    fn drop(&mut self) {
        self.manager
            .release_client(&self.group.root, &self.group.server, Some(&self.client));
    }
}

struct AcquireCleanupGuard {
    timer: Option<Box<dyn TimerHandle>>,
    signal: Option<AbortSignal>,
    listener_id: Option<AbortListenerId>,
}

impl Drop for AcquireCleanupGuard {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.cancel();
        }
        if let (Some(signal), Some(id)) = (self.signal.as_ref(), self.listener_id.take()) {
            signal.remove_listener(id);
        }
    }
}

pub async fn scan_served_groups(options: GroupScanOptions<'_>) -> Result<(), LspError> {
    let limit = options.max_concurrency.max(1);
    let file_slots = Arc::new(Semaphore::new(limit));
    let next_group = AtomicUsize::new(0);
    let started_groups = AtomicUsize::new(0);
    let first_error: Mutex<Option<LspError>> = Mutex::new(None);
    let timers = options.timers.clone().unwrap_or_else(real_timer_provider);
    let runner_count = limit.min(options.groups.len());

    {
        let signal = options.signal.clone();
        let manager = options.manager;
        let groups = options.groups;
        let check_file = Arc::clone(&options.check_file);
        let on_group_unavailable = Arc::clone(&options.on_group_unavailable);
        let runner = || {
            let signal = signal.clone();
            let file_slots = Arc::clone(&file_slots);
            let check_file = Arc::clone(&check_file);
            let on_group_unavailable = Arc::clone(&on_group_unavailable);
            let timers = timers.clone();
            let next_group = &next_group;
            let started_groups = &started_groups;
            let first_error = &first_error;
            async move {
                loop {
                    if signal.as_ref().is_some_and(AbortSignal::aborted) {
                        return;
                    }
                    let index = next_group.fetch_add(1, Ordering::SeqCst);
                    let Some(group) = groups.get(index) else {
                        return;
                    };
                    let client =
                        match acquire_within_init_timeout(manager, group, signal.as_ref(), &timers)
                            .await
                        {
                            Ok(client) => client,
                            Err(error) => {
                                if signal.as_ref().is_some_and(AbortSignal::aborted) {
                                    return;
                                }
                                {
                                    let mut guard =
                                        first_error.lock().unwrap_or_else(PoisonError::into_inner);
                                    if guard.is_none() {
                                        *guard = Some(error.clone());
                                    }
                                }
                                on_group_unavailable(group, error.to_string());
                                continue;
                            }
                        };
                    started_groups.fetch_add(1, Ordering::SeqCst);
                    let _release_guard = ReleaseClientGuard {
                        manager,
                        group,
                        client: Arc::clone(&client),
                    };
                    let file_futures = group.files.iter().map(|file| {
                        let file_slots = Arc::clone(&file_slots);
                        let client = Arc::clone(&client);
                        let file = file.clone();
                        let check_file = Arc::clone(&check_file);
                        let signal = signal.clone();
                        async move {
                            let Ok(_permit) = file_slots.acquire().await else {
                                return;
                            };
                            if !signal.as_ref().is_some_and(AbortSignal::aborted) {
                                check_file(client, file).await;
                            }
                        }
                    });
                    futures::future::join_all(file_futures).await;
                }
            }
        };
        futures::future::join_all((0..runner_count).map(|_| runner())).await;
    }

    if let Some(signal) = options.signal.as_ref() {
        signal.throw_if_aborted()?;
    }
    if started_groups.load(Ordering::SeqCst) == 0
        && let Some(error) = first_error
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    {
        return Err(error);
    }
    Ok(())
}

async fn acquire_within_init_timeout(
    manager: &Arc<LspManager>,
    group: &ServedFileGroup,
    signal: Option<&AbortSignal>,
    timers: &SharedTimerProvider,
) -> Result<SharedClient, LspError> {
    let controller = AbortController::new();
    let listener_id = signal.map(|sig| {
        let ctrl = controller.clone();
        sig.on_abort(move |reason| ctrl.abort_with(reason.clone()))
    });
    if let Some(sig) = signal
        && let Some(reason) = sig.reason()
    {
        controller.abort_with(reason);
    }
    let timed_out = Arc::new(AtomicBool::new(false));
    let timer = {
        let timed_out = Arc::clone(&timed_out);
        let ctrl = controller.clone();
        timers.set_timeout(
            Box::new(move || {
                timed_out.store(true, Ordering::SeqCst);
                ctrl.abort();
            }),
            INIT_TIMEOUT_MS,
        )
    };
    let _cleanup = AcquireCleanupGuard {
        timer: Some(timer),
        signal: signal.cloned(),
        listener_id,
    };
    let local_signal = controller.signal();
    let result = manager
        .get_client(&group.root, &group.server, Some(&local_signal))
        .await;
    match result {
        Ok(client) => Ok(client),
        Err(error) => {
            if timed_out.load(Ordering::SeqCst) && !signal.is_some_and(AbortSignal::aborted) {
                Err(LspError::other(format!(
                    "{} did not start within {INIT_TIMEOUT_MS}ms",
                    group.server.id
                )))
            } else {
                Err(error)
            }
        }
    }
}
