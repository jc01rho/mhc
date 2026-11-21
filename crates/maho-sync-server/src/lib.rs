//! An open, self-hostable memory-sync server for mhc.
//!
//! The hosted omo service is closed, so this crate derives the server from the *client* the
//! repository already ships: `memory_core::sync` reads `GET /v1/sync/state`, pages
//! `GET /v1/sync/batches?after=n`, downloads `GET /v1/sync/objects/{id}`, reserves and fills
//! uploads, appends at `POST /v1/sync/batches`, keeps a keyring through `/v1/sync/keys` and
//! `/v1/sync/entries`, and follows `GET /v1/me/events` for its trigger stream. Every route that
//! client calls is answered here.
//!
//! The server is a dumb ciphertext log. It stores batch objects opaquely, orders them per account,
//! checks `base_seq`, honours idempotency, and never holds a key: sealing, signatures and the
//! chain all happen on the device. What it does enforce is the log's own shape: one sequence per
//! account, an 8 MiB object cap, a digest that matches the bytes, and a bearer token that decides
//! which account (and which device) a request belongs to.
//!
//! ```no_run
//! # async fn run() -> std::io::Result<()> {
//! let config = maho_sync_server::Config::new("sync.db", "tokens.json");
//! let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 8787));
//! let server = maho_sync_server::Server::bind(config, addr).await?;
//! server.serve().await
//! # }
//! ```

pub mod auth;
pub mod canonical;
pub mod http;
pub mod store;
pub mod wire;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use tokio::net::TcpListener;

pub use auth::{Principal, TokenTable};
pub use http::{AppState, router};
pub use store::Store;

/// Where the server keeps its state and who may talk to it.
#[derive(Debug, Clone)]
pub struct Config {
    /// The SQLite file. `:memory:` keeps everything in this process.
    pub db: PathBuf,
    /// The token file: one entry per device, plus any admin token.
    pub tokens: PathBuf,
}

impl Config {
    pub fn new(db: impl Into<PathBuf>, tokens: impl Into<PathBuf>) -> Self {
        Self {
            db: db.into(),
            tokens: tokens.into(),
        }
    }

    /// An in-process server with a fixed token table; what tests use.
    pub fn in_memory() -> Self {
        Self {
            db: PathBuf::from(":memory:"),
            tokens: PathBuf::new(),
        }
    }
}

/// A bound server: the listener is open, the database is migrated, the tokens are registered.
pub struct Server {
    state: std::sync::Arc<AppState>,
    listener: TcpListener,
}

impl Server {
    /// Binds `addr` and prepares the state behind it.
    pub async fn bind(config: Config, addr: SocketAddr) -> std::io::Result<Self> {
        let store = Store::open(&config.db).map_err(std::io::Error::other)?;
        let tokens = if config.tokens.as_os_str().is_empty() {
            TokenTable::empty()
        } else {
            TokenTable::load(&config.tokens).map_err(std::io::Error::other)?
        };
        for principal in tokens.principals() {
            store
                .ensure_device(
                    &principal.account,
                    &principal.device,
                    &principal.sync_principal,
                )
                .map_err(std::io::Error::other)?;
        }
        let listener = TcpListener::bind(addr).await?;
        Ok(Self {
            state: std::sync::Arc::new(AppState::new(store, tokens)),
            listener,
        })
    }

    /// The address actually bound; a `:0` port resolves here.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// `http://127.0.0.1:PORT` for the bound listener.
    pub fn base_url(&self) -> std::io::Result<String> {
        Ok(format!("http://{}", self.local_addr()?))
    }

    /// Serves until the listener fails or the task is aborted.
    pub async fn serve(self) -> std::io::Result<()> {
        let Self { state, listener } = self;
        axum::serve(listener, router(state)).await
    }

    /// Serves on a background task; abort the handle to stop it.
    pub fn spawn(self) -> tokio::task::JoinHandle<std::io::Result<()>> {
        tokio::spawn(self.serve())
    }
}

/// The largest object the server accepts, in bytes.
pub const MAX_OBJECT_BYTES: usize = 8 * 1024 * 1024;

/// True when `path` names the in-memory database rather than a file.
pub fn is_memory_db(path: &Path) -> bool {
    path == Path::new(":memory:")
}
