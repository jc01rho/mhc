//! What a verb runs against: the device service's state dir, this agent dir, output, and the unit host.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use device_service_core::cold_wake::{ColdWakeHost, ServicePlatform, SystemCommandRunner};
use device_service_core::local_api::{
    ClientInfo, ConnectOptions, LocalApiClient, LocalApiError, REQUEST_TIMEOUT_MS,
    connect_local_api,
};

pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct RemoteContext {
    pub state_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub version: String,
    /// This mhc binary, which the units run.
    pub mhc_bin: Option<String>,
    pub output: LineSink,
    pub error: LineSink,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// The user's service manager; `None` where mhc installs no unit.
    pub cold_wake: Option<ColdWakeHost>,
    pub request_timeout: Duration,
}

/// `version` when it is `major.minor.patch[-pre]`, else `0.0.0`: the local API reads a semver.
pub fn semver_of(version: &str) -> String {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = parts
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    if parts.len() == 3 && numeric {
        version.to_owned()
    } else {
        "0.0.0".to_owned()
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        device_service_core::local_api::access::current_uid()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

impl RemoteContext {
    /// This process: the device service's state dir, stdout and stderr, the real service manager.
    pub fn for_process(agent_dir: PathBuf, version: &str) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let cold_wake = ServicePlatform::current().map(|platform| ColdWakeHost {
            platform,
            home,
            uid: current_uid(),
            runner: Arc::new(SystemCommandRunner),
            pause: Arc::new(std::thread::sleep),
        });
        Self {
            state_dir: device_service_core::device_service_state_dir(),
            agent_dir,
            version: version.to_owned(),
            mhc_bin: std::env::current_exe()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            output: Arc::new(|line| println!("{line}")),
            error: Arc::new(|line| eprintln!("{line}")),
            now: Arc::new(now_ms),
            cold_wake,
            request_timeout: Duration::from_millis(REQUEST_TIMEOUT_MS),
        }
    }

    pub fn out(&self, line: &str) {
        (self.output)(line);
    }

    pub fn err(&self, line: &str) {
        (self.error)(line);
    }

    pub fn now(&self) -> i64 {
        (self.now)()
    }

    pub fn client_info(&self) -> ClientInfo {
        ClientInfo::cli(semver_of(&self.version))
    }

    pub async fn connect(&self) -> Result<LocalApiClient, LocalApiError> {
        let mut options = ConnectOptions::new(&self.state_dir, self.client_info());
        options.request_timeout = self.request_timeout;
        connect_local_api(options).await
    }
}
