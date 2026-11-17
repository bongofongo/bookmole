//! `bookmoled`: serves `bookmole_api::Api::call` on a unix socket as newline-delimited JSON.
//! Transport only; features belong in the API so in-process hosts get them too.

#[cfg(not(unix))]
compile_error!("bookmoled is a unix-socket daemon; there is no Windows transport yet");

mod server;

use std::path::PathBuf;
use std::sync::Arc;

use bookmole::{Engine, EngineConfig};
use bookmole_api::Api;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "bookmoled",
    about = "bookmole's local API daemon (unix socket, newline-delimited JSON)"
)]
struct Cli {
    /// Data root (database/, vault/, logs/). Defaults to BOOKMOLE_DATA_DIR, else the platform data dir.
    // Not clap's `env =`: the shared resolver reads the variable, so empty means unset everywhere.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Where to listen. Defaults to `<data-dir>/bookmoled.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Google Books API key. The CLI's config file is not read.
    #[arg(long, env = "GOOGLE_BOOKS_API_KEY")]
    google_api_key: Option<String>,

    /// Log filter, e.g. `bookmoled=debug,bookmole=info`.
    #[arg(long, env = "RUST_LOG", default_value = "bookmoled=info")]
    log: String,

    /// Also answer paired readers on the LAN, for this daemon's whole life.
    #[arg(long)]
    listen: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&cli.log))
        .with_writer(std::io::stderr)
        .init();

    let data_dir = bookmole::data_dir::resolve(cli.data_dir.as_deref())?;
    let mut config = EngineConfig::rooted_at(&data_dir);
    if cli.google_api_key.is_some() {
        config.google_api_key = cli.google_api_key.clone();
    }
    let socket = cli
        .socket
        .clone()
        .unwrap_or_else(|| data_dir.join("bookmoled.sock"));

    let engine = Engine::open(config).await?;
    let engine = Arc::new(engine);

    // Vault reconcile and watch failures only warn: the daemon still serves without them.
    if let Err(e) = engine.reconcile_vault().await {
        tracing::warn!(error = %e, "could not reconcile the vault");
    }
    match engine.watch_vault() {
        Ok(mut watcher) => {
            tokio::spawn(async move { while watcher.next().await.is_some() {} });
        }
        Err(e) => tracing::warn!(error = %e, "not watching the vault"),
    }

    let api = Api::new(engine);

    // Before `bind`, so `--listen` is answering once the socket is up; failure only warns.
    if cli.listen {
        match api.start_listening(Some(0)).await {
            Ok(status) => tracing::info!(
                tcp_port = ?status.tcp_port,
                "answering paired readers on the LAN"
            ),
            Err(e) => tracing::warn!(error = %e, "not answering readers on the LAN"),
        }
    }

    let listener = server::bind(&socket).await?;
    // After `bind`, so a failed start never deletes another daemon's socket.
    let _guard = server::SocketGuard(socket.clone());
    tracing::info!(socket = %socket.display(), api = bookmole_api::API_VERSION, "listening");

    server::serve(api, listener, shutdown()).await
}

/// Resolves on Ctrl-C or SIGTERM; both are needed or the socket is left behind.
async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            // Never resolve here, or the daemon would stop at once; Ctrl-C still works.
            Err(e) => {
                tracing::warn!(error = %e, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
