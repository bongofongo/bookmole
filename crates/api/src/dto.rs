//! The wire types.
//!
//! Every one of these is the serializable mirror of a domain type, with a
//! `From` in the direction the data actually flows. **The domain types stay
//! free of `serde`, and that is the decision this module exists to keep.**
//!
//! Three reasons, and none of them is taste:
//!
//! - `Book` carries `OffsetDateTime`, `FileIdentity` and half the reports carry
//!   `PathBuf`, and `Diagnostic` carries a `Duration`. Deriving `Serialize` on
//!   those picks a wire encoding for each **by accident** — whatever the
//!   dependency happens to do this year — and then that accident is the API.
//! - A `#[derive(Serialize)]` on a domain struct makes every field name a
//!   public promise. Renaming `ko_percent` would then be a breaking API change
//!   rather than a refactor.
//! - The engine would gain `serde` on its hot path for the benefit of a caller
//!   it cannot see.
//!
//! ## Paths and JSON
//!
//! A `PathBuf` is bytes on unix and JSON is UTF-8, so a path crosses this seam
//! as `String` via `to_string_lossy`. **A path that is not valid UTF-8 does not
//! round-trip** — it comes back with replacement characters and names a
//! different file. That is a real limit and not a hidden one: there is no
//! lossless JSON encoding of an arbitrary unix path short of base64, which
//! would make every path in the protocol unreadable to a human debugging it.
//! The trade is made deliberately in favour of the ordinary case.
//!
//! ## Optional fields
//!
//! Everything nullable or repeated carries `#[serde(default)]`, so a payload
//! written by an older client still deserializes when a field is added. Nothing
//! carries `skip_serializing_if`: an explicit `null` is a stable shape, and a
//! field that vanishes when empty is a field every client has to special-case.

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

/// A path, as far as JSON can carry one. See the module doc.
fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn opt_path(p: &Option<PathBuf>) -> Option<String> {
    p.as_deref().map(path_str)
}

// ---- books ----------------------------------------------------------------

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
    /// What a provider says this book is about (migration `0013`). **Not
    /// `book_tags`**, which are minted shelves — see `Book::subjects`.
    ///
    /// Merges as a set, whole or not at all: `[]` on the way in means "this
    /// record does not say", never "this book has no subjects", so a client
    /// cannot clear them by omission and `save_book` has no way to clear them at
    /// all. That is `Engine::set_book_fields`' rule, not a wire limitation.
    #[serde(default)]
    pub subjects: Vec<String>,
    /// The series, and the place in it. **A pair**: `series_index` is only
    /// meaningful beside `series`, and sending an index alone writes a number
    /// under whatever name the row already had. The engine's own merge refuses
    /// that (`Rule::pair`), but this struct is a record rather than a merge, and
    /// a rule enforced here would be a rule the in-process caller never meets —
    /// the same reason `Api::dispatch` decides nothing.
    #[serde(default)]
    pub series: Option<String>,
    /// Fractional on purpose: novellas are 0.5.
    #[serde(default)]
    pub series_index: Option<f64>,
    /// The **shelf tier** of the cover: a downscaled sibling written when the
    /// stored jacket is bigger than a grid cell can use, else the jacket
    /// itself, else absent.
    ///
    /// **Derived, read-only, and the field a grid should load.** Providers are
    /// now asked for the largest cover they publish, which is right for a hero
    /// shot and wrong for sixty tiles; this is the one place "thumb, else the
    /// original" is decided, because a frontend reading `cover_thumb_path`
    /// directly shows nothing for every cover small enough not to have one.
    /// `cover_path` stays exactly what it was — a **whole path** — and is still
    /// the field a detail view wants.
    #[serde(default)]
    pub cover_shelf_path: Option<String>,
    /// Width over height of the stored cover — the box a tile reserves before
    /// the image loads.
    ///
    /// **Derived, read-only.** `None` means the cover has not been measured
    /// (a library predating migration `0014`, until
    /// `Engine::measure_stored_covers` runs), never that there is no cover; a
    /// frontend meeting `None` falls back to whatever it did before — a fixed
    /// box, say, or (as the TUI does) decoding the file.
    #[serde(default)]
    pub cover_aspect: Option<f64>,
    /// The cover's border colour, which is what a spine or a placeholder gets
    /// when there is no spine art — and there never is, since providers ship
    /// front covers only.
    ///
    /// **Derived, read-only, and a value rather than a name.** Channels, not a
    /// hex string: `#a02020` is CSS's spelling and a terminal wants floats, so
    /// the wire carries the measurement and each frontend writes it its own
    /// way. Unclamped — the TUI's legibility band is its own policy.
    #[serde(default)]
    pub cover_accent: Option<AccentDto>,
    /// Read-only projections of the **current** reading. Sending them back in a
    /// `save_book` changes nothing: `upsert_book` has ignored them since
    /// migration `0005`, and `update_progress` is the writer.
    #[serde(default)]
    pub current_page: Option<i64>,
    #[serde(default)]
    pub finished: bool,
    /// The current reading's state, or absent when the book has no reading at
    /// all — which is the commonest state in a real library and the one
    /// `finished: false` cannot be told apart from *abandoned*.
    ///
    /// **Typed, though it once crossed as a bare `String`.** It is stored as
    /// one and always will be (an importer can write a status this
    /// build does not know, and a parse that refused one would turn a foreign
    /// device's vocabulary into an error on the read path) — but a *wire* that
    /// hands a frontend a bare string hands every frontend the same `switch`
    /// with the same three magic words in it, and the one that misspells
    /// `abandoned` styles a put-down book as an active read. `Other { raw }`
    /// carries the unknown value whole, so nothing is lost. See
    /// [`bookmole::ReadingState`], and note there is deliberately **no
    /// variant for "no reading"** — that is this field's `None`.
    #[serde(default)]
    pub reading_state: Option<ReadingStateDto>,
    #[serde(default)]
    pub date_started: Option<i64>,
    #[serde(default)]
    pub date_finished: Option<i64>,
    /// How far into the current reading this is.
    ///
    /// **Derived, read-only, and ignored on the way in.** Here rather than left
    /// to each frontend because the arithmetic has three hazards that a
    /// frontend reading `current_page` and `page_count` cannot see: a real book
    /// with `page_count = 0` (a false denominator, not a length), a real book
    /// with `page_count = NULL` (absence, not zero), and the device's own
    /// `ko_percent`, which is the better answer for a book whose length we do
    /// not know and which no `BookDto` field exposes. See
    /// [`bookmole::Progress`].
    #[serde(default)]
    pub progress: ProgressDto,
    /// The series and its place in it, written the way a person writes it:
    /// `Dune #2`, never `Dune #2.0`.
    ///
    /// **Derived, read-only.** `series_index` is a `REAL`, so a frontend
    /// formatting the pair itself will eventually print `#2.5` one way and
    /// `#2.50` another — `bookmole::Book::series_label` exists precisely so
    /// two frontends cannot disagree. Without it on this struct, a JavaScript
    /// client reconstructing the pair would agree only because JS happens to
    /// print whole floats without a decimal point — agreement by coincidence.
    #[serde(default)]
    pub series_label: Option<String>,
    /// The authors, each written the way a person says it — `Jorge Luis
    /// Borges`, never `Borges, Jorge Luis`.
    ///
    /// **Derived, read-only.** `authors` stays exactly as the origin spelled
    /// it, because that is the record; this is the parse. The **join** between
    /// names is still phrasing and still the frontend's, which is why this is a
    /// list and not a sentence.
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// The name this book files under — the first author's surname, as the
    /// origin cased it: `Le Guin` for `Ursula K. Le Guin`, `Borges` for
    /// `Borges, Jorge Luis`, `Colette` for `Colette`. Absent for a book with no
    /// author, which `BookSort::Author` files last.
    ///
    /// **Derived, read-only, and the surname half of `books.sort_author`
    /// rather than the column itself.** The column is a packed, lowercased
    /// tuple (`'0' SEP last SEP whole`, `SEP = \u{1}`) whose only job is to
    /// make `ORDER BY` compare like `names::sort_key`; `sort.rs` keeps it off
    /// the wire so no frontend can print a string full of `\u{1}`, and that
    /// still holds. This is the same parse, off the same author, through the
    /// same [`bookmole::names::last_name`] — so a run heading cut on its
    /// first letter cuts exactly where the engine's Author order turns over,
    /// and no frontend re-derives a surname above the seam to get there.
    ///
    /// `optional = nullable` in TypeScript because the field is additive to a
    /// struct that clients build as literals; a payload without it is a book
    /// with no filing name yet, not an error.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional = nullable))]
    pub sort_author: Option<String>,
    /// What shape this edition is, in multiples of its own height
    /// ([`bookmole::EditionShape`]).
    ///
    /// **Derived, read-only.** A shelf needs it *per row*, and a frontend that
    /// links this crate and not the engine would otherwise have to re-derive
    /// the arithmetic in its own language to draw three hundred spines — where
    /// it would eventually disagree with the TUI's Unicode book about how fat
    /// *Infinite Jest* is, with nothing on either screen looking wrong. One
    /// derivation, in the engine, is the only way to prevent that.
    ///
    /// Never `None`: a book always has *a* shape, and
    /// [`EditionShapeDto::width_source`]/`thickness_source` say which of its two
    /// numbers were assumed.
    #[serde(default)]
    pub shape: EditionShapeDto,
    /// Unix seconds. `OffsetDateTime`'s own serde format is a dependency's
    /// choice; an integer is ours.
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub last_modified: Option<i64>,
}

impl From<Book> for BookDto {
    fn from(b: Book) -> Self {
        // Derived first, because the literal below moves the fields they read.
        // Every one of these is a value the engine decided; none of them is a
        // word a user sees.
        let reading_state = b.reading_state().map(ReadingStateDto::from);
        let progress = ProgressDto::from(Progress::of_book(&b));
        let series_label = b.series_label();
        let authors_display = b.authors_in_display_order();
        // `names::sort_key`'s own choice of author — the first non-blank one —
        // so the name here is the one the stored key was derived from.
        let sort_author = b
            .authors
            .iter()
            .find(|a| !a.trim().is_empty())
            .map(|a| bookmole::names::last_name(a))
            .filter(|s| !s.is_empty());
        let cover_shelf_path = b.shelf_cover_path().map(str::to_string);
        let cover_aspect = b.cover_aspect();
        let cover_accent = b.cover_accent_rgb().map(AccentDto::from);
        // The cover's stored dimensions are what make this a division of two
        // columns rather than three hundred image decodes.
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
    /// The way back, for `save_book`.
    ///
    /// `created_at`/`last_modified` are **dropped, not parsed back**: they are
    /// the storage layer's to stamp, and a client that could set them could
    /// backdate a row by mistake. The reading projections are carried
    /// through because the struct has the fields, and are ignored downstream by
    /// `upsert_book` — which is where that rule already lives, and where it
    /// should stay rather than being re-implemented here.
    ///
    /// The **derived** fields — `progress`, `series_label`, `authors_display`,
    /// `sort_author`, `shape`, and the three cover measurements — are dropped
    /// outright. They are readings of other columns and there is nothing to write them to; a client
    /// that sends a `series_label` of "Dune #2" and a `series_index` of 3 has
    /// not asked for anything, and taking the label would be inventing a second
    /// writer for a pair that already has one. The cover measurements are
    /// stronger than that: `Storage::set_cover` is their only writer *by
    /// design*, since a width arriving without the file it measures is a row
    /// describing an image it is not pointing at. `ko_percent` is dropped for
    /// the same reason it is not on this struct: it is device-owned and
    /// `update_progress` is its writer.
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
            // Not on this struct at all, and that is the point: the wire carries
            // the three *readings* above and no way to write the columns behind
            // them. `Storage::set_cover` writes all four beside the file it
            // measured, or none of them.
            cover_width: None,
            cover_height: None,
            cover_accent: None,
            cover_thumb_path: None,
            created_at: None,
            last_modified: None,
        }
    }
}

