//! The stable error taxonomy: [`ApiError`] is a code plus a message, and the code is the promise.
//! `EngineError` is neither serializable nor a contract, so it never crosses the seam.

use serde::{Deserialize, Serialize};

use bookmole::{EngineError, ErrorClass};

/// What went wrong, coarsely. Codes are appended, never renamed; an unknown code reads as
/// `Internal`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    InvalidIsbn,
    /// A review's rating has no entry in the Goodreads lookup table; the fix is a mapping, no rounding.
    UnmappedRating,
    /// Calibre, or the half this feature needs, is not installed. Not a failure: show the feature as
    /// unavailable.
    CalibreMissing,
    CalibreFailed,
    Provider,
    Timeout,
    /// A provider refused us for quota reasons; the one class with its own UI text.
    RateLimited,
    Network,
    Io,
    Parse,
    Decode,
    Database,
    /// The platform has no filesystem-notification service to offer.
    Watch,
    /// bookmole declined to write to a mounted reader. A decision, not a fault: show the reader's
    /// state and the next move.
    PluginRefused,
    WirelessRefused,
    /// The request itself was malformed — bad JSON, or a method this build does
    /// not have. Produced by the transport, never by the engine.
    BadRequest,
    /// Anything else, and anything a client's build is too old to name.
    #[serde(other)]
    Internal,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    /// The engine's own `Display`, already scrubbed of API keys.
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(ErrorCode::BadRequest, message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        ApiError {
            code: code_for(&e),
            message: e.to_string(),
        }
    }
}

fn code_for(e: &EngineError) -> ErrorCode {
    match e {
        EngineError::NotFound(_) => ErrorCode::NotFound,
        EngineError::InvalidInput(_) => ErrorCode::InvalidInput,
        EngineError::InvalidIsbn(_) => ErrorCode::InvalidIsbn,
        EngineError::UnmappedRating { .. } => ErrorCode::UnmappedRating,
        EngineError::CalibreMissing { .. } => ErrorCode::CalibreMissing,
        EngineError::Calibre { .. } => ErrorCode::CalibreFailed,
        EngineError::Db(_) | EngineError::Migrate(_) => ErrorCode::Database,
        EngineError::Io(_) => ErrorCode::Io,
        EngineError::Epub(_) | EngineError::Pdf(_) | EngineError::Sidecar(_) => ErrorCode::Parse,
        EngineError::Watch(_) => ErrorCode::Watch,
        EngineError::PluginRefused(_) => ErrorCode::PluginRefused,
        EngineError::WirelessRefused(_) => ErrorCode::WirelessRefused,
        EngineError::Timeout { .. } => ErrorCode::Timeout,
        EngineError::Provider { .. } | EngineError::Http(_) => match ErrorClass::from(e) {
            ErrorClass::RateLimited => ErrorCode::RateLimited,
            ErrorClass::Timeout => ErrorCode::Timeout,
            ErrorClass::Decode => ErrorCode::Decode,
            ErrorClass::Network => ErrorCode::Network,
            _ => ErrorCode::Provider,
        },
        EngineError::Json(_) => ErrorCode::Decode,
        EngineError::Url(_) | EngineError::Format(_) | EngineError::Other(_) => ErrorCode::Internal,
        EngineError::Sync(_) => ErrorCode::Internal,
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variants_a_caller_branches_on_get_their_own_code() {
        assert_eq!(
            ApiError::from(EngineError::NotFound("book id 3".into())).code,
            ErrorCode::NotFound
        );
        assert_eq!(
            ApiError::from(EngineError::CalibreMissing {
                tool: "calibredb".into()
            })
            .code,
            ErrorCode::CalibreMissing
        );
        assert_eq!(
            ApiError::from(EngineError::UnmappedRating {
                value: 4.5,
                scale: "default".into()
            })
            .code,
            ErrorCode::UnmappedRating
        );
    }

    #[test]
    fn the_message_is_the_engines_own_display() {
        let e = EngineError::InvalidIsbn("978-oops".into());
        let want = e.to_string();
        assert_eq!(ApiError::from(e).message, want);
    }

    #[test]
    fn rate_limiting_survives_the_translation() {
        let e = EngineError::Provider {
            provider: bookmole::ProviderId::GoogleBooks,
            message: "HTTP 429 rate limit".into(),
        };
        assert_eq!(ApiError::from(e).code, ErrorCode::RateLimited);
    }

    #[test]
    fn an_unknown_code_degrades_instead_of_failing_to_parse() {
        let raw = r#"{"code":"invented_in_a_later_version","message":"boom"}"#;
        let e: ApiError = serde_json::from_str(raw).unwrap();
        assert_eq!(e.code, ErrorCode::Internal);
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn a_key_never_survives_into_an_api_error() {
        // Pins that translating an error does not undo the provider's key scrubbing.
        let e = EngineError::Provider {
            provider: bookmole::ProviderId::GoogleBooks,
            message: bookmole::providers::googlebooks::scrub_key(
                "https://www.googleapis.com/books/v1/volumes?q=x&key=SUPERSECRET failed",
            ),
        };
        assert!(!ApiError::from(e).message.contains("SUPERSECRET"));
    }
}
