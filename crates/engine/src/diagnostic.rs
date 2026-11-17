//! In-band degradation reports; `Display` reproduces the original CLI text byte for byte.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::EngineError;
use crate::providers::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// `Copy` classification of an `EngineError`, which is not `Clone`, so a `Diagnostic` cannot hold
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Network,
    Timeout,
    RateLimited,
    Decode,
    Parse,
    Io,
    Other,
}

impl From<&EngineError> for ErrorClass {
    fn from(e: &EngineError) -> Self {
        match e {
            EngineError::Timeout { .. } => ErrorClass::Timeout,
            EngineError::Http(h) => {
                if h.status().is_some_and(|s| s.as_u16() == 429) {
                    ErrorClass::RateLimited
                } else if h.is_timeout() {
                    ErrorClass::Timeout
                } else if h.is_decode() {
                    ErrorClass::Decode
                } else {
                    ErrorClass::Network
                }
            }
            EngineError::Json(_) => ErrorClass::Decode,
            EngineError::Sidecar(_) | EngineError::Epub(_) | EngineError::Pdf(_) => {
                ErrorClass::Parse
            }
            EngineError::Io(_) => ErrorClass::Io,
            EngineError::Provider { message, .. } => {
                // Provider errors arrive pre-rendered, so the status is recovered from the text.
                if message.contains("429") || message.to_ascii_lowercase().contains("rate limit") {
                    ErrorClass::RateLimited
                } else {
                    ErrorClass::Network
                }
            }
            _ => ErrorClass::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    ProviderFailed {
        provider: ProviderId,
        class: ErrorClass,
    },
    ProviderTimedOut {
        provider: ProviderId,
        after: Duration,
    },
    /// Not asked, because the provider said to come back later; nothing is retried in a loop.
    ProviderRateLimited {
        provider: ProviderId,
        retry_after: Duration,
    },
    /// Shown beside the provider's own failure, never instead of it.
    ProviderAnsweredFromCache {
        provider: ProviderId,
        fetched_at: i64,
    },
    KeptRecordUnreadable {
        provider: ProviderId,
    },
    OfflineIndexUnavailable {
        class: Option<ErrorClass>,
    },
    SidecarUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    SidecarUnparsable {
        path: PathBuf,
    },
    NoSidecarsFound {
        path: PathBuf,
    },
    /// Carried through as `KoStatus::Other`; this warning is the only signal of an unmodelled status.
    UnknownDeviceStatus {
        path: PathBuf,
        status: String,
    },
    /// The pull still happens but cannot be idempotent: a second pull creates a second book.
    SidecarNotIdentified {
        path: PathBuf,
    },
    /// One per file with a count, not one per entry. No `ErrorClass`: nothing failed, so there is no
    /// error to classify.
    SidecarAnchorsUnsupported {
        path: PathBuf,
        /// Never zero; the diagnostic is not constructed otherwise.
        entries: usize,
    },

    GoodreadsRowSkipped {
        row: usize,
    },
    /// The user's own review is kept; an import must not overwrite their edit.
    GoodreadsReviewDiverged {
        title: String,
    },
    /// The CSV integer is not written: the rating map is many-to-one, so the two cannot be compared.
    GoodreadsRatingDiverged {
        title: String,
    },
    GoodreadsUnanchoredReview {
        title: String,
    },
    /// `idx_readings_one_open` permits one open reading per book, and a reading with no end date is
    /// open.
    GoodreadsUndatableRereads {
        title: String,
        dropped: usize,
    },
    GoodreadsRatingUnmapped {
        title: String,
    },
    GoodreadsRereadsDropped {
        title: String,
        dropped: usize,
    },

    StatisticsDbAbsent {
        path: PathBuf,
    },
    StatisticsDbUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// Refuses rather than guesses: a wrong minutes total is indistinguishable from a right one
    /// downstream.
    StatisticsSchemaUnknown {
        path: PathBuf,
        version: i64,
    },
    /// Reported because it is also what a broken join looks like.
    StatisticsBookUnmatched {
        md5: String,
    },
    StatisticsBookNotIdentified {
        title: String,
    },

    CalibreRowSkipped {
        calibre_id: i64,
    },
    /// Still imports but cannot be idempotent: a second run creates a second copy.
    CalibreRowNotIdentified {
        calibre_id: i64,
    },
    CalibreCoverUnreadable {
        path: PathBuf,
    },

    /// The URL is deliberately not carried, so no API key can reach a diagnostic.
    CoverUnavailable {
        class: ErrorClass,
    },

    PdfUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    PdfEncrypted {
        path: PathBuf,
    },
    PdfAnnotationsKept {
        path: PathBuf,
        count: usize,
    },
    /// Nothing is removed on the strength of an annotation missing from a partial read.
    PdfScanIncomplete {
        path: PathBuf,
    },

    AppleBooksAbsent {
        path: PathBuf,
    },
    AppleBooksPermissionDenied {
        path: PathBuf,
    },
    AppleBooksDbUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    AppleBooksSchemaUnknown {
        path: PathBuf,
        table: String,
    },
    AppleBooksColumnsMissing {
        path: PathBuf,
        table: String,
        columns: Vec<String>,
    },
    AppleBooksHighlightsKept {
        asset_id: String,
        count: usize,
    },

    KindleClippingsUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    KindleEntriesUnparsed {
        path: PathBuf,
        count: usize,
    },
    KindleTitleUnmatched {
        title: String,
        entries: usize,
    },
    KindleNotebookUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    KindleNotebookUnmatched {
        path: PathBuf,
        title: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub severity: Severity,
    /// Always scrubbed of API keys: every constructor goes through `googlebooks::scrub_key`.
    pub detail: String,
}

impl Diagnostic {
    pub fn provider_failed(provider: ProviderId, err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::ProviderFailed {
                provider,
                class: ErrorClass::from(err),
            },
            severity: Severity::Warning,
            detail: crate::providers::googlebooks::scrub_key(&err.to_string()),
        }
    }

    pub fn provider_timed_out(provider: ProviderId, after: Duration) -> Self {
        Diagnostic {
            kind: DiagnosticKind::ProviderTimedOut { provider, after },
            severity: Severity::Warning,
            // Pinned: `search`'s CLI output depends on this exact text.
            detail: "timed out".to_string(),
        }
    }

    /// Carries no URL and no query.
    pub fn provider_rate_limited(provider: ProviderId, retry_after: Duration) -> Self {
        // Whole seconds, rounded up: "0s" for a wait that has not ended would read as a mistake.
        let secs = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
        Diagnostic {
            kind: DiagnosticKind::ProviderRateLimited {
                provider,
                retry_after,
            },
            severity: Severity::Warning,
            detail: format!("asked to wait; not asked again for {secs}s"),
        }
    }

    /// Carries no URL, query or title.
    pub fn provider_answered_from_cache(provider: ProviderId, fetched_at: i64) -> Self {
        Diagnostic {
            kind: DiagnosticKind::ProviderAnsweredFromCache {
                provider,
                fetched_at,
            },
            severity: Severity::Warning,
            detail: "not reached; showing what was kept from an earlier search".to_string(),
        }
    }

    pub fn kept_record_unreadable(provider: ProviderId) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KeptRecordUnreadable { provider },
            severity: Severity::Warning,
            detail: "a kept record no longer reads; dropped and asked again".to_string(),
        }
    }

