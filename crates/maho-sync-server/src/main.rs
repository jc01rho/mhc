//! `mhc-sync-server`: serves one self-hosted memory-sync account store on a loopback address by
//! default. `--tokens` is a file of `token account device-id sync-principal` lines; `--db` is the
//! SQLite file that holds every account's ciphertext rows. Nothing is uploaded or reported anywhere.

use std::net::SocketAddr;
use std::path::PathBuf;

use maho_sync_server::{ServerState, Tokens, router};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut listen = "127.0.0.1:8787".to_string();
    let mut db: Option<PathBuf> = None;
    let mut tokens: Option<PathBuf> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => listen = arguments.next().ok_or("--listen needs an address")?,
            "--db" => db = Some(arguments.next().ok_or("--db needs a path")?.into()),
            "--tokens" => tokens = Some(arguments.next().ok_or("--tokens needs a path")?.into()),
            "--help" | "-h" => {
                println!(
                    "usage: mhc-sync-server [--listen 127.0.0.1:8787] [--db FILE] [--tokens FILE]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other}").into()),
        }
    }
    let tokens = match tokens {
        Some(path) => Tokens::load(&path)?,
        None => return Err("--tokens FILE is required".into()),
    };
    let state = match db {
        Some(path) => ServerState::open(&path, tokens)?,
        None => ServerState::in_memory(tokens)?,
    };
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    let address: SocketAddr = listener.local_addr()?;
    println!("mhc-sync-server listening on {address}");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
