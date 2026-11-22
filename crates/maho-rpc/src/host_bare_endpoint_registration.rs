use crate::{
    host_daemon_paths::{create_daemon_directories, create_host_daemon_paths},
    host_ensure_lock::acquire_host_ensure_lock,
};
use std::path::Path;

const REGISTRATION_LOCK_WAIT_MS: u64 = 10_000;

pub struct BareEndpointRegistration<'a> {
    pub agent_dir: &'a Path,
    pub socket: &'a str,
    pub log: &'a (dyn Fn(&str) + Send + Sync),
}

pub async fn bind_registered_endpoint<T, F>(
    registration: BareEndpointRegistration<'_>,
    bind: F,
) -> T
where
    F: FnOnce() -> T,
{
    if registration.socket.starts_with('\0') {
        return bind();
    }
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let mut lock =
        match acquire_host_ensure_lock(registration.socket, REGISTRATION_LOCK_WAIT_MS, platform)
            .await
        {
            Ok(lock) => Some(lock),
            Err(cause) => {
                (registration.log)(&format!(
                    "endpoint ensure lock unavailable, registering without it: {cause}"
                ));
                None
            }
        };
    if let Err(cause) = create_daemon_directories(&create_host_daemon_paths(
        registration.socket,
        registration.agent_dir,
    )) {
        (registration.log)(&format!("endpoint registry record not written: {cause}"));
    }
    let bound = bind();
    if let Some(lock) = &mut lock {
        let _ = lock.release();
    }
    bound
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_endpoints::{EndpointKind, list_host_endpoints};

    #[tokio::test]
    async fn bind_registered_endpoint_writes_endpoint_record_before_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let socket = dir.path().join("bare.sock").to_string_lossy().into_owned();
        let paths = create_host_daemon_paths(&socket, &agent_dir);

        let recorded_during_bind = bind_registered_endpoint(
            BareEndpointRegistration {
                agent_dir: &agent_dir,
                socket: &socket,
                log: &|_| {},
            },
            || paths.endpoint_file.exists(),
        )
        .await;

        assert!(recorded_during_bind);
        let endpoints = list_host_endpoints(&agent_dir).expect("list endpoints");
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].socket, socket);
        assert_eq!(endpoints[0].endpoint_kind, EndpointKind::RpcHost);
    }

    #[tokio::test]
    async fn abstract_socket_binds_without_writing_daemon_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let bound = bind_registered_endpoint(
            BareEndpointRegistration {
                agent_dir: &agent_dir,
                socket: "\0abstract-sock",
                log: &|_| {},
            },
            || 42,
        )
        .await;
        assert_eq!(bound, 42);
        assert!(!agent_dir.exists());
    }
}
