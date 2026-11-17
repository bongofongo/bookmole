//! The request/response vocabulary, and the envelope it travels in.
//!
//! This is the part `crates/daemon` exists to move bytes for and **not to
//! understand**. A transport reads a [`Call`], hands it to [`crate::Api::call`]
//! and writes the [`Reply`]; there is no branch on the method anywhere outside
//! this crate, which is the whole reason a host with no daemon (an iOS app, a
//! GUI linking this crate in-process) gets exactly the same surface.
//!
//! ## The wire, end to end
//!
//! One JSON object per line, in each direction. A client writes a [`Call`]:
//!
//! ```text
//! {"id":1,"request":{"method":"get_book","params":{"id":3}}}
//! ```
//!
//! and reads back a [`Reply`] carrying the same `id`, the server's
//! [`API_VERSION`], and an [`Outcome`] tagged by `status`:
//!
//! ```text
//! {"id":1,"api_version":2,"outcome":{"status":"ok","response":{"shape":"book","value":{…}}}}
//! {"id":1,"api_version":2,"outcome":{"status":"error","error":{"code":"not_found","message":"…"}}}
//! ```
//!
//! A method with no arguments omits `params`. A line that does not parse as a
//! `Call` at all is answered with `id: 0` and `bad_request`, because there was
//! no id to echo; a well-formed envelope naming a method this build does not
//! have keeps its id. [`Reply::to_line`] owns the framing guarantee: a reply is
//! always exactly one line, whatever newlines its strings contain.
//!
//! ## Why the requests are named and the responses are shaped
//!
//! [`Request`] has one variant per facade method, adjacently tagged, so a
//! payload reads as `{"method":"get_book","params":{"id":3}}` — a name a human
//! can grep for and a client can construct without a schema in front of them.
//!
//! [`Response`] is deliberately *not* the mirror of that. Sixty-odd response
//! variants that each appear exactly once would be sixty names to keep in sync
//! for no information: a reply is already tied to its call by [`Call::id`], so
//! the client knows what it asked. What it needs from the wire is the *shape*,
//! and there are about thirty of those.
//!
//! ## Not here: the mount watcher
//!
//! `MountWatcher` is a stream of events with no request to answer, and
//! request/response has no shape for "tell me when a reader arrives". Wrapping
//! it as a poll would be an invention rather than a translation — it would give
//! the far side a different debounce from the one `watch.rs` guarantees. A
//! subscription is its own design and belongs with the transports that need
//! one; a client on this machine calls `scan_device` when it likes.

use serde::{Deserialize, Serialize};

use crate::dto::*;
use crate::error::ApiError;

/// The version of *this vocabulary*, bumped only when an existing method or
/// shape changes meaning.
///
/// Adding a method does not bump it: an older client never sends the new name,
/// and a newer client meets [`crate::error::ErrorCode::BadRequest`] on an older
/// daemon — which is a clear failure rather than a silent misread, and is why
/// the number can stay still through ordinary growth. A new field on an
/// existing request is additive too, provided it is `#[serde(default)]`: a
/// payload written before it existed still parses to the call it always meant.
///
/// **2 — `SearchNotes` and `Response::NoteHits` were removed.** Their
/// replacement, [`Request::SearchMarks`], answers the same question and more of
/// it, so the two could have stood side by side and the number could have
/// stayed at 1. Keeping both was rejected: a client able to ask for notes alone
/// and highlights alone has to merge two rankings, has no rule for doing it,
/// and will merge them by source order — which is exactly the failure the
/// unified request exists to remove, so a second door makes the guarantee
/// optional. That is a removal, and a removal is what this number is for. It
/// costs one client (`rb notes --search`, updated here) and it can never be
/// done as cheaply again.
pub const API_VERSION: u32 = 2;