/// A colour as its three channels, 0–255.
///
/// The wire shape of a cover's border accent. **Channels rather than a hex
/// string**: `#a02020` is CSS's spelling and the TUI wants floats in 0..1, so a
/// hex string would be one frontend's dialect crossing the seam and the other
/// one parsing it back out. This is the measurement; the spelling stays where
/// the spelling belongs.
///
/// It is not a colour *name* and never will be: the engine hands over facts and
/// leaves the wording to frontends, and that holds even for a value a user never
/// reads as words.
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

/// A reading's state, typed. The mirror of [`bookmole::ReadingState`], and
/// deliberately the same shape as [`KoStatusDto`] — one is our vocabulary, the
/// other is the device's, and both have to survive a word neither build knows.
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
    /// Put down. Never *failed*, never *did not finish* — the word a frontend
    /// chooses is its own, but this is the state it must be able to see
    /// without a stringly comparison.
    Abandoned,
    Other {
        raw: String,
    },
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

/// Where a fraction came from. A frontend may say so or not; what it may not do
/// is compute one and forget which it was.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FractionSourceDto {
    /// `current_page / page_count`, with an honest denominator.
    Pages,
    /// The device's own percentage, for a book whose length we do not know.
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

/// How far into a book a reading is. The mirror of [`bookmole::Progress`],
/// and the point of it: the arithmetic crosses the wire as a value so that no
/// frontend does it.
///
/// `percent` is carried beside `fraction` rather than left to
/// `Math.round(fraction * 100)` on the other side, and it is not redundant: the
/// page-based percentage is an **integer division**, and `29/100` is
/// `0.28999999999999998` in binary, so flooring the float gives 28 where the
/// division gives 29. Two frontends already did it in integers and were right
/// to; this is how the third gets the same number.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "progress", rename_all = "snake_case")]
pub enum ProgressDto {
    /// Nothing recorded. **Not zero**, and not *unread* — a book nobody opened
    /// and a book opened at page one are the same here, and the difference is
    /// `reading_state`.
    #[default]
    Untouched,
    Started {
        #[serde(default)]
        page: Option<i64>,
        /// The length. **Never `Some(0)`**: a zero page count is a false
        /// denominator and is absence by the time it reaches here.
        #[serde(default)]
        of: Option<i64>,
        /// `0.0..=1.0`, or absent — a page with no length has no percentage,
        /// and a bar must not draw an empty track over one.
        #[serde(default)]
        fraction: Option<f64>,
        #[serde(default)]
        percent: Option<i64>,
        #[serde(default)]
        source: Option<FractionSourceDto>,
    },
    /// Closed as read. Carries no page: every frontend that had this case
    /// dropped the numbers for a word.
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
    /// Alphabetically by the first author's last name, authorless books last.
    /// The one key SQL cannot express, since it is a parse of a human
    /// name; see [`BookSort::Author`].
    Author,
    /// Newest first, undated last.
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

/// Which books a list is about, before it is ordered or paged.
///
/// Every field absent is *every book*, so a client with no opinion sends
/// nothing — and `Request::ListBooks` carries this as an `Option`, which is why
/// the default and the absence are the same answer twice over rather than a
/// distinction nobody wanted.
///
/// The mirror of [`bookmole::BookFilter`]; the predicates and their
/// case-sensitivity are documented there, because they are properties of the
/// SQL and not of the wire.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookFilterDto {
    #[serde(default)]
    pub status: Option<StatusFilterDto>,
    /// A substring of the title, case-insensitive — the same rule `author`
    /// states below it, and **not** the full-text index `SearchMarks` uses.
    ///
    /// A title search is a predicate here rather than a seventh endpoint
    /// because every list is already a filter + sort + page over one shared
    /// `WHERE`, and this joins the five that were already there: it
    /// composes with `CountBooks`, with the offset paging and with every sort,
    /// none of which a separate method would get. The engine's `BookFilter`
    /// carries the argument for `LIKE` over fts5 in full.
    #[serde(default)]
    pub title: Option<String>,
    /// A substring of the stored author list, case-insensitive.
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub year: Option<i64>,
    #[serde(default)]
    pub language: Option<String>,
    /// A minted shelf name out of `book_tags` — **not** `subjects`.
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
            // This conversion serves only `ListBooks` and `CountBooks`, the
            // library's own listing: books you have (a copy decides membership;
            // a book known only from a search or an import is not "in" the
            // library). Search, `GetBook`, history, notes and lists never build
            // a `BookFilter` from this DTO and keep `All`.
            library: bookmole::LibraryScope::InLibrary,
            has_cover: f.has_cover,
        }
    }
}

/// What the current reading's state has to be for a book to be in the answer.
///
/// Four reachable cases: the three states this build writes, an importer's own
/// word through [`ReadingStateDto::Other`], and **absence**.
///
/// [`StatusFilterDto::NoReading`] is here rather than as a fifth
/// `ReadingStateDto` variant, and that placement is deliberate: absence is a
/// question somebody asked once, not a permanent claim about every book. A
/// variant is a thing a UI filters on, counts, and eventually puts a badge
/// beside — the task-completion framing bookmole avoids (nothing counts
/// what the reader has not done), and which
/// `no_reading_is_absence_rather_than_a_variant` exists to keep out.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "match", rename_all = "snake_case")]
pub enum StatusFilterDto {
    /// The book's current reading is in this state.
    State { is: ReadingStateDto },
    /// The book has no reading at all.
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

/// One page of the library: which books, in what order, and which slice.
///
/// The typed argument to [`crate::Api::list_books`]. `Request::ListBooks`
/// carries the same four values **flat**, so a payload of `{"limit":20,
/// "sort":"title"}` still means what it always did and no client had to change
/// when paging arrived — the offset and the filter are additive, and an omitted one is
/// the old behaviour exactly.
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
    /// **Negative means no limit.** The whole-library read, and the same reading
    /// SQLite gives `LIMIT -1`.
    pub limit: i64,
    /// Rows to skip. **Pagination here is an offset and not a cursor**: two of
    /// the five sorts have no cursor key that exists in the database, and they
    /// are the two whose pages are already expensive. See
    /// [`Request::ListBooks`](crate::Request::ListBooks) for the rest.
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

/// What is behind one book — one call per page of a list instead of four calls
/// per row.
///
/// **Counts, not marks.** A tile saying *3 highlights* and a tile showing a dot
/// are different products, and this is the more informative one at the same
/// cost; a frontend that wants the dot reads `> 0`, which
/// [`bookmole::BookSummary::has`] spells once so twelve components do not
/// each spell it.
///
/// Every number here is **past tense** — highlights taken, notes written, files
/// owned. Nothing in this struct counts what has not been done, and nothing
/// derived from it may.
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

/// The physical shape of one edition, in multiples of its own height.
///
/// **Derived, read-only**, and here because `EditionShape` lives in the engine
/// while a GUI links this crate and not the engine — so a 3D shelf either gets
/// this field or re-derives the arithmetic in its own language, where it would
/// drift from the TUI's. Height is `1.0` and is not
/// a field; see [`bookmole::EditionShape`] for why millimetres were refused.
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

/// Whether one of [`EditionShapeDto`]'s numbers came from anything recorded.
///
/// Mirrored as an enum rather than flattened to `width_assumed: bool`, for
/// `DiagnosticKind`'s reason: the vocabulary is the information, and a boolean
/// named after one of two states is a coin flip about which way it reads.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShapeSourceDto {
    /// Derived from something recorded about this edition.
    Recorded,
    /// Nobody recorded it, so a trade paperback stood in.
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
    /// The trade paperback the engine stands in with when nothing was recorded,
    /// **through the engine's own function** — a literal here would be a second
    /// copy of the fallback, and the two would part company the day the curve
    /// moves.
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
    /// The origin's own string, before our normalization. Kept because the
    /// normalization is ours and the shelf name is theirs — and collections are
    /// deliberately still deferred, so this is the raw material that design will
    /// eventually be made against.
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

// ---- readings and highlights ----------------------------------------------

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
    /// `null` means open, and at most one reading per book may be.
    #[serde(default)]
    pub finished_at: Option<i64>,
    /// Typed rather than a bare string — see [`BookDto::reading_state`] for the
    /// argument.
    /// The column stays a `String`; the wire does not.
    pub status: ReadingStateDto,
    /// `manual` | `koreader` | `migrated` | `goodreads` | `calibre`, and
    /// deliberately still a `String`.
    ///
    /// `status` is typed and this stops short of it on purpose. `status` has an
    /// engine type to mirror, a fixed three-word vocabulary, and frontends
    /// branching on it. `source` has none of the three: it is the *name of a
    /// writer*, it grows by one every time an importer is added, and nothing
    /// branches on it — it is shown. An enum here would be a second list of
    /// importers to keep in step with the first, which is the shape this repo
    /// spends `MERGE_RULES` avoiding.
    pub source: String,
    #[serde(default)]
    pub current_page: Option<i64>,
    /// The device's own status, typed — [`KoStatusDto`] already existed and was
    /// not being used here.
    #[serde(default)]
    pub ko_status: Option<KoStatusDto>,
    #[serde(default)]
    pub ko_percent: Option<f64>,
    #[serde(default)]
    pub ko_rating: Option<i64>,
    pub created_at: i64,
    pub last_modified: i64,
    /// **This** reading's progress, not the book's.
    ///
    /// `BookDto::progress` is the *current* read's, which on a reread would
    /// print today's numbers under an older read's heading —
    /// `bookmole::Progress::of_book` says so in as many words. The pairing
    /// with the book's length is done in the engine
    /// (`Engine::readings_with_progress`), because `readings` has no page count
    /// and deciding which length goes with which read is a derivation, not a
    /// projection.
    pub progress: ProgressDto,
}

impl ReadingDto {
    /// A reading beside the progress the engine computed for it.
    ///
    /// There is deliberately **no `From<Reading>`**: a `Reading` alone cannot
    /// answer `progress`, and an impl that filled it with
    /// `Progress::of_reading(&r, None)` would quietly report "no percentage"
    /// for every book whose length we know perfectly well.
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
    /// KOReader's note — theirs, and rewritten toward the device on every
    /// import.
    #[serde(default)]
    pub ko_note: Option<String>,
    /// The reader's own annotation — ours, and never touched by an import.
    #[serde(default)]
    pub annotation: Option<String>,
    #[serde(default)]
    pub ko_datetime: Option<String>,
    /// Which reading this was captured during, when the dates could place it.
    ///
    /// `#[serde(default)]` like every other optional field here, and `null` is
    /// an ordinary answer rather than a missing one: a highlight captured
    /// between two readings belongs to neither, and the device cannot tell us
    /// otherwise.
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

// ---- notes ----------------------------------------------------------------

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
    /// Relative to the vault root. The absolute path is `note_path`, which is
    /// the engine's to derive — a client joining it itself would need the vault
    /// root, and would then be holding a second copy of a fact that can move.
    pub file_path: String,
    pub title: String,
    /// `note` | `session` | `reflection` | `review`.
    pub kind: String,
    #[serde(default)]
    pub created_at: Option<i64>,
    /// Unix seconds: when the body was last kept. Stamped by the engine on
    /// every insert and every body save, so a frontend can say when the page
    /// was last written without remembering it itself — a memory that would
    /// not survive a reload.
    #[serde(default)]
    pub last_modified: Option<i64>,
}

