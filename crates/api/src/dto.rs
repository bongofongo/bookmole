//! Wire types: serializable mirrors of the domain types; the domain types stay free of `serde`.
//! Paths cross as lossy UTF-8 strings, so a non-UTF-8 path does not round-trip.
//! Nullable or repeated fields take `#[serde(default)]` and never `skip_serializing_if`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use bookmole::GoodreadsMatch;
use bookmole::koreader::UnmatchedSidecar;
use bookmole::providers::ProviderId;
use bookmole::{
    ActivitySummary, AnkiExport, Book, BookFile, BookFilter, BookImportStats, BookQuery, BookSort,
    BookSummary, BookTag, CalibreBook, CalibreBookReport, CalibreMatch, CalibreReport, CloudFolder,
    CloudService, Confidence, CreatedNote, DayActivity, DayRange, DeviceBook, DeviceScan,
    DeviceState, Diagnostic, DiagnosticKind, EditionShape, EnrichCandidate, EnrichMatch,
    EnrichOutcome, EnrichReport, ErrorClass, ExpandedPaths, FieldChange, FieldSource, FileIdentity,
    FileImportReport, FileMatch, FileOutcome, FillStats, FlashcardRow, FractionSource,
    GoodreadsBookReport, GoodreadsReport, HeldField, Highlight, ImportReport, InstallReport,
    KoStatus, LibraryLocation, LinkContext, LinkDirection, ListenerMode, ListenerStatus,
    MatchCandidate, MatchMethod, MergeReport, Moment, MomentKind, MonthActivity, MountSync,
    NewNoteInput, NoteCitations, NoteKind, NoteRecord, OutgoingLink, PairedDevice, Pairing,
    PluginCondition, PluginStatus, Progress, PullReport, RankedResult, Rating, RatingScale,
    Reading, ReadingEvent, ReadingFilter, ReadingQuery, ReadingRow, ReadingSort, ReadingState,
    ReadingYears, RefillReport, SavedAnkiExport, SearchHit, SearchOutcome, SearchRequest,
    SearchSource, Severity, ShapeSource, SkippedEntries, Source, StatsImportReport, StatusFilter,
    TableOfContents, TextOutcome, TocEntry, UninstallReport, UnmatchedRow,
};

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn opt_path(p: &Option<PathBuf>) -> Option<String> {
    p.as_deref().map(path_str)
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BookDto {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub sort_title: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub translators: Vec<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub publish_year: Option<i64>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub isbn_10: Option<String>,
    #[serde(default)]
    pub isbn_13: Option<String>,
    #[serde(default)]
    pub openlibrary_key: Option<String>,
    #[serde(default)]
    pub googlebooks_id: Option<String>,
    #[serde(default)]
    pub cover_url: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub page_count: Option<i64>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub first_sentence: Option<String>,
    /// Provider subjects, merged as a set; `[]` means "not stated", never "no subjects".
    #[serde(default)]
    pub subjects: Vec<String>,
    /// Series name; `series_index` is meaningful only beside it.
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub series_index: Option<f64>,
    /// Cover a grid should load: the thumbnail if one exists, else the original, else absent.
    /// Read-only.
    #[serde(default)]
    pub cover_shelf_path: Option<String>,
    /// Cover width over height; `None` means not yet measured, not that there is no cover. Read-only.
    #[serde(default)]
    pub cover_aspect: Option<f64>,
    /// The cover's border colour as channels. Unclamped. Read-only.
    #[serde(default)]
    pub cover_accent: Option<AccentDto>,
    /// Read-only projections of the current reading; ignored by `save_book`.
    #[serde(default)]
    pub current_page: Option<i64>,
    #[serde(default)]
    pub finished: bool,
    /// The current reading's state; absent when the book has no reading. Read-only.
    #[serde(default)]
    pub reading_state: Option<ReadingStateDto>,
    #[serde(default)]
    pub date_started: Option<i64>,
    #[serde(default)]
    pub date_finished: Option<i64>,
    /// Progress through the current reading. Read-only and ignored on the way in.
    #[serde(default)]
    pub progress: ProgressDto,
    /// The series and index as a person writes it (`Dune #2`, never `Dune #2.0`). Read-only.
    #[serde(default)]
    pub series_label: Option<String>,
    /// Authors as a person says them (`Jorge Luis Borges`). Read-only.
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// Surname the book files under, as the origin cased it; absent for authorless books. Read-only.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub sort_author: Option<String>,
    /// Edition shape in multiples of its own height. Read-only.
    #[serde(default)]
    pub shape: EditionShapeDto,
    /// Unix seconds.
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub last_modified: Option<i64>,
}

impl From<Book> for BookDto {
    fn from(b: Book) -> Self {
        let reading_state = b.reading_state().map(ReadingStateDto::from);
        let progress = ProgressDto::from(Progress::of_book(&b));
        let series_label = b.series_label();
        let authors_display = b.authors_in_display_order();
        let sort_author = b
            .authors
            .iter()
            .find(|a| !a.trim().is_empty())
            .map(|a| bookmole::names::last_name(a))
            .filter(|s| !s.is_empty());
        let cover_shelf_path = b.shelf_cover_path().map(str::to_string);
        let cover_aspect = b.cover_aspect();
        let cover_accent = b.cover_accent_rgb().map(AccentDto::from);
        let shape =
            EditionShapeDto::from(EditionShape::of_book(&b, cover_aspect.map(|a| a as f32)));
        BookDto {
            id: b.id,
            title: b.title,
            sort_title: b.sort_title,
            authors: b.authors,
            translators: b.translators,
            publisher: b.publisher,
            publish_year: b.publish_year,
            language: b.language,
            isbn_10: b.isbn_10,
            isbn_13: b.isbn_13,
            openlibrary_key: b.openlibrary_key,
            googlebooks_id: b.googlebooks_id,
            cover_url: b.cover_url,
            cover_path: b.cover_path,
            page_count: b.page_count,
            description: b.description,
            first_sentence: b.first_sentence,
            subjects: b.subjects,
            series: b.series,
            series_index: b.series_index,
            cover_shelf_path,
            cover_aspect,
            cover_accent,
            current_page: b.current_page,
            finished: b.finished,
            reading_state,
            date_started: b.date_started,
            date_finished: b.date_finished,
            progress,
            series_label,
            authors_display,
            sort_author,
            shape,
            created_at: b.created_at.map(|t| t.unix_timestamp()),
            last_modified: b.last_modified.map(|t| t.unix_timestamp()),
        }
    }
}

impl From<BookDto> for Book {
    /// `created_at` and `last_modified` are dropped: storage stamps them. Derived fields are ignored.
    fn from(d: BookDto) -> Self {
        Book {
            id: d.id,
            title: d.title,
            sort_title: d.sort_title,
            authors: d.authors,
            translators: d.translators,
            publisher: d.publisher,
            publish_year: d.publish_year,
            first_published: None,
            language: d.language,
            isbn_10: d.isbn_10,
            isbn_13: d.isbn_13,
            openlibrary_key: d.openlibrary_key,
            googlebooks_id: d.googlebooks_id,
            cover_url: d.cover_url,
            cover_path: d.cover_path,
            page_count: d.page_count,
            description: d.description,
            first_sentence: d.first_sentence,
            current_page: d.current_page,
            finished: d.finished,
            reading_status: d.reading_state.map(|s| ReadingState::from(s).to_string()),
            ko_percent: None,
            date_started: d.date_started,
            date_finished: d.date_finished,
            subjects: d.subjects,
            series: d.series,
            series_index: d.series_index,
            // Cover dimensions are not on the wire: `Storage::set_cover` writes them with the file or not
            // at all.
            cover_width: None,
            cover_height: None,
            cover_accent: None,
            cover_thumb_path: None,
            created_at: None,
            last_modified: None,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccentDto {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl From<[u8; 3]> for AccentDto {
    fn from([r, g, b]: [u8; 3]) -> Self {
        AccentDto { r, g, b }
    }
}

/// A reading's state; `Other` carries a word this build does not know.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReadingStateDto {
    Reading,
    Finished,
    Abandoned,
    Other { raw: String },
}

impl From<ReadingState> for ReadingStateDto {
    fn from(s: ReadingState) -> Self {
        match s {
            ReadingState::Reading => ReadingStateDto::Reading,
            ReadingState::Finished => ReadingStateDto::Finished,
            ReadingState::Abandoned => ReadingStateDto::Abandoned,
            ReadingState::Other(raw) => ReadingStateDto::Other { raw },
        }
    }
}

impl From<ReadingStateDto> for ReadingState {
    fn from(s: ReadingStateDto) -> Self {
        match s {
            ReadingStateDto::Reading => ReadingState::Reading,
            ReadingStateDto::Finished => ReadingState::Finished,
            ReadingStateDto::Abandoned => ReadingState::Abandoned,
            ReadingStateDto::Other { raw } => ReadingState::Other(raw),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FractionSourceDto {
    Pages,
    Device,
}

impl From<FractionSource> for FractionSourceDto {
    fn from(s: FractionSource) -> Self {
        match s {
            FractionSource::Pages => FractionSourceDto::Pages,
            FractionSource::Device => FractionSourceDto::Device,
        }
    }
}

/// How far into a book a reading is. `percent` is carried because integer division and float
/// flooring differ.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "progress", rename_all = "snake_case")]
pub enum ProgressDto {
    /// Nothing recorded; not zero and not unread.
    #[default]
    Untouched,
    Started {
        #[serde(default)]
        page: Option<i64>,
        /// The length; never `Some(0)`.
        #[serde(default)]
        of: Option<i64>,
        /// `0.0..=1.0`, or absent when the length is unknown.
        #[serde(default)]
        fraction: Option<f64>,
        #[serde(default)]
        percent: Option<i64>,
        #[serde(default)]
        source: Option<FractionSourceDto>,
    },
    Finished,
}

impl From<Progress> for ProgressDto {
    fn from(p: Progress) -> Self {
        match p {
            Progress::Untouched => ProgressDto::Untouched,
            Progress::Finished => ProgressDto::Finished,
            Progress::Started { page, of, fraction } => ProgressDto::Started {
                page,
                of,
                fraction: fraction.map(|f| f.value),
                percent: p.percent(),
                source: fraction.map(|f| f.source.into()),
            },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookSortDto {
    #[default]
    LastModified,
    Title,
    Progress,
    /// Alphabetically by first author's surname, authorless books last.
    Author,
    Year,
}

impl From<BookSortDto> for BookSort {
    fn from(s: BookSortDto) -> Self {
        match s {
            BookSortDto::LastModified => BookSort::LastModified,
            BookSortDto::Title => BookSort::Title,
            BookSortDto::Progress => BookSort::Progress,
            BookSortDto::Author => BookSort::Author,
            BookSortDto::Year => BookSort::Year,
        }
    }
}

/// Which books a list is about. Every field absent means every book.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFilterDto {
    #[serde(default)]
    pub status: Option<StatusFilterDto>,
    /// Case-insensitive title substring; not the full-text index.
    #[serde(default)]
    pub title: Option<String>,
    /// Case-insensitive substring of the stored author list.
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub language: Option<String>,
    /// A minted shelf name from `book_tags`, not `subjects`.
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub has_cover: Option<bool>,
}

impl From<BookFilterDto> for BookFilter {
    fn from(f: BookFilterDto) -> Self {
        BookFilter {
            status: f.status.map(Into::into),
            title: f.title,
            author: f.author,
            year: f.year,
            language: f.language,
            tag: f.tag,
            // Only the library listing converts this DTO; a book known only from a search is not in the
            // library.
            library: bookmole::LibraryScope::InLibrary,
            has_cover: f.has_cover,
        }
    }
}

/// Required state of the current reading. Absence is `NoReading`, not a state.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "match", rename_all = "snake_case")]
pub enum StatusFilterDto {
    State { is: ReadingStateDto },
    NoReading,
}

impl From<StatusFilterDto> for StatusFilter {
    fn from(s: StatusFilterDto) -> Self {
        match s {
            StatusFilterDto::State { is } => StatusFilter::Is(is.into()),
            StatusFilterDto::NoReading => StatusFilter::NoReading,
        }
    }
}

/// One page of the library: filter, order and slice. `Request::ListBooks` carries these flat.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookQueryDto {
    #[serde(default)]
    pub sort: BookSortDto,
    #[serde(default)]
    pub filter: Option<BookFilterDto>,
    /// Negative means no limit.
    pub limit: i64,
    /// Rows to skip; offset paging, since two sorts have no cursor key.
    #[serde(default)]
    pub offset: i64,
}

impl From<BookQueryDto> for BookQuery {
    fn from(q: BookQueryDto) -> Self {
        BookQuery {
            sort: q.sort.into(),
            filter: q.filter.unwrap_or_default().into(),
            limit: q.limit,
            offset: q.offset,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookSummaryDto {
    pub book_id: i64,
    pub highlights: i64,
    pub notes: i64,
    pub files: i64,
}

impl From<BookSummary> for BookSummaryDto {
    fn from(s: BookSummary) -> Self {
        BookSummaryDto {
            book_id: s.book_id,
            highlights: s.highlights,
            notes: s.notes,
            files: s.files,
        }
    }
}

/// Edition shape in multiples of its own height (height is 1.0).
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EditionShapeDto {
    pub width_over_height: f32,
    pub width_source: ShapeSourceDto,
    pub thickness_over_height: f32,
    pub thickness_source: ShapeSourceDto,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeSourceDto {
    Recorded,
    Assumed,
}

impl From<ShapeSource> for ShapeSourceDto {
    fn from(s: ShapeSource) -> Self {
        match s {
            ShapeSource::Recorded => ShapeSourceDto::Recorded,
            ShapeSource::Assumed => ShapeSourceDto::Assumed,
        }
    }
}

impl Default for EditionShapeDto {
    // Goes through the engine's own fallback so the two cannot drift.
    fn default() -> Self {
        EditionShape::new(None, None).into()
    }
}

impl From<EditionShape> for EditionShapeDto {
    fn from(s: EditionShape) -> Self {
        EditionShapeDto {
            width_over_height: s.width_over_height,
            width_source: s.width_source.into(),
            thickness_over_height: s.thickness_over_height,
            thickness_source: s.thickness_source.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookTagDto {
    pub tag: String,
    pub source: String,
    /// The origin's own shelf string, before normalization.
    #[serde(default)]
    pub raw: Option<String>,
}

impl From<BookTag> for BookTagDto {
    fn from(t: BookTag) -> Self {
        BookTagDto {
            tag: t.tag,
            source: t.source,
            raw: t.raw,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeReportDto {
    pub src_existed: bool,
    pub highlights_moved: usize,
    pub highlights_dropped: usize,
    pub notes_moved: usize,
    pub readings_moved: usize,
    pub flashcards_moved: usize,
    pub flashcards_dropped: usize,
    pub device_links_moved: usize,
    pub files_moved: usize,
    #[serde(default)]
    pub orphaned_cover: Option<String>,
}

impl From<MergeReport> for MergeReportDto {
    fn from(r: MergeReport) -> Self {
        MergeReportDto {
            src_existed: r.src_existed,
            highlights_moved: r.highlights_moved,
            highlights_dropped: r.highlights_dropped,
            notes_moved: r.notes_moved,
            readings_moved: r.readings_moved,
            flashcards_moved: r.flashcards_moved,
            flashcards_dropped: r.flashcards_dropped,
            device_links_moved: r.device_links_moved,
            files_moved: r.files_moved,
            orphaned_cover: r.orphaned_cover,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingDto {
    pub id: i64,
    pub book_id: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    /// `null` means open; at most one reading per book may be open.
    #[serde(default)]
    pub finished_at: Option<i64>,
    pub status: ReadingStateDto,
    /// `manual` | `koreader` | `migrated` | `goodreads` | `calibre`; a `String` because importers
    /// add values.
    pub source: String,
    #[serde(default)]
    pub current_page: Option<i64>,
    #[serde(default)]
    pub ko_status: Option<KoStatusDto>,
    #[serde(default)]
    pub ko_percent: Option<f64>,
    #[serde(default)]
    pub ko_rating: Option<i64>,
    pub created_at: i64,
    pub last_modified: i64,
    /// This reading's progress, not the book's.
    pub progress: ProgressDto,
}

impl ReadingDto {
    /// A reading beside its computed progress. No `From<Reading>`: a reading alone cannot give
    /// progress.
    pub fn new(r: Reading, progress: Progress) -> ReadingDto {
        ReadingDto {
            progress: progress.into(),
            ..ReadingDto::bare(r)
        }
    }

    fn bare(r: Reading) -> ReadingDto {
        ReadingDto {
            progress: ProgressDto::Untouched,
            id: r.id,
            book_id: r.book_id,
            started_at: r.started_at,
            finished_at: r.finished_at,
            status: ReadingState::from(r.status.as_str()).into(),
            source: r.source,
            current_page: r.current_page,
            ko_status: r.ko_status.map(|s| KoStatus::from(s.as_str()).into()),
            ko_percent: r.ko_percent,
            ko_rating: r.ko_rating,
            created_at: r.created_at,
            last_modified: r.last_modified,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightDto {
    pub id: i64,
    pub book_id: i64,
    pub text: String,
    #[serde(default)]
    pub chapter: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
    /// KOReader's note; rewritten toward the device on every import.
    #[serde(default)]
    pub ko_note: Option<String>,
    /// The reader's own annotation; never touched by an import.
    #[serde(default)]
    pub annotation: Option<String>,
    #[serde(default)]
    pub ko_datetime: Option<String>,
    /// The reading this was captured during, when dates could place it; `null` is an ordinary answer.
    #[serde(default)]
    pub reading_id: Option<i64>,
    pub source: String,
    pub created_at: i64,
}

impl From<Highlight> for HighlightDto {
    fn from(h: Highlight) -> Self {
        HighlightDto {
            id: h.id,
            book_id: h.book_id,
            text: h.text,
            chapter: h.chapter,
            page: h.page,
            ko_note: h.origin_note,
            annotation: h.annotation,
            ko_datetime: h.ko_datetime,
            reading_id: h.reading_id,
            source: h.source,
            created_at: h.created_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKindDto {
    #[default]
    Note,
    Session,
    Reflection,
    Review,
}

impl From<NoteKindDto> for NoteKind {
    fn from(k: NoteKindDto) -> Self {
        match k {
            NoteKindDto::Note => NoteKind::Note,
            NoteKindDto::Session => NoteKind::Session,
            NoteKindDto::Reflection => NoteKind::Reflection,
            NoteKindDto::Review => NoteKind::Review,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteDto {
    pub id: i64,
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub reading_id: Option<i64>,
    #[serde(default)]
    pub highlight_id: Option<i64>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub location: Option<String>,
    /// Relative to the vault root; `note_path` is the absolute path.
    pub file_path: String,
    pub title: String,
    /// `note` | `session` | `reflection` | `review`.
    pub kind: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds; stamped by the engine on every insert and body save.
    #[serde(default)]
    pub last_modified: Option<i64>,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResurfaceReasonDto {
    OpenBook,
    Notebook,
}

impl From<bookmole::ResurfaceReason> for ResurfaceReasonDto {
    fn from(r: bookmole::ResurfaceReason) -> Self {
        match r {
            bookmole::ResurfaceReason::OpenBook => ResurfaceReasonDto::OpenBook,
            bookmole::ResurfaceReason::Notebook => ResurfaceReasonDto::Notebook,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResurfacedDto {
    pub note: NoteDto,
    pub reason: ResurfaceReasonDto,
}

impl From<bookmole::Resurfaced> for ResurfacedDto {
    fn from(r: bookmole::Resurfaced) -> Self {
        ResurfacedDto {
            note: r.note.into(),
            reason: r.reason.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResurfacedPassageDto {
    pub highlight: HighlightDto,
    pub reason: ResurfaceReasonDto,
}

impl From<bookmole::ResurfacedPassage> for ResurfacedPassageDto {
    fn from(r: bookmole::ResurfacedPassage) -> Self {
        ResurfacedPassageDto {
            highlight: r.highlight.into(),
            reason: r.reason.into(),
        }
    }
}

impl From<NoteRecord> for NoteDto {
    fn from(n: NoteRecord) -> Self {
        NoteDto {
            id: n.id,
            book_id: n.book_id,
            reading_id: n.reading_id,
            highlight_id: n.highlight_id,
            page: n.page,
            location: n.location,
            file_path: n.file_path,
            title: n.title,
            kind: n.kind,
            created_at: n.created_at.map(|t| t.unix_timestamp()),
            last_modified: n.last_modified.map(|t| t.unix_timestamp()),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewNoteDto {
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub reading_id: Option<i64>,
    #[serde(default)]
    pub highlight_id: Option<i64>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub kind: NoteKindDto,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: String,
}

impl From<NewNoteDto> for NewNoteInput {
    fn from(d: NewNoteDto) -> Self {
        NewNoteInput {
            book_id: d.book_id,
            reading_id: d.reading_id,
            highlight_id: d.highlight_id,
            page: d.page,
            location: d.location,
            kind: d.kind.into(),
            title: d.title,
            body: d.body,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedNoteDto {
    pub id: i64,
    pub title: String,
    /// Absolute path of the markdown file.
    pub file: String,
    pub links: Vec<String>,
}

impl From<CreatedNote> for CreatedNoteDto {
    fn from(n: CreatedNote) -> Self {
        CreatedNoteDto {
            id: n.id,
            title: n.title,
            file: path_str(&n.file),
            links: n.links,
        }
    }
}

/// Which index a search covers, or `null` for both. A filter; ordering is never the caller's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSourceDto {
    Note,
    Highlight,
}

impl From<SearchSourceDto> for SearchSource {
    fn from(s: SearchSourceDto) -> Self {
        match s {
            SearchSourceDto::Note => SearchSource::Note,
            SearchSourceDto::Highlight => SearchSource::Highlight,
        }
    }
}

/// One hit in the ranked list, tagged on `kind`; exactly one of note or highlight is present.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchHitDto {
    Note {
        note: NoteDto,
        /// The matching text with `>>` and `<<` around the terms.
        snippet: String,
    },
    Highlight {
        highlight: HighlightDto,
        snippet: String,
    },
}

impl From<SearchHit> for SearchHitDto {
    fn from(h: SearchHit) -> Self {
        match h {
            SearchHit::Note { note, snippet } => SearchHitDto::Note {
                note: note.into(),
                snippet,
            },
            SearchHit::Highlight { highlight, snippet } => SearchHitDto::Highlight {
                highlight: highlight.into(),
                snippet,
            },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingLinkDto {
    pub target_title: String,
    /// The note it resolves to; `null` is a forward reference, not an error.
    #[serde(default)]
    pub note: Option<NoteDto>,
}

impl From<OutgoingLink> for OutgoingLinkDto {
    fn from(l: OutgoingLink) -> Self {
        OutgoingLinkDto {
            target_title: l.target_title,
            note: l.to.map(Into::into),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkDirectionDto {
    Incoming,
    Outgoing,
}

impl From<LinkDirection> for LinkDirectionDto {
    fn from(d: LinkDirection) -> Self {
        match d {
            LinkDirection::Incoming => Self::Incoming,
            LinkDirection::Outgoing => Self::Outgoing,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFaceDto {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub cover_shelf_path: Option<String>,
}

impl From<Book> for BookFaceDto {
    fn from(b: Book) -> Self {
        let cover_shelf_path = b.shelf_cover_path().map(str::to_string);
        BookFaceDto {
            id: b.id.unwrap_or_default(),
            title: b.title,
            cover_path: b.cover_path,
            cover_shelf_path,
        }
    }
}

/// One link of a note, either direction. `paragraph` is the reader's own writing; never log
/// above `trace!`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkContextDto {
    pub direction: LinkDirectionDto,
    /// The `[[wikilink]]` target as written; for an outgoing forward reference, the only name the
    /// other note has.
    pub target_title: String,
    /// The other note; `null` only for an outgoing forward reference.
    #[serde(default)]
    pub note: Option<NoteDto>,
    #[serde(default)]
    pub book: Option<BookFaceDto>,
    /// Incoming only: the paragraph holding the link, cut to about 600 bytes on a char boundary;
    /// `null` otherwise.
    #[serde(default)]
    pub paragraph: Option<String>,
    /// Byte offsets `[start, end)` of the `[[...]]` token in `paragraph`; bytes, not UTF-16 units.
    /// Present iff `paragraph` is.
    #[serde(default)]
    pub link_range: Option<(usize, usize)>,
    /// Some of the paragraph was cut, at either end.
    #[serde(default)]
    pub truncated: bool,
    /// The cut took the paragraph's head. Implies `truncated`.
    #[serde(default)]
    pub truncated_start: bool,
}

impl From<LinkContext> for LinkContextDto {
    fn from(l: LinkContext) -> Self {
        let (paragraph, link_range, truncated, truncated_start) = match l.paragraph {
            Some(p) => (
                Some(p.text),
                Some(p.link_range),
                p.truncated,
                p.truncated_start,
            ),
            None => (None, None, false, false),
        };
        LinkContextDto {
            direction: l.direction.into(),
            target_title: l.target_title,
            note: l.note.map(Into::into),
            book: l.book.map(Into::into),
            paragraph,
            link_range,
            truncated,
            truncated_start,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenReadingDto {
    pub book: BookDto,
    pub reading: ReadingDto,
}

impl From<(Book, Reading)> for OpenReadingDto {
    fn from((book, reading): (Book, Reading)) -> Self {
        let progress = Progress::of_reading(&reading, book.page_count);
        OpenReadingDto {
            book: book.into(),
            reading: ReadingDto::new(reading, progress),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingSortDto {
    /// Most recently finished first; open readings last.
    #[default]
    Finished,
    Started,
    LastModified,
}

impl From<ReadingSortDto> for ReadingSort {
    fn from(s: ReadingSortDto) -> Self {
        match s {
            ReadingSortDto::Finished => ReadingSort::Finished,
            ReadingSortDto::Started => ReadingSort::Started,
            ReadingSortDto::LastModified => ReadingSort::LastModified,
        }
    }
}

/// Which readings a list is about. Every field absent means every reading.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingFilterDto {
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub status: Option<ReadingStateDto>,
    /// `true` is a reading still open. Abandoned readings stay open, so this is not redundant with
    /// `status`.
    #[serde(default)]
    pub open: Option<bool>,
    /// Readings that finished inside this span of days, UTC; an inverted span is `InvalidInput`.
    #[serde(default)]
    pub finished_in: Option<DayRangeDto>,
}

impl TryFrom<ReadingFilterDto> for ReadingFilter {
    type Error = bookmole::EngineError;

    // Fallible because the engine validates the day range; this layer must not route around that.
    fn try_from(f: ReadingFilterDto) -> Result<Self, Self::Error> {
        Ok(ReadingFilter {
            book_id: f.book_id,
            status: f.status.map(Into::into),
            open: f.open,
            finished_in: f
                .finished_in
                .map(|r| DayRange::new(&r.from, &r.to))
                .transpose()?,
        })
    }
}

/// A page of the library's readings: filter, order and slice. The request carries these flat.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingQueryDto {
    #[serde(default)]
    pub sort: ReadingSortDto,
    #[serde(default)]
    pub filter: Option<ReadingFilterDto>,
    /// Negative means no limit. No serde default: an omitted `0` would silently be an empty page.
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

impl Default for ReadingQueryDto {
    // Hand-written: a derived `Default` would give `limit: 0` against the engine's `-1`.
    fn default() -> Self {
        let q = ReadingQuery::default();
        ReadingQueryDto {
            sort: ReadingSortDto::default(),
            filter: None,
            limit: q.limit,
            offset: q.offset,
        }
    }
}

impl TryFrom<ReadingQueryDto> for ReadingQuery {
    type Error = bookmole::EngineError;

    fn try_from(q: ReadingQueryDto) -> Result<Self, Self::Error> {
        Ok(ReadingQuery {
            sort: q.sort.into(),
            filter: q
                .filter
                .map(TryInto::try_into)
                .transpose()?
                .unwrap_or_default(),
            limit: q.limit,
            offset: q.offset,
        })
    }
}

/// One row of the library-wide readings list.
/// The book's progress fields describe its current read; `reading.progress` is this read's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingRowDto {
    pub book: BookDto,
    pub reading: ReadingDto,
    /// Which read this is, 1-based, oldest first over every reading of the book, never over the
    /// page. Never optional.
    pub read_number: i64,
    pub of_reads: i64,
    /// The passage a card shows for this reading, or `null` when its highlights are all unattributed.
    #[serde(default)]
    pub passage: Option<HighlightDto>,
}

impl From<ReadingRow> for ReadingRowDto {
    fn from(r: ReadingRow) -> Self {
        ReadingRowDto {
            book: r.book.into(),
            reading: ReadingDto::new(r.reading, r.progress),
            read_number: r.count.number,
            of_reads: r.count.of,
            passage: r.passage.map(Into::into),
        }
    }
}

/// Years a wall's readings ended in, and whether any is still open. Open readings are in no year.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingYearsDto {
    /// Newest first; a year is listed iff a matching reading closed in it.
    pub years: Vec<i32>,
    pub open: bool,
}

impl From<ReadingYears> for ReadingYearsDto {
    fn from(y: ReadingYears) -> Self {
        ReadingYearsDto {
            years: y.years,
            open: y.open,
        }
    }
}

/// One row of a note list. `excerpt` is the reader's own writing, about 280 bytes on a char
/// boundary; never log it.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteRowDto {
    pub note: NoteDto,
    #[serde(default)]
    pub book: Option<BookFaceDto>,
    /// `null` unless the body opens with `# heading` or the title was typed; always `null` for
    /// reflections and reviews.
    #[serde(default)]
    pub heading: Option<String>,
    pub excerpt: String,
    /// `1` for an ordinary note.
    pub parts: i64,
    /// Unix seconds: the newest part's date; the list's order.
    pub latest_part_at: i64,
    /// The latest part's page, else the note's.
    #[serde(default)]
    pub latest_part_page: Option<i64>,
    /// Distinct targets the body names, written or not.
    pub links_out: i64,
    pub links_in: i64,
    /// The hub note id of its thread; `null` when it links nothing and nothing links it.
    #[serde(default)]
    pub thread: Option<i64>,
    /// Which read of its book the note's reading is (1-based, oldest first); `null` for a note
    /// that names no reading.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub read_number: Option<i64>,
    /// How many reads that book has; `null` beside a `null` `read_number`.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub of_reads: Option<i64>,
}

impl From<bookmole::NoteRow> for NoteRowDto {
    fn from(r: bookmole::NoteRow) -> Self {
        NoteRowDto {
            note: r.note.into(),
            book: r.book.map(Into::into),
            heading: r.heading,
            excerpt: r.excerpt,
            parts: r.parts,
            latest_part_at: r.latest_part_at,
            latest_part_page: r.latest_part_page,
            links_out: r.links_out,
            links_in: r.links_in,
            thread: r.thread,
            read_number: r.read.map(|c| c.number),
            of_reads: r.read.map(|c| c.of),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotePeriodDto {
    pub year: i32,
    /// 1-12.
    pub month: u8,
}

/// A thread (connected component of resolved links, at least two notes). Its id is its hub's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDto {
    /// The member with the most distinct neighbours (ties to the newest, then the lowest id).
    pub hub: NoteRowDto,
    pub notes: i64,
    /// Distinct, in order of first appearance among members newest first.
    pub books: Vec<BookFaceDto>,
    /// Members whose latest part is at or after `since`; `0` without one.
    pub joined: i64,
    pub members: Vec<i64>,
}

impl From<bookmole::ThreadSummary> for ThreadDto {
    fn from(t: bookmole::ThreadSummary) -> Self {
        ThreadDto {
            hub: t.hub.into(),
            notes: t.notes,
            books: map(t.books),
            joined: t.joined,
            members: t.members,
        }
    }
}

/// One wikilink a note wrote. `to` is `null` for a note not yet written.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLinkDto {
    pub from: NoteRowDto,
    pub target_title: String,
    #[serde(default)]
    pub to: Option<NoteDto>,
}

impl From<bookmole::NoteLink> for NoteLinkDto {
    fn from(l: bookmole::NoteLink) -> Self {
        NoteLinkDto {
            from: l.from.into(),
            target_title: l.target_title,
            to: l.to.map(Into::into),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GraphScopeDto {
    Everything,
    Recent {
        since: i64,
    },
    Book {
        book_id: i64,
    },
    /// One note and its neighbours out to `steps` (1 or 2, else `invalid_input`), plus up to five
    /// unlinked mentions.
    Around {
        note_id: i64,
        steps: u8,
    },
}

impl From<GraphScopeDto> for bookmole::GraphScope {
    fn from(s: GraphScopeDto) -> Self {
        use bookmole::GraphScope as G;
        match s {
            GraphScopeDto::Everything => G::Everything,
            GraphScopeDto::Recent { since } => G::Recent { since },
            GraphScopeDto::Book { book_id } => G::Book { book_id },
            GraphScopeDto::Around { note_id, steps } => G::Around { note_id, steps },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKindDto {
    Link,
    /// `from` mentions `to`'s title without linking it; `around` only.
    Mention,
}

impl From<bookmole::GraphEdgeKind> for GraphEdgeKindDto {
    fn from(k: bookmole::GraphEdgeKind) -> Self {
        match k {
            bookmole::GraphEdgeKind::Link => GraphEdgeKindDto::Link,
            bookmole::GraphEdgeKind::Mention => GraphEdgeKindDto::Mention,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeDto {
    /// `n<id>` for a note, `t<lowercased title>` for an unwritten link; opaque except as edges name it.
    pub key: String,
    /// `null` for an unwritten link.
    #[serde(default)]
    pub row: Option<NoteRowDto>,
    pub title: String,
    /// Distinct nodes it shares an edge with in this graph.
    pub degree: i64,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeDto {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKindDto,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteGraphDto {
    pub nodes: Vec<GraphNodeDto>,
    /// Deduped per `(from, to)`; every end is a node in `nodes`.
    pub edges: Vec<GraphEdgeDto>,
}

impl From<bookmole::NoteGraphView> for NoteGraphDto {
    fn from(g: bookmole::NoteGraphView) -> Self {
        NoteGraphDto {
            nodes: g
                .nodes
                .into_iter()
                .map(|n| GraphNodeDto {
                    key: n.key,
                    row: n.row.map(Into::into),
                    title: n.title,
                    degree: n.degree,
                })
                .collect(),
            edges: g
                .edges
                .into_iter()
                .map(|e| GraphEdgeDto {
                    from: e.from,
                    to: e.to,
                    kind: e.kind.into(),
                })
                .collect(),
        }
    }
}

/// One dated part of a note. `n` is the 1-based position `UpdateNotePart` addresses. `body` is
/// the reader's writing.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotePartDto {
    pub n: i64,
    /// Unix seconds.
    pub at: i64,
    #[serde(default)]
    pub page: Option<i64>,
    pub body: String,
}

impl From<bookmole::NotePart> for NotePartDto {
    fn from(p: bookmole::NotePart) -> Self {
        NotePartDto {
            n: p.n,
            at: p.at,
            page: p.page,
            body: p.body,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreviewDto {
    pub outgoing: Vec<OutgoingLinkDto>,
    pub incoming: Vec<LinkContextDto>,
}

impl From<bookmole::LinkPreview> for LinkPreviewDto {
    fn from(p: bookmole::LinkPreview) -> Self {
        LinkPreviewDto {
            outgoing: map(p.outgoing),
            incoming: map(p.incoming),
        }
    }
}

fn map<T, U: From<T>>(items: Vec<T>) -> Vec<U> {
    items.into_iter().map(U::from).collect()
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingScaleDto {
    pub id: i64,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl From<RatingScale> for RatingScaleDto {
    fn from(s: RatingScale) -> Self {
        RatingScaleDto {
            id: s.id,
            name: s.name,
            min: s.min,
            max: s.max,
            step: s.step,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingDto {
    /// The scale travels with the value: the Goodreads map is user-editable, so a bare number is
    /// not re-derivable.
    pub scale: RatingScaleDto,
    pub value: f64,
}

impl From<Rating> for RatingDto {
    fn from(r: Rating) -> Self {
        RatingDto {
            scale: r.scale.into(),
            value: r.value,
        }
    }
}

/// One entry of the Goodreads lookup table; a scale point with no entry is not listed.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingMapEntryDto {
    pub value: f64,
    /// 0-5 in quarters.
    pub goodreads: f64,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFileDto {
    pub sha256: String,
    pub book_id: i64,
    pub format: String,
    #[serde(default)]
    pub original_name: Option<String>,
    pub size: i64,
    pub added_at: i64,
}

impl From<BookFile> for BookFileDto {
    fn from(f: BookFile) -> Self {
        BookFileDto {
            sha256: f.sha256,
            book_id: f.book_id,
            format: f.format,
            original_name: f.original_name,
            size: f.size,
            added_at: f.added_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMatchDto {
    Sha256,
    Isbn,
    Md5,
    Title,
}

impl From<FileMatch> for FileMatchDto {
    fn from(m: FileMatch) -> Self {
        match m {
            FileMatch::Sha256 => FileMatchDto::Sha256,
            FileMatch::Isbn => FileMatchDto::Isbn,
            FileMatch::Md5 => FileMatchDto::Md5,
            FileMatch::Title => FileMatchDto::Title,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOutcomeDto {
    Stored,
    AlreadyOwned,
    Unmatched,
}

impl From<FileOutcome> for FileOutcomeDto {
    fn from(o: FileOutcome) -> Self {
        match o {
            FileOutcome::Stored => FileOutcomeDto::Stored,
            FileOutcome::AlreadyOwned => FileOutcomeDto::AlreadyOwned,
            FileOutcome::Unmatched => FileOutcomeDto::Unmatched,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileIdentityDto {
    pub path: String,
    pub sha256: String,
    pub partial_md5: String,
    pub format: String,
    pub size: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// Page count the file declares (pdf only). Absent is not zero.
    #[serde(default)]
    pub page_count: Option<i64>,
    #[serde(default)]
    pub matched_book_id: Option<i64>,
    #[serde(default)]
    pub matched_by: Option<FileMatchDto>,
    #[serde(default)]
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<FileIdentity> for FileIdentityDto {
    // The domain's `(i64, FileMatch)` tuple becomes two named nullable fields so a later element
    // cannot shift positions.
    fn from(i: FileIdentity) -> Self {
        FileIdentityDto {
            path: path_str(&i.path),
            sha256: i.sha256,
            partial_md5: i.partial_md5,
            format: i.format,
            size: i.size,
            title: i.title,
            page_count: i.page_count,
            matched_book_id: i.matched.map(|(id, _)| id),
            matched_by: i.matched.map(|(_, m)| m.into()),
            candidates: i.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileImportReportDto {
    pub outcome: FileOutcomeDto,
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub matched_by: Option<FileMatchDto>,
    pub created_book: bool,
    pub sha256: String,
    #[serde(default)]
    pub stored_path: Option<String>,
    #[serde(default)]
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<FileImportReport> for FileImportReportDto {
    fn from(r: FileImportReport) -> Self {
        FileImportReportDto {
            outcome: r.outcome.into(),
            book_id: r.book_id,
            matched_by: r.matched_by.map(Into::into),
            created_book: r.created_book,
            sha256: r.sha256,
            stored_path: opt_path(&r.stored_path),
            candidates: r.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashcardDto {
    pub id: i64,
    /// The book this card was captured from. No serde default: `0` is not a book.
    pub book_id: i64,
    /// The passage the word came from; `null` or absent means the card is not anchored.
    #[serde(default)]
    pub highlight_id: Option<i64>,
    pub word: String,
    #[serde(default)]
    pub context: Option<String>,
    pub book_title: String,
    pub exported: bool,
}

impl From<FlashcardRow> for FlashcardDto {
    fn from(c: FlashcardRow) -> Self {
        FlashcardDto {
            id: c.id,
            book_id: c.book_id,
            highlight_id: c.highlight_id,
            word: c.word,
            context: c.context,
            book_title: c.book_title,
            exported: c.exported,
        }
    }
}

/// One Anki export as text: contents, card count and a name to save under. The caller owns the
/// file.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnkiExportDto {
    pub text: String,
    pub count: usize,
    pub filename: String,
}

impl From<AnkiExport> for AnkiExportDto {
    fn from(e: AnkiExport) -> Self {
        AnkiExportDto {
            text: e.text,
            count: e.count,
            filename: e.filename,
        }
    }
}

/// An Anki export written into `exports/` beside the vault; `path` is `null` when no card went out.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAnkiExportDto {
    pub path: Option<String>,
    pub count: usize,
}

impl From<SavedAnkiExport> for SavedAnkiExportDto {
    fn from(e: SavedAnkiExport) -> Self {
        SavedAnkiExportDto {
            path: opt_path(&e.path),
            count: e.count,
        }
    }
}

/// Ids of the passages one note cites, not rows, so private text is not resent per citing note.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteCitationsDto {
    pub note_id: i64,
    /// In book reading order. Empty is an answer.
    pub highlight_ids: Vec<i64>,
}

impl From<NoteCitations> for NoteCitationsDto {
    fn from(c: NoteCitations) -> Self {
        NoteCitationsDto {
            note_id: c.note_id,
            highlight_ids: c.highlight_ids,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequestDto {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub translator: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub isbn: Option<String>,
    #[serde(default)]
    pub limit: u32,
}

impl From<SearchRequestDto> for SearchRequest {
    fn from(d: SearchRequestDto) -> Self {
        SearchRequest {
            query: d.query,
            title: d.title,
            author: d.author,
            publisher: d.publisher,
            translator: d.translator,
            language: d.language,
            year: d.year,
            isbn: d.isbn,
            limit: d.limit,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderIdDto {
    OpenLibrary,
    GoogleBooks,
}

impl From<ProviderId> for ProviderIdDto {
    fn from(p: ProviderId) -> Self {
        match p {
            ProviderId::OpenLibrary => ProviderIdDto::OpenLibrary,
            ProviderId::GoogleBooks => ProviderIdDto::GoogleBooks,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedResultDto {
    pub book: BookDto,
    pub sources: Vec<ProviderIdDto>,
    pub score: f64,
    /// The library's copy of this book, matched on ISBN; tells a frontend whether choosing it adds
    /// or opens.
    #[serde(default)]
    pub shelf_id: Option<i64>,
}

impl From<RankedResult> for RankedResultDto {
    fn from(r: RankedResult) -> Self {
        RankedResultDto {
            book: r.book.into(),
            sources: r.sources.into_iter().map(Into::into).collect(),
            score: r.score,
            shelf_id: r.shelf_id,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SearchOutcomeDto {
    pub results: Vec<RankedResultDto>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<SearchOutcome> for SearchOutcomeDto {
    fn from(o: SearchOutcome) -> Self {
        SearchOutcomeDto {
            results: o.results.into_iter().map(Into::into).collect(),
            warnings: o.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityDto {
    Warning,
    Error,
}

impl From<Severity> for SeverityDto {
    fn from(s: Severity) -> Self {
        match s {
            Severity::Warning => SeverityDto::Warning,
            Severity::Error => SeverityDto::Error,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClassDto {
    Network,
    Timeout,
    RateLimited,
    Decode,
    Parse,
    Io,
    #[serde(other)]
    Other,
}

impl From<ErrorClass> for ErrorClassDto {
    fn from(c: ErrorClass) -> Self {
        match c {
            ErrorClass::Network => ErrorClassDto::Network,
            ErrorClass::Timeout => ErrorClassDto::Timeout,
            ErrorClass::RateLimited => ErrorClassDto::RateLimited,
            ErrorClass::Decode => ErrorClassDto::Decode,
            ErrorClass::Parse => ErrorClassDto::Parse,
            ErrorClass::Io => ErrorClassDto::Io,
            ErrorClass::Other => ErrorClassDto::Other,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagnosticKindDto {
    ProviderFailed {
        provider: ProviderIdDto,
        class: ErrorClassDto,
    },
    ProviderTimedOut {
        provider: ProviderIdDto,
        after_secs: u64,
    },
    ProviderRateLimited {
        provider: ProviderIdDto,
        /// Seconds until it is asked again, rounded up.
        retry_after_secs: u64,
    },
    /// The provider did not answer and a cached answer was shown; `fetched_at` is unix seconds.
    ProviderAnsweredFromCache {
        provider: ProviderIdDto,
        fetched_at: i64,
    },
    KeptRecordUnreadable {
        provider: ProviderIdDto,
    },
    /// The offline index of well-known works could not be used; `class` is null when this build
    /// has none.
    OfflineIndexUnavailable {
        class: Option<ErrorClassDto>,
    },
    SidecarUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    SidecarUnparsable {
        path: String,
    },
    NoSidecarsFound {
        path: String,
    },
    UnknownDeviceStatus {
        path: String,
        status: String,
    },
    SidecarNotIdentified {
        path: String,
    },
    /// Reported once per file, however many highlights it names.
    SidecarAnchorsUnsupported {
        path: String,
        entries: usize,
    },
    GoodreadsRowSkipped {
        row: usize,
    },
    GoodreadsReviewDiverged {
        title: String,
    },
    GoodreadsRatingDiverged {
        title: String,
    },
    GoodreadsUnanchoredReview {
        title: String,
    },
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
    CalibreRowSkipped {
        calibre_id: i64,
    },
    CalibreRowNotIdentified {
        calibre_id: i64,
    },
    CalibreCoverUnreadable {
        path: String,
    },
    // Five variants rather than one "unavailable": absence is ordinary here and the next move
    // differs for each.
    StatisticsDbAbsent {
        path: String,
    },
    StatisticsDbUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    StatisticsSchemaUnknown {
        path: String,
        version: i64,
    },
    StatisticsBookUnmatched {
        md5: String,
    },
    StatisticsBookNotIdentified {
        title: String,
    },
    /// A provider's cover URL would not download; the URL is not carried across the wire.
    CoverUnavailable {
        class: ErrorClassDto,
    },
    /// A PDF's annotations could not be read; nothing was imported or removed.
    PdfUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    PdfEncrypted {
        path: String,
    },
    /// Highlights whose annotation is gone from the PDF but carry the reader's own text, so they
    /// were kept.
    PdfAnnotationsKept {
        path: String,
        count: u32,
    },
    /// The PDF was read only in part, so nothing was removed.
    PdfScanIncomplete {
        path: String,
    },
    /// No Apple Books database in the folder searched; not an error.
    AppleBooksAbsent {
        path: String,
    },
    AppleBooksPermissionDenied {
        path: String,
    },
    AppleBooksDbUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    AppleBooksSchemaUnknown {
        path: String,
        table: String,
    },
    /// A table lacks optional columns; its rows imported without them.
    AppleBooksColumnsMissing {
        path: String,
        table: String,
        columns: Vec<String>,
    },
    AppleBooksHighlightsKept {
        asset_id: String,
        count: u32,
    },
    KindleClippingsUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    /// Entries in a language or layout this build does not read, or broken; one per file, not imported.
    KindleEntriesUnparsed {
        path: String,
        count: u32,
    },
    /// A clippings title with no matching book; its entries wait to be attached.
    KindleTitleUnmatched {
        title: String,
        entries: u32,
    },
    KindleNotebookUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    KindleNotebookUnmatched {
        path: String,
        title: Option<String>,
    },
}

impl From<DiagnosticKind> for DiagnosticKindDto {
    fn from(k: DiagnosticKind) -> Self {
        use DiagnosticKind as K;
        match k {
            K::ProviderFailed { provider, class } => DiagnosticKindDto::ProviderFailed {
                provider: provider.into(),
                class: class.into(),
            },
            K::ProviderTimedOut { provider, after } => DiagnosticKindDto::ProviderTimedOut {
                provider: provider.into(),
                after_secs: after.as_secs(),
            },
            K::ProviderRateLimited {
                provider,
                retry_after,
            } => DiagnosticKindDto::ProviderRateLimited {
                provider: provider.into(),
                retry_after_secs: retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0),
            },
            K::ProviderAnsweredFromCache {
                provider,
                fetched_at,
            } => DiagnosticKindDto::ProviderAnsweredFromCache {
                provider: provider.into(),
                fetched_at,
            },
            K::KeptRecordUnreadable { provider } => DiagnosticKindDto::KeptRecordUnreadable {
                provider: provider.into(),
            },
            K::OfflineIndexUnavailable { class } => DiagnosticKindDto::OfflineIndexUnavailable {
                class: class.map(Into::into),
            },
            K::SidecarUnreadable { path, class } => DiagnosticKindDto::SidecarUnreadable {
                path: path_str(&path),
                class: class.into(),
            },
            K::SidecarUnparsable { path } => DiagnosticKindDto::SidecarUnparsable {
                path: path_str(&path),
            },
            K::NoSidecarsFound { path } => DiagnosticKindDto::NoSidecarsFound {
                path: path_str(&path),
            },
            K::UnknownDeviceStatus { path, status } => DiagnosticKindDto::UnknownDeviceStatus {
                path: path_str(&path),
                status,
            },
            K::SidecarNotIdentified { path } => DiagnosticKindDto::SidecarNotIdentified {
                path: path_str(&path),
            },
            K::SidecarAnchorsUnsupported { path, entries } => {
                DiagnosticKindDto::SidecarAnchorsUnsupported {
                    path: path_str(&path),
                    entries,
                }
            }
            K::GoodreadsRowSkipped { row } => DiagnosticKindDto::GoodreadsRowSkipped { row },
            K::GoodreadsReviewDiverged { title } => {
                DiagnosticKindDto::GoodreadsReviewDiverged { title }
            }
            K::GoodreadsRatingDiverged { title } => {
                DiagnosticKindDto::GoodreadsRatingDiverged { title }
            }
            K::GoodreadsUnanchoredReview { title } => {
                DiagnosticKindDto::GoodreadsUnanchoredReview { title }
            }
            K::GoodreadsUndatableRereads { title, dropped } => {
                DiagnosticKindDto::GoodreadsUndatableRereads { title, dropped }
            }
            K::GoodreadsRatingUnmapped { title } => {
                DiagnosticKindDto::GoodreadsRatingUnmapped { title }
            }
            K::GoodreadsRereadsDropped { title, dropped } => {
                DiagnosticKindDto::GoodreadsRereadsDropped { title, dropped }
            }
            K::CalibreRowSkipped { calibre_id } => {
                DiagnosticKindDto::CalibreRowSkipped { calibre_id }
            }
            K::CalibreRowNotIdentified { calibre_id } => {
                DiagnosticKindDto::CalibreRowNotIdentified { calibre_id }
            }
            K::CalibreCoverUnreadable { path } => DiagnosticKindDto::CalibreCoverUnreadable {
                path: path_str(&path),
            },
            K::StatisticsDbAbsent { path } => DiagnosticKindDto::StatisticsDbAbsent {
                path: path_str(&path),
            },
            K::StatisticsDbUnreadable { path, class } => {
                DiagnosticKindDto::StatisticsDbUnreadable {
                    path: path_str(&path),
                    class: class.into(),
                }
            }
            K::StatisticsSchemaUnknown { path, version } => {
                DiagnosticKindDto::StatisticsSchemaUnknown {
                    path: path_str(&path),
                    version,
                }
            }
            K::StatisticsBookUnmatched { md5 } => {
                DiagnosticKindDto::StatisticsBookUnmatched { md5 }
            }
            K::StatisticsBookNotIdentified { title } => {
                DiagnosticKindDto::StatisticsBookNotIdentified { title }
            }
            K::CoverUnavailable { class } => DiagnosticKindDto::CoverUnavailable {
                class: class.into(),
            },
            K::PdfUnreadable { path, class } => DiagnosticKindDto::PdfUnreadable {
                path: path_str(&path),
                class: class.into(),
            },
            K::PdfEncrypted { path } => DiagnosticKindDto::PdfEncrypted {
                path: path_str(&path),
            },
            K::PdfAnnotationsKept { path, count } => DiagnosticKindDto::PdfAnnotationsKept {
                path: path_str(&path),
                count: u32::try_from(count).unwrap_or(u32::MAX),
            },
            K::PdfScanIncomplete { path } => DiagnosticKindDto::PdfScanIncomplete {
                path: path_str(&path),
            },
            K::AppleBooksAbsent { path } => DiagnosticKindDto::AppleBooksAbsent {
                path: path_str(&path),
            },
            K::AppleBooksPermissionDenied { path } => {
                DiagnosticKindDto::AppleBooksPermissionDenied {
                    path: path_str(&path),
                }
            }
            K::AppleBooksDbUnreadable { path, class } => {
                DiagnosticKindDto::AppleBooksDbUnreadable {
                    path: path_str(&path),
                    class: class.into(),
                }
            }
            K::AppleBooksSchemaUnknown { path, table } => {
                DiagnosticKindDto::AppleBooksSchemaUnknown {
                    path: path_str(&path),
                    table,
                }
            }
            K::AppleBooksColumnsMissing {
                path,
                table,
                columns,
            } => DiagnosticKindDto::AppleBooksColumnsMissing {
                path: path_str(&path),
                table,
                columns,
            },
            K::AppleBooksHighlightsKept { asset_id, count } => {
                DiagnosticKindDto::AppleBooksHighlightsKept {
                    asset_id,
                    count: u32::try_from(count).unwrap_or(u32::MAX),
                }
            }
            K::KindleClippingsUnreadable { path, class } => {
                DiagnosticKindDto::KindleClippingsUnreadable {
                    path: path_str(&path),
                    class: class.into(),
                }
            }
            K::KindleEntriesUnparsed { path, count } => DiagnosticKindDto::KindleEntriesUnparsed {
                path: path_str(&path),
                count: u32::try_from(count).unwrap_or(u32::MAX),
            },
            K::KindleTitleUnmatched { title, entries } => DiagnosticKindDto::KindleTitleUnmatched {
                title,
                entries: u32::try_from(entries).unwrap_or(u32::MAX),
            },
            K::KindleNotebookUnreadable { path, class } => {
                DiagnosticKindDto::KindleNotebookUnreadable {
                    path: path_str(&path),
                    class: class.into(),
                }
            }
            K::KindleNotebookUnmatched { path, title } => {
                DiagnosticKindDto::KindleNotebookUnmatched {
                    path: path_str(&path),
                    title,
                }
            }
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticDto {
    #[serde(flatten)]
    pub kind: DiagnosticKindDto,
    pub severity: SeverityDto,
    pub detail: String,
    /// `Diagnostic`'s `Display`, byte-for-byte what the CLI prints, so clients do not re-derive it.
    pub display: String,
}

impl From<Diagnostic> for DiagnosticDto {
    fn from(d: Diagnostic) -> Self {
        DiagnosticDto {
            display: d.to_string(),
            kind: d.kind.into(),
            severity: d.severity.into(),
            detail: d.detail,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
/// A library book offered as a choice when an import cannot match; a client sends `book_id`
/// back to link it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchCandidateDto {
    pub book_id: i64,
    pub title: String,
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// The year, where the record has one; separates same-author variants.
    #[serde(default)]
    pub publish_year: Option<i64>,
    pub score: f64,
}

impl From<MatchCandidate> for MatchCandidateDto {
    fn from(c: MatchCandidate) -> Self {
        MatchCandidateDto {
            book_id: c.book_id,
            title: c.title,
            authors_display: c.authors_display,
            publish_year: c.publish_year,
            score: c.score,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMethodDto {
    Md5,
    Isbn,
    Title,
    New,
}

impl From<MatchMethod> for MatchMethodDto {
    fn from(m: MatchMethod) -> Self {
        match m {
            MatchMethod::Md5 => MatchMethodDto::Md5,
            MatchMethod::Isbn => MatchMethodDto::Isbn,
            MatchMethod::Title => MatchMethodDto::Title,
            MatchMethod::New => MatchMethodDto::New,
        }
    }
}

/// The device's own status; `Other` keeps the raw string.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum KoStatusDto {
    Reading,
    Abandoned,
    Complete,
    Other { raw: String },
}

impl From<KoStatus> for KoStatusDto {
    fn from(s: KoStatus) -> Self {
        match s {
            KoStatus::Reading => KoStatusDto::Reading,
            KoStatus::Abandoned => KoStatusDto::Abandoned,
            KoStatus::Complete => KoStatusDto::Complete,
            KoStatus::Other(raw) => KoStatusDto::Other { raw },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookImportStatsDto {
    pub book_id: i64,
    pub book_title: String,
    pub inserted: usize,
    /// Rows whose device-owned fields the sidecar disagreed with; never includes `annotation`,
    /// which is ours.
    pub updated: usize,
    pub skipped: usize,
    pub flashcards: usize,
    pub matched_by: MatchMethodDto,
    #[serde(default)]
    pub percent_finished: Option<f64>,
    #[serde(default)]
    pub status: Option<KoStatusDto>,
    #[serde(default)]
    pub rating: Option<i64>,
}

impl From<BookImportStats> for BookImportStatsDto {
    fn from(s: BookImportStats) -> Self {
        BookImportStatsDto {
            book_id: s.book_id,
            book_title: s.book_title,
            inserted: s.inserted,
            updated: s.updated,
            skipped: s.skipped,
            flashcards: s.flashcards,
            matched_by: s.matched_by.into(),
            percent_finished: s.percent_finished,
            status: s.status.map(Into::into),
            rating: s.rating,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedSidecarDto {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub partial_md5: Option<String>,
    #[serde(default)]
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<UnmatchedSidecar> for UnmatchedSidecarDto {
    fn from(u: UnmatchedSidecar) -> Self {
        UnmatchedSidecarDto {
            path: path_str(&u.path),
            title: u.title,
            partial_md5: u.partial_md5,
            candidates: u.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ImportReportDto {
    pub imported: Vec<BookImportStatsDto>,
    pub unmatched: Vec<UnmatchedSidecarDto>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<ImportReport> for ImportReportDto {
    fn from(r: ImportReport) -> Self {
        ImportReportDto {
            imported: r.imported.into_iter().map(Into::into).collect(),
            unmatched: r.unmatched.into_iter().map(Into::into).collect(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullReportDto {
    pub stats: BookImportStatsDto,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<PullReport> for PullReportDto {
    fn from(r: PullReport) -> Self {
        PullReportDto {
            stats: r.stats.into(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceStateDto {
    New {
        candidates: Vec<MatchCandidateDto>,
    },
    Unchanged,
    Updated {
        new_highlights: usize,
        refreshed: usize,
    },
    Unreadable {
        diagnostic: DiagnosticDto,
    },
}

impl From<DeviceState> for DeviceStateDto {
    fn from(s: DeviceState) -> Self {
        match s {
            DeviceState::New { candidates } => DeviceStateDto::New {
                candidates: candidates.into_iter().map(Into::into).collect(),
            },
            DeviceState::Unchanged => DeviceStateDto::Unchanged,
            DeviceState::Updated {
                new_highlights,
                refreshed,
            } => DeviceStateDto::Updated {
                new_highlights,
                refreshed,
            },
            DeviceState::Unreadable(d) => DeviceStateDto::Unreadable {
                diagnostic: d.into(),
            },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceBookDto {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub authors: Option<String>,
    #[serde(default)]
    pub partial_md5: Option<String>,
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub matched_by: Option<MatchMethodDto>,
    pub state: DeviceStateDto,
    #[serde(default)]
    pub ko_percent: Option<f64>,
    #[serde(default)]
    pub ko_status: Option<KoStatusDto>,
}

impl From<DeviceBook> for DeviceBookDto {
    fn from(b: DeviceBook) -> Self {
        DeviceBookDto {
            path: path_str(&b.path),
            title: b.title,
            authors: b.authors,
            partial_md5: b.partial_md5,
            book_id: b.book_id,
            matched_by: b.matched_by.map(Into::into),
            state: b.state.into(),
            ko_percent: b.ko_percent,
            ko_status: b.ko_status.map(Into::into),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceScanDto {
    pub root: String,
    pub books: Vec<DeviceBookDto>,
    pub warnings: Vec<DiagnosticDto>,
    /// Sidecars this scan evaluated, and sidecars answered from the `sidecar_seen` cache.
    pub parsed: usize,
    pub cached: usize,
}

impl From<DeviceScan> for DeviceScanDto {
    fn from(s: DeviceScan) -> Self {
        DeviceScanDto {
            root: path_str(&s.root),
            books: s.books.into_iter().map(Into::into).collect(),
            warnings: s.warnings.into_iter().map(Into::into).collect(),
            parsed: s.parsed,
            cached: s.cached,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginConditionDto {
    Absent,
    Current,
    Upgradable,
    Unversioned,
    Obstructed,
}

impl From<PluginCondition> for PluginConditionDto {
    fn from(c: PluginCondition) -> Self {
        match c {
            PluginCondition::Absent => PluginConditionDto::Absent,
            PluginCondition::Current => PluginConditionDto::Current,
            PluginCondition::Upgradable => PluginConditionDto::Upgradable,
            PluginCondition::Unversioned => PluginConditionDto::Unversioned,
            PluginCondition::Obstructed => PluginConditionDto::Obstructed,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginStatusDto {
    pub mount: String,
    /// Exactly where an install would write; shown before asking.
    pub plugin_dir: String,
    pub installed: bool,
    pub installed_version: Option<i64>,
    pub our_version: i64,
    pub paired: bool,
    /// Our id for this reader; `None` when none of its pairings is ours.
    pub device_id: Option<String>,
    /// Every computer this reader is paired with, in file order. Carries no token.
    pub pairings: Vec<PairingDto>,
    pub modified: Vec<String>,
    pub unrecognised: Vec<String>,
    pub condition: PluginConditionDto,
}

impl From<PluginStatus> for PluginStatusDto {
    fn from(s: PluginStatus) -> Self {
        // Read before the struct is taken apart: `condition()` borrows fields the moves below consume.
        let condition = s.condition().into();
        PluginStatusDto {
            condition,
            mount: path_str(&s.mount),
            plugin_dir: path_str(&s.plugin_dir),
            installed: s.installed,
            installed_version: s.installed_version,
            our_version: s.our_version,
            paired: s.paired,
            device_id: s.device_id,
            pairings: s.pairings.into_iter().map(Into::into).collect(),
            modified: s.modified,
            unrecognised: s.unrecognised,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListenerModeDto {
    Off,
    /// Bound until `until` (unix seconds) or until a push completes, whichever is first.
    Window {
        until: i64,
    },
    Always,
}

impl From<ListenerMode> for ListenerModeDto {
    fn from(m: ListenerMode) -> Self {
        match m {
            ListenerMode::Off => ListenerModeDto::Off,
            ListenerMode::Window { until } => ListenerModeDto::Window { until },
            ListenerMode::Always => ListenerModeDto::Always,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerStatusDto {
    pub mode: ListenerModeDto,
    pub tcp_port: Option<u16>,
    /// Completed pushes since this host started; a count of things done, never of things waiting.
    pub pushes: u64,
    pub last_push_at: Option<i64>,
    /// This computer's LAN address, for a person to type into a reader. A hint: nothing may store
    /// it or join on it.
    pub lan_address: Option<String>,
}

impl ListenerStatusDto {
    // Not a `From<ListenerStatus>`: the listener does not know the address, so a conversion would
    // report none.
    pub fn new(s: ListenerStatus, lan_address: Option<String>) -> Self {
        ListenerStatusDto {
            mode: s.mode.into(),
            tcp_port: s.tcp_port,
            pushes: s.pushes,
            last_push_at: s.last_push_at,
            lan_address,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDto {
    /// Who this reader is to that computer; each computer mints its own.
    pub device_id: String,
    /// The computer's hostname at install time; `None` for older plugins.
    pub name: Option<String>,
    pub paired_at: Option<i64>,
}

impl From<Pairing> for PairingDto {
    fn from(p: Pairing) -> Self {
        PairingDto {
            device_id: p.device_id,
            name: p.name,
            paired_at: p.paired_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReportDto {
    pub plugin_dir: String,
    pub device_id: String,
    pub version: i64,
    /// Relative to `plugin_dir`.
    pub written: Vec<String>,
    pub upgraded_from: Option<i64>,
}

impl From<InstallReport> for InstallReportDto {
    fn from(r: InstallReport) -> Self {
        InstallReportDto {
            plugin_dir: path_str(&r.plugin_dir),
            device_id: r.device_id,
            version: r.version,
            written: r.written,
            upgraded_from: r.upgraded_from,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallReportDto {
    pub plugin_dir: String,
    pub removed: Vec<String>,
    pub forgot_device: Option<String>,
}

impl From<UninstallReport> for UninstallReportDto {
    fn from(r: UninstallReport) -> Self {
        UninstallReportDto {
            plugin_dir: path_str(&r.plugin_dir),
            removed: r.removed,
            forgot_device: r.forgot_device,
        }
    }
}

/// A paired reader without its token, which is a secret that must not reach clients or logs.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedDeviceDto {
    pub device_id: String,
    pub label: Option<String>,
    pub plugin_version: i64,
    pub installed_at: i64,
    /// Where it was last plugged in; a breadcrumb, nothing may key on it.
    pub last_mount_path: Option<String>,
    /// When this reader was last in our hands; stamped by `plugin_status`.
    pub last_seen_at: Option<i64>,
    /// When everything this reader had was last brought across; only `SyncMount` stamps it. `None`
    /// means not since tracking began.
    pub last_synced_at: Option<i64>,
    /// When this reader last reached us over the network; distinct from `last_seen_at` and
    /// `last_synced_at`.
    pub last_wireless_at: Option<i64>,
    /// The reader's address when it last reached us; a breadcrumb, nothing may join on it.
    pub last_lan_addr: Option<String>,
}

impl From<PairedDevice> for PairedDeviceDto {
    fn from(d: PairedDevice) -> Self {
        PairedDeviceDto {
            device_id: d.device_id,
            label: d.label,
            plugin_version: d.plugin_version,
            installed_at: d.installed_at,
            last_mount_path: d.last_mount_path,
            last_seen_at: d.last_seen_at,
            last_synced_at: d.last_synced_at,
            last_wireless_at: d.last_wireless_at,
            last_lan_addr: d.last_lan_addr,
        }
    }
}

/// What one whole-device sync did. `found` and `synced` differ: nothing to bring across versus
/// nothing on the device.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MountSyncDto {
    pub mount: String,
    /// The paired reader this mount is, when it is one; `None` is not a failure and books still import.
    pub device_id: Option<String>,
    pub found: usize,
    pub synced: usize,
    pub reports: Vec<PullReportDto>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<MountSync> for MountSyncDto {
    fn from(s: MountSync) -> Self {
        MountSyncDto {
            mount: path_str(&s.mount),
            device_id: s.device_id,
            found: s.found,
            synced: s.synced,
            reports: s.reports.into_iter().map(Into::into).collect(),
            warnings: s.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoodreadsMatchDto {
    ExternalId,
    Isbn,
    Title,
    New,
}

impl From<GoodreadsMatch> for GoodreadsMatchDto {
    fn from(m: GoodreadsMatch) -> Self {
        match m {
            GoodreadsMatch::ExternalId => GoodreadsMatchDto::ExternalId,
            GoodreadsMatch::Isbn => GoodreadsMatchDto::Isbn,
            GoodreadsMatch::Title => GoodreadsMatchDto::Title,
            GoodreadsMatch::New => GoodreadsMatchDto::New,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOutcomeDto {
    Absent,
    Written,
    Unchanged,
    KeptOurs,
}

impl From<TextOutcome> for TextOutcomeDto {
    fn from(o: TextOutcome) -> Self {
        match o {
            TextOutcome::Absent => TextOutcomeDto::Absent,
            TextOutcome::Written => TextOutcomeDto::Written,
            TextOutcome::Unchanged => TextOutcomeDto::Unchanged,
            TextOutcome::KeptOurs => TextOutcomeDto::KeptOurs,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoodreadsBookReportDto {
    #[serde(default)]
    pub book_id: Option<i64>,
    pub title: String,
    pub matched_by: GoodreadsMatchDto,
    pub readings_added: usize,
    pub tags_added: usize,
    pub review: TextOutcomeDto,
    pub private_notes: TextOutcomeDto,
    #[serde(default)]
    pub rating: Option<f64>,
}

impl From<GoodreadsBookReport> for GoodreadsBookReportDto {
    fn from(r: GoodreadsBookReport) -> Self {
        GoodreadsBookReportDto {
            book_id: r.book_id,
            title: r.title,
            matched_by: r.matched_by.into(),
            readings_added: r.readings_added,
            tags_added: r.tags_added,
            review: r.review.into(),
            private_notes: r.private_notes.into(),
            rating: r.rating,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedRowDto {
    pub row: usize,
    pub title: String,
    pub authors: Vec<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<UnmatchedRow> for UnmatchedRowDto {
    fn from(u: UnmatchedRow) -> Self {
        UnmatchedRowDto {
            row: u.row,
            title: u.title,
            authors: u.authors,
            external_id: u.external_id,
            candidates: u.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GoodreadsReportDto {
    pub dry_run: bool,
    pub rows: usize,
    pub books: Vec<GoodreadsBookReportDto>,
    pub unmatched: Vec<UnmatchedRowDto>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<GoodreadsReport> for GoodreadsReportDto {
    fn from(r: GoodreadsReport) -> Self {
        GoodreadsReportDto {
            dry_run: r.dry_run,
            rows: r.rows,
            books: r.books.into_iter().map(Into::into).collect(),
            unmatched: r.unmatched.into_iter().map(Into::into).collect(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibreStatusDto {
    /// Absolute path of `ebook-convert`, when found.
    #[serde(default)]
    pub ebook_convert: Option<String>,
    #[serde(default)]
    pub calibredb: Option<String>,
}

impl From<&bookmole::Calibre> for CalibreStatusDto {
    fn from(c: &bookmole::Calibre) -> Self {
        CalibreStatusDto {
            ebook_convert: c.convert_path().map(path_str),
            calibredb: c.calibredb_path().map(path_str),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CalibreBookDto {
    pub calibre_id: i64,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default)]
    pub publish_year: Option<i64>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub isbn_10: Option<String>,
    #[serde(default)]
    pub isbn_13: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub cover: Option<String>,
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub series: Option<String>,
    #[serde(default)]
    pub added: Option<i64>,
}

impl From<CalibreBook> for CalibreBookDto {
    fn from(b: CalibreBook) -> Self {
        CalibreBookDto {
            calibre_id: b.calibre_id,
            uuid: b.uuid,
            title: b.title,
            authors: b.authors,
            publisher: b.publisher,
            publish_year: b.publish_year,
            language: b.language,
            isbn_10: b.isbn_10,
            isbn_13: b.isbn_13,
            description: b.description,
            tags: b.tags,
            cover: opt_path(&b.cover),
            formats: b.formats.iter().map(|p| path_str(p)).collect(),
            series: b.series,
            added: b.added,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibreMatchDto {
    Uuid,
    Isbn,
    Md5,
    Title,
    New,
}

impl From<CalibreMatch> for CalibreMatchDto {
    fn from(m: CalibreMatch) -> Self {
        match m {
            CalibreMatch::Uuid => CalibreMatchDto::Uuid,
            CalibreMatch::Isbn => CalibreMatchDto::Isbn,
            CalibreMatch::Md5 => CalibreMatchDto::Md5,
            CalibreMatch::Title => CalibreMatchDto::Title,
            CalibreMatch::New => CalibreMatchDto::New,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibreBookReportDto {
    pub calibre_id: i64,
    #[serde(default)]
    pub book_id: Option<i64>,
    pub title: String,
    pub matched_by: CalibreMatchDto,
    pub tags_added: usize,
    pub cover: bool,
    pub files_linked: usize,
}

impl From<CalibreBookReport> for CalibreBookReportDto {
    fn from(r: CalibreBookReport) -> Self {
        CalibreBookReportDto {
            calibre_id: r.calibre_id,
            book_id: r.book_id,
            title: r.title,
            matched_by: r.matched_by.into(),
            tags_added: r.tags_added,
            cover: r.cover,
            files_linked: r.files_linked,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedCalibreBookDto {
    pub calibre_id: i64,
    #[serde(default)]
    pub uuid: Option<String>,
    pub title: String,
    pub authors: Vec<String>,
    #[serde(default)]
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<bookmole::UnmatchedCalibreBook> for UnmatchedCalibreBookDto {
    fn from(u: bookmole::UnmatchedCalibreBook) -> Self {
        UnmatchedCalibreBookDto {
            calibre_id: u.calibre_id,
            uuid: u.uuid,
            title: u.title,
            authors: u.authors,
            candidates: u.candidates.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CalibreReportDto {
    pub dry_run: bool,
    pub rows: usize,
    pub books: Vec<CalibreBookReportDto>,
    pub unmatched: Vec<UnmatchedCalibreBookDto>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<CalibreReport> for CalibreReportDto {
    fn from(r: CalibreReport) -> Self {
        CalibreReportDto {
            dry_run: r.dry_run,
            rows: r.rows,
            books: r.books.into_iter().map(Into::into).collect(),
            unmatched: r.unmatched.into_iter().map(Into::into).collect(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntryDto {
    pub label: String,
    pub depth: usize,
    /// The book's own name for the place (`OEBPS/ch2.xhtml#part-two`), not a local path.
    pub target: String,
    /// Where `target` sits in the spine, when it names a spine item. Not a page number.
    #[serde(default)]
    pub spine_index: Option<usize>,
}

impl From<TocEntry> for TocEntryDto {
    fn from(e: TocEntry) -> Self {
        TocEntryDto {
            label: e.label,
            depth: e.depth,
            target: e.target,
            spine_index: e.spine_index,
        }
    }
}

/// A book's chapter list and the file it was read from; derived per call, with `sha256`
/// identifying which file.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableOfContentsDto {
    pub sha256: String,
    pub entries: Vec<TocEntryDto>,
}

impl From<TableOfContents> for TableOfContentsDto {
    fn from(t: TableOfContents) -> Self {
        TableOfContentsDto {
            sha256: t.sha256,
            entries: t.entries.into_iter().map(Into::into).collect(),
        }
    }
}

/// How much a row claims; an unrecognised stored token reads back as `inferred`, never `measured`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceDto {
    Measured,
    Inferred,
}

impl From<Confidence> for ConfidenceDto {
    fn from(c: Confidence) -> Self {
        match c {
            Confidence::Measured => ConfidenceDto::Measured,
            Confidence::Inferred => ConfidenceDto::Inferred,
        }
    }
}

/// One day of one book as one source saw it. Absent `minutes`/`pages` means not known, never zero.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingEventDto {
    pub book_id: i64,
    /// Which read this day belongs to, when the evidence agrees on one; absent if none or two
    /// windows hold it.
    #[serde(default)]
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub pages: Option<i64>,
    /// Free text (`koreader`, `vault`, a reading's own source); not an enum so the vocabulary can grow.
    pub source: String,
    pub confidence: ConfidenceDto,
    pub created_at: i64,
}

impl From<ReadingEvent> for ReadingEventDto {
    fn from(e: ReadingEvent) -> Self {
        ReadingEventDto {
            book_id: e.book_id,
            reading_id: e.reading_id,
            day: e.day,
            minutes: e.minutes,
            pages: e.pages,
            source: e.source,
            confidence: e.confidence.into(),
            created_at: e.created_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FillStatsDto {
    pub inserted: u64,
    pub updated: u64,
}

impl From<FillStats> for FillStatsDto {
    fn from(s: FillStats) -> Self {
        FillStatsDto {
            inserted: s.inserted,
            updated: s.updated,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefillReportDto {
    pub highlights: FillStatsDto,
    pub notes: FillStatsDto,
    pub readings: FillStatsDto,
}

impl From<RefillReport> for RefillReportDto {
    fn from(r: RefillReport) -> Self {
        RefillReportDto {
            highlights: r.highlights.into(),
            notes: r.notes.into(),
            readings: r.readings.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRangeDto {
    pub from: String,
    pub to: String,
}

impl From<&DayRange> for DayRangeDto {
    fn from(r: &DayRange) -> Self {
        DayRangeDto {
            from: r.from().to_string(),
            to: r.to().to_string(),
        }
    }
}

/// What is known about a period. `minutes` and `pages` absent means no data, not zero.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySummaryDto {
    pub range: DayRangeDto,
    pub books_finished: i64,
    pub activity_days: i64,
    pub notes_created: i64,
    pub links_created: i64,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub pages: Option<i64>,
}

impl From<ActivitySummary> for ActivitySummaryDto {
    fn from(s: ActivitySummary) -> Self {
        ActivitySummaryDto {
            range: DayRangeDto::from(&s.range),
            books_finished: s.books_finished,
            activity_days: s.activity_days,
            notes_created: s.notes_created,
            links_created: s.links_created,
            minutes: s.minutes,
            pages: s.pages,
        }
    }
}

/// One day of a period. Only days carrying an event appear; gaps are the client's to draw.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayActivityDto {
    pub day: String,
    pub books: i64,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub pages: Option<i64>,
}

impl From<DayActivity> for DayActivityDto {
    fn from(d: DayActivity) -> Self {
        DayActivityDto {
            day: d.day,
            books: d.books,
            minutes: d.minutes,
            pages: d.pages,
        }
    }
}

/// One month of a period. Only months carrying an event appear; not a client bucketing of
/// days, since null minutes would collapse to 0.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthActivityDto {
    /// `YYYY-MM`.
    pub month: String,
    pub books: i64,
    pub activity_days: i64,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub pages: Option<i64>,
}

impl From<MonthActivity> for MonthActivityDto {
    fn from(m: MonthActivity) -> Self {
        MonthActivityDto {
            month: m.month,
            books: m.books,
            activity_days: m.activity_days,
            minutes: m.minutes,
            pages: m.pages,
        }
    }
}

/// What happened, adjacently tagged on `kind`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MomentKindDto {
    ReadingClosed,
    FirstAnnotation,
    ReflectionReached {
        note_id: i64,
        reached_book_id: i64,
    },
    /// A run of consecutive days with activity that has already ended; there is no variant for a
    /// run in progress.
    RunEnded {
        from: String,
        to: String,
        /// How many days it held; past tense, never a target.
        days: i64,
    },
}

impl From<MomentKind> for MomentKindDto {
    fn from(k: MomentKind) -> Self {
        match k {
            MomentKind::ReadingClosed => MomentKindDto::ReadingClosed,
            MomentKind::FirstAnnotation => MomentKindDto::FirstAnnotation,
            MomentKind::ReflectionReached {
                note_id,
                reached_book_id,
            } => MomentKindDto::ReflectionReached {
                note_id,
                reached_book_id,
            },
            MomentKind::RunEnded { from, to, days } => MomentKindDto::RunEnded { from, to, days },
        }
    }
}

/// One thing worth noticing, derived on every ask and stored nowhere. `reading_id` is absent
/// when no single read fits.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentDto {
    /// Opaque. Give it back to acknowledge; do not read it.
    pub id: String,
    pub kind: MomentKindDto,
    /// Absent only for `run_ended`, which is about the library.
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    /// Unix seconds; the list arrives newest first by this.
    pub occurred_at: i64,
}

impl From<Moment> for MomentDto {
    fn from(m: Moment) -> Self {
        MomentDto {
            id: m.id,
            kind: m.kind.into(),
            book_id: m.book_id,
            reading_id: m.reading_id,
            day: m.day,
            occurred_at: m.occurred_at,
        }
    }
}

/// What an import of the device's `statistics.sqlite3` did; a device without the plugin gives
/// zero counts and a warning, not an error.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsImportReportDto {
    #[serde(default)]
    pub schema_version: Option<i64>,
    pub books_in_db: usize,
    pub books_matched: usize,
    pub days: usize,
    pub events: FillStatsDto,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<StatsImportReport> for StatsImportReportDto {
    fn from(r: StatsImportReport) -> Self {
        StatsImportReportDto {
            schema_version: r.schema_version,
            books_in_db: r.books_in_db,
            books_matched: r.books_matched,
            days: r.days,
            events: r.events.into(),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

/// Who supplied a field; `user` outranks every provider.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDto {
    OpenLibrary,
    GoogleBooks,
    Calibre,
    Epub,
    /// A PDF we own; supplies a length and nothing else, unlike `Epub`.
    Pdf,
    Koreader,
    Goodreads,
    User,
}

impl From<Source> for SourceDto {
    fn from(s: Source) -> Self {
        match s {
            Source::OpenLibrary => SourceDto::OpenLibrary,
            Source::GoogleBooks => SourceDto::GoogleBooks,
            Source::Calibre => SourceDto::Calibre,
            Source::Epub => SourceDto::Epub,
            Source::Pdf => SourceDto::Pdf,
            Source::KOReader => SourceDto::Koreader,
            Source::Goodreads => SourceDto::Goodreads,
            Source::User => SourceDto::User,
        }
    }
}

/// Where one field came from, and when. An absent entry means unclaimed: render
/// "unattributed", not "unknown provider".
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSourceDto {
    pub field: String,
    /// The stored token, as text so a value a newer engine wrote still parses.
    pub source: String,
    pub fetched_at: i64,
}

impl From<FieldSource> for FieldSourceDto {
    fn from(f: FieldSource) -> Self {
        FieldSourceDto {
            field: f.field,
            source: f.source,
            fetched_at: f.fetched_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichMatchDto {
    /// By ISBN, an identity: this edition.
    Isbn,
    Title {
        score: f64,
    },
}

impl From<EnrichMatch> for EnrichMatchDto {
    fn from(m: EnrichMatch) -> Self {
        match m {
            EnrichMatch::Isbn => EnrichMatchDto::Isbn,
            EnrichMatch::Title(score) => EnrichMatchDto::Title { score },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrichCandidateDto {
    pub book: BookDto,
    pub score: f64,
    pub sources: Vec<ProviderIdDto>,
}

impl From<EnrichCandidate> for EnrichCandidateDto {
    fn from(c: EnrichCandidate) -> Self {
        EnrichCandidateDto {
            book: c.book.into(),
            score: c.score,
            sources: c.sources.into_iter().map(Into::into).collect(),
        }
    }
}

/// What happened, at the granularity a frontend branches on; the variants are not interchangeable.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichOutcomeDto {
    Enriched { matched_by: EnrichMatchDto },
    Refused { candidates: Vec<EnrichCandidateDto> },
    NothingFound,
    NoAnswer,
    Unaskable,
}

impl From<EnrichOutcome> for EnrichOutcomeDto {
    fn from(o: EnrichOutcome) -> Self {
        match o {
            EnrichOutcome::Enriched(how) => EnrichOutcomeDto::Enriched {
                matched_by: how.into(),
            },
            EnrichOutcome::Refused { candidates } => EnrichOutcomeDto::Refused {
                candidates: candidates.into_iter().map(Into::into).collect(),
            },
            EnrichOutcome::NothingFound => EnrichOutcomeDto::NothingFound,
            EnrichOutcome::NoAnswer => EnrichOutcomeDto::NoAnswer,
            EnrichOutcome::Unaskable => EnrichOutcomeDto::Unaskable,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldChangeDto {
    pub field: String,
    /// Absent where the write could not name an origin.
    #[serde(default)]
    pub source: Option<SourceDto>,
    #[serde(default)]
    pub before: Option<String>,
    pub after: String,
}

impl From<FieldChange> for FieldChangeDto {
    fn from(c: FieldChange) -> Self {
        FieldChangeDto {
            field: c.field.to_string(),
            source: c.source.map(Into::into),
            before: c.before,
            after: c.after,
        }
    }
}

/// A field a provider offered and was not allowed to write; carries the offered value.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldFieldDto {
    pub field: String,
    pub offered: String,
    #[serde(default)]
    pub offered_by: Option<SourceDto>,
    /// What was kept instead; absent where the user owns the field's pair rather than the field.
    #[serde(default)]
    pub kept: Option<String>,
}

impl From<HeldField> for HeldFieldDto {
    fn from(h: HeldField) -> Self {
        HeldFieldDto {
            field: h.field.to_string(),
            offered: h.offered,
            offered_by: h.offered_by.map(Into::into),
            kept: h.kept,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnrichReportDto {
    pub book_id: i64,
    pub outcome: EnrichOutcomeDto,
    pub filled: Vec<FieldChangeDto>,
    pub held: Vec<HeldFieldDto>,
    #[serde(default)]
    pub cover: Option<String>,
    pub warnings: Vec<DiagnosticDto>,
}

impl From<EnrichReport> for EnrichReportDto {
    fn from(r: EnrichReport) -> Self {
        EnrichReportDto {
            book_id: r.book_id,
            outcome: r.outcome.into(),
            filled: r.filled.into_iter().map(Into::into).collect(),
            held: r.held.into_iter().map(Into::into).collect(),
            cover: opt_path(&r.cover),
            warnings: r.warnings.into_iter().map(Into::into).collect(),
        }
    }
}

/// The paths a settings screen shows; informational for a client on another machine.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathsDto {
    pub db_url: String,
    pub images_dir: String,
    pub vault_dir: String,
    pub files_dir: String,
    pub log_dir: String,
    pub exports_dir: String,
}

/// Where the library's database is and whether that folder is inside a cloud-synced location;
/// `None` for in-memory.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryLocationDto {
    pub database_dir: Option<String>,
    /// `null` when the folder is in no synced location this build recognises.
    pub cloud_folder: Option<CloudFolderDto>,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudFolderDto {
    pub service: CloudServiceDto,
    /// The synced root, spelled as the database's path spells it.
    pub root: String,
}

/// Which kind of synced location; `cloud_storage` is a client under `~/Library/CloudStorage`
/// this build does not name.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudServiceDto {
    #[serde(rename = "icloud_drive")]
    ICloudDrive,
    Dropbox,
    GoogleDrive,
    OneDrive,
    ProtonDrive,
    Box,
    CloudStorage,
}

impl From<CloudService> for CloudServiceDto {
    fn from(s: CloudService) -> Self {
        match s {
            CloudService::ICloudDrive => CloudServiceDto::ICloudDrive,
            CloudService::Dropbox => CloudServiceDto::Dropbox,
            CloudService::GoogleDrive => CloudServiceDto::GoogleDrive,
            CloudService::OneDrive => CloudServiceDto::OneDrive,
            CloudService::ProtonDrive => CloudServiceDto::ProtonDrive,
            CloudService::Box => CloudServiceDto::Box,
            CloudService::CloudStorage => CloudServiceDto::CloudStorage,
        }
    }
}

impl From<CloudFolder> for CloudFolderDto {
    fn from(f: CloudFolder) -> Self {
        CloudFolderDto {
            service: f.service.into(),
            root: path_str(&f.root),
        }
    }
}

impl From<LibraryLocation> for LibraryLocationDto {
    fn from(l: LibraryLocation) -> Self {
        LibraryLocationDto {
            database_dir: opt_path(&l.database_dir),
            cloud_folder: l.cloud_folder.map(Into::into),
        }
    }
}

/// What `ExpandBookPaths` made of a list of paths: files to stage, calibre libraries not
/// walked, skips and any cut-off.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedPathsDto {
    pub files: Vec<String>,
    /// Folders holding calibre's `metadata.db`; calibre owns those files, so a frontend points at
    /// the calibre importer.
    pub calibre_libraries: Vec<String>,
    pub skipped: SkippedEntriesDto,
    /// `Some(n)` when the walk stopped at `n` files; `null` when nothing was left behind.
    pub cut_off_at: Option<usize>,
}

/// Entries in the walked folders that are not among the files, counted by reason; counts,
/// never paths.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedEntriesDto {
    pub hidden: usize,
    pub not_books: usize,
    /// Links to folders, never followed.
    pub linked_folders: usize,
    /// Folders below the walk's depth, not read.
    pub too_deep: usize,
    pub unreadable: usize,
}

impl From<SkippedEntries> for SkippedEntriesDto {
    fn from(s: SkippedEntries) -> Self {
        SkippedEntriesDto {
            hidden: s.hidden,
            not_books: s.not_books,
            linked_folders: s.linked_folders,
            too_deep: s.too_deep,
            unreadable: s.unreadable,
        }
    }
}

impl From<ExpandedPaths> for ExpandedPathsDto {
    fn from(e: ExpandedPaths) -> Self {
        ExpandedPathsDto {
            files: e.files.iter().map(|p| path_str(p)).collect(),
            calibre_libraries: e.calibre_libraries.iter().map(|p| path_str(p)).collect(),
            skipped: e.skipped.into(),
            cut_off_at: e.cut_off_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingDto {
    pub key: String,
    /// JSON text; the engine never parses it.
    pub value: String,
}

impl From<bookmole::Setting> for SettingDto {
    fn from(s: bookmole::Setting) -> Self {
        SettingDto {
            key: s.key,
            value: s.value,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShelfBookDto {
    pub id: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// Each author as a person says the name; the join is the frontend's.
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// The first author's surname, for a run heading. To sort, use `author_order`.
    #[serde(default)]
    pub sort_author: Option<String>,
    #[serde(default)]
    pub page_count: Option<i64>,
    #[serde(default)]
    pub cover_path: Option<String>,
    #[serde(default)]
    pub cover_shelf_path: Option<String>,
    #[serde(default)]
    pub cover_aspect: Option<f64>,
    #[serde(default)]
    pub cover_accent: Option<AccentDto>,
}

impl From<Book> for ShelfBookDto {
    fn from(b: Book) -> Self {
        let full = BookDto::from(b);
        ShelfBookDto {
            id: full.id.unwrap_or_default(),
            title: full.title,
            authors_display: full.authors_display,
            sort_author: full.sort_author,
            page_count: full.page_count,
            cover_path: full.cover_path,
            cover_shelf_path: full.cover_shelf_path,
            cover_aspect: full.cover_aspect,
            cover_accent: full.cover_accent,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadSummaryDto {
    pub reading: ReadingDto,
    /// 1-based, oldest first, over every sibling.
    pub read_number: i64,
    pub of_reads: i64,
    /// When it was put down; `null` for a read not put down and for one put down before the column
    /// existed (not recorded).
    #[serde(default)]
    pub put_down_at: Option<i64>,
    /// Finished at, or put down at; `null` while open.
    #[serde(default)]
    pub ended_at: Option<i64>,
    /// Timeline position (unix seconds): `ended_at`, else now for an open read, else
    /// `last_modified` as an upper bound.
    pub at: i64,
    /// Whole UTC days from start to end; `null` without both ends.
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub rating: Option<RatingDto>,
    pub passages: i64,
    /// Notes written during this read: a reflection or review names it, an ordinary note is placed
    /// by its writing window.
    pub notes: i64,
    /// `YYYY-MM` (UTC) from start to `at`, oldest first.
    pub months: Vec<String>,
}

impl From<bookmole::HistoryRead> for ReadSummaryDto {
    fn from(r: bookmole::HistoryRead) -> Self {
        ReadSummaryDto {
            reading: ReadingDto::new(r.reading, r.progress),
            read_number: r.count.number,
            of_reads: r.count.of,
            put_down_at: r.put_down_at,
            ended_at: r.ended_at,
            at: r.at,
            days: r.days,
            rating: r.rating.map(Into::into),
            passages: r.passages,
            notes: r.notes,
            months: r.months,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryReadDto {
    pub book: ShelfBookDto,
    pub read: ReadSummaryDto,
}

impl From<bookmole::HistoryRead> for HistoryReadDto {
    fn from(r: bookmole::HistoryRead) -> Self {
        HistoryReadDto {
            book: r.book.clone().into(),
            read: r.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceNameDto {
    pub key: String,
    pub label: String,
}

impl From<bookmole::SourceName> for SourceNameDto {
    fn from(s: bookmole::SourceName) -> Self {
        SourceNameDto {
            key: s.key,
            label: s.label,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayRunDto {
    pub days: i64,
    pub first: String,
    pub last: String,
}

/// Headline figures for a span. No current run: only ended runs are reported, so nothing counts up.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryFiguresDto {
    pub books_finished: i64,
    pub reading_now: i64,
    pub put_down: i64,
    /// `null` when nothing measured pages in the span; not zero.
    #[serde(default)]
    pub pages: Option<i64>,
    /// Days of the span so far; the per-day denominator.
    pub elapsed_days: i64,
    #[serde(default)]
    pub pages_per_day: Option<f64>,
    /// `null` when nothing measured minutes.
    #[serde(default)]
    pub minutes: Option<i64>,
    pub minutes_from: Vec<SourceNameDto>,
    pub passages: i64,
    #[serde(default)]
    pub passages_per_finished_book: Option<f64>,
    pub notes: i64,
    pub reviews: i64,
    pub reflections: i64,
    #[serde(default)]
    pub longest_run: Option<DayRunDto>,
}

impl From<bookmole::HistoryFigures> for HistoryFiguresDto {
    fn from(f: bookmole::HistoryFigures) -> Self {
        HistoryFiguresDto {
            books_finished: f.books_finished,
            reading_now: f.reading_now,
            put_down: f.put_down,
            pages: f.pages,
            elapsed_days: f.elapsed_days,
            pages_per_day: f.pages_per_day,
            minutes: f.minutes,
            minutes_from: map(f.minutes_from),
            passages: f.passages,
            passages_per_finished_book: f.passages_per_finished_book,
            notes: f.notes,
            reviews: f.reviews,
            reflections: f.reflections,
            longest_run: f.longest_run.map(|r| DayRunDto {
                days: r.days,
                first: r.first,
                last: r.last,
            }),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordDto {
    pub read: HistoryReadDto,
    #[serde(default)]
    pub pages: Option<i64>,
    /// Pages over days; a same-day read counts one day.
    #[serde(default)]
    pub pages_per_day: Option<f64>,
    pub marks: i64,
    pub tied_with: Vec<HistoryReadDto>,
}

impl From<bookmole::HistoryRecord> for HistoryRecordDto {
    fn from(r: bookmole::HistoryRecord) -> Self {
        HistoryRecordDto {
            read: r.read.into(),
            pages: r.pages,
            pages_per_day: r.pages_per_day,
            marks: r.marks,
            tied_with: map(r.tied_with),
        }
    }
}

/// Records over the reads finished in the span; every tie goes to the most recent finish.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordsDto {
    #[serde(default)]
    pub fastest: Option<HistoryRecordDto>,
    #[serde(default)]
    pub longest: Option<HistoryRecordDto>,
    #[serde(default)]
    pub most_marked: Option<HistoryRecordDto>,
    /// Ratings compare as a fraction of their own scale.
    #[serde(default)]
    pub best_rated: Option<HistoryRecordDto>,
}

impl From<bookmole::HistoryRecords> for HistoryRecordsDto {
    fn from(r: bookmole::HistoryRecords) -> Self {
        HistoryRecordsDto {
            fastest: r.fastest.map(Into::into),
            longest: r.longest.map(Into::into),
            most_marked: r.most_marked.map(Into::into),
            best_rated: r.best_rated.map(Into::into),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMetricDto {
    Pages,
    Minutes,
    BooksFinished,
    Passages,
    Notes,
}

impl From<HistoryMetricDto> for bookmole::HistoryMetric {
    fn from(m: HistoryMetricDto) -> Self {
        use bookmole::HistoryMetric as M;
        match m {
            HistoryMetricDto::Pages => M::Pages,
            HistoryMetricDto::Minutes => M::Minutes,
            HistoryMetricDto::BooksFinished => M::BooksFinished,
            HistoryMetricDto::Passages => M::Passages,
            HistoryMetricDto::Notes => M::Notes,
        }
    }
}

/// The bucket a chart groups by. Weeks start on Monday.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryGroupDto {
    Day,
    Week,
    Month,
}

impl From<HistoryGroupDto> for bookmole::HistoryGroup {
    fn from(g: HistoryGroupDto) -> Self {
        use bookmole::HistoryGroup as G;
        match g {
            HistoryGroupDto::Day => G::Day,
            HistoryGroupDto::Week => G::Week,
            HistoryGroupDto::Month => G::Month,
        }
    }
}

/// How a bucket is split; `device` splits by the log's claimant, not a finer device identity.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistorySplitDto {
    #[serde(rename = "none")]
    Unsplit,
    Book,
    Device,
}

impl From<HistorySplitDto> for bookmole::HistorySplit {
    fn from(s: HistorySplitDto) -> Self {
        use bookmole::HistorySplit as S;
        match s {
            HistorySplitDto::Unsplit => S::Unsplit,
            HistorySplitDto::Book => S::Book,
            HistorySplitDto::Device => S::Device,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryCompareDto {
    #[serde(rename = "none")]
    Nothing,
    /// The same bucket a year earlier (52 weeks for a week).
    LastYear,
    Average,
}

impl From<HistoryCompareDto> for bookmole::HistoryCompare {
    fn from(c: HistoryCompareDto) -> Self {
        use bookmole::HistoryCompare as C;
        match c {
            HistoryCompareDto::Nothing => C::Nothing,
            HistoryCompareDto::LastYear => C::LastYear,
            HistoryCompareDto::Average => C::Average,
        }
    }
}

/// One part of a split bucket; `key` is a book id (or `none`) or a claimant.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeriesPartDto {
    pub key: String,
    pub label: String,
    pub value: i64,
}

/// One bucket of a chart. Days are `YYYY-MM-DD`, UTC.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesBucketDto {
    /// The calendar bucket's own first day (a Monday, a 1st).
    pub period: String,
    /// First and last day counted: the bucket clipped to the span and to today; buckets partition
    /// the span.
    pub start: String,
    pub end: String,
    pub value: i64,
    /// Pages/minutes: `false` means nothing measured; draw an absence, not a zero. Always `true`
    /// for counted metrics.
    pub known: bool,
    /// Largest first; empty when unsplit; sums to `value`.
    pub parts: Vec<SeriesPartDto>,
    #[serde(default)]
    pub compare: Option<f64>,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySeriesDto {
    pub buckets: Vec<SeriesBucketDto>,
    pub total: i64,
    pub known: bool,
    #[serde(default)]
    pub average: Option<f64>,
    /// Index into `buckets` of the largest (earliest of equals); `null` when every bucket is zero.
    #[serde(default)]
    pub busiest: Option<i64>,
    /// `last_year` only: the comparison summed, last year to date.
    #[serde(default)]
    pub compare_total: Option<i64>,
    #[serde(default)]
    pub compare_change: Option<f64>,
}

impl From<bookmole::HistorySeries> for HistorySeriesDto {
    fn from(s: bookmole::HistorySeries) -> Self {
        HistorySeriesDto {
            buckets: s
                .buckets
                .into_iter()
                .map(|b| SeriesBucketDto {
                    period: b.period,
                    start: b.start,
                    end: b.end,
                    value: b.value,
                    known: b.known,
                    parts: b
                        .parts
                        .into_iter()
                        .map(|p| SeriesPartDto {
                            key: p.key,
                            label: p.label,
                            value: p.value,
                        })
                        .collect(),
                    compare: b.compare,
                })
                .collect(),
            total: s.total,
            known: s.known,
            average: s.average,
            busiest: s.busiest.map(|i| i as i64),
            compare_total: s.compare_total,
            compare_change: s.compare_change,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeptRowDto {
    pub book: ShelfBookDto,
    pub passages: i64,
    pub notes: i64,
}

impl From<bookmole::KeptRow> for KeptRowDto {
    fn from(k: bookmole::KeptRow) -> Self {
        KeptRowDto {
            book: k.book.into(),
            passages: k.passages,
            notes: k.notes,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedCsvDto {
    pub path: String,
    /// Data rows, header excluded.
    pub rows: i64,
}

impl From<bookmole::SavedCsv> for SavedCsvDto {
    fn from(s: bookmole::SavedCsv) -> Self {
        SavedCsvDto {
            path: path_str(&s.path),
            rows: s.rows,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryRowDto {
    pub book: ShelfBookDto,
    /// 0-based position in the engine's Title order (leading article dropped, `NOCASE`); sort by this.
    pub title_order: i64,
    /// 0-based position in the engine's Author order (surname, then whole name; authorless last).
    pub author_order: i64,
    /// The current read (open if any, else the most recent); `null` for a book never started.
    #[serde(default)]
    pub latest: Option<ReadSummaryDto>,
    pub passages: i64,
    pub notes: i64,
}

/// The status chips; they partition `all`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCountsDto {
    pub all: i64,
    pub reading: i64,
    pub read: i64,
    pub put_down: i64,
    /// A latest read in a status this build does not model.
    pub other: i64,
    pub no_reading: i64,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryTableDto {
    pub rows: Vec<LibraryRowDto>,
    pub counts: LibraryCountsDto,
    pub total_pages: i64,
}

impl From<bookmole::LibraryTable> for LibraryTableDto {
    fn from(t: bookmole::LibraryTable) -> Self {
        let c = t.counts;
        LibraryTableDto {
            rows: t
                .rows
                .into_iter()
                .map(|r| LibraryRowDto {
                    book: r.book.into(),
                    title_order: r.title_order,
                    author_order: r.author_order,
                    latest: r.latest.map(Into::into),
                    passages: r.passages,
                    notes: r.notes,
                })
                .collect(),
            counts: LibraryCountsDto {
                all: c.all,
                reading: c.reading,
                read: c.read,
                put_down: c.put_down,
                other: c.other,
                no_reading: c.no_reading,
            },
            total_pages: t.total_pages,
        }
    }
}

/// A keyset cursor into a note list: the last row seen, by `latest_part_at DESC, id DESC`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteCursorDto {
    pub at: i64,
    pub id: i64,
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteBookDto {
    pub book: BookFaceDto,
    pub notes: i64,
}

/// One mark of a read, internally tagged on `kind`. `at` is unix seconds.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadMarkDto {
    Note {
        at: i64,
        row: Box<NoteRowDto>,
    },
    /// `highlight.text` is the reader's private passage.
    Passage {
        at: i64,
        highlight: HighlightDto,
    },
}

impl From<bookmole::ReadMark> for ReadMarkDto {
    fn from(m: bookmole::ReadMark) -> Self {
        match m {
            bookmole::ReadMark::Note { at, row } => ReadMarkDto::Note {
                at,
                row: Box::new((*row).into()),
            },
            bookmole::ReadMark::Passage { at, highlight } => ReadMarkDto::Passage {
                at,
                highlight: (*highlight).into(),
            },
        }
    }
}

/// Maps an engine enum to its snake_case wire twin. The ts-rs export attribute is written at
/// each use: the type-count check greps its literal text, so do not spell it here.
macro_rules! wire_enum {
    ($(#[$m:meta])* $dto:ident <=> $eng:path { $($(#[$vm:meta])* $var:ident),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $dto { $($(#[$vm])* $var),+ }

        impl From<$dto> for $eng {
            fn from(d: $dto) -> Self {
                match d { $($dto::$var => <$eng>::$var),+ }
            }
        }
        impl From<$eng> for $dto {
            fn from(e: $eng) -> Self {
                match e { $(<$eng>::$var => $dto::$var),+ }
            }
        }
    };
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    ListSortDto <=> bookmole::ListSort { Name, #[default] RecentlyChanged }
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    EntryShowDto <=> bookmole::EntryShow { #[default] All, InLibrary, NotInLibrary }
}

wire_enum! {
    /// A view over a list's entries; none rewrites `position`.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    EntrySortDto <=> bookmole::EntrySort { #[default] Yours, Published, Title }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDto {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// `"user"`, or `"goodreads"` for a kept shelf.
    pub source: String,
    pub book_count: i64,
    pub not_in_library: i64,
    /// Entries whose book is in the library, counted by the same rule as `not_in_library`.
    pub in_library_count: i64,
    /// Enough books have a year for a Timeline to be worth drawing.
    pub timeline_ready: bool,
    pub updated_at: i64,
}

impl ListDto {
    pub(crate) fn new(
        l: bookmole::BookList,
        book_count: i64,
        not_in_library: i64,
        in_library_count: i64,
        timeline_ready: bool,
    ) -> Self {
        ListDto {
            id: l.id,
            name: l.name,
            description: l.description,
            source: l.source,
            book_count,
            not_in_library,
            in_library_count,
            timeline_ready,
            updated_at: l.updated_at,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListSummaryDto {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub source: String,
    pub book_count: i64,
    pub not_in_library: i64,
    /// Entries whose book is in the library, counted in the query that counts `not_in_library`.
    pub in_library_count: i64,
    pub updated_at: i64,
    pub preview: Vec<ShelfBookDto>,
}

impl From<bookmole::ListSummary> for ListSummaryDto {
    fn from(s: bookmole::ListSummary) -> Self {
        ListSummaryDto {
            id: s.list.id,
            name: s.list.name,
            description: s.list.description,
            source: s.list.source,
            book_count: s.book_count,
            not_in_library: s.not_in_library,
            in_library_count: s.in_library,
            updated_at: s.list.updated_at,
            preview: s.preview.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListEntryDto {
    pub book: ShelfBookDto,
    /// 0-based within the list, whatever order the entry was read in.
    pub position: i64,
    #[serde(default)]
    pub note: Option<String>,
    /// The year the Timeline places the book at: first publication, else the edition's.
    #[serde(default)]
    pub year: Option<i64>,
    /// `year` is an edition's year because the first publication is unknown.
    pub year_is_edition: bool,
    /// The book's own `publish_year` (an edition's), for display.
    #[serde(default)]
    pub publish_year: Option<i64>,
    pub in_library: bool,
}

impl From<bookmole::ListEntry> for ListEntryDto {
    fn from(e: bookmole::ListEntry) -> Self {
        let publish_year = e.book.publish_year;
        ListEntryDto {
            book: e.book.into(),
            position: e.position,
            note: e.note,
            year: e.year,
            year_is_edition: e.year_is_edition,
            publish_year,
            in_library: e.in_library,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMembershipDto {
    pub list_id: i64,
    pub name: String,
    /// 1-based.
    pub position: i64,
    pub list_size: i64,
    #[serde(default)]
    pub note: Option<String>,
}

impl From<bookmole::ListMembership> for ListMembershipDto {
    fn from(m: bookmole::ListMembership) -> Self {
        ListMembershipDto {
            list_id: m.list_id,
            name: m.name,
            position: m.position,
            list_size: m.list_size,
            note: m.note,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListTimelineDto {
    pub dated: Vec<ListEntryDto>,
    /// Books without a year, in the list's own order; they go last.
    pub undated: Vec<ListEntryDto>,
    pub ready: bool,
}

impl From<bookmole::ListTimeline> for ListTimelineDto {
    fn from(t: bookmole::ListTimeline) -> Self {
        ListTimelineDto {
            dated: t.dated.into_iter().map(Into::into).collect(),
            undated: t.undated.into_iter().map(Into::into).collect(),
            ready: t.ready,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShelfSuggestionDto {
    pub source: String,
    pub tag: String,
    /// The shelf as the reader wrote it, else the tag.
    pub name: String,
    pub book_count: i64,
    pub preview: Vec<ShelfBookDto>,
}

impl From<bookmole::ShelfSuggestion> for ShelfSuggestionDto {
    fn from(s: bookmole::ShelfSuggestion) -> Self {
        ShelfSuggestionDto {
            source: s.source,
            tag: s.tag,
            name: s.name,
            book_count: s.book_count,
            preview: s.preview.into_iter().map(Into::into).collect(),
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    CopyFormDto <=> bookmole::CopyForm { File, Print, App }
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    TenureDto <=> bookmole::Tenure { Own, Borrowed, Rented, Subscription, Unrecorded }
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    PlaceKindDto <=> bookmole::PlaceKind { Calibre, Reader, Path, Store, AppleBooks }
}

wire_enum! {
    /// How we know about a copy. The API only ever writes `user`.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    LearnedFromDto <=> bookmole::LearnedFrom {
        Calibre, Koreader, FileImport, AppleBooks, Kindle, User, Migration
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyPlaceDto {
    pub copy_id: i64,
    pub kind: PlaceKindDto,
    pub reference: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub last_hash: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<i64>,
    #[serde(default)]
    pub last_mtime: Option<i64>,
    #[serde(default)]
    pub missing_since: Option<i64>,
    /// A `reader` place's paired device name; absent when the device is not known.
    #[serde(default)]
    pub device_name: Option<String>,
}

impl From<bookmole::CopyPlace> for CopyPlaceDto {
    fn from(p: bookmole::CopyPlace) -> Self {
        CopyPlaceDto {
            copy_id: p.copy_id,
            kind: p.kind.into(),
            reference: p.reference,
            detail: p.detail,
            last_hash: p.last_hash,
            last_seen_at: p.last_seen_at,
            last_mtime: p.last_mtime,
            missing_since: p.missing_since,
            device_name: None,
        }
    }
}

/// One copy of a book. Only the user changes `have_now`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyDto {
    pub id: i64,
    pub book_id: i64,
    /// Absent is "not said".
    #[serde(default)]
    pub form: Option<CopyFormDto>,
    pub tenure: TenureDto,
    /// `true`, `false` (you no longer have it) or absent (not said).
    #[serde(default)]
    pub have_now: Option<bool>,
    #[serde(default)]
    pub from_whom: Option<String>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub due_at: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    #[serde(default)]
    pub lent_to: Option<String>,
    pub unreadable: bool,
    #[serde(default)]
    pub note: Option<String>,
    pub learned_from: LearnedFromDto,
    pub created_at: i64,
    pub updated_at: i64,
    pub places: Vec<CopyPlaceDto>,
    /// The file's lowercased format (`epub`, `pdf`, ...); absent for a reader-only or app copy.
    #[serde(default)]
    pub format: Option<String>,
    /// Whether *Keep a copy here* works on this copy; show the action only where true.
    pub can_keep_here: bool,
}

impl CopyDto {
    pub fn from_parts(c: bookmole::Copy, facts: bookmole::CopyFacts) -> Self {
        let mut places: Vec<CopyPlaceDto> = c.places.iter().cloned().map(Into::into).collect();
        for (place, dto) in c.places.iter().zip(places.iter_mut()) {
            dto.device_name = facts
                .reader_devices
                .iter()
                .find(|(r, _)| place.kind == bookmole::PlaceKind::Reader && *r == place.reference)
                .map(|(_, n)| n.clone());
        }
        CopyDto {
            id: c.id,
            book_id: c.book_id,
            form: c.form.map(Into::into),
            tenure: c.tenure.into(),
            have_now: c.have_now,
            from_whom: c.from_whom,
            app: c.app,
            location: c.location,
            started_at: c.started_at,
            due_at: c.due_at,
            ended_at: c.ended_at,
            lent_to: c.lent_to,
            unreadable: c.unreadable,
            note: c.note,
            learned_from: c.learned_from.into(),
            created_at: c.created_at,
            updated_at: c.updated_at,
            places,
            format: facts.format,
            can_keep_here: facts.can_keep_here,
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    HighlightOriginDto <=> bookmole::HighlightOrigin {
        Koreader, Preview, AppleBooks, Kindle, KindleApp, User
    }
}

wire_enum! {
    /// The unit a highlight's stored position is in (never converted).
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    PosKindDto <=> bookmole::PosKind {
        KoreaderXpointer, KindleLocation, EpubCfi, PdfRect
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightOriginFactDto {
    pub highlight_id: i64,
    pub origin: HighlightOriginDto,
    /// The copy it was made in; absent is history with no copy.
    #[serde(default)]
    pub copy_id: Option<i64>,
    #[serde(default)]
    pub device_name: Option<String>,
    /// The copy has a `path`, `store`, `calibre` or `apple_books` place.
    pub on_this_computer: bool,
    #[serde(default)]
    pub pos_kind: Option<PosKindDto>,
    /// The highlight is real but its words could not be extracted.
    pub text_unavailable: bool,
}

impl From<bookmole::HighlightOriginFact> for HighlightOriginFactDto {
    fn from(f: bookmole::HighlightOriginFact) -> Self {
        HighlightOriginFactDto {
            highlight_id: f.highlight_id,
            origin: f.origin.into(),
            copy_id: f.copy_id,
            device_name: f.device_name,
            on_this_computer: f.on_this_computer,
            pos_kind: f.pos_kind.map(Into::into),
            text_unavailable: f.text_unavailable,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookStandingDto {
    pub in_library: bool,
    pub touched: bool,
    /// A finished reading and no copy at all: the book page asks how it was read.
    pub read_without_copy: bool,
}

impl From<bookmole::BookStanding> for BookStandingDto {
    fn from(s: bookmole::BookStanding) -> Self {
        BookStandingDto {
            in_library: s.in_library,
            touched: s.touched,
            read_without_copy: s.read_without_copy,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCopyRowDto {
    pub book_id: i64,
    /// A copy whose `have_now` is not false.
    pub have_now: bool,
    /// Has copies and every one is `have_now = false`.
    pub had: bool,
    /// A borrowed, rented or subscription copy still had.
    pub borrowed: bool,
    /// Any copy of unrecorded tenure.
    pub copy_not_recorded: bool,
    /// The tenure to mark the row with (first copy still had, else the oldest).
    pub tenure: TenureDto,
}

/// The Library filters' counts. They overlap, so they do not sum.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCopyCountsDto {
    pub have_now: i64,
    pub had: i64,
    pub borrowed: i64,
    pub copy_not_recorded: i64,
}

/// A row per in-library book that has copies (no row means no copies), and the counts.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryCopiesDto {
    pub rows: Vec<LibraryCopyRowDto>,
    pub counts: LibraryCopyCountsDto,
}

impl From<bookmole::LibraryCopies> for LibraryCopiesDto {
    fn from(l: bookmole::LibraryCopies) -> Self {
        let c = l.counts;
        LibraryCopiesDto {
            rows: l
                .rows
                .into_iter()
                .map(|r| LibraryCopyRowDto {
                    book_id: r.book_id,
                    have_now: r.have_now,
                    had: r.had,
                    borrowed: r.borrowed,
                    copy_not_recorded: r.copy_not_recorded,
                    tenure: r.tenure.into(),
                })
                .collect(),
            counts: LibraryCopyCountsDto {
                have_now: c.have_now,
                had: c.had,
                borrowed: c.borrowed,
                copy_not_recorded: c.copy_not_recorded,
            },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HighlightImportDto {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// Rows removed because the source dropped them; always 0 for the Kindle.
    pub removed: usize,
    pub unparsed: usize,
    pub diagnostics: Vec<DiagnosticDto>,
}

impl From<bookmole::HighlightImportSummary> for HighlightImportDto {
    fn from(s: bookmole::HighlightImportSummary) -> Self {
        HighlightImportDto {
            inserted: s.inserted,
            updated: s.updated,
            unchanged: s.unchanged,
            removed: s.removed,
            unparsed: s.unparsed,
            diagnostics: map_vec(s.diagnostics),
        }
    }
}

fn map_vec<T, U: From<T>>(items: Vec<T>) -> Vec<U> {
    items.into_iter().map(U::from).collect()
}

wire_enum! {
    /// What a Preview rescan of one PDF did: `unchanged` (mtime matches, not read), `reimported`, or
    /// `missing` (file gone; the place is marked, the copy untouched).
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    RescanOutcomeDto <=> bookmole::preview::RescanOutcome { Unchanged, Reimported, Missing }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewPlaceDto {
    pub copy_id: i64,
    pub path: String,
    pub outcome: RescanOutcomeDto,
    /// All zero unless `outcome` is `reimported`.
    pub summary: HighlightImportDto,
}

impl From<bookmole::PreviewPlaceRescan> for PreviewPlaceDto {
    fn from(p: bookmole::PreviewPlaceRescan) -> Self {
        PreviewPlaceDto {
            copy_id: p.copy_id,
            path: path_str(&p.path),
            outcome: p.outcome.into(),
            summary: p.summary.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewImportDto {
    pub places: Vec<PreviewPlaceDto>,
    /// The places' counts summed; its `diagnostics` is empty (each stays on
    /// its place).
    pub totals: HighlightImportDto,
}

impl From<bookmole::PreviewBookImport> for PreviewImportDto {
    fn from(i: bookmole::PreviewBookImport) -> Self {
        PreviewImportDto {
            places: map_vec(i.places),
            totals: i.totals.into(),
        }
    }
}

/// Which rung matched an Apple Books asset to a book; tagged on `rung`, only `file` carries more.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "rung", rename_all = "snake_case")]
pub enum AppleBooksMatchDto {
    /// Imported before: its `apple_books` place.
    Place,
    File {
        file_match: FileMatchDto,
    },
    StoreId,
    /// Title and author, auto-match band.
    Title,
}

impl From<bookmole::AssetMatch> for AppleBooksMatchDto {
    fn from(m: bookmole::AssetMatch) -> Self {
        use bookmole::AssetMatch as M;
        match m {
            M::Place => AppleBooksMatchDto::Place,
            M::File(f) => AppleBooksMatchDto::File {
                file_match: f.into(),
            },
            M::StoreId => AppleBooksMatchDto::StoreId,
            M::Title => AppleBooksMatchDto::Title,
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AppleBooksOutcomeDto {
    Matched {
        book_id: i64,
        by: AppleBooksMatchDto,
    },
    /// Nothing matched or came close, and it had highlights: a book was made.
    Created {
        book_id: i64,
    },
    WouldCreate,
    /// Nothing matched outright and nothing was written. The candidates are
    /// the decision (`LinkAppleBooksAsset`); empty when the asset has no
    /// title to make a book from.
    Unmatched {
        candidates: Vec<MatchCandidateDto>,
    },
}

impl From<bookmole::AssetOutcome> for AppleBooksOutcomeDto {
    fn from(o: bookmole::AssetOutcome) -> Self {
        use bookmole::AssetOutcome as O;
        match o {
            O::Matched { book_id, by } => AppleBooksOutcomeDto::Matched {
                book_id,
                by: by.into(),
            },
            O::Created { book_id } => AppleBooksOutcomeDto::Created { book_id },
            O::WouldCreate => AppleBooksOutcomeDto::WouldCreate,
            O::Unmatched { candidates } => AppleBooksOutcomeDto::Unmatched {
                candidates: map_vec(candidates),
            },
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppleBooksBookDto {
    /// `ZASSETID`; what `LinkAppleBooksAsset` takes.
    pub asset_id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub store_bought: bool,
    pub outcome: AppleBooksOutcomeDto,
    #[serde(default)]
    pub copy_id: Option<i64>,
    pub highlights_in_source: usize,
    pub deleted_in_source: usize,
    /// All zero but `unparsed` on a dry run.
    pub summary: HighlightImportDto,
}

impl From<bookmole::AppleBooksBook> for AppleBooksBookDto {
    fn from(b: bookmole::AppleBooksBook) -> Self {
        AppleBooksBookDto {
            asset_id: b.asset_id,
            title: b.title,
            authors: b.authors,
            store_bought: b.store_bought,
            outcome: b.outcome.into(),
            copy_id: b.copy_id,
            highlights_in_source: b.highlights_in_source,
            deleted_in_source: b.deleted_in_source,
            summary: b.summary.into(),
        }
    }
}

/// One pass over Apple Books. The counts are facts about this pass, not a
/// standing number of anything waiting.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppleBooksReportDto {
    pub dry_run: bool,
    #[serde(default)]
    pub annotation_db: Option<String>,
    #[serde(default)]
    pub library_db: Option<String>,
    pub books: Vec<AppleBooksBookDto>,
    /// Every book's counts summed (plus annotations no asset could take); its
    /// `diagnostics` is empty.
    pub totals: HighlightImportDto,
    pub assets_in_library: usize,
    /// Assets with nothing to import that matched no book: counted, not listed.
    pub assets_skipped: usize,
    pub not_highlights: usize,
    pub orphaned: usize,
    /// About the databases themselves (`apple_books_absent`,
    /// `apple_books_permission_denied`, ...).
    pub diagnostics: Vec<DiagnosticDto>,
}

impl From<bookmole::AppleBooksReport> for AppleBooksReportDto {
    fn from(r: bookmole::AppleBooksReport) -> Self {
        AppleBooksReportDto {
            dry_run: r.dry_run,
            annotation_db: opt_path(&r.annotation_db),
            library_db: opt_path(&r.library_db),
            books: map_vec(r.books),
            totals: r.totals.into(),
            assets_in_library: r.assets_in_library,
            assets_skipped: r.assets_skipped,
            not_highlights: r.not_highlights,
            orphaned: r.orphaned,
            diagnostics: map_vec(r.diagnostics),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindleClippingsBookDto {
    pub title_line: String,
    pub book_id: i64,
    #[serde(default)]
    pub copy_id: Option<i64>,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

impl From<bookmole::kindle_clippings::KindleClippingsBook> for KindleClippingsBookDto {
    fn from(b: bookmole::kindle_clippings::KindleClippingsBook) -> Self {
        KindleClippingsBookDto {
            title_line: b.title_line,
            book_id: b.book_id,
            copy_id: b.copy_id,
            inserted: b.inserted,
            updated: b.updated,
            unchanged: b.unchanged,
        }
    }
}

/// A clippings title no book matched in this import; nothing was written.
/// `AttachKindleTitle` with this `title_line` resolves it.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedKindleTitleDto {
    pub title_line: String,
    pub title: String,
    pub authors: Vec<String>,
    /// Highlights and notes it holds (bookmarks excluded).
    pub entries: usize,
    /// The matcher's ambiguous band, best first; possibly empty.
    pub candidates: Vec<MatchCandidateDto>,
}

impl From<bookmole::kindle_clippings::UnmatchedKindleTitle> for UnmatchedKindleTitleDto {
    fn from(u: bookmole::kindle_clippings::UnmatchedKindleTitle) -> Self {
        UnmatchedKindleTitleDto {
            title_line: u.title_line,
            title: u.title,
            authors: u.authors,
            entries: u.entries,
            candidates: map_vec(u.candidates),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindleClippingsReportDto {
    /// The file read; `AttachKindleTitle` takes it back for an unmatched title.
    pub path: String,
    pub dry_run: bool,
    pub books: Vec<KindleClippingsBookDto>,
    pub unmatched: Vec<UnmatchedKindleTitleDto>,
    /// Bookmarks seen (skipped by design).
    pub bookmarks: usize,
    /// Totals over `books`, `unparsed`, and every diagnostic. `removed` is 0.
    pub summary: HighlightImportDto,
}

impl KindleClippingsReportDto {
    pub fn new(path: &Path, r: bookmole::kindle_clippings::KindleClippingsReport) -> Self {
        KindleClippingsReportDto {
            path: path_str(path),
            dry_run: r.dry_run,
            books: map_vec(r.books),
            unmatched: map_vec(r.unmatched),
            bookmarks: r.bookmarks,
            summary: r.summary.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindleNotebookReportDto {
    pub dry_run: bool,
    #[serde(default)]
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// The book written to; absent when nothing matched (nothing imported).
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub copy_id: Option<i64>,
    /// Rows the file holds (bookmarks excluded), written or not.
    pub entries: usize,
    pub bookmarks: usize,
    /// The matcher's ambiguous band when no book matched; else empty.
    pub candidates: Vec<MatchCandidateDto>,
    /// `removed` is always 0: an export is not a sync.
    pub summary: HighlightImportDto,
}

impl From<bookmole::kindle_notebook::KindleNotebookReport> for KindleNotebookReportDto {
    fn from(r: bookmole::kindle_notebook::KindleNotebookReport) -> Self {
        KindleNotebookReportDto {
            dry_run: r.dry_run,
            title: r.title,
            authors: r.authors,
            book_id: r.book_id,
            copy_id: r.copy_id,
            entries: r.entries,
            bookmarks: r.bookmarks,
            candidates: map_vec(r.candidates),
            summary: r.summary.into(),
        }
    }
}

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyFileDto {
    pub copy_id: i64,
    pub kind: PlaceKindDto,
    pub reference: String,
    #[serde(default)]
    pub format: Option<String>,
    /// Absolute. Absent for a calibre place whose files were never recorded.
    #[serde(default)]
    pub path: Option<String>,
    /// For a calibre place: the library root; `""` is calibre's default library.
    #[serde(default)]
    pub calibre_library: Option<String>,
    /// A file is at `path` now. Absent when there is no path to look at.
    #[serde(default)]
    pub on_disk: Option<bool>,
    /// Bytes; absent when the file is not there.
    #[serde(default)]
    pub size: Option<i64>,
    #[serde(default)]
    pub missing_since: Option<i64>,
    #[serde(default)]
    pub last_seen_at: Option<i64>,
}

impl From<bookmole::CopyFile> for CopyFileDto {
    fn from(f: bookmole::CopyFile) -> Self {
        CopyFileDto {
            copy_id: f.copy_id,
            kind: f.kind.into(),
            reference: f.reference,
            format: f.format,
            path: f.path.map(|p| path_str(&p)),
            calibre_library: f.calibre_library,
            on_disk: f.on_disk,
            size: f.size,
            missing_since: f.missing_since,
            last_seen_at: f.last_seen_at,
        }
    }
}

/// Where one copy's highlights of one origin came from, and when that source was last read.
/// Deliberately no count.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyHighlightSourceDto {
    /// Absent is history with no copy.
    #[serde(default)]
    pub copy_id: Option<i64>,
    pub origin: HighlightOriginDto,
    #[serde(default)]
    pub device_name: Option<String>,
    /// KOReader: the reader place last seen; Preview: the PDF's place last
    /// looked at; Apple Books: its place last seen. Absent for Kindle, the
    /// Kindle app, highlights made here and highlights with no copy.
    #[serde(default)]
    pub last_read_at: Option<i64>,
}

impl From<bookmole::CopyHighlightSource> for CopyHighlightSourceDto {
    fn from(s: bookmole::CopyHighlightSource) -> Self {
        CopyHighlightSourceDto {
            copy_id: s.copy_id,
            origin: s.origin.into(),
            device_name: s.device_name,
            last_read_at: s.last_read_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_book_round_trips_through_json() {
        let dto = BookDto {
            id: Some(7),
            title: Some("Station Eleven".into()),
            authors: vec!["Emily St. John Mandel".into()],
            isbn_13: Some("9781447268963".into()),
            page_count: Some(333),
            ..BookDto::default()
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert_eq!(serde_json::from_str::<BookDto>(&json).unwrap(), dto);
    }

    #[test]
    fn an_empty_object_is_a_valid_book() {
        let dto: BookDto = serde_json::from_str("{}").unwrap();
        assert_eq!(dto, BookDto::default());
    }

    #[test]
    fn the_trip_through_the_dto_keeps_every_field_but_the_stamps() {
        let book = Book {
            id: Some(3),
            title: Some("t".into()),
            sort_title: Some("st".into()),
            authors: vec!["a".into()],
            translators: vec!["tr".into()],
            publisher: Some("p".into()),
            publish_year: Some(1999),
            first_published: None,
            language: Some("en".into()),
            isbn_10: Some("0316769487".into()),
            isbn_13: Some("9780316769488".into()),
            openlibrary_key: Some("OL1M".into()),
            googlebooks_id: Some("g1".into()),
            cover_url: Some("http://x/y.jpg".into()),
            cover_path: Some("images/y.jpg".into()),
            page_count: Some(200),
            description: Some("d".into()),
            first_sentence: Some("f".into()),
            current_page: Some(12),
            finished: true,
            reading_status: Some("finished".into()),
            ko_percent: Some(0.5),
            date_started: Some(1),
            date_finished: Some(2),
            subjects: vec!["Fiction / Literary".into()],
            series: Some("Dune".into()),
            series_index: Some(2.0),
            cover_width: Some(600),
            cover_height: Some(900),
            cover_accent: Some(0x00a0_2020),
            cover_thumb_path: Some("images/y.thumb.jpg".into()),
            created_at: Some(time::OffsetDateTime::from_unix_timestamp(1000).unwrap()),
            last_modified: Some(time::OffsetDateTime::from_unix_timestamp(2000).unwrap()),
        };
        let dto = BookDto::from(book.clone());
        assert_eq!(dto.created_at, Some(1000));
        assert_eq!(dto.last_modified, Some(2000));

        let back = Book::from(dto.clone());
        assert_eq!(back.id, book.id);
        assert_eq!(back.title, book.title);
        assert_eq!(back.sort_title, book.sort_title);
        assert_eq!(back.authors, book.authors);
        assert_eq!(back.translators, book.translators);
        assert_eq!(back.publisher, book.publisher);
        assert_eq!(back.publish_year, book.publish_year);
        assert_eq!(back.language, book.language);
        assert_eq!(back.isbn_10, book.isbn_10);
        assert_eq!(back.isbn_13, book.isbn_13);
        assert_eq!(back.openlibrary_key, book.openlibrary_key);
        assert_eq!(back.googlebooks_id, book.googlebooks_id);
        assert_eq!(back.cover_url, book.cover_url);
        assert_eq!(back.cover_path, book.cover_path);
        assert_eq!(back.page_count, book.page_count);
        assert_eq!(back.description, book.description);
        assert_eq!(back.first_sentence, book.first_sentence);
        assert_eq!(back.current_page, book.current_page);
        assert_eq!(back.finished, book.finished);
        assert_eq!(back.reading_status, book.reading_status);
        assert_eq!(back.date_started, book.date_started);
        assert_eq!(back.date_finished, book.date_finished);
        assert_eq!(back.subjects, book.subjects);
        assert_eq!(back.series, book.series);
        assert_eq!(back.series_index, book.series_index);
        // Storage stamps these; a client must not be able to backdate a row.
        assert_eq!(back.created_at, None);
        assert_eq!(back.last_modified, None);
        // `ko_percent` is a read-only projection of a device-owned column; it deliberately does not
        // round-trip.
        assert_eq!(back.ko_percent, None);
        // The cover columns do not round-trip: only `Storage::set_cover` writes them, so a width
        // never arrives without the file it measures.
        assert_eq!(dto.cover_aspect, Some(600.0 / 900.0));
        assert_eq!(
            dto.cover_accent,
            Some(AccentDto {
                r: 0xa0,
                g: 0x20,
                b: 0x20
            })
        );
        assert_eq!(dto.cover_shelf_path.as_deref(), Some("images/y.thumb.jpg"));
        assert_eq!(back.cover_width, None);
        assert_eq!(back.cover_height, None);
        assert_eq!(back.cover_accent, None);
        assert_eq!(back.cover_thumb_path, None);
    }

    #[test]
    fn the_derived_fields_cross_outward_only() {
        let book = Book {
            authors: vec!["Borges, Jorge Luis".into()],
            series: Some("Dune".into()),
            // A REAL that must not print as `#2.0`.
            series_index: Some(2.0),
            current_page: Some(50),
            page_count: Some(200),
            ..Book::default()
        };
        let dto = BookDto::from(book);
        assert_eq!(dto.series_label.as_deref(), Some("Dune #2"));
        assert_eq!(dto.authors_display, vec!["Jorge Luis Borges".to_string()]);
        assert_eq!(dto.authors, vec!["Borges, Jorge Luis".to_string()]);
        assert_eq!(
            dto.progress,
            ProgressDto::Started {
                page: Some(50),
                of: Some(200),
                fraction: Some(0.25),
                percent: Some(25),
                source: Some(FractionSourceDto::Pages),
            }
        );

        let back = Book::from(dto);
        assert_eq!(back.series, Some("Dune".to_string()));
        assert_eq!(back.series_index, Some(2.0));
        assert_eq!(back.authors, vec!["Borges, Jorge Luis".to_string()]);
    }

    #[test]
    fn a_book_crosses_with_the_name_it_files_under() {
        let filed = |authors: &[&str]| {
            BookDto::from(Book {
                authors: authors.iter().map(|a| a.to_string()).collect(),
                ..Book::default()
            })
            .sort_author
        };
        assert_eq!(filed(&["Ursula K. Le Guin"]).as_deref(), Some("Le Guin"));
        assert_eq!(filed(&["Borges, Jorge Luis"]).as_deref(), Some("Borges"));
        assert_eq!(filed(&["Colette"]).as_deref(), Some("Colette"));
        assert_eq!(filed(&["  ", "Richard Powers"]).as_deref(), Some("Powers"));
        assert_eq!(filed(&[]), None);
        assert_eq!(filed(&[" "]), None);

        let authors = vec!["Emily St. John Mandel".to_string()];
        let (_, key_last, _) = bookmole::names::sort_key(&authors);
        let dto = BookDto::from(Book {
            authors,
            ..Book::default()
        });
        assert_eq!(
            dto.sort_author.as_deref().map(str::to_lowercase),
            Some(key_last)
        );

        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["sort_author"], "Mandel");
        let old: BookDto = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert_eq!(old.sort_author, None);
        assert!(!serde_json::to_string(&dto).unwrap().contains('\u{1}'));
    }

    #[test]
    fn a_candidate_crosses_the_seam_knowing_who_wrote_it() {
        let dto = MatchCandidateDto::from(MatchCandidate {
            book_id: 7,
            title: "Dune".into(),
            authors_display: vec!["Frank Herbert".into()],
            publish_year: Some(1965),
            score: 0.71,
        });
        let json = serde_json::to_string(&dto).unwrap();
        let back: MatchCandidateDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto);
        assert_eq!(back.authors_display, ["Frank Herbert"]);
        assert_eq!(back.publish_year, Some(1965));

        let old: MatchCandidateDto =
            serde_json::from_str(r#"{"book_id":7,"title":"Dune","score":0.71}"#).unwrap();
        assert!(old.authors_display.is_empty());
        assert_eq!(old.publish_year, None);
    }

    #[test]
    fn a_zero_page_count_never_reaches_a_client_as_a_denominator() {
        let dto = BookDto::from(Book {
            current_page: Some(12),
            page_count: Some(0),
            ..Book::default()
        });
        assert_eq!(
            dto.progress,
            ProgressDto::Started {
                page: Some(12),
                of: None,
                fraction: None,
                percent: None,
                source: None,
            }
        );
    }

    #[test]
    fn an_unknown_reading_state_survives_the_wire() {
        let dto = BookDto::from(Book {
            reading_status: Some("tsundoku".into()),
            ..Book::default()
        });
        assert_eq!(
            dto.reading_state,
            Some(ReadingStateDto::Other {
                raw: "tsundoku".into()
            })
        );
        let json = serde_json::to_string(&dto).unwrap();
        let round: BookDto = serde_json::from_str(&json).unwrap();
        assert_eq!(round.reading_state, dto.reading_state);
        assert_eq!(
            Book::from(round).reading_status.as_deref(),
            Some("tsundoku")
        );
    }

    /// No reading is absence, not a state: there is no `NeverOpened` variant, since a named state
    /// gets filtered, counted and badged.
    #[test]
    fn no_reading_is_absence_rather_than_a_variant() {
        let dto = BookDto::from(Book::default());
        assert_eq!(dto.reading_state, None);
        assert_eq!(dto.progress, ProgressDto::Untouched);
    }

    #[test]
    fn a_diagnostic_keeps_its_structure_across_json() {
        let d = Diagnostic::sidecar_unparsable(
            PathBuf::from("/books/x.sdr/metadata.epub.lua"),
            &bookmole::EngineError::Sidecar("lua eval: nope".into()),
        );
        let display = d.to_string();
        let dto = DiagnosticDto::from(d);
        let json = serde_json::to_string(&dto).unwrap();
        let back: DiagnosticDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto);
        assert_eq!(
            back.kind,
            DiagnosticKindDto::SidecarUnparsable {
                path: "/books/x.sdr/metadata.epub.lua".into()
            }
        );
        assert_eq!(back.display, display);
    }

    #[test]
    fn a_timeout_carries_its_provider_and_its_seconds() {
        let d = Diagnostic::provider_timed_out(
            ProviderId::OpenLibrary,
            std::time::Duration::from_secs(5),
        );
        let dto = DiagnosticDto::from(d);
        assert_eq!(
            dto.kind,
            DiagnosticKindDto::ProviderTimedOut {
                provider: ProviderIdDto::OpenLibrary,
                after_secs: 5
            }
        );
        assert_eq!(dto.severity, SeverityDto::Warning);
    }

    #[test]
    fn a_closed_provider_carries_how_long_on_the_wire() {
        let d = Diagnostic::provider_rate_limited(
            ProviderId::OpenLibrary,
            std::time::Duration::from_millis(59_500),
        );
        let dto = DiagnosticDto::from(d);
        assert_eq!(
            dto.kind,
            DiagnosticKindDto::ProviderRateLimited {
                provider: ProviderIdDto::OpenLibrary,
                retry_after_secs: 60
            }
        );
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["kind"], "provider_rate_limited");
    }

    #[test]
    fn a_device_state_names_itself_on_the_wire() {
        let dto = DeviceStateDto::from(DeviceState::Updated {
            new_highlights: 3,
            refreshed: 1,
        });
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["state"], "updated");
        assert_eq!(json["new_highlights"], 3);
    }

    #[test]
    fn an_unknown_device_status_keeps_its_raw_string() {
        let dto = KoStatusDto::from(KoStatus::Other("tsundoku".into()));
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["status"], "other");
        assert_eq!(json["raw"], "tsundoku");
    }

    #[test]
    fn a_card_carries_both_handles_across_the_seam() {
        let dto: FlashcardDto = FlashcardRow {
            id: 1,
            book_id: 4,
            highlight_id: Some(9),
            word: "middlemarch".into(),
            context: Some("Ch 1".into()),
            book_title: "Middlemarch".into(),
            exported: false,
        }
        .into();
        assert_eq!(dto.book_id, 4);
        assert_eq!(dto.highlight_id, Some(9));

        let json = serde_json::to_string(&dto).unwrap();
        assert_eq!(serde_json::from_str::<FlashcardDto>(&json).unwrap(), dto);
        assert!(json.contains(r#""book_id":4"#));
        assert!(json.contains(r#""highlight_id":9"#));
    }

    /// `highlight_id` may be absent; `book_id` may not, or an old reply would parse into a card
    /// for book 0.
    #[test]
    fn an_unanchored_card_parses_and_a_bookless_one_does_not() {
        let anchorless: FlashcardDto = serde_json::from_str(
            r#"{"id":1,"book_id":4,"word":"mot","book_title":"Middlemarch","exported":false}"#,
        )
        .unwrap();
        assert_eq!(anchorless.highlight_id, None);
        assert_eq!(anchorless.context, None);

        assert!(
            serde_json::from_str::<FlashcardDto>(
                r#"{"id":1,"word":"mot","book_title":"Middlemarch","exported":false}"#
            )
            .is_err(),
            "a card with no book is not a card, and 0 would be a lie"
        );
    }

    /// The serialized keys are asserted: a renamed field would type-check on both sides and
    /// arrive as `null`.
    #[test]
    fn a_wireless_contact_crosses_the_seam_with_its_address() {
        let reached = PairedDevice {
            device_id: "d1f0c2ae9b7645bc".into(),
            label: Some("Kindle".into()),
            token: "never-on-the-wire".into(),
            plugin_version: 2,
            installed_at: 1_745_000_000,
            last_mount_path: None,
            last_seen_at: Some(1_756_200_000),
            last_synced_at: None,
            last_wireless_at: Some(1_756_250_000),
            last_lan_addr: Some("192.0.2.63".into()),
        };
        let dto = PairedDeviceDto::from(reached);
        assert_eq!(dto.last_wireless_at, Some(1_756_250_000));
        assert_eq!(dto.last_lan_addr.as_deref(), Some("192.0.2.63"));

        let json: serde_json::Value = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["last_wireless_at"], 1_756_250_000);
        assert_eq!(json["last_lan_addr"], "192.0.2.63");
        assert!(
            !serde_json::to_string(&dto)
                .unwrap()
                .contains("never-on-the-wire"),
            "the token must not ride along with the new fields"
        );

        // Absent is `null`, not a missing key, so a client can tell no row from not yet recorded.
        let quiet = PairedDeviceDto {
            last_wireless_at: None,
            last_lan_addr: None,
            ..dto
        };
        let json: serde_json::Value = serde_json::to_value(&quiet).unwrap();
        assert!(json.get("last_wireless_at").is_some_and(|v| v.is_null()));
        assert!(json.get("last_lan_addr").is_some_and(|v| v.is_null()));
    }
}
