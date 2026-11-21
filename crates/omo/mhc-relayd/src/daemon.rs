//! Starting and stopping the service: the per-boot token, the local socket, the peer listener.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::RelaydConfig;
use crate::identity::DeviceIdentity;
use crate::session_host::SessionHost;
use crate::state::RelaydState;

pub struct RelaydOptions {
    pub state_dir: PathBuf,
    pub config: RelaydConfig,
    pub identity: DeviceIdentity,
    pub host: Arc<dyn SessionHost>,
    pub service_version: String,
}

/// A running service; [`Relayd::shutdown`] stops it.
pub struct Relayd {
    state: Arc<RelaydState>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    peer_address: Option<SocketAddr>,
}

/// Whether a device service (this one or another, omo's included) already answers on `state_dir`.
pub fn service_answers(state_dir: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(
            state_dir.join(device_service_core::local_api::access::SOCKET_FILE),
        )
        .is_ok()
    }
    #[cfg(not(unix))]
    {
        let _ = state_dir;
        false
    }
}

async fn peer_accept_loop(
    state: Arc<RelaydState>,
    listener: TcpListener,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(crate::peer::serve_peer(Arc::clone(&state), stream));
                }
                Err(_) => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

impl Relayd {
    /// Writes a new per-boot token, binds the local socket and (when configured) the peer listener.
    pub async fn start(options: RelaydOptions) -> Result<Self, String> {
        options.config.validate()?;
        #[cfg(unix)]
        {
            use device_service_core::local_api::access::{ensure_private_dir, write_token};
            ensure_private_dir(&options.state_dir).map_err(|error| error.to_string())?;
            // Before the token is rewritten: a running service's clients must keep proving its token.
            if service_answers(&options.state_dir) {
                return Err("another device service already answers on this state dir".into());
            }
            let token = write_token(&options.state_dir).map_err(|error| error.to_string())?;
            let local = crate::local_server::bind_local(&options.state_dir)
                .map_err(|error| error.to_string())?;
            let state = Arc::new(RelaydState::new(
                options.identity,
                options.config,
                options.state_dir,
                options.service_version,
                options.host,
                token,
            ));
            let (shutdown, _) = watch::channel(false);
            let mut tasks = vec![tokio::spawn(crate::local_server::accept_loop(
                Arc::clone(&state),
                local,
                shutdown.subscribe(),
            ))];
            let mut peer_address = None;
            if let Some(listen) = state.config.listen.clone() {
                let listener = TcpListener::bind(listen.trim())
                    .await
                    .map_err(|error| format!("cannot listen on {listen}: {error}"))?;
                peer_address = listener.local_addr().ok();
                tasks.push(tokio::spawn(peer_accept_loop(
                    Arc::clone(&state),
                    listener,
                    shutdown.subscribe(),
                )));
            }
            Ok(Self {
                state,
                shutdown,
                tasks,
                peer_address,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (peer_accept_loop, options);
            Err("mhc-relayd serves its local API on a Unix socket; this platform is not supported yet".into())
        }
    }

    pub fn state(&self) -> &Arc<RelaydState> {
        &self.state
    }

    /// The address the peer listener is bound to (the port when `listen` asked for port 0).
    pub fn peer_address(&self) -> Option<SocketAddr> {
        self.peer_address
    }

    /// Stops accepting, ends the listeners and removes the socket.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        for task in self.tasks {
            let _ = task.await;
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(
                self.state
                    .state_dir
                    .join(device_service_core::local_api::access::SOCKET_FILE),
            );
        }
    }
}
