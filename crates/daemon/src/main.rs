//! `bookmoled` — the transport wrapper.
//!
//! bookmole's boundary is the API, not the process: `bookmole-api`
//! holds the whole versioned surface, and this binary is a thin transport
//! wrapper with no logic. It is deliberately the least interesting crate in the
//! workspace: it opens an engine, binds a socket, and hands every line it reads
//! to [`bookmole_api::Api::call`]. **If a feature is ever added here, it is
//! in the wrong crate** — a host that links the API in-process (an iOS app has
//! no daemons) would not get it.
//!
//! ```text
//! bookmoled --data-dir ~/reading
//! printf '{"id":1,"request":{"method":"list_books","params":{"limit":5}}}\n' \
//!   | nc -U ~/reading/bookmoled.sock
//! ```
//!
//! Unix only, and that is the scope rather than a gap: the daemon exists for a
//! desktop app and a menu-bar companion on the user's own machine, and the
//! platforms `device.rs` knows how to find a reader on are macOS and Linux.

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
    /// Data root: database/, vault/, logs/. Same meaning and same default as
    /// the CLI's: BOOKMOLE_DATA_DIR, else the platform data dir
    /// (~/Library/Application Support/bookmole on macOS,
    /// $XDG_DATA_HOME/bookmole or ~/.local/share/bookmole on Linux).
    // Not clap's `env =`: the variable is read by the one resolver every
    // frontend shares, so "set but empty" means the same thing everywhere.
    #[arg(long)]
    data_dir: Option<PathBuf>,

    /// Where to listen. Defaults to `<data-dir>/bookmoled.sock`, so a
    /// sandbox run with its own `--data-dir` gets its own socket for free and
    /// cannot collide with the real one.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Google Books API key. The engine takes it from `GOOGLE_BOOKS_API_KEY`
    /// too; the daemon deliberately does **not** read the CLI's config file,
    /// which is the CLI's to own.
    #[arg(long, env = "GOOGLE_BOOKS_API_KEY")]
    google_api_key: Option<String>,

    /// Log filter, e.g. `bookmoled=debug,bookmole=info`.
    #[arg(long, env = "RUST_LOG", default_value = "bookmoled=info")]
    log: String,

    /// Also answer paired readers on the LAN, for this daemon's whole life.
    ///
    /// **Off by default, and that is the feature rather than a caution.** With
    /// nothing bound there is no service to find, nothing to fingerprint and
    /// nothing to leak: the wireless link *fails closed* at the transport
    /// layer. See `server.rs` for why the argument against a listening TCP port
    /// gives way in exactly one clause.
    ///
    /// This is the flag form of `ListenerStatus`'s `Always`, for the desktop
    /// that lives on the LAN. It is **not** a second listener: it is one call
    /// to the ordinary `start_listening` request before `bind`, so a GUI with
    /// no daemon reaches the identical thing.
    #[arg(long)]
    listen: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // The engine emits and never subscribes; a frontend — and this is one —
    // decides where the events go. stderr, so `bookmoled > /dev/null` still
    // shows what went wrong and a supervisor can capture it.
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

    // The vault watcher is the one thing this binary drives that is not
    // transport, and it is *lifecycle*, not logic: a watcher is a long-lived
    // thing a host owns, and every host owns its own. Because the watcher does
    // the re-index itself, keeping a socket client's note searches correct costs
    // this binary one task and **no wire-protocol change at all**. Had the
    // write stayed on the frontend's side, this is where the protocol would
    // have had to grow server-initiated frames — every reply here carries the
    // id it answers, and a push has no id to carry.
    //
    // Once before listening, for the edits made while nothing was running; then
    // a task that owns the watcher and drives it, since polling it *is* the
    // work. Both degrade to nothing: a daemon that cannot watch a vault is
    // still a daemon, and refusing to start over one would turn an absent
    // feature into a broken program — absence should only ever mean the feature
    // is not there.
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

    // Before `bind`, so a daemon started with `--listen` is answering readers
    // by the time it says it is listening. Degrades to a warning for the vault
    // watcher's reason: a daemon that could not take the rendezvous port — most
    // likely because another bookmole already has it — is still a daemon,
    // and refusing to start over it would make the unix socket hostage to a
    // feature that is off by default.
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
    // After `bind`, so a refusal to start does not delete the socket of the
    // daemon that was already there.
    let _guard = server::SocketGuard(socket.clone());
    tracing::info!(socket = %socket.display(), api = bookmole_api::API_VERSION, "listening");

    server::serve(api, listener, shutdown()).await
}

/// Ctrl-C or SIGTERM. Both, because a supervisor sends the second and a
/// terminal sends the first, and a daemon that only handles one of them leaves
/// its socket behind half the time.
async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            // No SIGTERM handler is not a reason to refuse to run; ctrl-C still
            // works and the guard still removes the socket.
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