/// Why a note came back: it touches a book with an open reading, or nothing
/// qualifying did and the whole notebook was drawn from.
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

/// One older note offered back for the day — see `Request::ResurfaceNote`.
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

/// One older passage offered back for the day — see
/// `Request::ResurfacePassage`.
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

/// Which index a search is about, or `null` for both.
///
/// A **filter**, in `BookFilterDto`'s idiom rather than a second method:
/// `null` is not asking, and naming one narrows what is in the list without
/// touching how the list is ordered. It exists because "search my notes" is a
/// real question — `rb notes --search` asks it — while the ordering never
/// belongs to the caller.
///
/// It is emphatically **not** a way to fetch the two halves separately and
/// interleave them above the seam. Two calls give a client two rankings and no
/// rule for merging them, which is the thing [`Request::SearchMarks`] exists to
/// make unnecessary.
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

/// One hit in the one ranked list, carrying whichever row matched.
///
/// Internally tagged on `kind`, the shape [`StatusFilterDto`] already uses, so
/// a client switches on one string rather than probing for which field is
/// present. **Not two nullable fields**: exactly one of the two rows exists,
/// and a shape that can represent neither or both is a shape somebody
/// eventually reads wrong.
///
/// The order these arrive in is the engine's and is the whole point of the
/// method — see [`Request::SearchMarks`]. Nothing above this layer re-sorts it.
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
    /// The `[[wikilink]]` as written.
    pub target_title: String,
    /// The note it resolves to, when one exists. `null` is a **forward
    /// reference**, not an error: it resolves itself the moment that note is
    /// written, and a client shows it as text rather than dropping it.
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

/// Which end of an edge a [`LinkContextDto`] is read from.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkDirectionDto {
    /// The other note links to this one.
    Incoming,
    /// This note links to the other.
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

/// A book as a link row draws it: enough for a face, and nothing more.
///
/// Not a [`BookDto`], which is a whole record a row has no use for and which
/// would make every link answer carry every book's description. The two cover
/// fields are `BookDto`'s, with the same meanings: `cover_shelf_path` is the
/// small tier a 32 px jacket should load, `cover_path` the whole image.
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

/// One link of a note, either direction, with what a row needs to say what
/// it is (`LinkContexts`).
///
/// **`paragraph` is the reader's own writing.** It may cross this seam, as a
/// note body does, and must never be logged above `trace!` on either side.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkContextDto {
    pub direction: LinkDirectionDto,
    /// The `[[wikilink]]` target as the linking body wrote it — for an
    /// outgoing forward reference, the only name the other note has.
    pub target_title: String,
    /// The other note. `null` only for an outgoing forward reference.
    #[serde(default)]
    pub note: Option<NoteDto>,
    /// The other note's book, when it is against one.
    #[serde(default)]
    pub book: Option<BookFaceDto>,
    /// Incoming only: the paragraph of the other note's body that holds the
    /// link — the run of lines between blank lines around it, cut to about
    /// 600 bytes on a char boundary. `null` for outgoing links, and for an
    /// incoming one whose body is unreadable or no longer holds the link.
    #[serde(default)]
    pub paragraph: Option<String>,
    /// Byte offsets `[start, end)` of the whole `[[…]]` token inside
    /// `paragraph`, on char boundaries. Present exactly when `paragraph` is.
    /// **Bytes, not UTF-16 units**: a JS client converts before slicing.
    #[serde(default)]
    pub link_range: Option<(usize, usize)>,
    /// Some of the paragraph was cut, at either end.
    #[serde(default)]
    pub truncated: bool,
    /// The cut took the paragraph's head, so the text starts mid-paragraph.
    /// Implies `truncated`.
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

/// One row of the currently-reading shelf.
///
/// The [`ReadingDto`] rides beside the [`BookDto`] rather than being folded
/// into it, because the two are not interchangeable: a book's progress fields
/// are projections of the *current* reading, which for a finished book is a
/// closed one, while this row is specifically the open one and carries its own
/// status, source and device mirror. A sidecar-seeded book has `ko_percent` and
/// no `current_page` at all, so the book alone would render blank.
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
        // The reading's own progress, through the engine's own function. The
        // length is right here on the book, so this is the one place the pair
        // needs no round trip — and calling `Progress::of_reading` is not the
        // derivation `ReadingDto::progress` warns about, it *is* the one
        // implementation of it.
        let progress = Progress::of_reading(&reading, book.page_count);
        OpenReadingDto {
            book: book.into(),
            reading: ReadingDto::new(reading, progress),
        }
    }
}

/// How a library-wide readings list is ordered.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingSortDto {
    /// Most recently finished first; readings still open land last.
    #[default]
    Finished,
    /// Most recently begun first.
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

/// Which readings a list is about, before it is ordered or paged.
///
/// Every field absent is *every reading*, so a client with no opinion sends
/// nothing — [`BookFilterDto`]'s rule, and the mirror of
/// [`bookmole::ReadingFilter`], where the predicates are documented because
/// they are properties of the SQL and not of the wire.
///
/// There is no `NoReading` case here and there cannot be one: every row of this
/// list *is* a reading, so the absence [`StatusFilterDto`] had to name is
/// unrepresentable rather than merely unwanted. Which is why `status` is a bare
/// [`ReadingStateDto`] and not a second filter enum.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingFilterDto {
    /// One book's readings — what lets the card page for a single book ask the
    /// **same** question the wall asks, rather than `ListReadings` plus one
    /// `CardPassage` per row beside it.
    #[serde(default)]
    pub book_id: Option<i64>,
    #[serde(default)]
    pub status: Option<ReadingStateDto>,
    /// `true` is a reading still open. Not redundant with `status`:
    /// `abandon_reading` deliberately leaves the reading open, so *abandoned*
    /// and *reading* are both open and a wall of finished reads cannot be
    /// written as a status.
    #[serde(default)]
    pub open: Option<bool>,
    /// Readings that **finished** inside this span of days, UTC — the year
    /// filter `gui-vision.md` asks the shelf for.
    ///
    /// One nested range rather than two flat optional days, because the pair is
    /// inseparable: a half-set span is a question nobody asked, and
    /// [`DayRangeDto`]'s two fields are both required. An inverted span is an
    /// [`crate::error::ErrorCode::InvalidInput`] and never a confident empty
    /// answer — the engine's own `DayRange` refuses it, exactly as it does for
    /// `ActivitySummary`.
    #[serde(default)]
    pub finished_in: Option<DayRangeDto>,
}

impl TryFrom<ReadingFilterDto> for ReadingFilter {
    type Error = bookmole::EngineError;

    /// Fallible where [`BookFilterDto`] is not, and only because of the range:
    /// two day strings are validated by the engine, and this layer must not be
    /// able to route around the refusal — the same rule the two activity
    /// aggregates follow.
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

/// A page of the library's readings: which ones, in what order, which slice.
///
/// The typed argument to [`crate::Api::list_reading_rows`], and
/// [`BookQueryDto`]'s shape for [`BookQueryDto`]'s reason — `limit` and `offset`
/// are both `i64` and adjacent, which is a swap no type checker catches. The
/// request carries the same four values **flat**.
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
    /// **Negative means no limit** — SQLite's own reading of `LIMIT -1`.
    ///
    /// No `#[serde(default)]`, deliberately: `0` is a perfectly good limit and
    /// an omitted one would silently be a page of nothing. A client asking for
    /// everything says `-1`, which is the one spelling of that.
    pub limit: i64,
    /// Rows to skip. An offset and not a cursor, for [`BookQueryDto::offset`]'s
    /// reason.
    #[serde(default)]
    pub offset: i64,
}

impl Default for ReadingQueryDto {
    /// The whole library, most recently finished first — **the same query
    /// `ReadingQuery::default()` means**, and `the_two_defaults_are_one_query`
    /// asserts it.
    ///
    /// Hand-written rather than derived for that reason alone: a derived
    /// `Default` would put `limit: 0` here against the engine's `-1`, so the two
    /// types with one name would mean opposite things by *the whole-library
    /// read*. [`BookQueryDto`] has no `Default` at all, which is the other way
    /// to avoid the same trap.
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
///
/// **Not a card.** There is deliberately no `CardDto`, because a card is a
/// *layout* — cover, dates, rating, passage, notes — and a layout is a
/// frontend's composition of facts this crate already serves. This row carries no rating and no notes and is not shaped by
/// what a card draws; it is shaped by what **one round trip** must contain for a
/// page of readings to be drawable at all. That is `BookSummaries`' argument
/// (one round trip per page, not per row) rather than a layout one, and the two are told apart by asking what
/// would be added next: a card would grow the rating, and this will not.
///
/// So the passage is here and still not on [`ReadingDto`]. The objection to
/// that field is that it would ride the reader's private highlight text
/// along on every row of every `ListReadings`, *including the ones nobody is
/// drawing a card for* — and every row of this list is one somebody is.
///
/// The book's four progress projections describe the **current** read while
/// `reading` is this one; on a reread they genuinely differ, and a frontend
/// reading the book's numbers under an older read's heading is the mistake to
/// avoid. `reading.progress` is this read's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadingRowDto {
    pub book: BookDto,
    pub reading: ReadingDto,
    /// Which read this is, 1-based, counted oldest-first over **every** reading
    /// of the book — never over the page.
    ///
    /// **Not an `Option`, and that is the decision.**
    /// `bookmole::ReadNumbering::number_of` folds *read once* into the same
    /// `None` as *unattributed* and *not in this list*, which is right for the
    /// TUI's gutter, wanting one answer to all three. Two of those three are
    /// facts about a **highlight**, and on a list whose rows are readings they
    /// are unreachable — so shipping the fold would have made `null` mean a
    /// fourth thing, *whoever built this row could not tell*, which is the
    /// ambiguity a row type must not carry. A client showing "your second read"
    /// asks `of_reads > 1`, which is the same test the gutter makes.
    pub read_number: i64,
    /// How many readings this book has in total.
    pub of_reads: i64,
    /// The passage a card shows for this reading (the one the reader chose,
    /// see `Engine::card_passage`), or `null`.
    ///
    /// `null` for a reading whose highlights are all unattributed, which is an
    /// ordinary and well-understood set of marks — `attribute_highlights`
    /// leaves `reading_id` NULL when no window holds a timestamp. A card drawing
    /// that absence *as* an absence is right.
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

/// Which years a wall's readings ended in, and whether any is still open.
///
/// **The open flag is why this is a struct and not a `Vec<i32>`.** A wall under
/// the default filter holds open readings on purpose — gating on `finished_at`
/// would tell a reader the book they are in has no card — and those rows are
/// in no year. A bare list of
/// years therefore does not partition the wall: picking every year in turn
/// would never show them again, with nothing on screen to say where they went.
///
/// **A bool and never a count.** The picker needs to know whether the chip
/// exists; a number of books-in-progress sitting on a control is one product
/// decision away from a badge counting what the reader has not done, and the
/// chip's own
/// figure is the same `CountReadings` every other chip asks, with
/// `open: true`. For the same reason there are **no per-year counts** — a row
/// of years each carrying a number is a scoreboard.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingYearsDto {
    /// Newest first. A year is here iff at least one matching reading **closed**
    /// in it — never because a note, a highlight or a device measurement is
    /// dated in it, which is the whole difference from `ActivityByMonth`.
    pub years: Vec<i32>,
    /// Whether any matching reading has not ended.
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

// ---- note rows, threads, links and parts ----------------------------------
//
// Every shape here is a **response**, except `GraphScopeDto`, which rides in a
// new request. Nothing existing was reshaped: ts-rs emits every field as
// required TypeScript, so a field added to an existing request would break a
// typed client invisibly.

/// One row of a note list — everything a notes view draws for a note without
/// reading its file: the book's face, a heading only when the reader wrote one,
/// the latest part's first words, and the counts.
///
/// **`excerpt` is the reader's own writing** — plain text, no markup, cut on a
/// char boundary at about 280 bytes with no ellipsis added. Never log it.
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
    /// `null` unless the body opens with `# heading` or the title was typed;
    /// always `null` for a reflection or a review (their titles are generated).
    #[serde(default)]
    pub heading: Option<String>,
    pub excerpt: String,
    /// `1` for an ordinary note.
    pub parts: i64,
    /// Unix seconds: the newest part's date. The list's order.
    pub latest_part_at: i64,
    /// The latest part's page, else the note's.
    #[serde(default)]
    pub latest_part_page: Option<i64>,
    /// Distinct targets the body names, written or not.
    pub links_out: i64,
    /// Distinct notes linking here.
    pub links_in: i64,
    /// The hub note id of its thread; `null` when it links nothing and nothing
    /// links it.
    #[serde(default)]
    pub thread: Option<i64>,
    /// Which read of its book the note's reading is (1-based, oldest first,
    /// over every sibling — `ReadingRowDto.read_number`'s count) — *the first
    /// read's review*. `null` for a note that names no reading.
    ///
    /// **Added later, on a response DTO**, which reshapes no request.
    /// `optional = nullable` in TypeScript, `sort_author`'s precedent, so a
    /// row literal written before it still type-checks.
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

/// A calendar month (UTC) some note's latest part falls in. The frontend
/// phrases it — *Today*, *This week*, *October*, *2025*; which months exist is
/// the engine's fact.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotePeriodDto {
    pub year: i32,
    /// 1–12.
    pub month: u8,
}

