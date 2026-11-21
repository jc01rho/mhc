use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_service_core::local_api::{
    ClientInfo, ConnectOptions, LocalApiClient, connect_local_api,
};
use mhc_relayd::session_host::BoxFuture;
use mhc_relayd::{
    DeviceIdentity, HostCommandOutcome, HostSession, PeerConfig, Relayd, RelaydConfig,
    RelaydOptions, SessionHost,
};
use serde_json::{Value, json};

/// A host serving fixed sessions and recording every record it is handed.
#[derive(Default)]
pub struct FakeHost {
    pub sessions: Mutex<Vec<HostSession>>,
    pub records: Mutex<Vec<(String, Value)>>,
}

impl FakeHost {
    pub fn with_sessions(ids: &[&str]) -> Arc<Self> {
        let host = Self::default();
        *host.sessions.lock().expect("sessions") = ids
            .iter()
            .map(|id| HostSession {
                session_id: (*id).to_owned(),
                durable_id: Some(format!("durable-{id}")),
                title: Some(format!("title {id}")),
                project: Some("/work/project".into()),
                path: Some(format!("/work/sessions/{id}.jsonl")),
                state: "idle".into(),
                updated_ms: Some(1_000),
                kind: Some("interactive".into()),
                model: None,
            })
            .collect();
        Arc::new(host)
    }

    pub fn records(&self) -> Vec<(String, Value)> {
        self.records.lock().expect("records").clone()
    }
}

impl SessionHost for FakeHost {
    fn list_sessions<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
    ) -> BoxFuture<'a, Result<Vec<HostSession>, String>> {
        Box::pin(async move { Ok(self.sessions.lock().expect("sessions").clone()) })
    }

    fn command<'a>(
        &'a self,
        _agent_dirs: &'a [PathBuf],
        session_id: &'a str,
        record: &'a Value,
    ) -> BoxFuture<'a, HostCommandOutcome> {
        Box::pin(async move {
            let listed = self
                .sessions
                .lock()
                .expect("sessions")
                .iter()
                .any(|session| session.is_named(session_id));
            if !listed {
                return HostCommandOutcome::NotListed;
            }
            self.records
                .lock()
                .expect("records")
                .push((session_id.to_owned(), record.clone()));
            HostCommandOutcome::Applied(json!({ "disposition": "started" }))
        })
    }
}

pub struct Device {
    pub _dir: tempfile::TempDir,
    pub state_dir: PathBuf,
    pub identity: DeviceIdentity,
}

impl Device {
    pub fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("ds");
        let identity = DeviceIdentity::generate(name).expect("identity");
        Self {
            _dir: dir,
            state_dir,
            identity,
        }
    }

    pub fn as_peer(&self, address: &str) -> PeerConfig {
        PeerConfig {
            name: self.identity.name().to_owned(),
            address: address.to_owned(),
            public_key: self.identity.public_key(),
        }
    }

    pub async fn start(&self, config: RelaydConfig, host: Arc<FakeHost>) -> Relayd {
        Relayd::start(RelaydOptions {
            state_dir: self.state_dir.clone(),
            config,
            identity: self.identity.clone(),
            host,
            service_version: "0.0.0-test".into(),
        })
        .await
        .expect("relayd starts")
    }
}

pub async fn client(state_dir: &Path) -> LocalApiClient {
    let mut options = ConnectOptions::new(state_dir, ClientInfo::cli("0.0.0"));
    options.request_timeout = Duration::from_secs(20);
    connect_local_api(options).await.expect("local api client")
}

pub fn listening_config(peers: Vec<PeerConfig>) -> RelaydConfig {
    RelaydConfig {
        account: "home".into(),
        name: None,
        listen: Some("127.0.0.1:0".into()),
        peers,
    }
}
