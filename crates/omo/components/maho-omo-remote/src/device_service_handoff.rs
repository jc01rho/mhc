//! Port of `packages/omo-native/device-service-handoff.ts` and
//! `packages/device-service-core/src/hand-device-keys.ts` (`connectOrNotRunning`, `handDeviceKeys`).

use std::cmp::Ordering;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use device_service_core::hand_device_keys::device_service_endpoint_path;
use device_service_core::local_api::transport::is_service_not_running;
use device_service_core::local_api::{
    ClientInfo, ConnectOptions, LocalApiClient, LocalApiError, connect_local_api,
};
use device_service_core::release::version::compare_versions;
use serde_json::{Value, json};

use crate::account_store::{BoxFuture, CredentialDevice, Credentials};
use crate::context::{LineSink, semver_of};

pub const ADOPT_ACCOUNT_MIN_SERVICE_VERSION: &str = "0.1.3";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldDeviceCredential {
    pub device: CredentialDevice,
    pub signing_private_key: String,
    pub sealing_private_key: String,
    pub account: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandWhen {
    Always,
    NotAdopted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandOutcome {
    Adopted,
    AlreadyAdopted,
    NotRunning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MachineDeviceState {
    Absent,
    Held {
        device: CredentialDevice,
    },
    Unknown {
        reason: String,
        endpoint: Option<PathBuf>,
    },
}

pub trait HandoffClient: Send + Sync + 'static {
    fn service_version(&self) -> &str;
    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>>;
    fn adopt(
        &self,
        credential: &HeldDeviceCredential,
    ) -> BoxFuture<'_, Result<Value, LocalApiError>>;
    fn forget(&self) -> BoxFuture<'_, Result<(), LocalApiError>>;
    fn close(&self) -> BoxFuture<'_, ()>;
}

pub fn accepts_adopt_account(service_version: &str) -> bool {
    compare_versions(service_version, ADOPT_ACCOUNT_MIN_SERVICE_VERSION)
        .is_ok_and(|order| order >= Ordering::Equal)
}

impl HandoffClient for LocalApiClient {
    fn service_version(&self) -> &str {
        LocalApiClient::service_version(self)
    }

    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        Box::pin(LocalApiClient::status(self))
    }

    fn adopt(
        &self,
        credential: &HeldDeviceCredential,
    ) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        let mut params = json!({
            "device": { "id": credential.device.id, "name": credential.device.name },
            "signing_private_key": credential.signing_private_key,
            "sealing_private_key": credential.sealing_private_key,
        });
        if !credential.account.is_empty() && accepts_adopt_account(self.service_version()) {
            params["account"] = Value::String(credential.account.clone());
        }
        Box::pin(async move { self.request("device.adopt", params).await })
    }

    fn forget(&self) -> BoxFuture<'_, Result<(), LocalApiError>> {
        Box::pin(LocalApiClient::forget_device(self))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(LocalApiClient::close(self))
    }
}

fn classify_connect_failure(error: &LocalApiError) -> &'static str {
    if is_service_not_running(error) {
        "dead_endpoint"
    } else if error.is("refused") || error.is("server_proof_invalid") {
        "refused"
    } else if error.is("insecure_state_dir") {
        "unreadable"
    } else if error.is("handshake_timeout") || error.is("request_timeout") {
        "timed_out"
    } else {
        "failed"
    }
}

pub async fn connect_or_not_running<C, F, Fut>(connect: F, log: Option<&dyn Fn(&str)>) -> Option<C>
where
    C: HandoffClient,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<C, LocalApiError>>,
{
    match connect().await {
        Ok(client) => Some(client),
        Err(error) => {
            let cause = classify_connect_failure(&error);
            if let Some(log) = log {
                log(&format!(
                    "OmO device service: {cause}, treated as not running ({}).",
                    error.reason
                ));
            }
            None
        }
    }
}

fn is_adopted(status: &Value) -> bool {
    status.get("device").is_some_and(|device| !device.is_null())
        && status.get("state").and_then(Value::as_str) != Some("sign_in_required")
}

pub async fn hand_device_keys<C, F, Fut>(
    state_dir: &Path,
    connect: F,
    credential: &HeldDeviceCredential,
    when: HandWhen,
    log: Option<&dyn Fn(&str)>,
) -> Result<HandOutcome, LocalApiError>
where
    C: HandoffClient,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<C, LocalApiError>>,
{
    if !device_service_endpoint_path(state_dir).exists() {
        return Ok(HandOutcome::NotRunning);
    }
    let Some(service) = connect_or_not_running(connect, log).await else {
        return Ok(HandOutcome::NotRunning);
    };
    let result = async {
        if when == HandWhen::NotAdopted && is_adopted(&service.status().await?) {
            return Ok(HandOutcome::AlreadyAdopted);
        }
        let _ = service.adopt(credential).await?;
        Ok(HandOutcome::Adopted)
    }
    .await;
    service.close().await;
    result
}

pub async fn forget_device_service<C, F, Fut>(
    state_dir: &Path,
    connect: F,
    log: Option<&dyn Fn(&str)>,
) -> Result<(), LocalApiError>
where
    C: HandoffClient,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<C, LocalApiError>>,
{
    if !device_service_endpoint_path(state_dir).exists() {
        return Ok(());
    }
    let Some(service) = connect_or_not_running(connect, log).await else {
        return Ok(());
    };
    let result = service.forget().await;
    service.close().await;
    result
}