/// A thread — a connected component of resolved links, at least two notes —
/// that some asked-about note is in. Its id is its hub's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadDto {
    /// The member with the most distinct neighbours (ties to the newest, then
    /// the lowest id).
    pub hub: NoteRowDto,
    /// Notes in the thread.
    pub notes: i64,
    /// Distinct, in order of first appearance among the members newest first.
    pub books: Vec<BookFaceDto>,
    /// Members whose latest part is at or after `since`; `0` without one.
    pub joined: i64,
    /// Every member's id, newest first.
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

/// One wikilink a note wrote. `to` is `null` for a link to a note not
/// written yet.
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

/// Which part of the link graph to draw. Internally tagged on `scope`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum GraphScopeDto {
    /// Every note with at least one link, and every unwritten target.
    Everything,
    /// Notes whose latest part is at or after `since` (unix seconds), their
    /// neighbours, and the unwritten targets they name.
    Recent { since: i64 },
    /// One book's linked notes and their neighbours.
    Book { book_id: i64 },
    /// One note, its neighbours out to `steps` (1 or 2 — anything else is
    /// `invalid_input`), its own unwritten targets, and up to five notes that
    /// mention it without a link.
    Around { note_id: i64, steps: u8 },
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
    /// A `[[wikilink]]`, from the note that wrote it.
    Link,
    /// `from` mentions `to`'s title without linking it. `around` only.
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

/// One node of the drawn graph. Layout is the frontend's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeDto {
    /// `n<id>` for a note, `t<lowercased title>` for an unwritten link. Opaque
    /// to a client except as the thing edges name.
    pub key: String,
    /// `null` for an unwritten link.
    #[serde(default)]
    pub row: Option<NoteRowDto>,
    /// The note's title, or the link text as first written.
    pub title: String,
    /// Distinct nodes it shares an edge with **in this graph**.
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

/// One dated part of a note continued in parts. Part 1 is dated by the
/// note's `created_at` and paged by its `page`; `n` is the 1-based position
/// `UpdateNotePart` addresses. **`body` is the reader's writing.**
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

/// What an unsaved note would link to and be linked from. `incoming`
/// carries each linking note's paragraph, as `LinkContexts` does.
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

// ---- ratings ---------------------------------------------------------------

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
    /// The scale travels **with** the value, never without it: the Goodreads
    /// map is user-editable, so a bare number is not re-derivable into anything.
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

/// One entry of the Goodreads lookup table.
///
/// A named pair rather than a JSON array, because a two-element array is
/// positional and this table is **explicit by design** — the whole reason it is
/// a lookup and not a formula is that the ends must be readable at a glance.
/// A scale point with no entry simply is not in this list, which is what a
/// review on that value will report instead of a number.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingMapEntryDto {
    pub value: f64,
    /// 0–5 in quarters since migration `0026`, when Goodreads took quarters.
    pub goodreads: f64,
}

// ---- files and flashcards --------------------------------------------------

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
    /// How long the file says it is — pdf only, and absent far more often than
    /// present.
    ///
    /// **Absent is not zero**, and a client must not render it as one: a `0`
    /// here would be a false denominator, which is the exact thing
    /// `ProgressDto` was built to make unrepresentable.
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
    /// The domain's `matched: Option<(i64, FileMatch)>` becomes two nullable
    /// fields. A JSON tuple is positional, so adding a third element later
    /// would silently change what index 1 means; two named fields cannot.
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
    /// The book this card was captured from.
    ///
    /// **Deliberately without `#[serde(default)]`**, unlike its neighbour. The
    /// additive rule this crate lives by is about *requests* — a payload
    /// written before a field existed must still parse — and no request carries
    /// a flashcard. What a default would buy here is an old daemon's reply
    /// parsing into a card whose `book_id` is `0`, and `0` is not a book: a
    /// client would follow it to a `NotFound` indistinguishable from a book
    /// somebody deleted. `pdf.rs` states the rule this follows — a sentinel
    /// nothing downstream can tell from a real value is worse than the parse
    /// failure it replaces.
    pub book_id: i64,
    /// The passage the word came from, where there is one.
    ///
    /// The field that makes a card worth having: without it a card can only ever
    /// be shown beside its book's *title*. `#[serde(default)]` because absence
    /// and `null` mean the same true thing — this card is not anchored — which
    /// is exactly what `book_id` cannot say.
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

/// One Anki export as text: the file's contents, how many
/// cards went into it, and a name to save it under. The caller owns the file —
/// the contract `GoodreadsExport` and `FlashcardExport` already have.
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

/// An Anki export the engine wrote into `exports/` beside the vault, for a
/// client with no way to write a file itself. `path` is `null` when no card
/// went out and nothing was written.
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

/// Which passages one note cites, as handles.
///
/// **Ids, not [`HighlightDto`]s.** The surface asking this is a book's passage
/// list wanting to mark the ones already quoted somewhere, and it is holding
/// those passages already — it loaded the book's highlights and notes together
/// to draw the page. Rows here would send the
/// reader's private text back once *per citing note*, on a screen whose entire
/// output is a tick. [`Request::CitationsFor`](crate::Request::CitationsFor)
/// stays as it is and is not made redundant: it feeds the pane that shows the
/// passages themselves, where the text is the point.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteCitationsDto {
    pub note_id: i64,
    /// In the order a book reads — the same order
    /// [`Response::Highlights`](crate::Response::Highlights) comes back in for
    /// the single-note call, from the same `ORDER BY`. Empty is an answer.
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

// ---- search ----------------------------------------------------------------

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
    /// The shelf's copy of this book, when the library already holds one
    /// (`Engine::shelf_of`, matched on ISBN). A frontend reads it to decide
    /// whether choosing the result **adds** a book or **opens** one; without
    /// this field each frontend would derive that itself, at the cost of one
    /// `ResolveBooks` per row. `#[serde(default)]` so a reply written by an
    /// older API still deserializes.
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
    /// A dead provider degrades the search, never kills it — so the warnings
    /// ride back **with** the results rather than replacing them.
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

// ---- diagnostics -----------------------------------------------------------

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

/// The full mirror of `DiagnosticKind`, and it is full on purpose.
///
/// The cheap version of this type is `{ kind: String, detail: String }`, and it
/// would have thrown away the whole reason `Diagnostic` stopped being a
/// `String` in the first place: a caller has to be able to tell a timeout from
/// a 500, and *which file* was unparsable, without scraping prose. Every
/// variant here has a different next move on the far side.
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
        /// Seconds. A `Duration`'s serde shape is a dependency's choice.
        after_secs: u64,
    },
    /// The provider asked us to come back later and was not asked.
    ProviderRateLimited {
        provider: ProviderIdDto,
        /// Seconds until it is asked again, rounded up.
        retry_after_secs: u64,
    },
    /// The provider did not answer and what it answered to the same search
    /// before was shown instead. `fetched_at` is unix seconds.
    ProviderAnsweredFromCache {
        provider: ProviderIdDto,
        fetched_at: i64,
    },
    /// A kept record no longer read; it was dropped and the provider asked
    /// again.
    KeptRecordUnreadable {
        provider: ProviderIdDto,
    },
    /// The offline index of well-known works could not be used; the search
    /// still ran. `class` is null when this build has no index.
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
    /// The count is the field, not a list: the engine reports this once per
    /// file, so a frontend renders one line however many highlights it names.
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
    // ---- koreader statistics -----------------------------------------------
    //
    // These cross the seam as five variants rather than folding into one
    // "statistics unavailable", because *absence is the ordinary path* here and
    // a frontend's next move differs for each: no database is a device whose
    // user never enabled the plugin (say nothing, or offer to explain), an
    // unknown schema is a bookmole that needs updating, and an unmatched
    // book is an invitation to link it.
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
    /// A provider's cover URL would not download. The metadata it came with
    /// landed; the URL itself is deliberately not carried across the wire.
    CoverUnavailable {
        class: ErrorClassDto,
    },
    /// A PDF whose annotations could not be read at all. Nothing was imported
    /// or removed from it.
    PdfUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    /// The PDF is encrypted; its annotations are behind a password we lack.
    PdfEncrypted {
        path: String,
    },
    /// Highlights whose annotation is gone from the PDF but which carry
    /// something of the reader's own, so they were kept.
    PdfAnnotationsKept {
        path: String,
        count: u32,
    },
    /// The PDF was read only in part, so nothing was removed from it.
    PdfScanIncomplete {
        path: String,
    },
    /// No Apple Books database in the folder searched. Not an error.
    AppleBooksAbsent {
        path: String,
    },
    /// macOS refused access to Apple Books' data (the "access data from other
    /// apps" prompt was declined). Nothing was read.
    AppleBooksPermissionDenied {
        path: String,
    },
    /// An Apple Books database that could not be copied or read.
    AppleBooksDbUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    /// A table the import needs is missing or lacks its key column.
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
    /// Highlights deleted in Apple Books that carry the reader's own notes,
    /// so they were kept.
    AppleBooksHighlightsKept {
        asset_id: String,
        count: u32,
    },
    // ---- Kindle ----
    /// `My Clippings.txt` could not be read at all. Nothing was imported.
    KindleClippingsUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    /// Entries of a Kindle file in a language or layout this build does not
    /// read, or broken. One per file; not imported.
    KindleEntriesUnparsed {
        path: String,
        count: u32,
    },
    /// A clippings title with no matching book; its entries wait to be
    /// attached to one.
    KindleTitleUnmatched {
        title: String,
        entries: u32,
    },
    /// A Kindle app notebook export that could not be read, or is not one.
    KindleNotebookUnreadable {
        path: String,
        class: ErrorClassDto,
    },
    /// The notebook's book matches no library book and none was named.
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
            // ---- Kindle ----
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
    /// `Diagnostic`'s own `Display`, which is byte-for-byte what the CLI
    /// prints. Carried rather than left to each client to re-derive: the
    /// formatting rule lives in the engine, and three clients re-implementing it
    /// is three chances to disagree with the CLI about the same warning.
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

