//! Port of `device-service-core/src/hand-device-keys.ts` (`readDeviceServiceStatus`): connects to
//! the local device service, reads its `Status`, and closes without installing or mutating it.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::Value;
use tokio::sync::watch;

use crate::local_api::protocol::{ClientInfo, Status};
use crate::local_api::transport::is_service_not_running;
use crate::local_api::{ConnectOptions, LocalApiClient, LocalApiError, connect_local_api};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Cancellation half matching `AbortSignal` for [`read_device_service_status`].
#[derive(Clone, Debug)]
pub struct CancelSignal {
    rx: watch::Receiver<bool>,
}

impl CancelSignal {
    pub fn aborted() -> Self {
        let (tx, rx) = watch::channel(true);
        drop(tx);
        Self { rx }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    pub async fn cancelled(&mut self) {
        loop {
            if *self.rx.borrow() {
                return;
            }
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Controller half matching `AbortController` for [`CancelSignal`].
#[derive(Debug)]
pub struct CancelController {
    tx: watch::Sender<bool>,
}

impl Default for CancelController {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelController {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        drop(rx);
        Self { tx }
    }

    pub fn signal(&self) -> CancelSignal {
        CancelSignal {
            rx: self.tx.subscribe(),
        }
    }

    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}

/// Result of reading the local device service's status without starting or modifying it.
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceServiceStatus {
    Status(Box<Status>),
    NotRunning,
    NoAnswer,
}

/// The connection interface [`read_device_service_status_with`] drives (implemented by
/// [`LocalApiClient`] and test doubles).
pub trait StatusClient: Send + Sync + 'static {
    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>>;
    fn close(&self) -> BoxFuture<'_, ()>;
}

impl StatusClient for LocalApiClient {
    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        Box::pin(LocalApiClient::status(self))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(LocalApiClient::close(self))
    }
}

pub fn device_service_endpoint_path(state_dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        state_dir.join(crate::local_api::access::SOCKET_FILE)
    }
    #[cfg(not(unix))]
    {
        state_dir.join(crate::local_api::transport::ENDPOINT_FILE)
    }
}

enum ConnectFailureCause {
    DeadEndpoint,
    TimedOut,
    Other,
}

fn classify_failure(error: &LocalApiError) -> ConnectFailureCause {
    if is_service_not_running(error) {
        ConnectFailureCause::DeadEndpoint
    } else if error.is("handshake_timeout") || error.is("request_timeout") {
        ConnectFailureCause::TimedOut
    } else {
        ConnectFailureCause::Other
    }
}

async fn wait_signal(signal: &mut Option<CancelSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

fn status_of(value: Value) -> Result<DeviceServiceStatus, LocalApiError> {
    serde_json::from_value::<Status>(value)
        .map(Box::new)
        .map(DeviceServiceStatus::Status)
        .map_err(|error| {
            LocalApiError::protocol_violation(format!("invalid status payload: {error}"))
        })
}

enum Connected<C> {
    Aborted,
    Answered(Result<C, LocalApiError>),
}

/// One read of this machine's device service status: connects, reads `status.get`, and closes.
pub async fn read_device_service_status(
    state_dir: &Path,
    version: &str,
    signal: Option<CancelSignal>,
) -> Result<DeviceServiceStatus, LocalApiError> {
    let state_dir_owned = state_dir.to_path_buf();
    let version_owned = version.to_owned();
    read_device_service_status_with(state_dir, signal, move || {
        let state_dir = state_dir_owned.clone();
        let version = version_owned.clone();
        async move {
            connect_local_api(ConnectOptions::new(state_dir, ClientInfo::cli(version))).await
        }
    })
    .await
}

/// [`read_device_service_status`] with an injected connection builder for testing.
pub async fn read_device_service_status_with<C, F, Fut>(
    state_dir: &Path,
    mut signal: Option<CancelSignal>,
    connect: F,
) -> Result<DeviceServiceStatus, LocalApiError>
where
    C: StatusClient,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<C, LocalApiError>> + Send + 'static,
{
    if signal.as_ref().is_some_and(CancelSignal::is_cancelled) {
        return Ok(DeviceServiceStatus::NoAnswer);
    }
    if !device_service_endpoint_path(state_dir).exists() {
        return Ok(DeviceServiceStatus::NotRunning);
    }

    let mut connecting = tokio::spawn(connect());
    let connected = tokio::select! {
        biased;
        () = wait_signal(&mut signal) => Connected::Aborted,
        joined = &mut connecting => {
            let answer = match joined {
                Ok(answer) => answer,
                Err(join_error) => Err(LocalApiError::new("internal", join_error.to_string())),
            };
            Connected::Answered(answer)
        }
    };
    let client = match connected {
        Connected::Aborted => {
            tokio::spawn(async move {
                if let Ok(Ok(late)) = connecting.await {
                    late.close().await;
                }
            });
            return Ok(DeviceServiceStatus::NoAnswer);
        }
        Connected::Answered(Ok(client)) => client,
        Connected::Answered(Err(error)) => {
            return match classify_failure(&error) {
                ConnectFailureCause::DeadEndpoint => Ok(DeviceServiceStatus::NotRunning),
                ConnectFailureCause::TimedOut => Ok(DeviceServiceStatus::NoAnswer),
                ConnectFailureCause::Other => Err(error),
            };
        }
    };

    let outcome = {
        let mut reading = client.status();
        tokio::select! {
            biased;
            () = wait_signal(&mut signal) => Ok(DeviceServiceStatus::NoAnswer),
            answered = &mut reading => match answered {
                Ok(value) => status_of(value),
                Err(error) => match classify_failure(&error) {
                    ConnectFailureCause::TimedOut => Ok(DeviceServiceStatus::NoAnswer),
                    ConnectFailureCause::DeadEndpoint | ConnectFailureCause::Other => Err(error),
                },
            },
        }
    };
    client.close().await;
    outcome
}