    pub fn offline_index_unavailable(why: crate::search::wellknown::Unavailable) -> Self {
        use crate::search::wellknown::Unavailable as U;
        let (class, detail) = match why {
            U::NotInThisBuild => (None, "not in this build; searched without it"),
            U::NotConfigured => (None, "none for this library; searched without it"),
            U::Failed(class) => (Some(class), "could not be opened; searched without it"),
        };
        Diagnostic {
            kind: DiagnosticKind::OfflineIndexUnavailable { class },
            severity: Severity::Warning,
            detail: detail.to_string(),
        }
    }

    pub fn sidecar_unreadable(path: PathBuf, err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::SidecarUnreadable {
                path,
                class: ErrorClass::from(err),
            },
            severity: Severity::Warning,
            detail: err.to_string(),
        }
    }

    pub fn sidecar_unparsable(path: PathBuf, err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::SidecarUnparsable { path },
            severity: Severity::Warning,
            detail: err.to_string(),
        }
    }

    pub fn no_sidecars_found(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::NoSidecarsFound { path },
            severity: Severity::Warning,
            detail: String::new(),
        }
    }

    pub fn unknown_device_status(path: PathBuf, status: &str) -> Self {
        Diagnostic {
            kind: DiagnosticKind::UnknownDeviceStatus {
                path,
                status: status.to_string(),
            },
            severity: Severity::Warning,
            detail: format!("unknown KOReader status {status:?}; imported as-is"),
        }
    }

    pub fn sidecar_not_identified(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::SidecarNotIdentified { path },
            severity: Severity::Warning,
            detail: "no partial_md5_checksum; this book will be created again on a second pull"
                .to_string(),
        }
    }

    pub fn sidecar_anchors_unsupported(path: PathBuf, entries: usize) -> Self {
        Diagnostic {
            kind: DiagnosticKind::SidecarAnchorsUnsupported { path, entries },
            severity: Severity::Warning,
            detail: format!(
                "{entries} highlight(s) anchored to a page and coordinates, which KOReader \
                 writes on PDF and this build cannot store; nothing else in the file was affected"
            ),
        }
    }

    /// A `Warning`, never an error: the statistics plugin is optional.
    pub fn statistics_db_absent(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::StatisticsDbAbsent { path },
            severity: Severity::Warning,
            detail: "no KOReader statistics database; no reading time to import".to_string(),
        }
    }

    pub fn statistics_db_unreadable(path: PathBuf, err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::StatisticsDbUnreadable {
                path,
                class: ErrorClass::from(err),
            },
            severity: Severity::Warning,
            detail: err.to_string(),
        }
    }

    pub fn statistics_schema_unknown(path: PathBuf, version: i64) -> Self {
        Diagnostic {
            kind: DiagnosticKind::StatisticsSchemaUnknown { path, version },
            severity: Severity::Warning,
            detail: format!(
                "statistics schema {version} is not one this build understands \
                 (expected {}); nothing imported",
                crate::ko_statistics::KNOWN_SCHEMA_VERSION
            ),
        }
    }

    pub fn statistics_book_unmatched(md5: &str) -> Self {
        Diagnostic {
            kind: DiagnosticKind::StatisticsBookUnmatched {
                md5: md5.to_string(),
            },
            severity: Severity::Warning,
            detail: "no book in this library has that file".to_string(),
        }
    }

    pub fn statistics_book_not_identified(title: &str) -> Self {
        Diagnostic {
            kind: DiagnosticKind::StatisticsBookNotIdentified {
                title: title.to_string(),
            },
            severity: Severity::Warning,
            detail: "no md5 on the statistics row; nothing to join on".to_string(),
        }
    }

    pub fn pdf_unreadable(path: PathBuf, class: ErrorClass, detail: impl Into<String>) -> Self {
        Diagnostic {
            kind: DiagnosticKind::PdfUnreadable { path, class },
            severity: Severity::Warning,
            detail: detail.into(),
        }
    }

    pub fn pdf_encrypted(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::PdfEncrypted { path },
            severity: Severity::Warning,
            detail: "encrypted; annotations not read".to_string(),
        }
    }

    pub fn pdf_annotations_kept(path: PathBuf, count: usize) -> Self {
        Diagnostic {
            kind: DiagnosticKind::PdfAnnotationsKept { path, count },
            severity: Severity::Warning,
            detail: format!(
                "{count} highlight(s) no longer in the file kept: they carry your own notes"
            ),
        }
    }

    pub fn pdf_scan_incomplete(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::PdfScanIncomplete { path },
            severity: Severity::Warning,
            detail: "not every annotation could be read; nothing was removed".to_string(),
        }
    }

    pub fn apple_books_absent(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::AppleBooksAbsent { path },
            severity: Severity::Warning,
            detail: "Apple Books has no data here".to_string(),
        }
    }

    pub fn apple_books_permission_denied(path: PathBuf) -> Self {
        Diagnostic {
            kind: DiagnosticKind::AppleBooksPermissionDenied { path },
            severity: Severity::Warning,
            detail: "macOS did not allow access to Apple Books' data; nothing was read".to_string(),
        }
    }

    pub fn apple_books_db_unreadable(path: PathBuf, err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::AppleBooksDbUnreadable {
                path,
                class: ErrorClass::from(err),
            },
            severity: Severity::Warning,
            detail: err.to_string(),
        }
    }

    pub fn apple_books_schema_unknown(path: PathBuf, table: &str) -> Self {
        Diagnostic {
            kind: DiagnosticKind::AppleBooksSchemaUnknown {
                path,
                table: table.to_string(),
            },
            severity: Severity::Warning,
            detail: format!("no readable {table} table; nothing read from it"),
        }
    }

    pub fn apple_books_columns_missing(path: PathBuf, table: &str, columns: Vec<String>) -> Self {
        let detail = format!(
            "{table} lacks {}; imported without them",
            columns.join(", ")
        );
        Diagnostic {
            kind: DiagnosticKind::AppleBooksColumnsMissing {
                path,
                table: table.to_string(),
                columns,
            },
            severity: Severity::Warning,
            detail,
        }
    }

    pub fn apple_books_highlights_kept(asset_id: &str, count: usize) -> Self {
        Diagnostic {
            kind: DiagnosticKind::AppleBooksHighlightsKept {
                asset_id: asset_id.to_string(),
                count,
            },
            severity: Severity::Warning,
            detail: format!(
                "{count} highlight(s) deleted in Apple Books kept: they carry your own notes"
            ),
        }
    }

    pub fn cover_unavailable(err: &EngineError) -> Self {
        Diagnostic {
            kind: DiagnosticKind::CoverUnavailable {
                class: ErrorClass::from(err),
            },
            severity: Severity::Warning,
            detail: crate::providers::googlebooks::scrub_key(&err.to_string()),
        }
    }

    pub fn kindle_clippings_unreadable(
        path: PathBuf,
        class: ErrorClass,
        detail: impl Into<String>,
    ) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KindleClippingsUnreadable { path, class },
            severity: Severity::Warning,
            detail: detail.into(),
        }
    }

    /// One per file; `count` is never zero.
    pub fn kindle_entries_unparsed(path: PathBuf, count: usize) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KindleEntriesUnparsed { path, count },
            severity: Severity::Warning,
            detail: format!(
                "{count} entr{} in a language or layout this build does not read; not imported",
                if count == 1 { "y" } else { "ies" }
            ),
        }
    }

    pub fn kindle_title_unmatched(title: &str, entries: usize) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KindleTitleUnmatched {
                title: title.to_string(),
                entries,
            },
            severity: Severity::Warning,
            detail: format!(
                "no book in this library matches; {entries} highlight(s) and note(s) not imported"
            ),
        }
    }

    pub fn kindle_notebook_unreadable(
        path: PathBuf,
        class: ErrorClass,
        detail: impl Into<String>,
    ) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KindleNotebookUnreadable { path, class },
            severity: Severity::Warning,
            detail: detail.into(),
        }
    }

    pub fn kindle_notebook_unmatched(path: PathBuf, title: Option<&str>) -> Self {
        Diagnostic {
            kind: DiagnosticKind::KindleNotebookUnmatched {
                path,
                title: title.map(str::to_string),
            },
            severity: Severity::Warning,
            detail: match title {
                Some(t) => format!("no book in this library matches {t:?}; nothing imported"),
                None => "the notebook names no book and none was chosen; nothing imported".into(),
            },
        }
    }

    pub fn provider(&self) -> Option<ProviderId> {
        match self.kind {
            DiagnosticKind::ProviderFailed { provider, .. }
            | DiagnosticKind::ProviderTimedOut { provider, .. }
            | DiagnosticKind::ProviderRateLimited { provider, .. }
            | DiagnosticKind::ProviderAnsweredFromCache { provider, .. }
            | DiagnosticKind::KeptRecordUnreadable { provider } => Some(provider),
            _ => None,
        }
    }

    pub fn is_timeout(&self) -> bool {
        matches!(
            self.kind,
            DiagnosticKind::ProviderTimedOut { .. }
                | DiagnosticKind::ProviderFailed {
                    class: ErrorClass::Timeout,
                    ..
                }
        )
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(
            self.kind,
            DiagnosticKind::ProviderFailed {
                class: ErrorClass::RateLimited,
                ..
            } | DiagnosticKind::ProviderRateLimited { .. }
        )
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            DiagnosticKind::ProviderFailed { provider, .. }
            | DiagnosticKind::ProviderTimedOut { provider, .. }
            | DiagnosticKind::ProviderRateLimited { provider, .. }
            | DiagnosticKind::ProviderAnsweredFromCache { provider, .. }
            | DiagnosticKind::KeptRecordUnreadable { provider } => {
                write!(f, "{provider}: {}", self.detail)
            }
            DiagnosticKind::OfflineIndexUnavailable { .. } => {
                write!(f, "offline index: {}", self.detail)
            }
            DiagnosticKind::SidecarUnreadable { path, .. }
            | DiagnosticKind::SidecarUnparsable { path }
            | DiagnosticKind::UnknownDeviceStatus { path, .. }
            | DiagnosticKind::SidecarNotIdentified { path }
            | DiagnosticKind::SidecarAnchorsUnsupported { path, .. } => {
                write!(f, "{}: {}", path.display(), self.detail)
            }
            DiagnosticKind::NoSidecarsFound { path } => {
                write!(f, "no KOReader sidecars found under {}", path.display())
            }
            DiagnosticKind::GoodreadsRowSkipped { row } => {
                write!(f, "row {row}: {}", self.detail)
            }
            DiagnosticKind::GoodreadsReviewDiverged { title }
            | DiagnosticKind::GoodreadsRatingDiverged { title }
            | DiagnosticKind::GoodreadsUnanchoredReview { title }
            | DiagnosticKind::GoodreadsUndatableRereads { title, .. }
            | DiagnosticKind::GoodreadsRatingUnmapped { title }
            | DiagnosticKind::GoodreadsRereadsDropped { title, .. } => {
                write!(f, "{title}: {}", self.detail)
            }
            DiagnosticKind::StatisticsDbAbsent { path }
            | DiagnosticKind::StatisticsDbUnreadable { path, .. }
            | DiagnosticKind::StatisticsSchemaUnknown { path, .. } => {
                write!(f, "{}: {}", path.display(), self.detail)
            }
            DiagnosticKind::StatisticsBookUnmatched { md5 } => {
                write!(f, "{md5}: {}", self.detail)
            }
            DiagnosticKind::StatisticsBookNotIdentified { title } => {
                write!(f, "{title}: {}", self.detail)
            }
            DiagnosticKind::CalibreRowSkipped { calibre_id }
            | DiagnosticKind::CalibreRowNotIdentified { calibre_id } => {
                write!(f, "calibre #{calibre_id}: {}", self.detail)
            }
            DiagnosticKind::CalibreCoverUnreadable { path } => {
                write!(f, "{}: {}", path.display(), self.detail)
            }
            DiagnosticKind::PdfUnreadable { path, .. }
            | DiagnosticKind::PdfEncrypted { path }
            | DiagnosticKind::PdfAnnotationsKept { path, .. }
            | DiagnosticKind::PdfScanIncomplete { path }
            | DiagnosticKind::AppleBooksAbsent { path }
            | DiagnosticKind::AppleBooksPermissionDenied { path }
            | DiagnosticKind::AppleBooksDbUnreadable { path, .. }
            | DiagnosticKind::AppleBooksSchemaUnknown { path, .. }
            | DiagnosticKind::AppleBooksColumnsMissing { path, .. } => {
                write!(f, "{}: {}", path.display(), self.detail)
            }
            DiagnosticKind::AppleBooksHighlightsKept { asset_id, .. } => {
                write!(f, "Apple Books {asset_id}: {}", self.detail)
            }
            DiagnosticKind::CoverUnavailable { .. } => {
                write!(f, "cover: {}", self.detail)
            }
            DiagnosticKind::KindleClippingsUnreadable { path, .. }
            | DiagnosticKind::KindleEntriesUnparsed { path, .. }
            | DiagnosticKind::KindleNotebookUnreadable { path, .. }
            | DiagnosticKind::KindleNotebookUnmatched { path, .. } => {
                write!(f, "{}: {}", path.display(), self.detail)
            }
            DiagnosticKind::KindleTitleUnmatched { title, .. } => {
                write!(f, "{title}: {}", self.detail)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_matches_the_pre_refactor_strings() {
        let timed_out =
            Diagnostic::provider_timed_out(ProviderId::OpenLibrary, Duration::from_secs(5));
        assert_eq!(timed_out.to_string(), "openlibrary: timed out");

        let failed = Diagnostic::provider_failed(
            ProviderId::GoogleBooks,
            &EngineError::Other("boom".into()),
        );
        assert_eq!(failed.to_string(), "googlebooks: boom");

        let none = Diagnostic::no_sidecars_found(PathBuf::from("/books"));
        assert_eq!(none.to_string(), "no KOReader sidecars found under /books");

        let bad = Diagnostic::sidecar_unparsable(
            PathBuf::from("/books/x.sdr/metadata.epub.lua"),
            &EngineError::Sidecar("lua eval: nope".into()),
        );
        assert_eq!(
            bad.to_string(),
            "/books/x.sdr/metadata.epub.lua: koreader sidecar error: lua eval: nope"
        );
    }

    #[test]
    fn a_closed_provider_says_for_how_long() {
        let d = Diagnostic::provider_rate_limited(
            ProviderId::OpenLibrary,
            Duration::from_millis(59_200),
        );
        assert_eq!(
            d.to_string(),
            "openlibrary: asked to wait; not asked again for 60s"
        );
        assert!(d.is_rate_limited());
        assert!(!d.is_timeout());
        assert_eq!(d.provider(), Some(ProviderId::OpenLibrary));
    }

    #[test]
    fn kept_answers_say_so_and_name_no_query() {
        let d = Diagnostic::provider_answered_from_cache(ProviderId::OpenLibrary, 1_700_000_000);
        assert_eq!(
            d.to_string(),
            "openlibrary: not reached; showing what was kept from an earlier search"
        );
        assert_eq!(d.provider(), Some(ProviderId::OpenLibrary));
        assert!(!d.is_timeout() && !d.is_rate_limited());
        let d = Diagnostic::kept_record_unreadable(ProviderId::OpenLibrary);
        assert_eq!(
            d.to_string(),
            "openlibrary: a kept record no longer reads; dropped and asked again"
        );
    }

    #[test]
    fn a_key_never_survives_into_a_diagnostic() {
        let leaky = EngineError::Other(
            "https://www.googleapis.com/books/v1/volumes?q=x&key=SUPERSECRET failed".into(),
        );
        let d = Diagnostic::provider_failed(ProviderId::GoogleBooks, &leaky);
        assert!(
            !d.detail.contains("SUPERSECRET"),
            "detail leaked: {}",
            d.detail
        );
        assert!(!d.to_string().contains("SUPERSECRET"));
    }

    #[test]
    fn classification_is_recoverable_from_the_kind() {
        let t = Diagnostic::provider_timed_out(ProviderId::OpenLibrary, Duration::from_secs(5));
        assert!(t.is_timeout());
        assert_eq!(t.provider(), Some(ProviderId::OpenLibrary));

        let r = Diagnostic::provider_failed(
            ProviderId::GoogleBooks,
            &EngineError::Provider {
                provider: ProviderId::GoogleBooks,
                message: "HTTP 429 rate limit".into(),
            },
        );
        assert!(r.is_rate_limited());
        assert!(!r.is_timeout());
    }
}