// ---- koreader and the device ----------------------------------------------

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
/// A library book that might be the thing being imported, offered as a choice.
///
/// **A row to show, not a record to write**, and that is what decides its
/// fields. The only thing a client does with one is send `book_id` back to
/// `link_sidecar`/`link_calibre_book`/`link_goodreads_row`, so nothing here
/// round-trips; what it has to do is answer *which of these is it* in the one
/// screen a refusal leads to.
///
/// `authors_display` and `publish_year` were added later. Without them a chooser
/// could not answer the question it was asking — two Dunes with one title and
/// two authors read identically — and the only way to recover the author was a
/// `get_book` per row, which is an N+1 against a list that is per *row* of a
/// calibre or Goodreads import. Additive, so `API_VERSION` did not move.
///
/// **`cover_path` is deliberately absent.** No chooser draws a jacket: the TUI's
/// picker is a text list and the CLI prints lines. And when one does, the field
/// it needs is not this one — `cover_shelf_path` is the thing a grid loads, beside `cover_aspect` and
/// `cover_accent`, because a frontend reading `cover_path` directly shows
/// nothing for every cover small enough to have no thumb. So the honest addition
/// is a *cluster*, decided by the screen that draws it, and it stays additive
/// and cheap; a path shipped now is a string per row on a list that can be one
/// per book in the library.
///
/// **The whole `BookDto` was considered and refused.** `band` is holding the
/// entire `Book`, and picking fields off it is what created this bug — but a
/// candidate list is a question about *identity*, and a `BookDto` answers a
/// different one: it carries `description` (kilobytes), `subjects`,
/// `first_sentence`, and `progress`/`reading_status`, which would invite a
/// chooser to draw a progress bar on a book it is asking you to identify.
/// Measured rather than asserted: a `BookDto` for a book with a real
/// description serializes to roughly twenty times a candidate, and a calibre
/// import reports candidates *per row*. Getting the picking wrong again costs
/// one more additive field; `BookDto` costs every import report, for ever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchCandidateDto {
    pub book_id: i64,
    pub title: String,
    /// The authors, each in reading order — `Frank Herbert`, never
    /// `Herbert, Frank`.
    ///
    /// **Derived and read-only**, the same fact `BookDto::authors_display`
    /// carries and under the same name. There is no raw `authors` beside it:
    /// the order within a name is a parse and is the engine's
    /// (`bookmole::names`), and a client of this crate does not link the
    /// engine — a raw list would be that parse re-implemented above the seam.
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// The year, where the record has one. What is left to choose by when the
    /// band's ordinary case — a variant title of a book already on the shelf —
    /// puts the same author on both rows.
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

/// The device's own status. `Other` keeps the raw string rather than collapsing
/// to a known one — a status KOReader grew and we do not model is exactly the
/// thing worth reporting, and guessing at it would be silent.
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
    /// Rows whose **device-owned** fields the sidecar disagreed with. Never
    /// includes `annotation`, which is ours.
    pub updated: usize,
    /// Present and identical.
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

/// The four per-book states a device scan reports — new, unchanged, updated,
/// unreadable — and no fifth.
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
    /// Sidecars this scan actually evaluated, and sidecars answered from the
    /// `sidecar_seen` cache. Reported rather than logged because the
    /// pre-filter's whole claim is that the first is zero on a second scan of an
    /// unmodified tree, and a claim only a stopwatch can check is not one.
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

// ---- the plugin ------------------------------------------------------------

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
    /// Exactly where an install would write. A client shows this *before* it
    /// asks, which is what makes putting something on a reader an explicit act
    /// rather than a convenience.
    pub plugin_dir: String,
    pub installed: bool,
    pub installed_version: Option<i64>,
    pub our_version: i64,
    pub paired: bool,
    /// **Our** id for this reader, and `None` when none of its pairings is
    /// ours.
    ///
    /// It used to hold whatever id the file named, which was the same thing
    /// while a reader could be paired with only one computer and became a
    /// stranger's id under a heading meaning *ours* the moment it could not.
    pub device_id: Option<String>,
    /// Every computer this reader is paired with, ours included, in file order.
    ///
    /// Not `device_id`'s replacement — that answers *which of these is us* and
    /// this answers *who else is there*, which is the question the
    /// single-computer `pairing.lua` could not represent at all. It carries **no
    /// token**: the credential exists so a reader can prove itself to us later,
    /// and a wire format holding it would put it in every client's memory and
    /// every devtools panel, which is `PairedDeviceDto`'s own omission applied
    /// one level in.
    pub pairings: Vec<PairingDto>,
    pub modified: Vec<String>,
    pub unrecognised: Vec<String>,
    /// What an install here would do — **the field a screen branches on**.
    ///
    /// The four booleans and two numbers above are the evidence and this is the
    /// verdict. It is here rather than recomputed above the seam because derived
    /// facts belong to the engine: `installed_version < our_version` is a
    /// domain rule with a name,
    /// the CLI had already spelled it once, and a second spelling in TypeScript
    /// is how two frontends come to disagree about whether a reader needs an
    /// upgrade with neither looking wrong.
    pub condition: PluginConditionDto,
}

