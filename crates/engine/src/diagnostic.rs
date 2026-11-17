//! In-band, user-facing degradation reports.
//!
//! A [`Diagnostic`] is what the engine returns when an operation *partly*
//! succeeded: one provider timed out but the other answered, one sidecar was
//! unreadable but the rest imported. It is distinct from `tracing`, and the two
//! are not substitutes — tracing is out-of-band and for the developer, a
//! diagnostic is part of the return value and for the user. Only a diagnostic
//! can put "openlibrary timed out, these results are partial" in a status bar.
//!
//! These used to be pre-formatted `String`s, which meant no caller could tell a
//! timeout from a 500 and no test could assert on the degradation path. The
//! [`std::fmt::Display`] impls below reproduce those old strings **byte for
//! byte**, so frontends that just print them needed no change.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::error::EngineError;
use crate::providers::ProviderId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The operation continued and returned partial results.
    Warning,
    /// The operation could not proceed.
    Error,
}

/// A coarse, `Copy` classification of an [`EngineError`], for callers that want
/// to branch without matching the full error.
///
/// This exists instead of storing the `EngineError` itself: `EngineError` is
/// not `Clone` (neither `sqlx::Error` nor `reqwest::Error` is), and a
/// `Diagnostic` that cannot be cloned is one the TUI cannot buffer and the test
/// harness cannot compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Network,
    Timeout,
    /// A provider refused us for quota reasons — the keyless Google Books path
    /// hits this, and it is the one class that plausibly deserves its own UI
    /// text ("add an API key"), which is why it is not folded into `Network`.
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
                // Provider errors arrive pre-rendered (scrubbed), so the status
                // has to be recovered from the text.
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
    /// The provider said to come back later (a 429 or 503 with
    /// `Retry-After`), so it was not asked; `retry_after` is how much longer.
    /// A state rather than a failure: nothing is retried in a loop, and the
    /// next search after that time asks again.
    ProviderRateLimited {
        provider: ProviderId,
        retry_after: Duration,
    },
    /// The provider was asked and did not answer, and what it answered to the
    /// same search before was shown instead: a kept answer
    /// fetched at `fetched_at` (unix seconds), or kept records found by the
    /// query's own words. Beside the provider's own failure, never instead of
    /// it — the failure is still what happened.
    ProviderAnsweredFromCache {
        provider: ProviderId,
        fetched_at: i64,
    },
    /// A kept record of this provider no longer reads as what the provider
    /// sends (a parser changed under it). It was dropped, and the search asked
    /// the provider again rather than show a row it could not read.
    KeptRecordUnreadable {
        provider: ProviderId,
    },
    /// The offline index of well-known works (`search::wellknown`) could not be used: not
    /// in this build, or it could not be built or opened. The search still
    /// ran, without suggestions, correction or the index's standing.
    OfflineIndexUnavailable {
        /// `None`: this build has no index (the `wellknown` feature is off).
        class: Option<ErrorClass>,
    },
    /// The file could not be read at all (permissions, non-UTF-8 bytes).
    SidecarUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// The file was read but is not a valid sidecar.
    SidecarUnparsable {
        path: PathBuf,
    },
    NoSidecarsFound {
        path: PathBuf,
    },
    /// The sidecar's `summary.status` was a value KOReader is not known to
    /// write. The import carries it through as `KoStatus::Other` and continues;
    /// this warning exists because it is otherwise the one thing that could
    /// tell us the device grew a status we do not model, and silence would look
    /// exactly like success.
    UnknownDeviceStatus {
        path: PathBuf,
        status: String,
    },
    /// The sidecar carries no root `partial_md5_checksum`, so there is nothing
    /// to key a `device_books` mapping on.
    ///
    /// The pull still happens — refusing would be a dead end for a file the
    /// user explicitly pointed at — but it cannot be made idempotent: a second
    /// pull of the same sidecar creates a second book. Saying so is the whole
    /// point of the variant, because the duplicate appears silently and long
    /// after the fact.
    SidecarNotIdentified {
        path: PathBuf,
    },
    /// The sidecar carries annotations that *are* highlights on the device but
    /// whose anchor this build cannot store: KOReader writes `pos0` as a
    /// **table** — a page plus coordinates — on a paging document (PDF, DjVu),
    /// where a reflowable one gets a cre xpointer string.
    ///
    /// They used to be dropped in **silence**, which made a PDF library
    /// indistinguishable from a book nobody had highlighted: the import
    /// reported zero highlights and no reason. Saying so is the whole variant.
    ///
    /// **One per file, carrying a count — not one per entry.** A 300-highlight
    /// PDF emitting 300 identical diagnostics would have replaced silence with
    /// noise, which is not an improvement; the count is what a caller needs and
    /// the path is what it acts on.
    ///
    /// **No `ErrorClass`, deliberately.** That type is `From<&EngineError>` and
    /// exists to classify something that *failed*. Nothing failed here: the file
    /// read, the chunk evaluated, the entry is well formed. There is no error to
    /// classify, so a class on this variant would be a field with no source.
    SidecarAnchorsUnsupported {
        path: PathBuf,
        /// How many entries. Never zero — the diagnostic is not constructed
        /// otherwise, so a caller may read it as "at least one highlight did not
        /// arrive".
        entries: usize,
    },

    // ---- goodreads (migration `0009`) -------------------------------------
    //
    // Seven variants rather than one carrying a reason string, for the reason
    // the module doc gives: a degradation the caller cannot tell apart from
    // another degradation is a `String` wearing a type. Each of these has a
    // different next move, and the CLI prints a different one for each.
    /// A CSV row with nothing to match on (no `Title`). The rest of the file
    /// still imports — one bad row must not cost the other four hundred.
    GoodreadsRowSkipped {
        row: usize,
    },
    /// The CSV's review and the review already in the vault disagree. **Ours is
    /// kept**: the words came from Goodreads, but they live here now, and an
    /// import overwriting the user's own edit is the one outcome with no undo.
    GoodreadsReviewDiverged {
        title: String,
    },
    /// The review is already rated on a scale that is not `goodreads`, so the
    /// CSV's integer was not written. The map is many-to-one and its inverse is
    /// a guess, so the two numbers cannot even be compared.
    GoodreadsRatingDiverged {
        title: String,
    },
    /// A row carries a review but describes no reading to anchor it to (it is on
    /// `to-read`). The prose is kept as a plain note rather than opening a
    /// reading the user never started.
    GoodreadsUnanchoredReview {
        title: String,
    },
    /// `Read Count` asked for more readings than the row carries a date to close
    /// them at. A reading with no end date *is* an open one, and
    /// `idx_readings_one_open` permits exactly one per book.
    GoodreadsUndatableRereads {
        title: String,
        dropped: usize,
    },
    /// Export: this book's rating has no Goodreads mapping, so the row was
    /// skipped rather than rounded.
    GoodreadsRatingUnmapped {
        title: String,
    },
    /// Export: Goodreads' importable CSV has no read-count column, so a reread
    /// exports as its most recent reading only.
    GoodreadsRereadsDropped {
        title: String,
        dropped: usize,
    },

    // ---- koreader statistics -----------------------------------------------
    //
    // Four variants rather than one, and the module doc's rule is the reason:
    // *absence is ordinary here*, so these are the ordinary path rather than an
    // exceptional one, and a caller that cannot tell "this device has never run
    // the statistics plugin" from "we do not understand this device's schema"
    // has no way to tell the user which of the two it is looking at.
    /// The volume has no `settings/statistics.sqlite3`. The commonest case by
    /// far — the plugin is optional and a fresh install has never written one —
    /// and emphatically not an error.
    StatisticsDbAbsent {
        path: PathBuf,
    },
    /// The file is there and SQLite would not read it: a partial copy off a
    /// yanked volume, a permissions problem, a file that is not a database.
    StatisticsDbUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// `PRAGMA user_version` is a schema this build has never seen.
    ///
    /// **Refuses rather than guesses.** KOReader stamps the version precisely so
    /// a reader can tell; the columns we aggregate could have been renamed or
    /// re-scaled under us, and importing a wrong number of minutes is far worse
    /// than importing none, because nothing downstream can tell it apart from a
    /// right one.
    StatisticsSchemaUnknown {
        path: PathBuf,
        version: i64,
    },
    /// A `book` row whose `md5` matches no book in the library.
    ///
    /// Ordinary: the device holds books that were never imported here. Reported
    /// because it is also what a *broken join* looks like, and the two are
    /// indistinguishable in silence.
    StatisticsBookUnmatched {
        md5: String,
    },
    /// A `book` row carrying no `md5` at all, so there is nothing to join on.
    /// The sibling of [`DiagnosticKind::SidecarNotIdentified`].
    StatisticsBookNotIdentified {
        title: String,
    },

    // ---- calibre -----------------------------------------------------------
    /// A calibre row with no title, so there is nothing to match on. The rest
    /// of the library still imports.
    CalibreRowSkipped {
        calibre_id: i64,
    },
    /// A calibre row with no `uuid`. It still imports — refusing would be a
    /// dead end for a book the user plainly has — but it cannot be made
    /// idempotent, and a second run creates a second copy. The sibling of
    /// [`DiagnosticKind::SidecarNotIdentified`], and it exists for the same
    /// reason: the duplicate appears silently and long after the fact.
    CalibreRowNotIdentified {
        calibre_id: i64,
    },
    /// Calibre named a cover we could not copy — its library may live on a
    /// volume that is not mounted. The book still imports without one.
    CalibreCoverUnreadable {
        path: PathBuf,
    },

    // ---- enrichment -------------------------------------------------------
    /// A provider gave us a cover URL that would not download. The metadata it
    /// came with landed and is kept — the cover is the one field with a free
    /// second chance, since the next enrichment of this book tries again.
    ///
    /// **The URL is not carried.** It is of no use to a user, and the rule that
    /// nothing reaching a diagnostic may contain an API key is easier to keep
    /// than to check.
    CoverUnavailable {
        class: ErrorClass,
    },

    // ---- Preview / PDF annotations ------------------------------------------
    /// A PDF whose annotations could not be read at all: not a PDF, truncated,
    /// too large, or unreadable. Nothing was imported or removed from it.
    PdfUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// The PDF is encrypted, so its annotations are behind a password we do not
    /// have. Nothing was imported or removed.
    PdfEncrypted {
        path: PathBuf,
    },
    /// Highlights whose annotation is gone from the PDF but which carry
    /// something of the reader's own (an annotation, a note, a flashcard...),
    /// so they were kept. `count` of them.
    PdfAnnotationsKept {
        path: PathBuf,
        count: usize,
    },
    /// The PDF was read only in part (a cap reached, a page not all read, an
    /// annotation unparsed), so its annotations are not a complete list and
    /// **nothing was removed** on the strength of one missing from it.
    PdfScanIncomplete {
        path: PathBuf,
    },

    // ---- Apple Books --------------------------------------------------------
    //
    // Separate variants because each has a different next move: "Apple Books
    // has no data here" is ordinary (it was never opened, or this is not a
    // Mac), "macOS said no" is the system's one-time prompt declined and is
    // the one a frontend explains, and a schema we cannot read is ours to fix.
    /// No Apple Books database where one was looked for. `path` is the folder
    /// searched (`…/AEAnnotation` or `…/BKLibrary`). Not an error.
    AppleBooksAbsent {
        path: PathBuf,
    },
    /// macOS refused access to Apple Books' container: the "access data from
    /// other apps" prompt was declined (or never answered). Nothing was read.
    AppleBooksPermissionDenied {
        path: PathBuf,
    },
    /// The database is there and could not be copied or read as SQLite.
    AppleBooksDbUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// A table this import needs is missing, or lacks its key column: a
    /// schema this build does not know. Nothing was read from that table.
    AppleBooksSchemaUnknown {
        path: PathBuf,
        table: String,
    },
    /// The table is readable but lacks optional columns; its rows imported
    /// without what those columns would have said (degraded, not failed).
    AppleBooksColumnsMissing {
        path: PathBuf,
        table: String,
        columns: Vec<String>,
    },
    /// Highlights Apple Books marks deleted that carry something of the
    /// reader's own (an annotation, a note, a flashcard...), so they were kept.
    AppleBooksHighlightsKept {
        asset_id: String,
        count: usize,
    },

    // ---- Kindle: `My Clippings.txt` and the app's notebook export ----
    /// `My Clippings.txt` could not be read at all: missing, too large, or not
    /// UTF-8 text. Nothing was imported.
    KindleClippingsUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// Entries of a Kindle file (clippings or notebook) that were seen and not
    /// imported: in a language no table row knows, or structurally broken.
    /// One per file, carrying the count; never guessed at, never dropped
    /// silently.
    KindleEntriesUnparsed {
        path: PathBuf,
        count: usize,
    },
    /// A clippings title that matches no library book. No book is created:
    /// `entries` highlights and notes wait for the reader to attach them.
    KindleTitleUnmatched {
        title: String,
        entries: usize,
    },
    /// A Kindle app notebook export that could not be read, or is not one.
    /// Nothing was imported.
    KindleNotebookUnreadable {
        path: PathBuf,
        class: ErrorClass,
    },
    /// The notebook's book matches no library book, and none was named. No
    /// book is created. `title` is the file's own, when it has one.
    KindleNotebookUnmatched {
        path: PathBuf,
        title: Option<String>,
    },
}

