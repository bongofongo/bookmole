//! The stable error taxonomy: [`ApiError`] is a code plus a message, and the code is the promise.
//! `EngineError` is neither serializable nor a contract, so it never crosses the seam.

use serde::{Deserialize, Serialize};

use bookmole::{EngineError, ErrorClass, SyncError};

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
    /// Sync is off for this library: `CreateSyncLibrary` or `JoinSyncLibrary` turns it on.
    SyncOff,
    /// This library already syncs: `LeaveSyncLibrary` first to create or join another.
    SyncAlreadyOn,
    /// The library syncs, but this device's sync identity is missing from the data directory.
    /// `LeaveSyncLibrary`, then `JoinSyncLibrary` with the recovery code, puts it right.
    SyncNeedsCredentials,
    /// What was given is not a recovery code; most likely mistyped.
    SyncCodeInvalid,
    /// The sync server could not be reached. Check the address and the connection, or try later.
    SyncUnreachable,
    /// The sync server does not accept this device or code for this library: it does not know
    /// the library, or no longer lists the device.
    SyncRefused,
    /// The sync server's limit on devices, storage or service for this library was reached.
    SyncLimitReached,
    /// This device's clock is too far from the sync server's; set it right and try again.
    SyncClockSkew,
    /// The server or the library was synced by a newer version: this one needs an update.
    SyncNeedsUpdate,
    /// Another writer held the library and sync gave way, it was stopped part-way, or another
    /// Bookmole on this data directory is syncing; what was done is kept, and trying again
    /// carries on.
    SyncBusy,
    /// The library needs an update and sync could not be paused for it, so it did not open.
    /// Closing the library's other windows and trying again may let it.
    SyncCannotPause,
    /// The address answers, but not as a sync server should.
    SyncServerFault,
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
        EngineError::Sync(e) => sync_code(e),
    }
}

