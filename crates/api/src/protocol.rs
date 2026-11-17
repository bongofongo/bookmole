//! Request, response and envelope types for the newline-delimited JSON wire.
//! Transports only move bytes; no code outside this crate branches on the method.

use serde::{Deserialize, Serialize};

use crate::dto::*;
use crate::error::ApiError;

/// Bumped when an existing request's fields or meaning change, or a request is removed. A new
/// request does not bump it.
pub const API_VERSION: u32 = 2;

/// The build, for a human reading a log. Never branch on it.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// One call. Every variant maps to exactly one [`crate::Api`] method.
#[allow(clippy::large_enum_variant)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    ApiVersion,
    Paths,
    LibraryLocation,
    GoogleApiKey,
    SetGoogleApiKey {
        #[serde(default)]
        key: Option<String>,
    },
    VerifyGoogleKey {
        key: String,
    },

    Search {
        request: SearchRequestDto,
    },
    LookupIsbn {
        isbn: String,
    },

    SaveBook {
        book: BookDto,
    },
    /// Save a search result as `save_book` does, recording each field in `claims` as supplied by its
    /// provider. Send the result's `book` and `claims` as search returned them; a field the user
    /// owns keeps its value. Refused with `invalid_input` when a claim names no book field.
    SaveSearchResult {
        book: BookDto,
        claims: Vec<FieldClaimDto>,
    },
    ListBooks {
        /// Negative is no limit.
        limit: i64,
        #[serde(default)]
        sort: BookSortDto,
        /// Rows to skip. An offset, not a cursor: two sorts have no cursor key in the database.
        #[serde(default)]
        offset: i64,
        #[serde(default)]
        filter: Option<BookFilterDto>,
    },
    CountBooks {
        #[serde(default)]
        filter: Option<BookFilterDto>,
    },
    /// Highlight, note and owned-file counts for a page of books; one row per id, in the order asked.
    BookSummaries {
        book_ids: Vec<i64>,
    },
    GetBook {
        id: i64,
    },
    ResolveBooks {
        selector: String,
    },
    BookTags {
        book_id: i64,
    },
    /// Ask the providers about a book already in the library. No bulk form.
    EnrichBook {
        book_id: i64,
    },
    /// Record user-entered fields. Sets and cannot clear; written fields are stamped `user` and
    /// held against provider merges.
    SetBookFields {
        book_id: i64,
        fields: BookDto,
    },
    /// Where each field of a book came from, and when. An absent field is unclaimed.
    FieldProvenance {
        book_id: i64,
    },
    CurrentlyReading {
        limit: i64,
    },
    DeleteBook {
        id: i64,
    },
    FetchCover {
        book_id: i64,
    },
    MergeBooks {
        src: i64,
        dst: i64,
    },

    ListReadings {
        book_id: i64,
    },
    ListReadingRows {
        /// Negative is no limit.
        limit: i64,
        #[serde(default)]
        sort: ReadingSortDto,
        #[serde(default)]
        offset: i64,
        #[serde(default)]
        filter: Option<ReadingFilterDto>,
    },
    CountReadings {
        #[serde(default)]
        filter: Option<ReadingFilterDto>,
    },
    /// Years in which the matching readings ended, newest first, and whether any is still open.
    ReadingYears {
        #[serde(default)]
        filter: Option<ReadingFilterDto>,
    },
    GetReading {
        id: i64,
    },
    ActiveReading {
        book_id: i64,
    },
    UpdateProgress {
        book_id: i64,
        #[serde(default)]
        page: Option<i64>,
        #[serde(default)]
        finished: Option<bool>,
    },
    Reread {
        book_id: i64,
    },
    /// Finish the book's open read on `finished_on` (`YYYY-MM-DD`, UTC; absent is now). Refused with
    /// `invalid_input` when no read is open or the day is before the start or in the future.
    FinishReading {
        book_id: i64,
        #[serde(default)]
        finished_on: Option<String>,
    },
    /// Put the book's open read down: it stays open and in the book's reads, marked `abandoned` with
    /// `put_down_at`, and leaves currently-reading, reading-now and the timeline. Refused with
    /// `invalid_input` when no read is open or it is already put down.
    PutDownReading {
        book_id: i64,
    },
    /// Pick a put-down read back up: `reading` again, `put_down_at` cleared. Refused with
    /// `invalid_input` when the open read is not put down.
    PickUpReading {
        book_id: i64,
    },
    /// Set the edition's page count, stamped `user`; refused below 1.
    SetPageCount {
        book_id: i64,
        page_count: i64,
    },

    ListHighlights {
        book_id: i64,
    },
    HighlightsForReading {
        reading_id: i64,
    },
    CardPassage {
        reading_id: i64,
    },
    /// Choose a reading's card passage, or clear it with a `null` `highlight_id`.
    /// Must be one of `CardPassageCandidates`, else `InvalidInput`.
    SetCardPassage {
        reading_id: i64,
        #[serde(default)]
        highlight_id: Option<i64>,
    },
    /// Highlights of the book attributed to this reading or to none: exactly what `SetCardPassage`
    /// accepts.
    CardPassageCandidates {
        reading_id: i64,
    },
    SetAnnotation {
        highlight_id: i64,
        #[serde(default)]
        annotation: Option<String>,
    },

    ImportEpub {
        path: String,
    },
    ImportFile {
        path: String,
        /// Create a book even over a near-miss candidate; otherwise an ambiguous file is `Unmatched` and
        /// nothing is written.
        #[serde(default)]
        new: bool,
    },
    AddFileToBook {
        book_id: i64,
        path: String,
    },
    IdentifyFile {
        path: String,
    },
    /// Paths as the book files they hold: files pass through, folders are walked (bounded),
    /// calibre libraries are named, not walked. Writes nothing.
    ExpandBookPaths {
        paths: Vec<String>,
    },
    BookFiles {
        book_id: i64,
    },
    /// The chapter list, read from the owned epub on every call and stored nowhere.
    TableOfContents {
        book_id: i64,
    },
    FilePath {
        sha256: String,
    },
    RemoveFile {
        sha256: String,
    },

    ImportKoreader {
        path: String,
        #[serde(default)]
        dry_run: bool,
    },
    PullBookFromSidecar {
        path: String,
    },
    SidecarCandidates {
        path: String,
    },
    LinkSidecar {
        path: String,
        book_id: i64,
    },

    CandidateMounts,
    IsKoreaderMount {
        path: String,
    },
    ScanDevice {
        root: String,
    },
    SyncDevice {
        paths: Vec<String>,
    },
    PluginStatus {
        mount: String,
    },
    /// Install or upgrade the plugin and pair with the reader. Explicit act only: never call it on a
    /// mount event.
    InstallPlugin {
        mount: String,
    },
    UninstallPlugin {
        mount: String,
    },
    PairedDevices,
    /// Forget a pairing without the reader: drops our row only. `false` means no such pairing.
    ForgetDevice {
        device_id: String,
    },
    /// Name a reader. An empty or whitespace-only `label` clears the name; `false` means no such
    /// pairing.
    RenameDevice {
        device_id: String,
        label: String,
    },
    /// Bring across everything one mounted reader has to offer, rescanning the mount.
    /// Does not import measured reading time; see `ImportDeviceStatistics`.
    SyncMount {
        mount: String,
    },

    ListenerStatus,
    /// Open the wireless listener for `minutes`: absent is five, `Some(0)` is until stopped.
    /// Closes after the first completed push. Explicit act only: never call it on startup.
    StartListening {
        #[serde(default)]
        minutes: Option<u32>,
    },
    StopListening,
    /// Fetch from a paired reader whose own window is open; needs no `StartListening`.
    /// Refused with `wireless_refused` when nothing answers. Replies with one report per sidecar.
    PullFromReader {
        device_id: String,
    },
    /// Pair with a reader over wifi using the code it shows (case, spaces and hyphens ignored).
    /// `code` is a secret: never log or keep it. `host` is an optional extra address to try.
    /// Refused with `wireless_refused`; a wrong code and no reader in reach look the same.
    PairWithReader {
        code: String,
        #[serde(default)]
        host: Option<String>,
    },

    /// Measured reading time from the device's `statistics.sqlite3`; never part of a device sync.
    ImportDeviceStatistics {
        mount: String,
    },

    /// Rebuild the activity log from stored highlights, notes and readings. Idempotent; no importer
    /// calls it.
    RefillReadingEvents,
    ReadingEvents {
        book_id: i64,
    },
    /// Both ends inclusive, `YYYY-MM-DD`. An inverted range is `InvalidInput`.
    ActivitySummary {
        from: String,
        to: String,
    },
    ActivityByDay {
        from: String,
        to: String,
    },
    /// The period by month; only months with an event, edge months clipped to the range.
    /// Not derivable from `ActivityByDay`: null minutes and distinct books do not sum.
    ActivityByMonth {
        from: String,
        to: String,
    },

    /// Unshown moments, newest first. Poll it; there is no push. `limit` takes from the newest end.
    PendingMoments {
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Record that a moment was surfaced. Idempotent; an unknown kind in `id` is `InvalidInput`.
    AcknowledgeMoment {
        id: String,
    },

    CreateNote {
        note: NewNoteDto,
    },
    /// Notes, newest first, for one book, one reading or the whole vault. Absent `limit` is every note.
    /// `book_id` and `reading_id` are alternatives: sending both is `InvalidInput`.
    ListNotes {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        reading_id: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Notes and highlights matching one query as a single ranked list. `source` narrows the indexes
    /// (absent is both), `book_id` narrows to one book (absent is the library); an empty query is no
    /// hits.
    SearchMarks {
        query: String,
        #[serde(default)]
        source: Option<SearchSourceDto>,
        #[serde(default)]
        book_id: Option<i64>,
        limit: i64,
    },
    GetNote {
        id: i64,
    },
    /// One older note to offer back today, or `null`. `today` is the caller's local `YYYY-MM-DD`.
    ResurfaceNote {
        today: String,
    },
    /// One older passage to offer back today, or `null`; same rule and `today` as `ResurfaceNote`.
    ResurfacePassage {
        today: String,
    },
    NoteForReading {
        reading_id: i64,
        kind: NoteKindDto,
    },
    NotePath {
        note_id: i64,
    },
    NoteBody {
        note_id: i64,
    },
    UpdateNoteBody {
        note_id: i64,
        body: String,
    },
    DeleteNote {
        note_id: i64,
    },
    RefreshNoteFromDisk {
        note_id: i64,
    },
    OutgoingLinks {
        note_id: i64,
    },
    Backlinks {
        note_id: i64,
    },
    /// Both directions of a note's links with the other note's book face and, for incoming links,
    /// the holding paragraph. Incoming first, then outgoing in body order; self-links excluded.
    LinkContexts {
        note_id: i64,
    },

    /// A page of note rows by `latest_part_at` DESC then id DESC. `kinds` absent is every kind;
    /// `until` (unix seconds, exclusive) bounds the latest part. `book_id` and `reading_id` are
    /// alternatives (both is `invalid_input`). A negative `limit` is no limit.
    ListNoteRows {
        #[serde(default)]
        kinds: Option<Vec<NoteKindDto>>,
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        reading_id: Option<i64>,
        #[serde(default)]
        ids: Option<Vec<i64>>,
        #[serde(default)]
        until: Option<i64>,
        limit: i64,
    },
    NotePeriods,
    /// The threads holding at least one of `note_ids`, newest member first. `since` (unix seconds)
    /// is what `joined` counts from.
    ThreadsFor {
        note_ids: Vec<i64>,
        #[serde(default)]
        since: Option<i64>,
    },
    /// Every wikilink written in these notes, newest source first, body order
    /// within one.
    LinksFromNotes {
        note_ids: Vec<i64>,
    },
    NoteGraph {
        scope: GraphScopeDto,
    },
    NoteParts {
        note_id: i64,
    },
    /// Append a dated part; answers every part after the append. Refuses an
    /// empty body and a body holding a part marker line (`invalid_input`).
    ContinueNote {
        note_id: i64,
        body: String,
        #[serde(default)]
        page: Option<i64>,
    },
    /// Rewrite part `n` (1-based) and nothing else — markers untouched.
    /// `UpdateNoteBody` still means the whole raw body, markers included.
    UpdateNotePart {
        note_id: i64,
        n: i64,
        body: String,
    },
    /// Notes that mention this note's title without linking it, at most
    /// `limit`, in the index's rank. A title under three characters, or
    /// one derived from the body's first words, mentions nothing.
    UnlinkedMentions {
        note_id: i64,
        limit: i64,
    },
    /// Link the first unlinked mention of `to`'s title in `from`'s body, and
    /// re-index it. `not_found` when the phrase is no longer there.
    LinkMention {
        from_note_id: i64,
        to_note_id: i64,
    },
    /// A passage typed in by hand: `source = "user"`, never touched by a
    /// KOReader import. Refuses empty text. Answers `Highlight` — always a
    /// value, the row as stored.
    CreateHighlight {
        book_id: i64,
        text: String,
        #[serde(default)]
        page: Option<i64>,
        #[serde(default)]
        chapter: Option<String>,
    },
    /// What an unsaved note would link to, and the dangling links in other
    /// notes that it would resolve. `title` absent means the body's
    /// `# heading`.
    PreviewLinks {
        #[serde(default)]
        title: Option<String>,
        body: String,
    },

    /// `ListNoteRows` with a keyset cursor: only rows strictly after `after`
    /// in the list's order (`latest_part_at DESC, id DESC`), so two notes
    /// sharing a second at a page edge are never skipped. `after: null` is the
    /// first page. Every other field means what it means on `ListNoteRows`.
    ListNoteRowsAfter {
        #[serde(default)]
        kinds: Option<Vec<NoteKindDto>>,
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        reading_id: Option<i64>,
        #[serde(default)]
        ids: Option<Vec<i64>>,
        #[serde(default)]
        until: Option<i64>,
        #[serde(default)]
        after: Option<NoteCursorDto>,
        limit: i64,
    },
    /// One passage by id. Answers `Highlight` — `null` when there is none.
    GetHighlight {
        id: i64,
    },
    /// The face of each asked book, in the order asked; an id naming no book
    /// is left out. (`BookSummaries` carries counts and no name.)
    BookFaces {
        book_ids: Vec<i64>,
    },
    NoteBooks,
    /// A read's notes and passages in one order, newest first. Notes are the
    /// read's by `reading_id` or, for an ordinary note, by the read whose
    /// window holds its writing. A negative `limit` is no limit; an unknown
    /// reading is `not_found`.
    ReadMarks {
        reading_id: i64,
        limit: i64,
    },
    GetSetting {
        key: String,
    },
    /// Store a frontend's choice. `key` is `[a-z0-9._-]{1,64}` and `value` must
    /// be JSON (`invalid_input` otherwise). The engine never interprets it.
    SetSetting {
        key: String,
        value: String,
    },
    /// Every setting whose key starts with `prefix` (literal; `""` is all).
    ListSettings {
        prefix: String,
    },
    /// Every read in the span — open, finished, put down — newest end first.
    /// `since` inclusive, `until` exclusive, unix seconds; either may be
    /// absent. An inverted span is `invalid_input`.
    HistoryTimeline {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    /// The headline figures for the span. Only ended runs are reported; there is no current run.
    HistoryFigures {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    HistoryRecords {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    HistorySeries {
        metric: HistoryMetricDto,
        group: HistoryGroupDto,
        split: HistorySplitDto,
        compare: HistoryCompareDto,
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    /// What was kept, by book: passages and notes dated in the span, most
    /// first. A negative `limit` is no limit.
    HistoryKept {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
        limit: i64,
    },
    ExportHistorySeries {
        metric: HistoryMetricDto,
        group: HistoryGroupDto,
        split: HistorySplitDto,
        compare: HistoryCompareDto,
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    LibraryTable,

    /// Which 1-based parts of a continued note `query` matches, ascending, by the same rules as
    /// `SearchMarks`; empty when only the title matched. `not_found` for an unknown note.
    MatchedParts {
        note_id: i64,
        query: String,
    },
    /// `NotePeriods` in the reader's zone: months read at a fixed offset
    /// `utc_offset_minutes` east of UTC (a browser sends
    /// `-new Date().getTimezoneOffset()`). Beyond ±1080 is `invalid_input`.
    NotePeriodsIn {
        utc_offset_minutes: i32,
    },
    /// The reads in progress now: the open reads, each with this read's passages, notes
    /// and progress, most recently begun first. No span — now is not a period.
    /// Answers `HistoryTimeline`, the same row.
    HistoryReadingNow,

    OpenReflection {
        book_id: i64,
        #[serde(default)]
        reading_id: Option<i64>,
    },
    OpenReview {
        book_id: i64,
        #[serde(default)]
        reading_id: Option<i64>,
    },

    SetRating {
        note_id: i64,
        value: f64,
    },
    ReviewRating {
        note_id: i64,
    },
    ClearReviewRating {
        note_id: i64,
    },
    GoodreadsRating {
        note_id: i64,
    },
    PutRatingScale {
        name: String,
        min: f64,
        max: f64,
        step: f64,
    },
    RatingScales,
    ActiveRatingScale,
    RatingScaleByName {
        name: String,
    },
    RatingMap {
        scale_id: i64,
    },
    MapRating {
        scale_id: i64,
        value: f64,
        goodreads: f64,
    },

    Cite {
        note_id: i64,
        highlight_id: i64,
    },
    Uncite {
        note_id: i64,
        highlight_id: i64,
    },
    CitationsFor {
        note_id: i64,
    },
    /// Which highlight ids each of these notes cites: one entry per requested id, in the order
    /// asked, empty for a note that cites nothing or does not exist.
    CitationsForNotes {
        note_ids: Vec<i64>,
    },

    ImportGoodreads {
        path: String,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        create_ambiguous: bool,
    },
    ExportGoodreads,
    LinkGoodreadsRow {
        external_id: String,
        book_id: i64,
    },

    CalibreStatus,
    ConvertEbook {
        input: String,
        output: String,
        #[serde(default)]
        overwrite: bool,
    },
    CalibreLibrary {
        #[serde(default)]
        library: Option<String>,
    },
    ImportCalibreLibrary {
        #[serde(default)]
        library: Option<String>,
        #[serde(default)]
        dry_run: bool,
        #[serde(default)]
        create_ambiguous: bool,
        /// Import only these calibre rows; empty is the whole library.
        #[serde(default)]
        only: Vec<i64>,
    },
    LinkCalibreBook {
        uuid: String,
        book_id: i64,
    },

    /// Capture a card from a book, optionally anchored to a highlight. Replies `true` if created,
    /// `false` if the book already had that word; a highlight of another book is `InvalidInput`.
    CreateFlashcard {
        book_id: i64,
        #[serde(default)]
        highlight_id: Option<i64>,
        word: String,
        #[serde(default)]
        context: Option<String>,
    },
    ListFlashcards {
        #[serde(default)]
        include_exported: bool,
    },
    ListFlashcardsForBook {
        book_id: i64,
    },
    ExportFlashcards {
        #[serde(default)]
        include_exported: bool,
    },
    /// Anki text-import format with a stable GUID per card. `book_id: None` is the whole library;
    /// exported cards are marked, so `include_exported: false` means cards made since the last file.
    ExportAnki {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        include_exported: bool,
    },
    /// `ExportAnki` written into `exports/` beside the vault; nothing is written when no card went out.
    SaveAnkiExport {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        include_exported: bool,
    },

    ListLists {
        #[serde(default)]
        sort: ListSortDto,
        #[serde(default)]
        preview: u8,
    },
    GetList {
        id: i64,
    },
    /// A blank name is `invalid_input`. Two lists may share a name.
    CreateList {
        name: String,
        #[serde(default)]
        description: Option<String>,
    },
    UpdateList {
        id: i64,
        name: String,
        #[serde(default)]
        description: Option<String>,
    },
    DeleteList {
        id: i64,
    },
    /// The entries as a **view**: `sort` never rewrites `position`.
    ListEntries {
        list_id: i64,
        #[serde(default)]
        show: EntryShowDto,
        #[serde(default)]
        sort: EntrySortDto,
    },
    AddToList {
        list_id: i64,
        book_id: i64,
        #[serde(default)]
        note: Option<String>,
    },
    RemoveFromList {
        list_id: i64,
        book_id: i64,
    },
    MoveInList {
        list_id: i64,
        book_id: i64,
        to: u32,
    },
    SetListNote {
        list_id: i64,
        book_id: i64,
        #[serde(default)]
        note: Option<String>,
    },
    ListsForBook {
        book_id: i64,
    },
    /// Put a search result on a list without owning it. An already-known book is reused (ISBN, then
    /// title); otherwise it is created with no copy. Idempotent.
    AddCandidateToList {
        list_id: i64,
        book: BookDto,
        #[serde(default)]
        note: Option<String>,
    },
    /// `AddCandidateToList` for a search result: a book it creates records `claims` as
    /// `SaveSearchResult` does. A book already known is reused unchanged.
    AddSearchResultToList {
        list_id: i64,
        book: BookDto,
        claims: Vec<FieldClaimDto>,
        #[serde(default)]
        note: Option<String>,
    },
    ListTimeline {
        list_id: i64,
    },
    /// Shelves from another system offered as lists; a dismissed or kept one is not offered again.
    ShelfSuggestions {
        #[serde(default)]
        preview: u8,
    },
    KeepShelf {
        source: String,
        tag: String,
    },
    DismissShelf {
        source: String,
        tag: String,
    },

    CopiesForBook {
        book_id: i64,
    },
    /// Store the file of this copy's `path` place in the library's folder and attach a `store` place.
    /// `invalid_input` for a copy with no file on this computer.
    KeepCopyHere {
        copy_id: i64,
    },
    HighlightOrigins {
        book_id: i64,
    },
    /// Groups of a book's highlight ids that are the same passage seen by more
    /// than one row (usually two apps). Rows are never merged.
    SamePassages {
        book_id: i64,
    },
    AddCopy {
        book_id: i64,
        #[serde(default)]
        form: Option<CopyFormDto>,
        tenure: TenureDto,
        #[serde(default)]
        have_now: Option<bool>,
        #[serde(default)]
        from_whom: Option<String>,
        #[serde(default)]
        app: Option<String>,
        #[serde(default)]
        location: Option<String>,
        #[serde(default)]
        started_at: Option<i64>,
        #[serde(default)]
        due_at: Option<i64>,
        #[serde(default)]
        note: Option<String>,
    },
    /// Write a copy's user-entered fields as given: an absent field clears it.
    /// `have_now` is written only by `EndCopy` and `ResumeCopy`.
    UpdateCopy {
        copy_id: i64,
        #[serde(default)]
        form: Option<CopyFormDto>,
        tenure: TenureDto,
        #[serde(default)]
        from_whom: Option<String>,
        #[serde(default)]
        app: Option<String>,
        #[serde(default)]
        location: Option<String>,
        #[serde(default)]
        started_at: Option<i64>,
        #[serde(default)]
        due_at: Option<i64>,
        #[serde(default)]
        ended_at: Option<i64>,
        #[serde(default)]
        lent_to: Option<String>,
        #[serde(default)]
        unreadable: bool,
        #[serde(default)]
        note: Option<String>,
    },
    /// Set `have_now = false`. `ended_at`, when given, is stored; otherwise it is left as it was.
    EndCopy {
        copy_id: i64,
        #[serde(default)]
        ended_at: Option<i64>,
    },
    ResumeCopy {
        copy_id: i64,
    },
    /// Delete a copy recorded by mistake; use `EndCopy` to end one.
    DeleteCopy {
        copy_id: i64,
    },
    BookStanding {
        book_id: i64,
    },
    LibraryCopies,

    ImportPreviewAnnotations {
        book_id: i64,
    },
    /// Preview: make this PDF a file of the book and import its annotations now.
    /// Not a `.pdf`, or already another book's copy, is `invalid_input`.
    ImportPreviewFile {
        book_id: i64,
        path: String,
    },
    /// Apple Books on this Mac. No Apple Books data is a report carrying `apple_books_absent`, not an
    /// error.
    ImportAppleBooks {
        #[serde(default)]
        dry_run: bool,
    },
    /// Resolve an `unmatched` asset to this book; records an app copy so the next import matches it.
    LinkAppleBooksAsset {
        asset_id: String,
        book_id: i64,
    },
    FindKindleClippings,
    ImportKindleClippings {
        path: String,
        #[serde(default)]
        dry_run: bool,
    },
    ImportMountedKindleClippings {
        #[serde(default)]
        dry_run: bool,
    },
    AttachKindleTitle {
        path: String,
        title_line: String,
        book_id: i64,
    },
    /// The Kindle app's Export Notebook (`.html`), into `book_id` or the book its title matches.
    /// No match creates no book.
    ImportKindleNotebook {
        path: String,
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        dry_run: bool,
    },
    /// `ResolveBooks` over in-library books only; a copy-less book on a list never answers.
    ResolveLibraryBooks {
        selector: String,
    },

    CopyFiles {
        book_id: i64,
    },
    CopyHighlightSources {
        book_id: i64,
    },
    /// This copy's file is at `path` now. Absent is `not_found`; not a file or another book's file
    /// is `invalid_input`. Missing `path` places are replaced; a calibre place stays.
    RelocateCopyFile {
        copy_id: i64,
        path: String,
    },
}

#[allow(clippy::large_enum_variant)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", content = "value", rename_all = "snake_case")]
pub enum Response {
    Unit,
    Bool(bool),
    Id(i64),
    Text(String),
    MaybePath(Option<String>),
    Paths(Vec<String>),
    Version {
        api: u32,
        crate_version: String,
    },
    Where(PathsDto),
    LibraryLocation(LibraryLocationDto),

    /// A number of rows. Not `Id`: a count measures and an id identifies.
    Count(i64),

    Book(Option<BookDto>),
    Books(Vec<BookDto>),
    BookSummaries(Vec<BookSummaryDto>),
    BookTags(Vec<BookTagDto>),
    OpenReadings(Vec<OpenReadingDto>),
    MergeReport(MergeReportDto),

    Reading(Option<ReadingDto>),
    Readings(Vec<ReadingDto>),
    ReadingRows(Vec<ReadingRowDto>),
    ReadingYears(ReadingYearsDto),

    Highlights(Vec<HighlightDto>),
    Highlight(Option<HighlightDto>),

    SearchOutcome(SearchOutcomeDto),

    Note(Option<NoteDto>),
    Resurfaced(Option<ResurfacedDto>),
    ResurfacedPassage(Option<ResurfacedPassageDto>),
    Notes(Vec<NoteDto>),
    /// Highlight ids each requested note cites, one entry per id asked, in order, empties included.
    NoteCitations(Vec<NoteCitationsDto>),
    SearchHits(Vec<SearchHitDto>),
    Links(Vec<OutgoingLinkDto>),
    LinkContexts(Vec<LinkContextDto>),
    CreatedNote(CreatedNoteDto),
    NoteRows(Vec<NoteRowDto>),
    NotePeriods(Vec<NotePeriodDto>),
    Threads(Vec<ThreadDto>),
    NoteLinks(Vec<NoteLinkDto>),
    NoteGraph(NoteGraphDto),
    /// Every part of one note, in file order.
    NoteParts(Vec<NotePartDto>),
    LinkPreview(LinkPreviewDto),
    BookFaces(Vec<BookFaceDto>),
    NoteBooks(Vec<NoteBookDto>),
    ReadMarks(Vec<ReadMarkDto>),
    /// `GetSetting`'s answer: the JSON text, or `null`.
    Setting(Option<String>),
    Settings(Vec<SettingDto>),
    HistoryTimeline(Vec<HistoryReadDto>),
    HistoryFigures(HistoryFiguresDto),
    HistoryRecords(HistoryRecordsDto),
    HistorySeries(HistorySeriesDto),
    HistoryKept(Vec<KeptRowDto>),
    SavedCsv(SavedCsvDto),
    LibraryTable(LibraryTableDto),
    /// `MatchedParts`' answer: part numbers, ascending.
    Parts(Vec<i64>),

    Rating(Option<RatingDto>),
    GoodreadsRating(Option<f64>),
    RatingScale(Option<RatingScaleDto>),
    RatingScales(Vec<RatingScaleDto>),
    RatingMap(Vec<RatingMapEntryDto>),

    BookFiles(Vec<BookFileDto>),
    FileIdentity(FileIdentityDto),
    ExpandedPaths(ExpandedPathsDto),
    FileImport(FileImportReportDto),
    /// `null` means no file to read a chapter list from; a present value may have no entries.
    TableOfContents(Option<TableOfContentsDto>),

    EnrichReport(EnrichReportDto),
    FieldProvenance(Vec<FieldSourceDto>),

    ReadingEvents(Vec<ReadingEventDto>),
    /// Newest first. The protocol states no count.
    Moments(Vec<MomentDto>),
    RefillReport(RefillReportDto),
    ActivitySummary(ActivitySummaryDto),
    ActivityByDay(Vec<DayActivityDto>),
    ActivityByMonth(Vec<MonthActivityDto>),
    StatsImport(StatsImportReportDto),

    ImportReport(ImportReportDto),
    PullReport(PullReportDto),
    PullReports(Vec<PullReportDto>),
    Candidates(Vec<MatchCandidateDto>),
    DeviceScan(DeviceScanDto),

    PluginStatus(PluginStatusDto),
    ListenerStatus(ListenerStatusDto),
    PluginInstalled(InstallReportDto),
    PluginUninstalled(UninstallReportDto),
    PairedDevices(Vec<PairedDeviceDto>),
    PairedDevice(PairedDeviceDto),
    MountSync(MountSyncDto),

    GoodreadsReport(GoodreadsReportDto),
    /// The CSV plus any failures along the way; the caller owns the file.
    GoodreadsExport {
        csv: String,
        warnings: Vec<DiagnosticDto>,
    },

    CalibreStatus(CalibreStatusDto),
    CalibreLibrary(Vec<CalibreBookDto>),
    CalibreReport(CalibreReportDto),

    Flashcards(Vec<FlashcardDto>),
    FlashcardExport {
        tsv: String,
        count: usize,
    },
    AnkiExport(AnkiExportDto),
    AnkiSaved(SavedAnkiExportDto),

    ListSummaries(Vec<ListSummaryDto>),
    List(ListDto),
    ListEntries(Vec<ListEntryDto>),
    ListEntry(ListEntryDto),
    ListMemberships(Vec<ListMembershipDto>),
    ListTimeline(ListTimelineDto),
    ShelfSuggestions(Vec<ShelfSuggestionDto>),

    Copies(Vec<CopyDto>),
    Copy(CopyDto),
    HighlightOrigins(Vec<HighlightOriginFactDto>),
    SamePassages(Vec<Vec<i64>>),
    BookStanding(BookStandingDto),
    LibraryCopies(LibraryCopiesDto),

    HighlightImport(HighlightImportDto),
    PreviewImport(PreviewImportDto),
    AppleBooksReport(AppleBooksReportDto),
    KindleClippingsReport(KindleClippingsReportDto),
    KindleClippingsReports(Vec<KindleClippingsReportDto>),
    KindleNotebookReport(KindleNotebookReportDto),

    CopyFiles(Vec<CopyFileDto>),
    CopyHighlightSources(Vec<CopyHighlightSourceDto>),
}

/// A request with a client-chosen id, echoed back untouched so pipelined replies can be told apart.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    #[serde(default)]
    pub id: u64,
    pub request: Request,
}

/// A reply, always well-formed. `outcome` is a tagged enum, not `Result`, whose serde shape is
/// `{"Ok":…}`.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub api_version: u32,
    pub outcome: Outcome,
}

#[allow(clippy::large_enum_variant)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { response: Response },
    Error { error: ApiError },
}

impl Reply {
    pub fn ok(id: u64, response: Response) -> Reply {
        Reply {
            id,
            api_version: API_VERSION,
            outcome: Outcome::Ok { response },
        }
    }

    pub fn err(id: u64, error: ApiError) -> Reply {
        Reply {
            id,
            api_version: API_VERSION,
            outcome: Outcome::Error { error },
        }
    }

    /// The reply as one line of JSON, newline included; serde escapes newlines inside strings,
    /// so a payload never breaks the framing. Serialization failure yields an error reply, not a panic.
    pub fn to_line(&self) -> String {
        match serde_json::to_string(self) {
            Ok(mut s) => {
                s.push('\n');
                s
            }
            Err(e) => {
                let fallback = Reply::err(
                    self.id,
                    ApiError::new(
                        crate::error::ErrorCode::Internal,
                        format!("reply could not be serialized: {e}"),
                    ),
                );
                // A plain literal, so the fallback cannot itself fail to serialize.
                serde_json::to_string(&fallback).unwrap_or_else(|_| {
                    format!(
                        r#"{{"id":{},"api_version":{API_VERSION},"outcome":{{"status":"error","error":{{"code":"internal","message":"unserializable"}}}}}}"#,
                        self.id
                    )
                }) + "\n"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_reads_as_a_method_and_params() {
        let json = serde_json::to_value(Request::GetBook { id: 3 }).unwrap();
        assert_eq!(json["method"], "get_book");
        assert_eq!(json["params"]["id"], 3);
    }

    #[test]
    fn a_parameterless_request_is_just_its_name() {
        let json = serde_json::to_string(&Request::ApiVersion).unwrap();
        assert_eq!(json, r#"{"method":"api_version"}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&json).unwrap(),
            Request::ApiVersion
        );
    }

    /// An omitted `dry_run`/`new`/`overwrite` must never take the destructive reading.
    #[test]
    fn an_omitted_flag_takes_the_safe_default() {
        let r: Request =
            serde_json::from_str(r#"{"method":"import_file","params":{"path":"/x.epub"}}"#)
                .unwrap();
        assert_eq!(
            r,
            Request::ImportFile {
                path: "/x.epub".into(),
                new: false
            }
        );
        let r: Request = serde_json::from_str(
            r#"{"method":"convert_ebook","params":{"input":"a.epub","output":"b.azw3"}}"#,
        )
        .unwrap();
        assert_eq!(
            r,
            Request::ConvertEbook {
                input: "a.epub".into(),
                output: "b.azw3".into(),
                overwrite: false
            }
        );
    }

    #[test]
    fn a_reply_round_trips() {
        let reply = Reply::ok(9, Response::Id(42));
        let line = reply.to_line();
        assert!(line.ends_with('\n'));
        assert_eq!(
            serde_json::from_str::<Reply>(line.trim_end()).unwrap(),
            reply
        );
    }

    #[test]
    fn a_newline_in_the_payload_never_becomes_a_frame_break() {
        let reply = Reply::ok(1, Response::Text("line one\nline two\n".into()));
        let line = reply.to_line();
        assert_eq!(line.matches('\n').count(), 1, "{line}");
        assert_eq!(
            serde_json::from_str::<Reply>(line.trim_end()).unwrap(),
            reply
        );
    }

    #[test]
    fn an_error_reply_names_its_code() {
        let reply = Reply::err(
            2,
            ApiError::new(crate::error::ErrorCode::NotFound, "book id 3"),
        );
        let json: serde_json::Value = serde_json::from_str(reply.to_line().trim_end()).unwrap();
        assert_eq!(json["outcome"]["status"], "error");
        assert_eq!(json["outcome"]["error"]["code"], "not_found");
        assert_eq!(json["api_version"], API_VERSION);
    }
}
