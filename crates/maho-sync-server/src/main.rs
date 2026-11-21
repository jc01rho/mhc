//! `mhc-sync-server`: serve the self-hosted memory-sync log.

use std::net::SocketAddr;
use std::path::PathBuf;

use maho_sync_server::{Config, Server};

const USAGE: &str = "\
usage: mhc-sync-server [--listen ADDR] [--db PATH] [--tokens PATH]

  --listen ADDR   bind address (default 127.0.0.1:8787; never 0.0.0.0 unless asked)
  --db PATH       SQLite file (default mhc-sync-server.db)
  --tokens PATH   token file (default tokens.json)
  -h, --help      this text
";

struct Options {
    listen: String,
    db: PathBuf,
    tokens: PathBuf,
}

fn parse() -> Result<Options, String> {
    let mut options = Options {
        listen: "127.0.0.1:8787".into(),
        db: PathBuf::from("mhc-sync-server.db"),
        tokens: PathBuf::from("tokens.json"),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |name: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--listen" => options.listen = value("--listen")?,
            "--db" => options.db = PathBuf::from(value("--db")?),
            "--tokens" => options.tokens = PathBuf::from(value("--tokens")?),
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown argument: {other}\n{USAGE}")),
        }
    }
    Ok(options)
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let options = match parse() {
        Ok(options) => options,
        Err(message) => {
            if message.is_empty() {
                print!("{USAGE}");
                return Ok(());
            }
            eprint!("{message}");
            std::process::exit(2);
        }
    };
    let addr: SocketAddr = options.listen.parse().map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("--listen {}: {error}", options.listen),
        )
    })?;
    let config = Config::new(options.db.clone(), options.tokens.clone());
    let server = Server::bind(config, addr).await?;
    println!(
        "mhc-sync-server listening on {} (db {}, tokens {})",
        server.local_addr()?,
        options.db.display(),
        options.tokens.display()
    );
    tokio::select! {
        result = server.serve() => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
