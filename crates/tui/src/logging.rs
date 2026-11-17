//! Log subscriber for the TUI: always a file, since stdout is the ratatui backend and stderr
//! scribbles across the pane.

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Must outlive `restore_terminal()`: dropping it flushes, and dropping early truncates the log
/// tail.
pub struct LogHandle {
    _guard: WorkerGuard,
    pub path: PathBuf,
}

/// Returns `None` if the directory cannot be created; a missing log never blocks startup.
pub fn init(log_dir: &Path, level: Option<&str>) -> Option<LogHandle> {
    if let Err(e) = std::fs::create_dir_all(log_dir) {
        eprintln!(
            "warning: could not create log dir {}: {e}",
            log_dir.display()
        );
        return None;
    }
    let path = log_dir.join("tui.log");

    let directive = level.unwrap_or("bookmole=info,bookmole_tui=info");
    let filter = std::env::var("BOOKMOLE_LOG")
        .ok()
        .map(EnvFilter::new)
        .or_else(|| EnvFilter::try_from_default_env().ok())
        .unwrap_or_else(|| EnvFilter::new(directive));

    let appender = tracing_appender::rolling::never(log_dir, "tui.log");
    // Non-blocking, so events are dropped when the queue fills: absence from the log proves nothing.
    let (writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();

    Some(LogHandle {
        _guard: guard,
        path,
    })
}