/// One degradation, carried in-band on a partly-successful result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub severity: Severity,
    /// Human-readable detail. **Always already scrubbed of API keys** — every
    /// constructor below goes through `googlebooks::scrub_key`.
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
            // "timed out" verbatim: this is what the pre-refactor string was,
            // and `search`'s CLI output is pinned to it.
            detail: "timed out".to_string(),
        }
    }

    /// Carries no URL and no query: a provider's refusal is about us, not about
    /// what was searched for.
    pub fn provider_rate_limited(provider: ProviderId, retry_after: Duration) -> Self {
        // Whole seconds, rounded up: "0s" for a wait that has not ended would
        // read as a mistake.
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

    /// No URL, no query, no title: which provider, and when what was shown
    /// instead had been fetched.
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

    /// One per **file**, so `entries` is a count and not a list of paths that
    /// are all the same path.
    ///
    /// The wording says *why* rather than naming a value the user cannot see:
    /// a page-and-coordinates anchor is a fact about their PDF, and the number
    /// is the part they can check against the device.
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

    // ---- koreader statistics -----------------------------------------------

    /// No statistics database on this volume. A `Warning`, never an error: the
    /// statistics plugin is optional, and *a device with no measured time is a
    /// device with no measured time*, not a failure.
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

    // ---- Apple Books --------------------------------------------------------

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

    // ---- Kindle: `My Clippings.txt` and the app's notebook export ----

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

    /// The provider this diagnostic is about, if any.
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
    /// Byte-for-byte identical to the strings this type replaced. Frontends
    /// print `warning: {d}` and were not touched by the refactor; a change here
    /// is a change to user-visible output.
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
            // ---- Kindle: `My Clippings.txt` and the app's notebook export ----
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

    /// The whole compatibility bar for the `Vec<String>` -> `Vec<Diagnostic>`
    /// refactor: these are the exact strings the CLI used to print.
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

    /// The new kind's words, pinned the way the old ones are, and its
    /// classification.
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

    /// The two kinds that report a kept answer, pinned the way the others are.
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