impl From<PluginStatus> for PluginStatusDto {
    fn from(s: PluginStatus) -> Self {
        // The verdict is read before the struct is taken apart: `condition()`
        // borrows fields the moves below consume.
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

// ---- the wireless listener -------------------------------------------------

/// Whether a paired reader could reach this computer over the LAN.
///
/// Three states and not a bool, because *open until 14:32* and *open until I
/// say stop* are different promises and a frontend draws them differently — a
/// countdown against a toggle.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListenerModeDto {
    /// Nothing is bound. The default, and what makes the design fail closed:
    /// there is no service to find and nothing to fingerprint.
    Off,
    /// Bound until `until` (unix seconds) **or until a push completes**,
    /// whichever is first.
    Window { until: i64 },
    /// Bound for as long as this host runs.
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

/// What the listener is doing, for a screen to draw.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerStatusDto {
    pub mode: ListenerModeDto,
    /// The port a reader would connect to. `None` when nothing is bound.
    ///
    /// Not fixed, and announced in every discovery reply — so a busy port is a
    /// runtime fact rather than a support conversation. Only the **UDP**
    /// rendezvous port is a constant.
    pub tcp_port: Option<u16>,
    /// Completed pushes since this host started.
    ///
    /// **A count of things done, never of things waiting.** bookmole never
    /// shows a badge counting what you have not got round to, and the
    /// distinction is the whole of why this field is allowed to exist: it is
    /// the same shape as *synced 3 minutes ago*, not as *4 unread*.
    pub pushes: u64,
    pub last_push_at: Option<i64>,
    /// This computer's address on the network a reader would reach it over.
    ///
    /// For a person to read and type into a reader: the plugin keeps a typed
    /// address for the network where it cannot find this computer by itself,
    /// and the number has to be shown somewhere. Present whether or not the
    /// door is open — it is a fact about the network, not about the listener —
    /// and `None` with the wifi off. **A hint, exactly like
    /// `PairedDeviceDto::last_lan_addr`**: nothing may store it or join on it.
    pub lan_address: Option<String>,
}

impl ListenerStatusDto {
    /// The only constructor, and deliberately not a `From<ListenerStatus>`:
    /// the listener does not know this computer's address and has no business
    /// asking, so a conversion that filled the field with `None` would report
    /// *no address* on a machine that plainly has one. `ReadingDto::new`'s
    /// reason, one DTO over.
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

/// One computer a reader is paired with, **without the token**.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDto {
    /// Who this reader is *to that computer*. Each computer mints its own, so
    /// it is also the handle a computer recognises its own entry by.
    pub device_id: String,
    /// What to call the computer — its hostname at install time. `None` for an
    /// entry written by an older plugin that did not record one, where a client falls back to the id's
    /// first bytes rather than inventing a name.
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
    /// Relative paths, so a client can print the footprint against
    /// `plugin_dir` without joining strings it did not build.
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

/// A paired reader **without its token**.
///
/// The omission is the point and not an oversight: the token is a shared
/// secret that exists so the reader can prove itself to us later, and a wire
/// format that carried it would put it in every client's memory, every log a
/// client writes, and every devtools panel. Nothing outside the engine has any
/// use for it. `PairedDevice`'s own `Debug` redacts it for the same reason.
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
    /// Where it was last plugged in. A sentence about the past — mount points
    /// move, and nothing may key on this.
    pub last_mount_path: Option<String>,
    /// When this reader was last in our hands: `plugin_status` stamps it, so
    /// it is not a synonym for `installed_at`.
    pub last_seen_at: Option<i64>,
    /// When *everything* this reader had was last brought across.
    ///
    /// A different fact from `last_seen_at` and the reason migration `0020`
    /// exists: plugging a reader in to charge it moves the first and not the
    /// second. Only [`crate::Request::SyncMount`] stamps it — a one-book pull
    /// leaves the question it answers unchanged. `None` is **not** *never synced* — the column arrived with no
    /// back-fill, because nothing recorded which device a past sync read from,
    /// so it means *not since we started recording* and copy must say so.
    pub last_synced_at: Option<i64>,
    /// When this reader last reached us **over the network**, with nobody
    /// holding it (migration `0021`'s column).
    ///
    /// The fourth timestamp on this row and a synonym for none of the other
    /// three: `installed_at` is the relationship, `last_seen_at` is a cable,
    /// `last_synced_at` is the data. A reader that pushed from the next room
    /// this morning and has not been on a cable since April is described by
    /// `last_seen_at` *and* this, and a card that folded them would tell
    /// somebody their Kobo had been plugged in when it had not. `None` is
    /// `0020`'s phrasing again — *not since we started recording* — never
    /// *never*.
    pub last_wireless_at: Option<i64>,
    /// Where it reached us from, as the reader's own address. A breadcrumb
    /// exactly like `last_mount_path`, and **nothing may join on it**: a DHCP
    /// lease moves and the reader is the same reader at a different number.
    /// It is a sentence about the past, not a way to find the device now.
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

/// What one whole-device sync did.
///
/// **`found` and `synced` are both here on purpose.** *Nothing to bring across*
/// and *there is nothing on this device* are different answers, and a report
/// carrying only `reports` renders them as the same empty list — which is the
/// difference between a reader that is up to date and one whose sidecars we
/// could not see.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MountSyncDto {
    pub mount: String,
    /// The paired reader this mount turned out to be, when it is one.
    ///
    /// `None` is ordinary twice over: a KOReader volume we have never installed
    /// onto, and one carrying a `pairing.lua` minted by another copy of
    /// bookmole. The books come across either way — importing sidecars has
    /// never needed a pairing — and only the `last_synced_at` stamp is skipped,
    /// so a client must not read this as a failure.
    pub device_id: Option<String>,
    pub found: usize,
    pub synced: usize,
    pub reports: Vec<PullReportDto>,
    /// The **scan's** warnings. Each entry in `reports` carries its import's.
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

// ---- goodreads -------------------------------------------------------------

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
    /// Already here and different. **Ours was kept** and a diagnostic says so.
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

// ---- calibre ---------------------------------------------------------------

/// What calibre this machine has. **Two options, not one flag** — a half
/// install degrades to the half that works, and a client shows the feature it
/// has rather than refusing both.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibreStatusDto {
    /// Absolute path of `ebook-convert`, when it was found.
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
    /// The calibre row this line is about — the only thing tying a report line
    /// back to the shelf row a client is showing.
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

// ---- the chapter list ------------------------------------------------------

/// One line of a book's table of contents.
///
/// Flat with a `depth`, exactly as the engine has it — a tree here would make
/// "the entries, in order" the awkward shape on the wire too, and every consumer
/// walks them in reading order. The nesting is a column, not a loss.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TocEntryDto {
    pub label: String,
    /// 0 for a top-level entry, 1 for a section inside one.
    pub depth: usize,
    /// The book's own name for the place (`OEBPS/ch2.xhtml#part-two`), not a
    /// path on this machine — a client on another machine can still match it
    /// against the file it is rendering.
    pub target: String,
    /// Where `target` sits in the spine, where it names a spine item at all.
    /// Absent is ordinary and is **not** a page number.
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

/// A book's chapter list, and the file it was read from.
///
/// **Derived on every call and stored nowhere** — see `epub::table_of_contents`
/// for the argument. What that means on the wire is that `sha256` is not
/// decoration: it is how a client knows *which* file answered, since a book may
/// own several and a better one may be attached tomorrow.
///
/// The method returns `null` for "no file here we can read" and this struct with
/// an empty `entries` for "this epub carries no TOC". Two different answers; a
/// client that collapses them tells the user the same thing about a missing file
/// and an ordinary EPUB3 book.
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

// ---- the activity log ------------------------------------------------------

/// How much a row is willing to claim.
///
/// Mirrored as an enum rather than crossing as the `"measured"`/`"inferred"`
/// text the column holds: the engine reads an unrecognised token *back* as
/// `inferred` on purpose, and a client doing its own string comparison would
/// get the opposite default — claiming a measurement nobody made.
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

/// One day of one book, as one source saw it.
///
/// **`minutes` and `pages` absent means "not known", never zero**, and that is
/// the whole reason this table exists rather than a KOReader-shaped one. A
/// library that arrived as a Goodreads CSV has no minutes at all; a screen that
/// renders `null` as `0` has told its reader something false about their own
/// reading. A measured twenty-second session, by contrast, really does record
/// `0`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadingEventDto {
    pub book_id: i64,
    /// Which read this day belongs to, where the evidence agrees on one.
    /// Absent when no reading's window holds it, or when two do.
    #[serde(default)]
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub pages: Option<i64>,
    /// Free text (`koreader`, `vault`, a reading's own source). Not an enum:
    /// the vocabulary lives in a comment rather than a `CHECK` so it can grow,
    /// and a closed enum here would make it a wire-breaking change to add one.
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

/// What one filler pass did. `updated` counts rows that **actually changed**.
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

/// One pass of every filler that needs no device.
///
/// Broken out per filler rather than totalled, because a refill reporting `0`
/// overall and `0` for the vault are different facts — the second says the
/// notes filler ran and found nothing, which is what a second identical run is
/// supposed to say.
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

/// The period a summary is about, echoed back.
///
/// Echoed rather than assumed: the caller sent two strings and the engine
/// validated them, so a reply carrying the range it actually used is what lets a
/// client label a chart without re-deriving what it asked for.
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

/// What is known about a period.
///
/// The counts are `i64` because the engine fully originates the evidence behind
/// them — a zero there is knowable. `minutes` and `pages` are not: read them as
/// "we have no data", and see [`ReadingEventDto`] for why that distinction is
/// carried this far.
///
/// There is deliberately nothing here counting what the user has *not* done.
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

/// One day of a period. **Only days carrying an event appear** — the gaps are
/// the client's to draw, and filling them with zero rows here would be the same
/// lie `minutes: null` exists to avoid, spread across a calendar.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayActivityDto {
    pub day: String,
    /// Distinct books with an event that day.
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

/// One month of a period. **Only months carrying an event appear**,
/// the same rule and the same reason as [`DayActivityDto`].
///
/// This is not a client's bucketing of [`DayActivityDto`] and could not be.
/// `minutes: null` collapses to `0` the first time a client reduces over it,
/// which is precisely the lie the nullability exists to prevent — spread over a
/// calendar, where a month the device never measured becomes a month you read
/// for zero minutes. And `books` is **distinct over the whole month**, so it
/// cannot be recovered from the days at all: a reader who opened the same two
/// books on twelve days read two books that month, not twenty-four.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthActivityDto {
    /// `YYYY-MM`.
    pub month: String,
    /// Distinct books with an event anywhere in the month.
    pub books: i64,
    /// Days of this month that carry an event.
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

// ---- moments ---------------------------------------------------------------

/// What happened, and whatever the phrasing needs beyond the ids.
///
/// Adjacently tagged on `kind`, the shape `DeviceStateDto` already uses, so a
/// client switches on one field and a payload that grows a member does not
/// change how the others read.
///
/// **The wording is a frontend's and the fact is the engine's**, which is item
/// 17's line applied here: `run_ended` carries a span and a number of days, and
/// the sentence built out of them — including whether "3" is spelled *three* —
/// belongs to whoever is drawing.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MomentKindDto {
    /// A read ended. `MomentDto::reading_id` is that read.
    ReadingClosed,
    /// The first mark of the reader's own on a book that carried none.
    FirstAnnotation,
    /// A reflection reached a book it had not reached before.
    /// `MomentDto::book_id` is the reflection's own book; `reached_book_id` is
    /// the far side.
    ReflectionReached { note_id: i64, reached_book_id: i64 },
    /// A run of consecutive days with activity, **which has already ended**.
    /// There is no variant for a run in progress, and that is deliberate — a
    /// run visible while it runs is a streak.
    RunEnded {
        from: String,
        to: String,
        /// How many days it held. Past tense, like a count of your own
        /// highlights; never a target and never compared with anything.
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

/// One thing worth noticing, derived on every ask and stored nowhere.
///
/// **`reading_id` is here beside `book_id` and is not decoration.** A reread is
/// a second read of one book, so anything minted per reading — a reading's
/// card, say — cannot be selected by the book alone. It is absent where the evidence does
/// not settle on one read, which is the same call `ReadingEventDto::reading_id`
/// makes and for the same reason.
///
/// **`id` is opaque.** It is built by the engine out of the rows the moment is
/// made of; hand it back to `acknowledge_moment` and never parse it. Its format
/// is not part of this vocabulary and may gain a kind without a version bump.
///
/// **There is no count anywhere on this surface** — no total, no unread, no
/// remaining. The length of the list is the frontend's business and nothing
/// else's. bookmole never shows a badge counting what you have not done,
/// and a `pending: 3` on a reply is exactly that with a different name.
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
    /// Absent only for `run_ended`, which is about the library rather than a
    /// book.
    #[serde(default)]
    pub book_id: Option<i64>,
    /// Which read this belongs to, where the evidence settles on one.
    #[serde(default)]
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC — the same day convention as the activity log.
    pub day: String,
    /// Unix seconds. The list arrives newest first by this.
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

/// What an import of the device's `statistics.sqlite3` did.
///
/// Every field is a number a user could check against their device, which is
/// the point: `books_in_db` against `books_matched` is the whole of "why did
/// this import so little", and answering it without a round trip is why the
/// counts cross rather than just the `FillStats`.
///
/// A device whose user never enabled the statistics plugin comes back here with
/// a `warnings` entry and every count zero — **not an error**. Absence is
/// ordinary; `schema_version` absent is how a client tells "no database" from
/// "a database with nothing in it".
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

// ---- enrichment and provenance ---------------------------------------------

/// Who supplied a field. Mirrored in full, for `DiagnosticKind`'s reason: a
/// client branches on `user` — the rank that outranks every provider — and
/// flattening this to a string would make that branch a string comparison
/// against a vocabulary nothing on the wire pins.
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
    /// A PDF we own said so. Beside `Epub` because the two answer
    /// different questions: an epub supplies a title and an ISBN and never a
    /// length, a pdf supplies a length and nothing else.
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

/// Where one field came from, and when.
///
/// **An absent entry means nobody has claimed the field** — every book predating
/// migration `0012` reports an empty list however well-populated it is. So a
/// client must render "unattributed", never "unknown provider".
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSourceDto {
    pub field: String,
    /// The stored token. Text rather than [`SourceDto`] because this is read
    /// straight off the column, whose vocabulary deliberately lives in a comment
    /// rather than a `CHECK` — a closed enum here would fail to parse a row a
    /// newer engine wrote.
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

/// How a provider record was tied to the book.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichMatchDto {
    /// By ISBN, which is an identity — this is about *this edition*.
    Isbn,
    /// By title and author, at this score.
    Title { score: f64 },
}

impl From<EnrichMatch> for EnrichMatchDto {
    fn from(m: EnrichMatch) -> Self {
        match m {
            EnrichMatch::Isbn => EnrichMatchDto::Isbn,
            EnrichMatch::Title(score) => EnrichMatchDto::Title { score },
        }
    }
}

/// A record that looked like this book but not enough to write unasked.
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

/// What happened, at the granularity a frontend branches on.
///
/// The five are not interchangeable and collapsing any two of them is the bug
/// this enum was carved to prevent: `nothing_found` is a fact about the book,
/// `no_answer` is a fact about the network wearing the same shape, `refused`
/// means results came back and none was certainly this book, and `unaskable`
/// means there was nothing to ask with.
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

/// One field that changed, and who is now answerable for it.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldChangeDto {
    pub field: String,
    /// Absent where the write could not name an origin, which is not the same
    /// as an origin nobody recognises.
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

/// A field a provider offered and was not allowed to write.
///
/// **The value the provider offered is carried, not just the field name.** A
/// held-back field reported by name alone is indistinguishable from a field the
/// provider had nothing for, which is the "silently not updated" state the whole
/// report exists to remove.
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
    /// What was kept instead. Absent where the user owns the field's *pair*
    /// rather than the field, so there is nothing of theirs in this column.
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

/// What one run of `enrich_book_from_providers` did.
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
    /// Where the cover landed, if one was fetched.
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

// ---- where things live -----------------------------------------------------

/// The paths a settings screen shows. Not handles — a client that is not on
/// this machine can read them and can do nothing with them, which is correct.
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
    /// Where files written for other software land (the Anki export).
    pub exports_dir: String,
}

/// Where the library's database is, and whether that folder is inside a
/// cloud-synced location (`bookmole::cloud_folder`).
///
/// A fact for a frontend to word, once per launch — the library is open either
/// way, and nothing refuses anything. `database_dir` is the folder holding the
/// database and its `-wal`/`-shm` siblings: that folder, and never the vault,
/// is what a sync client damages, so it is what was judged. `None` for an
/// in-memory library.
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

/// The synced location a library's folder is inside.
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

/// Which kind of synced location. `cloud_storage` is macOS's
/// `~/Library/CloudStorage` holding a client this build does not name.
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

// ---- expanding the paths a user picked into book files ---------------------

/// What [`crate::Request::ExpandBookPaths`] made of a list of paths.
///
/// Facts for a frontend to word: the files to stage (the inputs in the order
/// given, each folder replaced by the book files found in it in path order,
/// and every other input passed through as it was), the calibre libraries
/// that were named and not walked, what the walk passed over and why, and
/// where a limit cut it short. The bounds are
/// the engine's (`bookmole::folders`) and none of them is a parameter.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpandedPathsDto {
    pub files: Vec<String>,
    /// Folders holding calibre's `metadata.db`. calibre owns those files; a
    /// frontend points at the calibre importer instead of staging them.
    pub calibre_libraries: Vec<String>,
    pub skipped: SkippedEntriesDto,
    /// `Some(n)` when the walk found more book files than it may return and
    /// stopped at `n`. `null` when nothing was left behind.
    pub cut_off_at: Option<usize>,
}

/// Entries inside the walked folders that are not among the files, counted by
/// reason. Counts, never paths: four hundred cover images are one sentence.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedEntriesDto {
    /// Names starting with `.`.
    pub hidden: usize,
    /// Files of a kind that is not a book file.
    pub not_books: usize,
    /// Links to folders, never followed.
    pub linked_folders: usize,
    /// Folders below the walk's depth, not read.
    pub too_deep: usize,
    /// Folders or entries that could not be read.
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

// ---- settings, reading history, the library table, more notes ------------
//
// Every shape here is a **response** except the four history enums and
// `NoteCursorDto`, which ride in new requests. Nothing existing was reshaped
// except `NoteRowDto`, which gained two optional response fields.

/// One stored setting: a frontend's key and its JSON text, as written.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingDto {
    pub key: String,
    /// JSON text. Parse it; the engine never does.
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

/// A book as a shelf row or a timeline card draws it: the face, the authors in
/// display order, the filing surname, the length and the cover cluster.
///
/// **Not `BookDto`**, which is ~19× the size with a description and is a
/// record that round-trips; this is read-only and is what a table of every
/// book in the library can afford per row. **Not `BookFaceDto`**, whose four
/// fields are a contract clients build literals of: a field added there would
/// break a typed client that never asked for it.
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
    /// Each author as a person says the name. The join is the frontend's.
    #[serde(default)]
    pub authors_display: Vec<String>,
    /// The first author's surname — `BookDto.sort_author`'s value, for a run
    /// heading. To **sort**, use the row's `author_order`.
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

/// One read, summarised — a timeline card, a record, a Library row's latest
/// read. Its book rides beside it on the shapes that need one.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadSummaryDto {
    /// The row, with **this** read's progress (`p. 205 of 280`).
    pub reading: ReadingDto,
    /// 1-based, oldest first, over every sibling.
    pub read_number: i64,
    pub of_reads: i64,
    /// When it was put down (migration `0025`); `null` for a read that is not
    /// put down, **and for one put down before the column existed** — word
    /// that as *not recorded*, never as a date.
    #[serde(default)]
    pub put_down_at: Option<i64>,
    /// Finished at, or put down at; `null` while open.
    #[serde(default)]
    pub ended_at: Option<i64>,
    /// Where the card sits on the timeline (unix seconds): `ended_at`, else now
    /// for an open read, else — put down with no recorded instant — its
    /// `last_modified`, an upper bound used only to place it.
    pub at: i64,
    /// Whole UTC days from start to end (finished or put down) — *Days =
    /// finished − started*. `null` without both ends.
    #[serde(default)]
    pub days: Option<i64>,
    /// The review's rating, raw and with its scale.
    #[serde(default)]
    pub rating: Option<RatingDto>,
    /// Passages kept during this read.
    pub passages: i64,
    /// Notes written during this read: a reflection or review names it, an
    /// ordinary note is placed by the read whose window holds its writing.
    pub notes: i64,
    /// `YYYY-MM` (UTC) from start to `at`, oldest first — the spine's bar.
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

/// A read with its book — one card of the History timeline.
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

/// A claimant of the activity log and its proper name.
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

/// A run of consecutive reading days **that has ended**. Days are `YYYY-MM-DD`.
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

/// The headline figures for a span. **There is no current run on this
/// shape**: `longest_run` has ended, and a run still under way is never
/// reported, so nothing counts up while it happens.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryFiguresDto {
    pub books_finished: i64,
    /// Reads under way at the span's end.
    pub reading_now: i64,
    pub put_down: i64,
    /// `null` when nothing measured pages in the span — not zero.
    #[serde(default)]
    pub pages: Option<i64>,
    /// Days of the span so far; the per-day denominator.
    pub elapsed_days: i64,
    #[serde(default)]
    pub pages_per_day: Option<f64>,
    /// `null` when nothing measured minutes.
    #[serde(default)]
    pub minutes: Option<i64>,
    /// Who measured them, most minutes first — *from KOReader and …*.
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

/// One record, naming its read.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecordDto {
    pub read: HistoryReadDto,
    /// The book's page count.
    #[serde(default)]
    pub pages: Option<i64>,
    /// Pages over days, a same-day read counting one day.
    #[serde(default)]
    pub pages_per_day: Option<f64>,
    /// Passages plus notes during the read.
    pub marks: i64,
    /// Other reads in the span with the same value, most recent first.
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

/// *This year's records*, over the reads finished in the span. Every tie goes
/// to the most recent finish.
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
    /// Ratings meet as a fraction of their own scale.
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

/// What a chart counts.
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

/// The bucket a chart groups by. **Weeks start on Monday.**
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

/// How a bucket is split. `device` splits by the log's claimant
/// (`koreader`, `local`, …) — it records no finer device identity.
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

/// What a chart is compared with.
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
    /// The series' own per-bucket average.
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

/// One part of a split bucket. `key` is a book id (or `none`) or a claimant.
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
    /// The calendar bucket's own first day (a Monday, a 1st) — *Week of 5 Oct*.
    pub period: String,
    /// The first and last day counted: the bucket clipped to the span and to
    /// today. The buckets' `[start, end]` partition the span's days.
    pub start: String,
    pub end: String,
    pub value: i64,
    /// Pages/minutes: `false` means nothing measured this bucket — draw an
    /// absence, not a zero. Always `true` for the counted metrics.
    pub known: bool,
    /// Largest first; empty when unsplit; sums to `value`.
    pub parts: Vec<SeriesPartDto>,
    #[serde(default)]
    pub compare: Option<f64>,
}