/// The build, for a human reading a log. Never branch on it.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A protocol enum is built once per call and consumed immediately, so the size
/// of its largest variant is not a cost anyone pays. Boxing to even the
/// variants out would change nothing on the wire — `Box<T>` is transparent to
/// serde — and would put a `Box::new` at every construction site for a value
/// that lives for one `await`.
///
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
    // ---- meta and configuration ----
    ApiVersion,
    Paths,
    /// Where the database is and whether that folder is inside a cloud-synced
    /// location. Read-only; a frontend asks once per launch and says so
    /// plainly when it is. A request of its own rather than a field on
    /// [`Request::Paths`]'s answer, so nothing existing changes shape.
    LibraryLocation,
    GoogleApiKey,
    SetGoogleApiKey {
        #[serde(default)]
        key: Option<String>,
    },
    VerifyGoogleKey {
        key: String,
    },

    // ---- metadata search ----
    Search {
        request: SearchRequestDto,
    },
    LookupIsbn {
        isbn: String,
    },

    // ---- library ----
    SaveBook {
        book: BookDto,
    },
    /// One page of the library.
    ///
    /// `limit` and `sort` are what this method has always taken and mean exactly
    /// what they did; `offset` and `filter` are additive and an omitted one is
    /// the old behaviour, which is why this grew rather than being replaced and
    /// why [`API_VERSION`] does not move.
    ///
    /// **`limit` selects along the sort key** — the first `limit` books *by that
    /// key*, not `limit` arbitrary books shown in that order. See
    /// [`bookmole::BookSort`].
    ListBooks {
        /// Negative is no limit.
        limit: i64,
        #[serde(default)]
        sort: BookSortDto,
        /// Rows to skip. Pagination is an **offset**, not a cursor — two of the
        /// five sorts (`Author`, `Progress`) have no cursor key that exists in
        /// the database, and they are exactly the two whose pages are already
        /// expensive, so keyset would save them nothing. A count also composes
        /// with an offset and not with a cursor: a list that knows its total
        /// wants page numbers, which *are* offsets.
        #[serde(default)]
        offset: i64,
        /// Absent is every book.
        #[serde(default)]
        filter: Option<BookFilterDto>,
    },
    /// How many books match — the number a shelf needs *before* it needs the
    /// rows.
    ///
    /// Its own method rather than a field beside the page, because a count is a
    /// property of the **filter** and a page is not: a shelf asks this once and
    /// pages many times, and bundling would make every scroll pay for a scan of
    /// the whole matching set. The clause is shared in the engine, which is
    /// where sharing it actually guarantees the two agree.
    CountBooks {
        #[serde(default)]
        filter: Option<BookFilterDto>,
    },
    /// What is behind each of these books — highlights, notes, owned files — in
    /// one call for a whole page.
    ///
    /// The detail screen makes four calls for one book; a list of eight hundred
    /// cannot, which is why no list could show this before. One reply row per id
    /// **in the order asked**, zeros included.
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
    /// Ask the providers about a book already in the library.
    ///
    /// No bulk form, here or on the facade: the per-book cost is a provider
    /// fan-out and a loop over the shelf is a rate-limit policy nobody has
    /// decided.
    EnrichBook {
        book_id: i64,
    },
    /// Record what the user says, and that it was the user who said it.
    ///
    /// **Sets and cannot clear** — the merge is the ordinary partial-record one,
    /// so a field the record is silent about is left alone. Every field written
    /// is stamped `user` and is from then on held against every provider merge.
    SetBookFields {
        book_id: i64,
        fields: BookDto,
    },
    /// Where each field of a book came from, and when. An **absent**
    /// field means nobody has claimed it.
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

    // ---- readings ----
    ListReadings {
        book_id: i64,
    },
    /// One page of the library's readings, each with its book, its own
    /// progress, its read number and its card passage — what a "wall" of
    /// reading cards draws (the word used for such a view below).
    ///
    /// **The list `readings` never had.** Every other method here is scoped to
    /// one book except `CurrentlyReading`, which is filtered to the open ones —
    /// so a *finished* reading was reachable only by already knowing its book,
    /// while `ActivitySummary::books_finished` was already counting exactly
    /// those rows.
    ///
    /// A **new method rather than four fields on `ListReadings`**, and not only
    /// because reshaping a request breaks a client: the two answer different
    /// questions and return different rows. `ListReadings` is a book's history
    /// and returns `ReadingDto`; this is a page of the library and returns
    /// [`ReadingRowDto`], which carries a book and a passage beside it.
    ///
    /// One call answers a page. The alternatives are a `GetBook` and a
    /// `CardPassage` per row — an N+1 on a list whose whole purpose is to be
    /// long, which is exactly what a paged list request exists to remove.
    ListReadingRows {
        /// Negative is no limit.
        limit: i64,
        #[serde(default)]
        sort: ReadingSortDto,
        /// Rows to skip. An offset and not a cursor, for [`Request::ListBooks`]'
        /// reason, and it applies here more simply: a count composes with an
        /// offset, and a list that knows its total wants page numbers, which
        /// *are* offsets.
        #[serde(default)]
        offset: i64,
        /// Absent is every reading.
        #[serde(default)]
        filter: Option<ReadingFilterDto>,
    },
    /// How many readings match — the number a wall needs *before* it needs the
    /// rows.
    ///
    /// Its own method rather than a field beside the page, for `CountBooks`'
    /// reason: a count is a property of the **filter**, asked once per year the
    /// reader picks, while the page is asked on every scroll. The clause is
    /// shared in the engine, which is where sharing it guarantees the two agree.
    CountReadings {
        #[serde(default)]
        filter: Option<ReadingFilterDto>,
    },
    /// Which years the readings a filter matches actually **ended** in, newest
    /// first, and whether any of them has not ended.
    ///
    /// The question behind a year picker, and until this there was no way to
    /// ask it. `ActivityByMonth` is what a wall had to use, and it is a **proxy
    /// in five directions**: `reading_events` gets a row when a read *started*,
    /// when a note was written, when a highlight carries a device date, when a
    /// device measured minutes, and it keeps the days a since-deleted reading
    /// explained — and it holds nothing at all until `RefillReadingEvents` has
    /// run, so a library that never refilled offers no years while plainly
    /// having read something. It also takes no [`ReadingFilterDto`], so it can
    /// never answer for one book.
    ///
    /// A **new method rather than a field on `CountReadings`**: it answers a
    /// different question, returns a different shape, and a new field on an
    /// existing request is a breaking change in the generated TypeScript
    /// however `#[serde(default)]` the Rust is.
    ///
    /// It takes the same `filter` the page and the count take, and the engine
    /// builds all three clauses from one predicate — so every year offered has
    /// rows behind it under the filter the wall will draw with, by construction
    /// rather than by coincidence.
    ///
    /// **Deriving this above the seam is refused for three reasons**, not one:
    /// `ListReadingRows { limit: -1 }` puts the whole library on the wire —
    /// a book and the reader's private passage per row — to draw six controls;
    /// it re-derives a fact below the seam's own rule; and
    /// `getFullYear` and `getUTCFullYear` differ by three characters while
    /// `finished_in` is UTC, so a read closed on New Year's Eve files under one
    /// year in the picker and the other in the wall for every reader west of
    /// Greenwich.
    ReadingYears {
        /// Absent is every reading.
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
    /// Finish the book's **open** read on a day the reader names.
    /// `finished_on` is `YYYY-MM-DD`,
    /// UTC; absent is now. **Refuses** with `invalid_input` when no read is
    /// open, when the day is before the read began, and when it has not
    /// happened yet — where `UpdateProgress { finished: true }` would open a
    /// read in order to close it. Answers the book, re-read.
    ///
    /// A new method rather than a day beside `finished` on `UpdateProgress`:
    /// that would reshape a request every frontend already sends, and `ts-rs`
    /// emits a new field as required however `#[serde(default)]` it is.
    FinishReading {
        book_id: i64,
        #[serde(default)]
        finished_on: Option<String>,
    },
    /// How many pages this edition has, as the reader says it — stamped
    /// `user` like any `SetBookFields` write, and refused below 1. Its own
    /// method so a frontend never builds a whole `BookDto` to move one number.
    /// Answers the book, re-read.
    SetPageCount {
        book_id: i64,
        page_count: i64,
    },

    // ---- highlights ----
    ListHighlights {
        book_id: i64,
    },
    /// What was highlighted during one reading. The reading-scoped half of
    /// `ListHighlights`, keyed by the reading rather than the book.
    HighlightsForReading {
        reading_id: i64,
    },
    /// The passage a reading's card shows — the one the reader chose — or
    /// `null` when none is chosen.
    ///
    /// **A request rather than a field, because which passage is a rule and
    /// not a row.** Every client asking this gets the same answer, so two
    /// frontends cannot show different passages for one reading. It is
    /// deliberately *not* a field on `ReadingDto`: that would put the reader's
    /// private highlight text on every row of every `ListReadings`, including
    /// the ones nobody is drawing a card for. `ListReadingRows` carries the
    /// same answer per row for a wall, so this is one call per card only
    /// where a caller holds a lone `ReadingDto`.
    CardPassage {
        reading_id: i64,
    },
    /// Choose the passage a reading's card shows, or clear it with a `null`
    /// `highlight_id`. The highlight must be one of
    /// `CardPassageCandidates` for the reading: of the reading's book and not
    /// marked during another read of it — otherwise `InvalidInput`, never a
    /// foreign-key failure. Answers `Unit`.
    SetCardPassage {
        reading_id: i64,
        #[serde(default)]
        highlight_id: Option<i64>,
    },
    /// The passages a reading's card may choose from: the book's
    /// highlights attributed to this reading or to none. Exactly the set
    /// `SetCardPassage` accepts, so a chooser built on it cannot offer what the
    /// write then refuses. Answers `Highlights`.
    CardPassageCandidates {
        reading_id: i64,
    },
    SetAnnotation {
        highlight_id: i64,
        #[serde(default)]
        annotation: Option<String>,
    },

    // ---- epub and owned files ----
    ImportEpub {
        path: String,
    },
    ImportFile {
        path: String,
        /// Create a book even over a near-miss candidate. Without it an
        /// ambiguous file comes back as `Unmatched` with **nothing written**.
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
    /// Paths as the book files they hold, for *Add a book* to stage: a file
    /// passes through, a folder is walked (bounded), a calibre library is
    /// named and not walked. Reads directory listings only; nothing is
    /// written. See [`ExpandedPathsDto`].
    ExpandBookPaths {
        paths: Vec<String>,
    },
    BookFiles {
        book_id: i64,
    },
    /// The chapter list, read out of the owned epub on every call and stored
    /// nowhere — see [`TableOfContentsDto`].
    TableOfContents {
        book_id: i64,
    },
    FilePath {
        sha256: String,
    },
    RemoveFile {
        sha256: String,
    },

    // ---- koreader ----
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

    // ---- the device ----
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
    // ---- the plugin ----
    /// What bookmole's plugin looks like on a mounted reader, and whether
    /// the reader it names is one we are paired with. Read-only.
    PluginStatus {
        mount: String,
    },
    /// Install or upgrade the plugin, and pair with the reader.
    ///
    /// **Never on an automatic path.** Mount → import is automatic and
    /// read-only precisely so mount → write can be an
    /// explicit act; a client that called this on a mount event would be
    /// undoing that rather than saving a click. Show
    /// [`Response::PluginStatus`]'s `plugin_dir` first.
    InstallPlugin {
        mount: String,
    },
    /// Remove exactly what was installed, and forget the pairing.
    UninstallPlugin {
        mount: String,
    },
    /// Every reader we have paired with, plugged in or not.
    PairedDevices,
    /// Forget a pairing **without the reader in hand**.
    ///
    /// Not a second door onto [`Request::UninstallPlugin`], and the difference
    /// is the reason it exists: uninstall is *exact* — it takes the files off
    /// the device and then drops our row — so it needs the mount. This drops
    /// our row and **nothing else**, because a reader that has been sold, lost
    /// or reformatted cannot be reached and a list you can only leave by
    /// plugging something in is a list with no exit.
    ///
    /// A client's copy must say which happened. The plugin is still on that
    /// device and still holds the token; if it ever comes back, installing
    /// mints a fresh identity rather than resuming this one. The reply is
    /// [`Response::Bool`] — `false` is *there was no such pairing*.
    ForgetDevice {
        device_id: String,
    },
    /// Name a reader.
    ///
    /// The default label is the mount's directory name — `KOBOeReader`,
    /// `Kindle` — which is a fact about a filesystem rather than about the
    /// object on the bedside table. An empty or whitespace-only `label`
    /// **clears** the name rather than storing a blank, so a frontend's fallback
    /// keeps working; `false` is *no such pairing*.
    RenameDevice {
        device_id: String,
        label: String,
    },
    /// Bring across everything one mounted reader has to offer.
    ///
    /// **A method of its own rather than an argument to [`Request::SyncDevice`]**
    /// for two reasons that are one: `SyncDevice` takes sidecar *paths* and so
    /// cannot know whose they are — there is no device to stamp `last_synced_at`
    /// on — and a client that scanned, held the paths and sent them back is
    /// holding a handle across a round trip that the volume may have changed
    /// underneath. This re-scans below the seam, which is this crate's own
    /// "handles do not cross" applied to a filesystem.
    ///
    /// It does **not** import measured reading time; that is
    /// [`Request::ImportDeviceStatistics`], for the reason stated there.
    SyncMount {
        mount: String,
    },

    // ---- the wireless listener ----
    /// Whether a paired reader could reach this computer over the LAN.
    ///
    /// `Off` is the default and the answer for every host that has not asked.
    /// Read-only; a client polls it after starting or stopping, and when its
    /// view of paired readers loads.
    ListenerStatus,
    /// Open the door, for `minutes`.
    ///
    /// Absent means five. **`Some(0)` means until asked to stop** — the shape a
    /// desktop that lives on the LAN wants — rather than a window of zero
    /// minutes, which is a listener that shuts before a reader finishes
    /// probing and is a thing nobody has ever wanted.
    ///
    /// The window also closes on the **first completed push**, because one tap
    /// on the reader is one session and the door has then done its job. A
    /// session is every book that tap carried, not one file.
    ///
    /// **Never on an automatic path.** A reader's arrival is read-only
    /// precisely so that anything reaching outward is an explicit
    /// act, and a client that called this on startup would have made the
    /// control a lie.
    StartListening {
        #[serde(default)]
        minutes: Option<u32>,
    },
    /// Close it. Idempotent — a client cannot know the window did not expire
    /// between drawing the button and the user pressing it.
    StopListening,
    /// Fetch from a paired reader whose own window is open.
    ///
    /// **The same rendezvous dialled the other way**, with no new message in
    /// it: we broadcast the same `HELLO`, the reader answers the same `HERE`,
    /// and the MAC is verified before a byte is sent — so a rogue answering
    /// first cannot make us open a session with it. The entries still travel
    /// reader → desktop, which is *wireless is read-only toward us* as the
    /// shape of the protocol rather than a rule somebody remembers.
    ///
    /// It does **not** need [`Request::StartListening`]: a seeker sends first,
    /// so the reply comes back to the ephemeral port it sent from. Pulling with
    /// the door shut is the ordinary case.
    ///
    /// Refuses with `wireless_refused` when nothing answers. A client must not
    /// guess between *the window is shut*, *another subnet* and *the AP dropped
    /// the broadcast* — the message names the only one the user can act on.
    /// The reply is [`Response::PullReports`], one per sidecar that arrived.
    PullFromReader {
        device_id: String,
    },
    /// Pair with a reader **over wifi**, using the code it is showing.
    ///
    /// For a reader that already carries the plugin — a cable put it there
    /// once, from this computer or another. Somebody chooses *Link a computer
    /// over wifi* on it, the reader shows a sixteen-character code, and that
    /// code is `code`, as typed: case, spaces and hyphens do not matter.
    ///
    /// **The code is the secret, and a person carries it.** It is never sent;
    /// it signs two datagrams, and both sides then derive the long-lived token
    /// from it, so the token does not cross the network either. A client must
    /// treat `code` as it would a password field — not logged, not kept after
    /// the call returns. It is spent by one successful use and expires on the
    /// reader after two minutes.
    ///
    /// `host` is the reader's own address, which it shows beside the code for
    /// the network where it cannot be found by broadcast. Optional, and an
    /// *extra* place to ask rather than the only one.
    ///
    /// Needs no open door, exactly as [`Request::PullFromReader`] does not.
    /// The reply is [`Response::PairedDevice`]: the row as it now stands. A
    /// reader this computer was already paired with **keeps its row** — its
    /// name, its history — and gets a new secret, so linking twice is not two
    /// readers.
    ///
    /// Refuses with `wireless_refused`, and a client must not guess past the
    /// message: *a wrong code* and *no reader in reach* are one answer by
    /// design, since the reader is silent to anything it cannot verify.
    PairWithReader {
        code: String,
        #[serde(default)]
        host: Option<String>,
    },

    /// Measured reading time out of the device's `statistics.sqlite3`.
    ///
    /// **A method of its own, and deliberately not part of `sync_device`** —
    /// a reader's arrival is read-only, and a scan that quietly
    /// began importing months of timing data would not be read-only in spirit.
    /// The user asks for this by name, and so does a client.
    ImportDeviceStatistics {
        mount: String,
    },

    // ---- the activity log ----
    /// Rebuild the log from everything already stored: highlight stamps, note
    /// timestamps, reading endpoints. Idempotent, and called by no importer —
    /// a log that refilled itself as a side effect would be whichever importer
    /// ran last.
    RefillReadingEvents,
    ReadingEvents {
        book_id: i64,
    },
    /// Both ends inclusive, `YYYY-MM-DD`. An inverted range is a
    /// [`crate::error::ErrorCode::InvalidInput`], never a confident empty
    /// answer.
    ActivitySummary {
        from: String,
        to: String,
    },
    /// The days behind `ActivitySummary::activity_days`. Only days carrying an
    /// event come back.
    ActivityByDay {
        from: String,
        to: String,
    },
    /// The same period by month rather than by day, for a client
    /// drawing years. Only months carrying an event come back, and a month at
    /// the edge of the range covers the part of it inside the range.
    ///
    /// **Not something a client folds `ActivityByDay` into.** `minutes: null`
    /// collapses to `0` on the first `reduce` in any language that has one —
    /// which turns "the device never measured this month" into "you read for
    /// zero minutes" — and `books` is distinct *over the month*, which
    /// distinct-books-per-day cannot be summed into. The alternative shape,
    /// twelve `ActivitySummary` calls a year, is sixty round trips to draw five.
    ActivityByMonth {
        from: String,
        to: String,
    },

    // ---- moments ----
    /// What is worth noticing and has not been shown yet, newest first.
    ///
    /// **Poll this.** There is no push channel in this protocol — every reply
    /// carries the id it answers — and a moment stream would reopen the
    /// argument the mount watcher was deliberately left outside this
    /// vocabulary to avoid (see the module doc). A client asks on launch and
    /// after a write that could mint one.
    ///
    /// `limit` is absent for everything. It takes from the **newest** end, and
    /// it is the only lever a client gets: there is no count of what is
    /// pending, here or anywhere, because that is a badge.
    PendingMoments {
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Record that a moment was surfaced, so the ceremony does not replay.
    ///
    /// **Idempotent** — acknowledging twice, or from two clients, writes one
    /// row and keeps the first time. `id` is a `MomentDto::id` handed straight
    /// back; a string that names no kind this build knows is an
    /// [`crate::error::ErrorCode::InvalidInput`] rather than a silent row.
    AcknowledgeMoment {
        id: String,
    },

    // ---- notes ----
    CreateNote {
        note: NewNoteDto,
    },
    /// Notes, newest first, for one book, for one reading, or for the whole
    /// vault.
    ///
    /// `limit` selects along `created_at`. **Absent is every note**, which is
    /// what this method has always done and is deliberately still reachable: a
    /// client walking the whole graph needs it, and a default cap would turn a
    /// correctness pass into a silently truncated one. A screen passes a number.
    ///
    /// **`reading_id` was added later** and is `#[serde(default)]`, so a
    /// client that never heard of it sends the same bytes it always did and
    /// [`API_VERSION`] does not move. It exists for `search_marks`' reason in a
    /// second place: `NoteDto` carries `reading_id`, so a client is one
    /// `filter` away from doing this above the seam — and above the seam the
    /// limit has already cut the *book's* twelve most recent notes, so a card
    /// for an older read shows however few of them happened to be its, or
    /// nothing at all.
    ///
    /// The two ids are **alternatives, not a conjunction**. Sending both is an
    /// [`crate::error::ErrorCode::InvalidInput`] rather than a preference for
    /// one of them: a reading belongs to exactly one book, so the pair is
    /// either redundant or a contradiction, and a contradiction's honest answer
    /// is an empty list that no client can tell from an empty vault.
    ListNotes {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        reading_id: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    /// Notes and highlights matching one query, as **one ranked list**.
    ///
    /// This replaced `SearchNotes`, which is the change that moved
    /// [`API_VERSION`] to 2, and the replacement rather than the addition is
    /// the decision. Leaving both doors up would leave a client able to ask for
    /// notes and highlights separately and then interleave them itself — which
    /// it cannot do honestly, because it has no rule for whether a note
    /// outranks a highlight and would fall back to source order. Ranking once,
    /// below the seam, is the whole content of this method, and a second door
    /// past it makes that guarantee optional.
    ///
    /// `source` narrows which indexes are asked; **absent is both**, and it
    /// never changes the order. `limit` is the length of the answer. An empty
    /// query is no hits and no error.
    ///
    /// **`book_id` was added later** — absent is the whole library, and
    /// it is `#[serde(default)]`, so this is additive and [`API_VERSION`] does
    /// not move. It narrows *what is in* the two lists, in `source`'s own
    /// idiom, and like `source` it never changes how they are ordered.
    ///
    /// It is here because it **cannot** be done above the seam. `limit` cuts
    /// the global ranked list, so a client searching the library and then
    /// keeping one book's hits gets an empty answer whenever the top `limit`
    /// marks live in other books — which, at four hundred books, is most
    /// queries. `SearchHitDto` has always carried enough to write that filter,
    /// which is exactly why the request had to grow the parameter.
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
    /// One older note to offer back today, or `null`.
    ///
    /// `today` is the caller's **local** date as `YYYY-MM-DD`. The engine has
    /// no zone of its own, and "the same note all day" means the reader's day,
    /// not UTC's — so the frontend, which knows the zone, names the day.
    ResurfaceNote {
        today: String,
    },
    /// One older **passage** to offer back today, or `null` — the note's
    /// twin, with the same rule and the same caller's-local `today`. A new
    /// request rather than a field on
    /// `ResurfaceNote`, so no existing caller's shape moves.
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
    /// Both directions of a note's links in one answer, each with the other
    /// note's book face and — for a link in — the paragraph that holds it.
    /// Incoming first, newest-kept first; then
    /// outgoing in body order. A self-link is in neither half. Additive:
    /// `OutgoingLinks` and `Backlinks` are unchanged beside it.
    LinkContexts {
        note_id: i64,
    },

    // ---- note rows, threads, links and parts ----
    //
    // Every one of these is a new request; nothing above was reshaped (ts-rs
    // emits every field as required TypeScript). `API_VERSION` does not move.
    /// A page of note rows, ordered by `latest_part_at` DESC then id DESC — a
    /// continued note moves up when a part is added, never when it is edited.
    /// `kinds` absent is every kind; `ids` is exactly those notes, still
    /// in this order; `until` (unix seconds, exclusive) keeps rows whose latest
    /// part is before it. `book_id` and `reading_id` are **alternatives**, as in
    /// `ListNotes`: both is `invalid_input`. A negative `limit` is no limit.
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
    /// Every calendar month (UTC) some note's latest part falls in, newest
    /// first.
    NotePeriods,
    /// The threads holding at least one of `note_ids`, ordered by their newest
    /// member. `since` (unix seconds) is what `joined` counts from.
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
    /// The link graph for one scope.
    NoteGraph {
        scope: GraphScopeDto,
    },
    /// A note's dated parts, read from its file.
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

    // ---- more notes, reading history, the library table, settings ----
    //
    // Every one of these is a new request; nothing above was reshaped (ts-rs
    // emits every field as required TypeScript). `API_VERSION` does not move.
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
    /// Books with linked notes and how many of their notes the graph draws,
    /// most first — what a "pick one book's graph" control lists.
    NoteBooks,
    /// A read's notes and passages in one order, newest first. Notes are the
    /// read's by `reading_id` or, for an ordinary note, by the read whose
    /// window holds its writing. A negative `limit` is no limit; an unknown
    /// reading is `not_found`.
    ReadMarks {
        reading_id: i64,
        limit: i64,
    },
    /// A frontend's stored choice: the JSON text, or `null`.
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
    /// The headline figures for the span. There is no *current* run: only
    /// runs that have ended are reported, so nothing counts up while a run is
    /// under way and puts pressure on keeping it going.
    HistoryFigures {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    /// Records (longest, fastest, …) over the reads finished in the span.
    HistoryRecords {
        #[serde(default)]
        since: Option<i64>,
        #[serde(default)]
        until: Option<i64>,
    },
    /// One chart: buckets (UTC days; weeks start Monday) clipped to the span
    /// and to today, with the split, the comparison, the total, the average
    /// and the busiest bucket.
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
    /// `HistorySeries` written as a CSV into the exports folder; answers the
    /// path (`SavedCsv`).
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
    /// Every book as one table, with per-filter counts and the total pages.
    LibraryTable,

    // ---- note-part matches and other small additions ----
    //
    // Three new requests; nothing above was reshaped. `API_VERSION` does not move.
    /// Which parts of a continued note `query` matches, as 1-based part
    /// numbers, ascending — the half of a `SearchHitDto::Note` that names the
    /// part. Matched by `notes_fts`' own tokenizer and query rules, so it
    /// agrees with `SearchMarks`: the parts holding every term, else the parts
    /// holding any; empty when only the title matched. `not_found` for an
    /// unknown note. Answers `Parts`.
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

    // ---- reflection and review ----
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

    // ---- ratings ----
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
        /// Goodreads' 0–5 in quarters (migration `0026`). Was `u8`; the wire
        /// is a JSON number either way and the TypeScript type is unchanged.
        goodreads: f64,
    },

    // ---- citations ----
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
    /// Which passages each of these notes cites, in one call.
    ///
    /// The call that makes "mark the highlights some note already quotes"
    /// buildable. [`Request::CitationsFor`] is one call per note, and a client
    /// looping it over a page of notes is the N+1 this surface exists to
    /// spare it.
    ///
    /// It answers with **highlight ids** rather than rows —
    /// [`NoteCitationsDto`] carries the argument — and one entry per requested
    /// id, in the order asked, empties included. A note id that does not exist
    /// gets an empty entry: to this question, "no such note" and "cites
    /// nothing" are the same answer, and a missing row would make a caller
    /// unable to zip the reply against the page it already holds.
    ///
    /// A **new method rather than a `note_ids` beside `note_id`**: the two
    /// return different shapes, so one variant answering both would be a reply
    /// whose shape depends on which field a client filled in.
    CitationsForNotes {
        note_ids: Vec<i64>,
    },

    // ---- goodreads ----
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

    // ---- calibre ----
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
        /// Import only these calibre rows. Empty — and so an absent field — is
        /// the whole library, which is what keeps an older client's request
        /// meaning what it did before.
        #[serde(default)]
        only: Vec<i64>,
    },
    LinkCalibreBook {
        uuid: String,
        book_id: i64,
    },

    // ---- flashcards ----
    /// Capture a card from a book, optionally anchored to the passage the word
    /// came from.
    ///
    /// Until this there was no way to make one: `Storage::insert_flashcard`'s
    /// only production caller was the KOReader import's auto-capture of
    /// single-word highlights, so a card could be minted **by an import and by
    /// nothing else**.
    ///
    /// The reply is [`Response::Bool`] — `true` created, `false` *you already
    /// had this card*. `UNIQUE(book_id, word)` dedupes and the existing card is
    /// left exactly as it was, so the two are different facts and a caller
    /// drawing a confirmation needs to tell them apart. An existing shape
    /// rather than a new one: `Uncite` already answers a write with a bool
    /// meaning "there was something there".
    ///
    /// **The pair is re-read server-side**, this crate's own "handles do not
    /// cross": `highlight_id` belonging to a different book is `InvalidInput`
    /// and not a card quietly filed beside a passage from somewhere else.
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
    /// Anki's text-import format with a stable GUID per card, so importing a
    /// second file updates notes instead of duplicating them. A **new** request rather than a change to
    /// `ExportFlashcards`: ts-rs makes every added field required in
    /// TypeScript, so widening that one would break every client of it.
    ///
    /// `book_id: None` is the whole library. Cards that go out are marked
    /// exported, as `ExportFlashcards` marks them, so `include_exported: false`
    /// stays *the cards made since the last file*.
    ExportAnki {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        include_exported: bool,
    },
    /// `ExportAnki`, written by the engine into `exports/` beside the vault —
    /// for a client with no file-save dialog of its own. Nothing
    /// is written when no card went out.
    SaveAnkiExport {
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        include_exported: bool,
    },

    // ---- lists ----
    //
    // New requests only. Lists are the reader's own; none of these is scoped to
    // the library, and an entry carries `in_library` instead.
    /// Every list with its counts and its first `preview` books.
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
    /// `to` is a 0-based position in the list's own order.
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
    /// Lists stage B: put a search result (the record `Search` returned, the
    /// same DTO `SaveBook` takes) on a list **without owning it**. A book we
    /// already have is reused (the importers' matching: ISBN, then the title
    /// ladder); otherwise it is created with no copy, so it is on the list and
    /// not in the library. Idempotent.
    AddCandidateToList {
        list_id: i64,
        book: BookDto,
        #[serde(default)]
        note: Option<String>,
    },
    ListTimeline {
        list_id: i64,
    },
    /// Shelves from another system offered as lists (a dismissed or kept one
    /// is not offered again).
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

    // ---- copies ----
    //
    // `learned_from` is always `user` from here.
    /// Every copy of a book, oldest first, with its places.
    CopiesForBook {
        book_id: i64,
    },
    /// **Keep a copy here**: store the file of this copy's `path` place in the
    /// library's own folder and attach a `store` place. `invalid_input` for a
    /// copy with no file on this computer (`CopyDto.can_keep_here` says).
    KeepCopyHere {
        copy_id: i64,
    },
    /// Where each of a book's highlights came from, as facts.
    HighlightOrigins {
        book_id: i64,
    },
    /// Groups of a book's highlight ids that are the same passage seen by more
    /// than one row (usually two apps). Rows are never merged.
    SamePassages {
        book_id: i64,
    },
    /// The "+ I have this…" answer.
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
    /// Write a copy's user-entered fields **as given**: an absent field clears
    /// it. `have_now` is not here — [`Request::EndCopy`] and
    /// [`Request::ResumeCopy`] are the only writers of it.
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
    /// *I don't have it any more*: `have_now = false`. `ended_at`, when given,
    /// is stored; when absent the copy's own is left as it was.
    EndCopy {
        copy_id: i64,
        #[serde(default)]
        ended_at: Option<i64>,
    },
    /// *I have it again*: `have_now = true`, `ended_at` cleared.
    ResumeCopy {
        copy_id: i64,
    },
    /// For a copy recorded by mistake. Ending a copy is `EndCopy`.
    DeleteCopy {
        copy_id: i64,
    },
    /// In the library, touched, read without a copy — all derived by the engine.
    BookStanding {
        book_id: i64,
    },
    /// Per in-library book, what the Library table filters on (*Have now* /
    /// *Had* / *Borrowed* / *Copy not recorded*), with the filters' counts.
    LibraryCopies,

    // ---- reading-app imports ----
    //
    // New requests only. Every `path` a client sends is re-checked below the
    // seam: absent is `not_found`; not a file, or the wrong kind of file, is
    // `invalid_input`. An unmatched title or asset is on the report of the
    // import just run, never a standing count, and no request returns one.
    /// Preview: re-read every PDF that is a `path` place of one of this
    /// book's copies. mtime-gated, so asking again is cheap; a PDF whose
    /// annotations predate it being added is read by `ImportPreviewFile`.
    ImportPreviewAnnotations {
        book_id: i64,
    },
    /// Preview: make this PDF a file of the book (as adding a file does) and
    /// import its annotations now. Not a `.pdf`, or already another book's
    /// copy, is `invalid_input`.
    ImportPreviewFile {
        book_id: i64,
        path: String,
    },
    /// Apple Books on this Mac. No Apple Books data here (not a Mac, never
    /// opened) is a report carrying `apple_books_absent`, not an error.
    ImportAppleBooks {
        #[serde(default)]
        dry_run: bool,
    },
    /// The reader's answer to an `unmatched` asset: it is this book. Records
    /// an app copy holding the asset, so the next import matches it.
    LinkAppleBooksAsset {
        asset_id: String,
        book_id: i64,
    },
    /// Every `documents/My Clippings.txt` on a mounted volume. Read-only; a
    /// client asks when it knows a volume appeared.
    FindKindleClippings,
    /// A clippings file the reader named (`.txt`).
    ImportKindleClippings {
        path: String,
        #[serde(default)]
        dry_run: bool,
    },
    /// Every clippings file `FindKindleClippings` would find: one report each.
    ImportMountedKindleClippings {
        #[serde(default)]
        dry_run: bool,
    },
    /// The manual attach: import only `title_line`'s entries (as an
    /// `unmatched` title gave it) from `path` into `book_id`.
    AttachKindleTitle {
        path: String,
        title_line: String,
        book_id: i64,
    },
    /// The Kindle app's Export Notebook (`.html`), into `book_id` or, absent,
    /// the book its title matches. No match creates no book.
    ImportKindleNotebook {
        path: String,
        #[serde(default)]
        book_id: Option<i64>,
        #[serde(default)]
        dry_run: bool,
    },
    /// `ResolveBooks`, over the books in your library only: a copy-less book
    /// that is only on a list never answers. Same selector, same reply.
    ResolveLibraryBooks {
        selector: String,
    },

    // ---- a book's copies, file by file ----
    //
    // New requests only. Facts, never wording; no counts.
    /// The files of a book's copies: one row per file of a `path`, `store` or
    /// `calibre` place (reader and app places are on `CopyDto.places`), in
    /// copy order then place order, with whether each is on disk now.
    CopyFiles {
        book_id: i64,
    },
    /// Where a book's highlights came from, one row per (copy, origin), with
    /// the reader that copy sits on and when the source was last read in.
    CopyHighlightSources {
        book_id: i64,
    },
    /// *Find the file…*: this copy's file is at `path` now. The path is
    /// re-checked (absent is `not_found`, not a file is `invalid_input`);
    /// another book's file is `invalid_input`. The copy's `path` places that
    /// are missing or gone are replaced; a calibre place stays.
    RelocateCopyFile {
        copy_id: i64,
        path: String,
    },
}

/// What came back, by shape. See [`Request`] on the size of these variants.
#[allow(clippy::large_enum_variant)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", content = "value", rename_all = "snake_case")]
pub enum Response {
    /// It worked and there was nothing to say.
    Unit,
    Bool(bool),
    Id(i64),
    Text(String),
    /// A path, or nothing. `null` is an answer here, not an omission — a book
    /// with no cover URL has no cover to fetch.
    MaybePath(Option<String>),
    Paths(Vec<String>),
    Version {
        api: u32,
        crate_version: String,
    },
    Where(PathsDto),
    LibraryLocation(LibraryLocationDto),

    /// A number of rows. **Not [`Response::Id`]** — an id identifies and a count
    /// measures, and a shape that meant both would be a shape a client has to
    /// know the question to read.
    Count(i64),

    Book(Option<BookDto>),
    Books(Vec<BookDto>),
    BookSummaries(Vec<BookSummaryDto>),
    BookTags(Vec<BookTagDto>),
    OpenReadings(Vec<OpenReadingDto>),
    MergeReport(MergeReportDto),

    Reading(Option<ReadingDto>),
    Readings(Vec<ReadingDto>),
    /// A page of the library's readings. Its own shape rather than
    /// `Readings`, because a row here carries a book and a passage beside the
    /// reading and a client reading one as the other would find neither.
    ReadingRows(Vec<ReadingRowDto>),
    /// The years a filter's readings ended in, and whether any is still open.
    /// Its own shape and not `Count`: this measures nothing, it
    /// enumerates — and the open bucket rides with the years because without it
    /// they do not add up to the wall.
    ReadingYears(ReadingYearsDto),

    Highlights(Vec<HighlightDto>),
    /// One passage, or `null` because this reading has none attributed to it —
    /// an ordinary answer, and the same absence `Highlights` reports as an
    /// empty list.
    Highlight(Option<HighlightDto>),

    SearchOutcome(SearchOutcomeDto),

    Note(Option<NoteDto>),
    /// `ResurfaceNote`'s answer; `null` when nothing qualifies, which is
    /// ordinary for a young notebook.
    Resurfaced(Option<ResurfacedDto>),
    /// `ResurfacePassage`'s answer; `null` when nothing qualifies.
    ResurfacedPassage(Option<ResurfacedPassageDto>),
    Notes(Vec<NoteDto>),
    /// Which passages each requested note cites — one entry per id asked, in
    /// that order, empties included. **Not `Highlights`**: this shape carries
    /// handles, and the two calls answer the same question at two grains.
    NoteCitations(Vec<NoteCitationsDto>),
    SearchHits(Vec<SearchHitDto>),
    Links(Vec<OutgoingLinkDto>),
    LinkContexts(Vec<LinkContextDto>),
    CreatedNote(CreatedNoteDto),
    /// `ListNoteRows`' answer. Its own shape and not `Notes`: a row carries a
    /// book face, an excerpt and counts beside the note.
    NoteRows(Vec<NoteRowDto>),
    NotePeriods(Vec<NotePeriodDto>),
    Threads(Vec<ThreadDto>),
    NoteLinks(Vec<NoteLinkDto>),
    NoteGraph(NoteGraphDto),
    /// Every part of one note, in file order.
    NoteParts(Vec<NotePartDto>),
    LinkPreview(LinkPreviewDto),
    /// `BookFaces`' answer.
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
    /// A file the engine wrote into the exports folder.
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
    /// `null` is "no file here we can read a chapter list from", which is a
    /// different answer from a present value with no entries.
    TableOfContents(Option<TableOfContentsDto>),

    EnrichReport(EnrichReportDto),
    FieldProvenance(Vec<FieldSourceDto>),

    ReadingEvents(Vec<ReadingEventDto>),
    /// Newest first. Its **length is not a fact the protocol states** — there
    /// is no count field here and no count endpoint beside it.
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
    /// One reader, as its row stands after [`Request::PairWithReader`]. Its
    /// own shape rather than a one-element [`Response::PairedDevices`]: a list
    /// is *who do we know* and this is *who did we just meet*.
    PairedDevice(PairedDeviceDto),
    MountSync(MountSyncDto),

    GoodreadsReport(GoodreadsReportDto),
    /// The CSV, plus every honest failure along the way. The payload comes
    /// back and the caller owns the file — same contract the flashcard export
    /// has had since the beginning.
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

/// A request with an id, as it arrives.
///
/// The id is the client's, echoed back untouched: a transport may pipeline, and
/// without it a client with two calls in flight cannot tell the replies apart.
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

/// A reply, always well-formed.
///
/// `outcome` is an ordinary tagged enum rather than `Result`, because
/// `Result`'s serde shape is `{"Ok":…}` — capitalised, and a detail of the
/// standard library rather than a decision anyone made about this protocol.
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "bindings.ts")
)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    /// Stamped on every reply, so a client can refuse a daemon it does not
    /// understand without a handshake round trip.
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

    /// The reply as a single line of JSON, newline included.
    ///
    /// Framing lives here rather than in the transport for one reason: a
    /// `Reply` whose payload contained a raw newline would desynchronise a
    /// line-delimited stream, and `serde_json::to_string` is what guarantees it
    /// cannot — it escapes newlines inside strings. Putting the guarantee and
    /// the terminator in the same function is what keeps them true together.
    ///
    /// Serialization of a `Reply` cannot fail (no map with non-string keys, no
    /// non-finite float that is not already `null`), but "cannot" is not
    /// "does not", so the fallback is an error reply rather than a panic in
    /// somebody's daemon.
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
                // This one is a plain literal, so it cannot recurse.
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

    /// A method with nothing to pass carries no `params` at all, so a client
    /// need not send an empty object to ask the simplest question there is.
    #[test]
    fn a_parameterless_request_is_just_its_name() {
        let json = serde_json::to_string(&Request::ApiVersion).unwrap();
        assert_eq!(json, r#"{"method":"api_version"}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&json).unwrap(),
            Request::ApiVersion
        );
    }

    /// Defaults are the forward-compatibility lever, and the flags that carry
    /// them are all "do the safe thing": a `dry_run`/`new`/`overwrite` omitted
    /// must never be the destructive reading.
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

    /// The framing guarantee. A line-delimited transport is only safe while no
    /// payload can contain a bare newline — and note bodies contain them all
    /// day long.
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
