use std::time::Duration;

use crate::providers::ProviderId;

pub type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),
    #[error("timestamp format: {0}")]
    Format(#[from] time::error::Format),
    /// `message` is always scrubbed of API keys; build it via `googlebooks::scrubbed`.
    #[error("{provider} error: {message}")]
    Provider {
        provider: ProviderId,
        message: String,
    },
    #[error("{what} timed out after {}s", .after.as_secs())]
    Timeout { what: String, after: Duration },
    #[error("invalid ISBN: {0:?}")]
    InvalidIsbn(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("epub error: {0}")]
    Epub(String),
    /// The bytes are not a parseable PDF. A PDF that parses without a length is `PdfInfo { page_count:
    /// None, .. }` instead.
    #[error("pdf error: {0}")]
    Pdf(String),
    #[error("koreader sidecar error: {0}")]
    Sidecar(String),
    /// Calibre is feature-detected, so absence is an ordinary answer callers branch on, not a failure.
    #[error("calibre's `{tool}` is not installed")]
    CalibreMissing { tool: String },
    /// `message` is the tail of calibre's stderr, where the useful part is.
    #[error("calibre's `{tool}` failed: {message}")]
    Calibre { tool: String, message: String },
    #[error("not found: {0}")]
    NotFound(String),
    /// A rated review has no Goodreads mapping; callers must skip and say why, never round.
    #[error(
        "{value} on the '{scale}' scale has no Goodreads mapping (Goodreads takes 0–5 in quarters)"
    )]
    UnmappedRating { value: f64, scale: String },
    /// The platform cannot watch for mounts. Callers degrade; device screens and `ko scan`/`ko sync`
    /// still work.
    #[error("cannot watch for devices: {0}")]
    Watch(String),
    // `transparent`, not `"{0}"`: `#[from]` makes the refusal the source, so the same text would print
    // twice.
    #[error(transparent)]
    PluginRefused(#[from] crate::plugin::PluginRefusal),
    #[error(transparent)]
    WirelessRefused(#[from] crate::wireless::WirelessRefusal),
    #[error(transparent)]
    Sync(#[from] crate::sync::SyncError),
    #[error("{0}")]
    Other(String),
}