/// A chart's buckets and the figures beside it.
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
    /// Per bucket — *97 a week on average*.
    #[serde(default)]
    pub average: Option<f64>,
    /// Index into `buckets` of the largest (the earliest of equals); `null`
    /// when every bucket is zero.
    #[serde(default)]
    pub busiest: Option<i64>,
    /// `last_year` only: the comparison summed — last year **to date**.
    #[serde(default)]
    pub compare_total: Option<i64>,
    /// `(total − compare_total) / compare_total` — *+34% on 2025 to date*.
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

/// One book's share of what was kept in a span.
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

/// A CSV the engine wrote into the exports folder — the path is the answer.
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

/// One row of the Library table.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryRowDto {
    pub book: ShelfBookDto,
    /// 0-based position in the engine's Title order (leading article
    /// dropped, `NOCASE`). **Sort by this**, not by re-collating the title.
    pub title_order: i64,
    /// 0-based position in the engine's Author order (surname, then the whole
    /// name; authorless last).
    pub author_order: i64,
    /// The current read — open if any, else the most recent. `null` for a
    /// book never started.
    #[serde(default)]
    pub latest: Option<ReadSummaryDto>,
    /// Every passage of the book.
    pub passages: i64,
    /// Every note on the book.
    pub notes: i64,
}

/// The chips. They partition `all`.
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

/// The whole shelf as one table, in Title order.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibraryTableDto {
    pub rows: Vec<LibraryRowDto>,
    pub counts: LibraryCountsDto,
    /// Pages across every book with a page count.
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

/// A keyset cursor into a note list: the last row seen, by the list's own
/// order (`latest_part_at DESC, id DESC`).
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteCursorDto {
    /// The row's `latest_part_at`.
    pub at: i64,
    /// The row's note id.
    pub id: i64,
}

/// A book with linked notes, and how many of its notes are in the graph.
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

/// One mark of a read — *Also from this read*. Internally tagged on `kind`,
/// `SearchHitDto`'s shape. `at` is unix seconds.
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
    /// **`highlight.text` is the reader's private passage.**
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

// ---- lists (migration 0028) -----------------------------------------------

/// A two-way mapping between an engine enum and its snake_case wire twin.
///
/// The ts-rs export attribute is written at each use, not here: `gen-ts.sh`
/// counts its literal text to know how many types the bindings must hold, so
/// this comment must not spell it either.
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
    /// How the lists home orders its lists.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    ListSortDto <=> bookmole::ListSort { Name, #[default] RecentlyChanged }
}

wire_enum! {
    /// Which of a list's entries to show.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    EntryShowDto <=> bookmole::EntryShow { #[default] All, InLibrary, NotInLibrary }
}