pub trait DeviceServiceHandoff: Send + Sync {
    fn adopt<'a>(&'a self, credentials: &'a Credentials) -> BoxFuture<'a, ()>;
    fn forget(&self) -> BoxFuture<'_, ()>;
    fn machine_device(&self) -> BoxFuture<'_, MachineDeviceState>;
}

pub type AccountFetcher =
    Arc<dyn Fn(&str, &str) -> BoxFuture<'static, Result<String, String>> + Send + Sync>;
pub type HandoffConnector = Arc<
    dyn Fn() -> BoxFuture<'static, Result<Box<dyn HandoffClient>, LocalApiError>> + Send + Sync,
>;

impl HandoffClient for Box<dyn HandoffClient> {
    fn service_version(&self) -> &str {
        (**self).service_version()
    }
    fn status(&self) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        (**self).status()
    }
    fn adopt(
        &self,
        credential: &HeldDeviceCredential,
    ) -> BoxFuture<'_, Result<Value, LocalApiError>> {
        (**self).adopt(credential)
    }
    fn forget(&self) -> BoxFuture<'_, Result<(), LocalApiError>> {
        (**self).forget()
    }
    fn close(&self) -> BoxFuture<'_, ()> {
        (**self).close()
    }
}

pub struct CliDeviceServiceHandoff {
    pub api: String,
    pub state_dir: PathBuf,
    pub log: LineSink,
    pub connect: HandoffConnector,
    pub fetch_account: AccountFetcher,
}

impl CliDeviceServiceHandoff {
    pub fn for_state_dir(
        api: impl Into<String>,
        state_dir: PathBuf,
        version: &str,
        log: LineSink,
    ) -> Self {
        let semver = semver_of(version);
        let state_for_connect = state_dir.clone();
        let connect: HandoffConnector = Arc::new(move || {
            let dir = state_for_connect.clone();
            let ver = semver.clone();
            Box::pin(async move {
                let client =
                    connect_local_api(ConnectOptions::new(dir, ClientInfo::cli(ver))).await?;
                Ok(Box::new(client) as Box<dyn HandoffClient>)
            })
        });
        let fetch_account: AccountFetcher = Arc::new(|_api, _token| {
            Box::pin(async {
                maho_service_endpoints::refuse_if_local_only(
                    "omo-native/device-service-handoff.ts accountOf",
                )
                .map_err(|error| error.to_string())?;
                Err("the account read named no account".into())
            })
        });
        Self {
            api: api.into(),
            state_dir,
            log,
            connect,
            fetch_account,
        }
    }

    fn report(&self, what: &str, detail: &str) {
        (self.log)(&format!(
            "OmO device service: {what} did not complete ({detail})."
        ));
    }
}

impl DeviceServiceHandoff for CliDeviceServiceHandoff {
    fn adopt<'a>(&'a self, credentials: &'a Credentials) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if !device_service_endpoint_path(&self.state_dir).exists() {
                return;
            }
            let account = match credentials.effective_account_id() {
                Some(account) => account.to_owned(),
                None => match (self.fetch_account)(&self.api, &credentials.access_token).await {
                    Ok(account) if !account.is_empty() => account,
                    Ok(_) => {
                        self.report(
                            "handing this sign-in to it",
                            "the account read named no account",
                        );
                        return;
                    }
                    Err(error) => {
                        self.report("handing this sign-in to it", &error);
                        return;
                    }
                },
            };
            let held = HeldDeviceCredential {
                device: credentials.device.clone(),
                signing_private_key: credentials.signing_private_key.clone(),
                sealing_private_key: credentials.sealing_private_key.clone(),
                account,
            };
            let log = Arc::clone(&self.log);
            let connect = Arc::clone(&self.connect);
            if let Err(error) = hand_device_keys(
                &self.state_dir,
                move || connect(),
                &held,
                HandWhen::Always,
                Some(&move |line: &str| log(line)),
            )
            .await
            {
                self.report("handing this sign-in to it", &error.reason);
            }
        })
    }

    fn forget(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let log = Arc::clone(&self.log);
            let connect = Arc::clone(&self.connect);
            if let Err(error) = forget_device_service(
                &self.state_dir,
                move || connect(),
                Some(&move |line: &str| log(line)),
            )
            .await
            {
                self.report("forgetting this device", &error.reason);
            }
        })
    }

    fn machine_device(&self) -> BoxFuture<'_, MachineDeviceState> {
        Box::pin(async move {
            let endpoint = device_service_endpoint_path(&self.state_dir);
            if !endpoint.exists() {
                return MachineDeviceState::Absent;
            }
            let failure = std::sync::Mutex::new(None::<String>);
            let connect = Arc::clone(&self.connect);
            let service = connect_or_not_running(
                move || connect(),
                Some(&|line: &str| {
                    *failure
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(line.to_owned());
                }),
            )
            .await;
            let Some(service) = service else {
                let reason = failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .unwrap_or_else(|| {
                        "the device service endpoint is present but nothing answered".into()
                    });
                return MachineDeviceState::Unknown {
                    reason,
                    endpoint: Some(endpoint),
                };
            };
            let status = service.status().await;
            service.close().await;
            match status {
                Ok(status) => match status.get("device") {
                    None | Some(Value::Null) => MachineDeviceState::Absent,
                    Some(device) => MachineDeviceState::Held {
                        device: CredentialDevice {
                            id: device
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            name: device
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        },
                    },
                },
                Err(error) => MachineDeviceState::Unknown {
                    reason: error.reason,
                    endpoint: None,
                },
            }
        })
    }
}