fn sync_code(e: &SyncError) -> ErrorCode {
    match e {
        SyncError::Off => ErrorCode::SyncOff,
        SyncError::AlreadyOn => ErrorCode::SyncAlreadyOn,
        SyncError::NeedsCredentials => ErrorCode::SyncNeedsCredentials,
        SyncError::CodeInvalid => ErrorCode::SyncCodeInvalid,
        SyncError::Unreachable => ErrorCode::SyncUnreachable,
        SyncError::Refused { code, .. } => match code.as_str() {
            "unauthenticated" | "unknown_device" | "bad_signature" | "forbidden"
            | "library_not_found" | "invalid_proof" => ErrorCode::SyncRefused,
            "device_limit" | "library_full" | "quota_exceeded" | "not_entitled" => {
                ErrorCode::SyncLimitReached
            }
            "stale_request" => ErrorCode::SyncClockSkew,
            "replayed_request" => ErrorCode::SyncBusy,
            "internal" | "unknown" | "not_found" | "method_not_allowed" | "too_large" => {
                ErrorCode::SyncServerFault
            }
            _ => ErrorCode::Internal,
        },
        SyncError::ServerFault(_) => ErrorCode::SyncServerFault,
        SyncError::ClockSkew => ErrorCode::SyncClockSkew,
        SyncError::SchemaMismatch => ErrorCode::SyncNeedsUpdate,
        SyncError::Interrupted | SyncError::Busy => ErrorCode::SyncBusy,
        SyncError::CannotPause(_) => ErrorCode::SyncCannotPause,
        _ => ErrorCode::Internal,
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

    #[test]
    fn each_sync_failure_a_reader_can_act_on_has_its_own_code() {
        let refused = |code: &str| SyncError::Refused {
            status: 400,
            code: code.into(),
        };
        let cases = [
            (SyncError::Off, ErrorCode::SyncOff, "sync_off"),
            (
                SyncError::AlreadyOn,
                ErrorCode::SyncAlreadyOn,
                "sync_already_on",
            ),
            (
                SyncError::NeedsCredentials,
                ErrorCode::SyncNeedsCredentials,
                "sync_needs_credentials",
            ),
            (
                SyncError::CodeInvalid,
                ErrorCode::SyncCodeInvalid,
                "sync_code_invalid",
            ),
            (
                SyncError::Unreachable,
                ErrorCode::SyncUnreachable,
                "sync_unreachable",
            ),
            (
                refused("unknown_device"),
                ErrorCode::SyncRefused,
                "sync_refused",
            ),
            (
                refused("library_not_found"),
                ErrorCode::SyncRefused,
                "sync_refused",
            ),
            (
                refused("bad_signature"),
                ErrorCode::SyncRefused,
                "sync_refused",
            ),
            (
                refused("device_limit"),
                ErrorCode::SyncLimitReached,
                "sync_limit_reached",
            ),
            (
                refused("library_full"),
                ErrorCode::SyncLimitReached,
                "sync_limit_reached",
            ),
            (
                refused("not_entitled"),
                ErrorCode::SyncLimitReached,
                "sync_limit_reached",
            ),
            (
                refused("stale_request"),
                ErrorCode::SyncClockSkew,
                "sync_clock_skew",
            ),
            (
                refused("internal"),
                ErrorCode::SyncServerFault,
                "sync_server_fault",
            ),
            (
                refused("not_found"),
                ErrorCode::SyncServerFault,
                "sync_server_fault",
            ),
            (
                refused("malformed_request"),
                ErrorCode::Internal,
                "internal",
            ),
            (
                refused("replayed_request"),
                ErrorCode::SyncBusy,
                "sync_busy",
            ),
            (
                refused("too_large"),
                ErrorCode::SyncServerFault,
                "sync_server_fault",
            ),
            (
                SyncError::ServerFault("the server's response is malformed".into()),
                ErrorCode::SyncServerFault,
                "sync_server_fault",
            ),
            (
                SyncError::ClockSkew,
                ErrorCode::SyncClockSkew,
                "sync_clock_skew",
            ),
            (
                SyncError::SchemaMismatch,
                ErrorCode::SyncNeedsUpdate,
                "sync_needs_update",
            ),
            (SyncError::Interrupted, ErrorCode::SyncBusy, "sync_busy"),
            (SyncError::Busy, ErrorCode::SyncBusy, "sync_busy"),
            (
                SyncError::CannotPause("the database is locked".into()),
                ErrorCode::SyncCannotPause,
                "sync_cannot_pause",
            ),
        ];
        for (error, code, wire) in cases {
            let message = error.to_string();
            let api = ApiError::from(EngineError::Sync(error));
            assert_eq!(api.code, code, "{message}");
            assert_eq!(api.message, message);
            assert_eq!(serde_json::to_value(api.code).unwrap(), wire);
        }
    }

    /// Every refusal the sync protocol defines is decided here; a code added to the kernel does
    /// not compile until it is.
    #[test]
    fn every_refusal_a_server_may_send_has_a_decided_code() {
        use bookmole::test_server::RefusalCode as R;

        for &refusal in R::ALL.iter().chain(&[R::Unknown]) {
            let want = match refusal {
                R::Unauthenticated
                | R::UnknownDevice
                | R::BadSignature
                | R::Forbidden
                | R::LibraryNotFound
                | R::InvalidProof => ErrorCode::SyncRefused,
                R::DeviceLimit | R::LibraryFull | R::QuotaExceeded | R::NotEntitled => {
                    ErrorCode::SyncLimitReached
                }
                R::StaleRequest => ErrorCode::SyncClockSkew,
                R::ReplayedRequest => ErrorCode::SyncBusy,
                R::Internal | R::Unknown | R::NotFound | R::MethodNotAllowed | R::TooLarge => {
                    ErrorCode::SyncServerFault
                }
                R::MalformedRequest
                | R::MalformedEnvelope
                | R::LibraryExists
                | R::EnvelopeWrongLibrary
                | R::EnvelopeWrongDevice
                | R::EnvelopeBadSignature => ErrorCode::Internal,
            };
            let error = SyncError::Refused {
                status: refusal.status(),
                code: refusal.as_str().into(),
            };
            assert_eq!(
                ApiError::from(EngineError::Sync(error)).code,
                want,
                "{}",
                refusal.as_str()
            );
        }
    }
}