wire_enum! {
    /// A **view** over a list's entries; none of these rewrites `position`.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    #[derive(Default)]
    EntrySortDto <=> bookmole::EntrySort { #[default] Yours, Published, Title }
}

/// One list with the facts a list page draws.
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
    /// Entries whose book is not in the library.
    pub not_in_library: i64,
    /// Entries whose book is in the library (*Show: In your library (n)*),
    /// counted from the same entries by the same rule, never left to a
    /// frontend's subtraction.
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

/// A list as the lists home draws it: counts and the first few books.
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
    /// Entries whose book is in the library; the engine counts it in the query
    /// that counts `not_in_library`.
    pub in_library_count: i64,
    pub updated_at: i64,
    /// The first `preview` books by position.
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

/// One book on one list.
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
    /// The year the Timeline would place the book at; the engine's preference
    /// (first publication, else the edition's).
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

/// Which lists a book is on.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListMembershipDto {
    pub list_id: i64,
    pub name: String,
    /// 1-based ("No. 1 of 6").
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

/// A list's books laid along a line.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListTimelineDto {
    /// Books with a year, in the list's own order.
    pub dated: Vec<ListEntryDto>,
    /// Books without one, in the list's own order; they go last.
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

/// A shelf from another system, offered as a list.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShelfSuggestionDto {
    /// `"goodreads"`.
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

// ---- copies (migration 0027) ----------------------------------------------

wire_enum! {
    /// What kind of thing a copy is. A copy with no recorded form carries none.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    CopyFormDto <=> bookmole::CopyForm { File, Print, App }
}

wire_enum! {
    /// On what terms you have it.
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    TenureDto <=> bookmole::Tenure { Own, Borrowed, Rented, Subscription, Unrecorded }
}

wire_enum! {
    /// What a place points to.
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

/// One place a copy sits. A fact about the place, never a verdict on the copy.
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
    /// For a `reader` place: the paired device's name, looked up from the
    /// device id in `detail`. Absent when the device is not known.
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
    /// The file's lowercased format (`epub`, `pdf`, ...) when the engine
    /// knows it; absent for a reader-only or app copy.
    #[serde(default)]
    pub format: Option<String>,
    /// *Keep a copy here* would work on this copy (a file on this computer, not
    /// yet stored). Show the action only where this is true.
    pub can_keep_here: bool,
}

impl CopyDto {
    /// A copy with the facts the engine derived for it.
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
    /// Which app wrote a highlight.
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

/// Where one highlight came from, as facts. The wording (*Apple Books - this
/// Mac*) is the frontend's.
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
    /// The paired reader that copy sits on, when known.
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

/// Where a book stands, derived by the engine.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookStandingDto {
    pub in_library: bool,
    pub touched: bool,
    /// A finished reading and no copy at all: the book page asks *how did you
    /// read it?*.
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

/// One in-library book's copy facts for the Library table's filters.
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

/// What the Library table needs to filter by copy: a row per in-library book
/// that has copies (no row means no copies, every flag false), and the counts.
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

// ---- reading-app imports --------------------------------------------------

/// What one import of highlights from a reading app did: counts, and the
/// degradations. The engine's `HighlightImportSummary`, shared by all four
/// importers.
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
    /// Rows removed because the source dropped them (Preview, and Apple Books'
    /// own deletion mark). Always 0 for the Kindle, whose files never forget.
    pub removed: usize,
    /// Entries seen in the source that could not be made into a highlight.
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
    /// What a Preview rescan of one PDF did: `unchanged` (its mtime is the
    /// one recorded, so it was not read), `reimported`, or `missing` (the
    /// file is not there; the place is marked, the copy untouched).
    #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "bindings.ts"))]
    RescanOutcomeDto <=> bookmole::preview::RescanOutcome { Unchanged, Reimported, Missing }
}

/// One PDF of a book, as `ImportPreviewAnnotations` found it.
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

/// `ImportPreviewAnnotations`: every PDF place of the book, and the sum.
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

/// Which rung matched an Apple Books asset to a book. Internally tagged on
/// `rung`; only `file` carries more (which rung of the files ladder).
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
    /// Its file, through the files ladder.
    File { file_match: FileMatchDto },
    /// Its Book Store id.
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

/// What became of one Apple Books asset. Internally tagged on `state`.
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
    Created { book_id: i64 },
    /// A dry run's `created`.
    WouldCreate,
    /// Nothing matched outright and nothing was written. The candidates are
    /// the decision (`LinkAppleBooksAsset`); empty when the asset has no
    /// title to make a book from.
    Unmatched { candidates: Vec<MatchCandidateDto> },
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

/// One Apple Books asset (a book in the app) that had something to import.
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
    /// Bought in the Book Store.
    pub store_bought: bool,
    pub outcome: AppleBooksOutcomeDto,
    /// The copy its highlights are stamped with.
    #[serde(default)]
    pub copy_id: Option<i64>,
    /// Live highlights (and note-only rows) Apple Books holds for it.
    pub highlights_in_source: usize,
    /// Annotations Apple Books marks deleted.
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
    /// The annotation database read, if one was found.
    #[serde(default)]
    pub annotation_db: Option<String>,
    /// The library database read, if one was found.
    #[serde(default)]
    pub library_db: Option<String>,
    pub books: Vec<AppleBooksBookDto>,
    /// Every book's counts summed (plus annotations no asset could take); its
    /// `diagnostics` is empty.
    pub totals: HighlightImportDto,
    /// Rows in Apple Books' library table.
    pub assets_in_library: usize,
    /// Assets with nothing to import that matched no book: counted, not listed.
    pub assets_skipped: usize,
    /// Bookmarks and other annotations that are not highlights.
    pub not_highlights: usize,
    /// Highlights whose asset nothing knows.
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

/// One clippings title that matched a book (or was attached to one).
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindleClippingsBookDto {
    /// The title line as the Kindle wrote it.
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

/// A clippings title no book matched in **this** import. Nothing of it was
/// written and no book was made; `AttachKindleTitle` with this `title_line`
/// is the next move. A fact about the import just run, never a standing count.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnmatchedKindleTitleDto {
    /// The key `AttachKindleTitle` takes.
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

/// What an import of one `My Clippings.txt` did (or, dry, would do).
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindleClippingsReportDto {
    /// The file read: what `AttachKindleTitle` takes back for an unmatched
    /// title (a mounted import may read several).
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
    /// The engine's report says nothing of which file it read; the caller
    /// that named (or found) the file does.
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

/// What an import of one Kindle app notebook export did (or, dry, would do).
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KindleNotebookReportDto {
    pub dry_run: bool,
    /// The file's own title and authors, so a frontend can ask which book.
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

// ---- a copy's files -------------------------------------------------------

/// One file of a copy: where it is, how big, whether it is there now. Facts
/// only; the wording (*Not where it was*) is the frontend's.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyFileDto {
    pub copy_id: i64,
    /// `path`, `store` or `calibre`.
    pub kind: PlaceKindDto,
    /// The place's reference (a path, a sha256, a calibre uuid).
    pub reference: String,
    /// Lowercased (`epub`, `pdf`); absent when nothing says.
    #[serde(default)]
    pub format: Option<String>,
    /// Absolute. Absent for a calibre place whose files were never recorded
    /// (scanned before this existed): say it is in calibre, not where.
    #[serde(default)]
    pub path: Option<String>,
    /// For a calibre place: the library root, where `""` is calibre's own
    /// default library. Absent for other kinds.
    #[serde(default)]
    pub calibre_library: Option<String>,
    /// A file is at `path` now. Absent when there is no path to look at.
    #[serde(default)]
    pub on_disk: Option<bool>,
    /// Bytes; absent when the file is not there.
    #[serde(default)]
    pub size: Option<i64>,
    /// The place's own facts, as recorded.
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

/// Where one copy's highlights of one origin came from, and when that source
/// was last read in. **No count** on purpose.
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
    /// The paired reader the copy sits on, when known.
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

    /// The `#[serde(default)]` promise: a payload written before a field
    /// existed still parses. Every optional field carries it, so the minimum a
    /// client must send is nothing at all.
    #[test]
    fn an_empty_object_is_a_valid_book() {
        let dto: BookDto = serde_json::from_str("{}").unwrap();
        assert_eq!(dto, BookDto::default());
    }

    /// `Book -> BookDto -> Book` must not quietly drop a field, which is the
    /// one bug a hand-written `From` invites. **Timestamps are now the only
    /// deliberate exception**, and are asserted as such —
    /// `subjects`/`series`/`series_index` were the other three until this
    /// surfacing item, and their assertions were what made the gap an API gap
    /// with a name rather than a silent drop.
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
        // **And `ko_percent` does not come back**, which is deliberate rather
        // than a drop: it is a read-only projection of a device-owned column
        // whose writer is `update_progress`. The four fields beside it survive
        // only because `upsert_book` ignores them, which is where that rule
        // lives; carrying a value nothing can write would be the worse of the
        // two.
        assert_eq!(back.ko_percent, None);
        // **And neither do the four cover columns**, for a stronger version of
        // `ko_percent`'s reason: `Storage::set_cover` is their only writer *by
        // design*, since a width arriving without the file it measures is a row
        // describing an image it is not pointing at. The wire carries the three
        // readings of them (`cover_aspect`, `cover_accent`,
        // `cover_shelf_path`) and no way to write them back.
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

    /// The derived fields are **readings, not columns**: they arrive on the way
    /// out and are dropped on the way in, because there is nothing to write
    /// them to. A `series_label` of "Dune #2" beside a `series_index` of 3 is
    /// not a request.
    #[test]
    fn the_derived_fields_cross_outward_only() {
        let book = Book {
            authors: vec!["Borges, Jorge Luis".into()],
            series: Some("Dune".into()),
            // A REAL that must not print as `#2.0` — the drift `series_label`
            // exists to prevent.
            series_index: Some(2.0),
            current_page: Some(50),
            page_count: Some(200),
            ..Book::default()
        };
        let dto = BookDto::from(book);
        assert_eq!(dto.series_label.as_deref(), Some("Dune #2"));
        assert_eq!(dto.authors_display, vec!["Jorge Luis Borges".to_string()]);
        // The record itself is untouched: `authors` is what the origin spelled.
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

    /// A book crosses knowing the name it files under, and that name is the
    /// first letter's worth of `names::sort_key` — never the packed column.
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
        // The first *non-blank* author, which is `sort_key`'s choice too.
        assert_eq!(filed(&["  ", "Richard Powers"]).as_deref(), Some("Powers"));
        // No author is absence, not "": `BookSort::Author` files it last.
        assert_eq!(filed(&[]), None);
        assert_eq!(filed(&[" "]), None);

        // The surname agrees with the key the engine orders by.
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

        // Outward only, and additive: a payload written before it existed
        // still parses, and nothing on the way back carries it.
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["sort_author"], "Mandel");
        let old: BookDto = serde_json::from_str(r#"{"title":"t"}"#).unwrap();
        assert_eq!(old.sort_author, None);
        assert!(!serde_json::to_string(&dto).unwrap().contains('\u{1}'));
    }

    /// A candidate crosses the seam knowing who wrote it.
    ///
    /// The wire is where this matters most: a GUI links this crate and **not**
    /// the engine, so without the field the only way to put an author in
    /// a chooser row was a `get_book` per candidate — one request per row, on a
    /// list a calibre import produces per row of its own. The names arrive
    /// already parsed for the same reason: `bookmole::names` is not
    /// reachable from TypeScript.
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

        // Both fields are `#[serde(default)]`, so a payload written before they
        // existed still parses — which is what let them land without moving
        // `API_VERSION`.
        let old: MatchCandidateDto =
            serde_json::from_str(r#"{"book_id":7,"title":"Dune","score":0.71}"#).unwrap();
        assert!(old.authors_display.is_empty());
        assert_eq!(old.publish_year, None);
    }

    /// The false denominator, at the seam. A real book in `make dev-db` has
    /// `page_count = 0`; a CLI dividing by it once printed `[12/0]`.
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

    /// An importer can write a status this build does not know, and it must
    /// cross whole rather than collapsing to a placeholder or failing to parse.
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
        // And back to the column's own word, unchanged.
        assert_eq!(
            Book::from(round).reading_status.as_deref(),
            Some("tsundoku")
        );
    }

    /// No reading at all is **absence**, not a state. There is no `NeverOpened`
    /// variant and this is what asserts it stays that way — a named state is
    /// one a UI filters on, counts, and eventually puts a badge beside — a
    /// count of what the reader has not done, which bookmole never shows.
    #[test]
    fn no_reading_is_absence_rather_than_a_variant() {
        let dto = BookDto::from(Book::default());
        assert_eq!(dto.reading_state, None);
        assert_eq!(dto.progress, ProgressDto::Untouched);
    }

    /// The whole argument for mirroring `DiagnosticKind` in full rather than
    /// flattening it to a string: the far side can still tell *which* file, and
    /// still tell a timeout from a 500.
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
        // And the CLI's exact wording survives, so no client re-derives it.
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

    /// An unmodelled KOReader status must arrive as itself. Collapsing it would
    /// be exactly the silence `UnknownDeviceStatus` exists to break.
    #[test]
    fn an_unknown_device_status_keeps_its_raw_string() {
        let dto = KoStatusDto::from(KoStatus::Other("tsundoku".into()));
        let json = serde_json::to_value(&dto).unwrap();
        assert_eq!(json["status"], "other");
        assert_eq!(json["raw"], "tsundoku");
    }

    /// The last leg of a card's round trip. `FlashcardRow` gained the two
    /// handles; this is what makes them reach a client, and the reason to
    /// assert it is that a hand-written `From` is exactly where a new field is
    /// forgotten — with no symptom but a card that cannot find its passage.
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

    /// `highlight_id` may be absent — an unanchored card is ordinary — and
    /// `book_id` may **not**. A default there would parse an old reply into a
    /// card belonging to book `0`, which is not a book and which no client
    /// could tell from one somebody deleted.
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

    /// Migration `0021` stamped two breadcrumbs on `paired_devices` that the
    /// CLI read; a wire that did not carry them would let a client say
    /// *nothing brought across* about a reader that had pushed that morning.
    /// Both cross, under the keys generated TypeScript is keyed by — the
    /// serialized form is asserted because a renamed field would type-check on
    /// both sides and arrive as `null` in the client.
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

        // Absent is `null`, not a missing key: a client distinguishes *no row
        // for this* from *not since we started recording* only if the key is
        // always there.
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
