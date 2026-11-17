//! bookmole — engine for a personal reading companion.
//!
//! The engine performs **no terminal I/O**: every user interaction lives in a
//! frontend (CLI today, TUI later). Frontends drive it through [`Engine`].

/// Apple Books' highlights and notes, read out of copies of its databases.
pub mod apple_books;
pub mod book;
pub mod calibre;
pub mod cloud_folder;
pub mod config;
pub mod copies;
pub mod crash;
pub mod data_dir;
pub mod device;
pub mod diagnostic;
/// The physical shape of one edition, as proportions. Height is 1.0
/// and a frontend picks the unit, so no scene constant travels with it.
pub mod edition;
pub mod enrich;
pub mod epub;
pub mod error;
pub mod files;
pub mod flashcards;
/// A folder handed to *Add a book*, read as the book files in it.
pub mod folders;
pub mod goodreads;
pub mod highlight_origin;
pub mod history;
pub mod images;
/// The Kindle e-reader's `My Clippings.txt`.
pub mod kindle_clippings;
/// The Kindle app's Export Notebook file.
pub mod kindle_notebook;
pub mod ko_statistics;
pub mod koreader;
pub mod library;
pub mod lists;
/// The one answer to "is this the book I already have". Internal: a frontend
/// asks an import path, never the matcher.
pub(crate) mod matching;
/// Human names, parsed. Filing order and display order, off one
/// reading of the comma.
pub mod names;
pub mod note_graph;
mod notebook;
pub mod notes;
pub mod partial_md5;
/// A PDF's length and, occasionally, its name. `epub`'s twin, and the
/// module that makes "there is a page and no percentage" reachable rather than
/// a divide by a sentinel.
pub mod pdf;
pub mod plugin;
pub mod preview;
pub mod progress;
pub mod providers;
pub mod search;
/// The two derived sort keys the shelf is indexed on.
pub mod sort;
pub mod spreadsheet;
pub mod storage;
/// Which volume of a work a title names — the matcher's veto on *Volume 1*
/// against *Volume 2*. Internal, like `matching`.
pub(crate) mod volumes;
pub mod watch;
pub mod wireless;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use reqwest::Client;

pub use apple_books::{
    AppleBooksBook, AppleBooksReport, AssetMatch, AssetOutcome, import_apple_books,
    link_apple_books_asset,
};
pub use book::{Book, ReadingState, isbn10_to_13, normalize_isbn, series_index_text};
pub use calibre::{
    Calibre, CalibreBook, CalibreBookReport, CalibreMatch, CalibreReport, CalibreRowState,
    ImportOptions as CalibreImportOptions, UnmatchedCalibreBook,
};
pub use cloud_folder::{CloudFolder, CloudService, LibraryLocation};
pub use config::EngineConfig;
pub use copies::{
    BookStanding, Copy, CopyFacts, CopyFile, CopyForm, CopyHighlightSource, CopyPlace,
    CopyStanding, HighlightOriginFact, LearnedFrom, LibraryCopies, LibraryCopyCounts, NewCopy,
    NewPlace, PlaceAttach, PlaceFile, PlaceKind, Tenure,
};
pub use crash::CrashContext;
pub use data_dir::{DATA_DIR_ENV, DataDirError, LIBRARY_FOLDER_NAME, Platform};
pub use device::{
    DeviceBook, DeviceScan, DeviceState, MountSync, candidate_mounts, is_koreader_mount,
    koreader_dir, mount_roots, offers_reader,
};
pub use diagnostic::{Diagnostic, DiagnosticKind, ErrorClass, Severity};
pub use edition::{EditionShape, ShapeSource};
pub use enrich::{
    EnrichCandidate, EnrichMatch, EnrichOutcome, EnrichReport, FieldChange, HeldField,
};
pub use epub::{TableOfContents, TocEntry};
pub use error::{EngineError, Result};
pub use files::{
    FileIdentity, FileImportReport, FileMatch, FileOutcome, ImportOptions as FileImportOptions,
    KeptCopy,
};
pub use flashcards::{AnkiExport, SavedAnkiExport};
pub use folders::{ExpandedPaths, SkippedEntries};
pub use goodreads::{
    GoodreadsBookReport, GoodreadsMatch, GoodreadsReport, ImportOptions as GoodreadsImportOptions,
    TextOutcome, UnmatchedRow,
};
pub use highlight_origin::{
    HighlightImportSummary, HighlightOrigin, IdentityScope, ImportedHighlight, ImportedOutcome,
    PosKind, normalize_passage, same_passages,
};
pub use history::{
    DayRun, HistoryCompare, HistoryFigures, HistoryGroup, HistoryMetric, HistoryRead,
    HistoryRecord, HistoryRecords, HistorySeries, HistorySpan, HistorySplit, KeptRow, SavedCsv,
    SeriesBucket, SeriesPart, SeriesQuery, SourceName,
};
pub use ko_statistics::{StatsImportReport, statistics_db};
pub use koreader::{
    BookImportStats, ImportReport, KoStats, KoStatus, KoSummary, MatchCandidate, MatchMethod,
    PullReport,
};
pub use library::{LibraryCounts, LibraryRow, LibraryTable};
pub use lists::{
    BookList, EntryShow, EntrySort, ListEntry, ListMembership, ListSort, ListSummary, ListTimeline,
    ShelfSuggestion, TIMELINE_MIN_DATED,
};
pub use note_graph::{GraphEdgeKind, GraphScope};
pub use notebook::{
    GraphEdgeView, GraphNodeView, LinkPreview, MAX_UTC_OFFSET_MINUTES, NoteGraphView, NoteLink,
    NoteRow, NoteRowQuery, ReadMark, ThreadSummary,
};
pub use notes::{
    CreatedNote, EXCERPT_CAP, LINK_PARAGRAPH_CAP, LinkContext, LinkDirection, LinkParagraph,
    NewNoteInput, NoteKind, NotePart, PartMarker, VaultReconcile,
};
pub use partial_md5::partial_md5;
pub use pdf::{PdfInfo, pdf_info};
pub use plugin::{
    InstallReport, PLUGIN_DIR_NAME, PLUGIN_VERSION, Pairing, PluginCondition, PluginRefusal,
    PluginStatus, UninstallReport,
};
pub use progress::{Fraction, FractionSource, Progress};
pub use providers::googlebooks::verify_key as verify_google_key;
pub use providers::{ProviderId, SearchRequest};
pub use search::held::Held;
pub use search::session::{
    Correction, HeldList, HeldRow, ProviderState, ProviderStatus, QueryReading, SearchScope,
    SearchSnapshot, WorkRow,
};
pub use search::wellknown::{Unavailable as OfflineIndexUnavailable, WellKnownWork};
pub use search::works::{Edition, WorkResult};
pub use search::{IsbnLookup, RankedResult, SearchOutcome};
pub use storage::imported_identity_hash;
pub use storage::{
    ActivitySummary, BookFile, BookFilter, BookMatch, BookQuery, BookSort, BookSummary, BookTag,
    Confidence, DayActivity, DayRange, FieldSource, FillStats, FlashcardRow, Highlight,
    LibraryScope, MergeReport, Moment, MomentKind, MonthActivity, NewHighlight, NewReadingEvent,
    NoteCitations, NoteRecord, NoteScope, OutgoingLink, PairedDevice, RESURFACE_REST_DAYS,
    RUN_MIN_DAYS, Rating, RatingScale, ReadCount, ReadNumbering, Reading, ReadingEvent,
    ReadingFilter, ReadingQuery, ReadingRow, ReadingSort, ReadingYears, RefillReport,
    ResurfaceReason, Resurfaced, ResurfacedPassage, SearchHit, SearchSource, Setting, Source,
    StatusFilter, Storage, parse_local_date,
};
pub use watch::{
    MOUNT_QUIET, MountEvent, MountStir, MountWatcher, VAULT_QUIET, VaultEvent, VaultStir,
    VaultWatcher, watch_mounts,
};
pub use wireless::{ListenerMode, ListenerStatus, WirelessRefusal};

use providers::googlebooks::GoogleBooksProvider;
use providers::openlibrary::OpenLibraryProvider;
use providers::{Manners, MetadataProvider};

/// Each provider's rate limit and *come back later*, outliving any one
/// provider list: a rebuild (a key change, a provider switched on) must not
/// forget that a provider asked us to wait.
struct ProviderManners {
    open_library: Arc<Manners>,
    google_books: Arc<Manners>,
}

/// Every provider's availability: the reader's choice from
/// `provider_settings`, else the provider's default (keyless providers on,
/// keyed ones off), and whether a key is present.
fn roster_of(
    chosen: &HashMap<ProviderId, bool>,
    google_key: bool,
) -> Vec<(ProviderId, providers::ProviderAvailability)> {
    ProviderId::ALL
        .into_iter()
        .map(|id| {
            let has_key = match id {
                ProviderId::OpenLibrary => false,
                ProviderId::GoogleBooks => google_key,
            };
            (
                id,
                providers::availability(id, chosen.get(&id).copied(), has_key),
            )
        })
        .collect()
}

/// The provider list: every provider that is switched on and has what it
/// needs, and nothing else. **A provider that is off is not built**, so it is
/// sent nothing by anything that asks the list (search, ISBN lookup,
/// enrichment). That privacy rule (*a provider that is off is sent nothing*)
/// is then a property of the list rather than something each caller must
/// remember. Google Books is built only with the user's own key: keyless, it
/// answers 429 to every call, so asking it without one is pure noise.
fn build_providers(
    client: &Client,
    key: Option<String>,
    chosen: &HashMap<ProviderId, bool>,
    manners: &ProviderManners,
) -> Vec<Box<dyn MetadataProvider>> {
    let mut list: Vec<Box<dyn MetadataProvider>> = Vec::new();
    for (id, availability) in roster_of(chosen, key.is_some()) {
        if availability != providers::ProviderAvailability::Ready {
            continue;
        }
        match id {
            ProviderId::OpenLibrary => list.push(Box::new(
                OpenLibraryProvider::new(client.clone()).with_manners(manners.open_library.clone()),
            )),
            ProviderId::GoogleBooks => list.push(Box::new(
                GoogleBooksProvider::new(client.clone(), key.clone())
                    .with_manners(manners.google_books.clone()),
            )),
        }
    }
    list
}

/// The offline index's loader and, once asked, the index or why there is none.
struct WellKnownCell {
    loader: search::wellknown::Loader,
    open: tokio::sync::OnceCell<
        std::result::Result<Arc<search::wellknown::WellKnown>, search::wellknown::Unavailable>,
    >,
}

impl WellKnownCell {
    fn new(loader: search::wellknown::Loader) -> Arc<Self> {
        Arc::new(WellKnownCell {
            loader,
            open: tokio::sync::OnceCell::new(),
        })
    }
}

/// Where this engine's offline index lives: beside a database file
/// (`database/wellknown-<stamp>.db`), and nowhere for an in-memory one, which
/// has no directory to keep it in (and is what tests open: they inject a
/// fixture with [`Engine::use_wellknown`]).
fn wellknown_loader(db_url: &str) -> search::wellknown::Loader {
    let path = db_url
        .strip_prefix("sqlite://")
        .or_else(|| db_url.strip_prefix("sqlite:"))
        .map(|p| p.split('?').next().unwrap_or(p));
    match path {
        Some(p) if !p.is_empty() && !p.contains(":memory:") => {
            let dir = Path::new(p)
                .parent()
                .filter(|d| !d.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            search::wellknown::Loader::Embedded { dir }
        }
        _ => search::wellknown::Loader::None,
    }
}

/// A finished search session in [`Engine::search`]'s flat shape: each work
/// flattened to its default edition.
fn flatten(done: SearchSnapshot) -> SearchOutcome {
    SearchOutcome {
        results: done
            .works
            .into_iter()
            .map(|row| RankedResult {
                score: row.work.score,
                sources: row.work.sources.clone(),
                book: row.work.flattened().clone(),
                shelf_id: row.work.held.certain(),
            })
            .collect(),
        warnings: done.warnings,
    }
}

/// What to search for to find exactly one suggested work: its title and
/// author, marked so the parse reads them as such.
fn suggestion_query(title: &str, author: Option<&str>) -> String {
    match author {
        Some(a) if !a.is_empty() => format!("{title} by {a}"),
        _ => title.to_string(),
    }
}

/// Suggestions as you type ([`Engine::suggest_works`]).
#[derive(Debug, Clone, Default)]
pub struct WorkSuggestions {
    pub suggestions: Vec<WorkSuggestion>,
    /// The offline index was not there (it never says what was typed).
    pub warnings: Vec<Diagnostic>,
}

/// One suggestion: *Dune · Frank Herbert · 1965*.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkSuggestion {
    pub title: String,
    pub author: Option<String>,
    pub year: Option<i64>,
    pub source: SuggestionSource,
    /// The text that searches for exactly this work (`<title> by <author>`):
    /// what choosing the suggestion runs.
    pub query: String,
}

/// Where a suggestion comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum SuggestionSource {
    /// A book the app holds.
    Library { book_id: i64 },
    /// A work of the offline index: its Wikidata id (`Q190192`) and the Open
    /// Library work ids it names.
    WellKnown {
        wikidata: String,
        ol_works: Vec<String>,
    },
}

/// Take a lock, and take it back off a poisoned one.
///
/// A panic somewhere else must not turn every later search into a panic here:
/// the engine is a library, and neither of the two values behind these locks has
/// an invariant a half-finished write could break — each is replaced wholesale
/// or not at all.
/// Copy a stored cover onto an in-memory record — the `&mut Book` half of
/// [`Engine::download_cover`], which the CLI's add-flow holds before there is a
/// row to write to.
///
/// All five fields together, here as everywhere: `Storage::set_cover` is the
/// only writer of the four measurements *because* a width without the path it
/// measures is a record describing an image it is not pointing at, and a helper
/// that set three of them would be the first way to build one.
fn write_cover_onto(book: &mut Book, cover: &images::CoverFile) {
    book.cover_path = Some(cover.path.display().to_string());
    book.cover_thumb_path = cover.thumb_path.as_ref().map(|p| p.display().to_string());
    book.cover_width = cover.metrics.map(|m| m.width);
    book.cover_height = cover.metrics.map(|m| m.height);
    book.cover_accent = cover.metrics.map(|m| m.accent);
}

fn read<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

pub struct Engine {
    /// The storage boundary, and **private on purpose** — the facade is what a
    /// frontend talks to, and a `pub storage` field was once the hole every
    /// frontend reached through instead. [`Engine::storage`] still exists for the engine's own
    /// integration tests, but only behind the `internals` feature.
    storage: Storage,
    config: EngineConfig,
    /// The provider list, behind a lock because [`Engine::set_google_api_key`]
    /// rebuilds it at runtime, and that has to happen through `&self` (the API
    /// transport shares one engine between connections through an `Arc`).
    ///
    /// `Arc` inside the lock rather than a guard held across the await: a
    /// federated search takes seconds, and a read guard held that long would
    /// make a key change wait for it. Cloning the `Arc` is one refcount bump
    /// and the guard is dropped before anything is awaited.
    providers: RwLock<Arc<Vec<Box<dyn MetadataProvider>>>>,
    /// The **live** Google Books key. `EngineConfig::google_api_key` is only the
    /// value it was seeded with — after a `set_google_api_key` the two disagree,
    /// which is why nothing outside this module may read the config's copy and
    /// why [`Engine::google_api_key`] is the accessor.
    google_api_key: RwLock<Option<String>>,
    /// The one HTTP client, built in [`Engine::open`] with `User-Agent:
    /// bookmole/<version>` and nothing else: no contact address, no
    /// machine detail, nothing that identifies the user to a provider.
    client: Client,
    /// Each provider's rate limit (a token bucket at its published rate) and
    /// its *closed until* (set by a 429/503 with `Retry-After`), outliving any
    /// one provider list.
    manners: ProviderManners,
    /// The providers the reader switched on or off (`provider_settings`),
    /// read at open and kept in step by [`Engine::set_search_provider_enabled`].
    provider_choices: RwLock<HashMap<ProviderId, bool>>,
    /// The search sessions a frontend is polling: a search answers its local
    /// half at once and the providers fill in behind it, so a client starts a
    /// session and polls it rather than waiting for one long reply.
    searches: search::session::Registry,
    /// The offline index of well-known works (shipped with the engine, used
    /// for suggestions and typo correction): where it comes from,
    /// and — once something has asked — the index itself or why there is
    /// none. Opened on first use, never at `open`: building it the first time
    /// takes seconds, and `rb list` has no use for it.
    wellknown: RwLock<Arc<WellKnownCell>>,
    /// Which calibre tools this machine has, resolved **once per run**.
    ///
    /// Private, with [`Engine::calibre`] to read it: a public field is a door
    /// past the facade, and `storage` and `config` were closed for exactly
    /// that reason.
    calibre: Calibre,
    /// The wireless rendezvous: a paired reader pushing to us, or us pulling
    /// from it, over the LAN.
    ///
    /// **Nothing is bound until a caller asks**, so an `Engine` opened by
    /// `rb list` holds an idle struct and no socket. `Arc` because
    /// [`wireless::Listener::start`] hands clones of it to the tasks it spawns
    /// — see that type's doc for why this one subsystem spawns at all when
    /// `watch.rs` says the engine never does.
    wireless: Arc<wireless::Listener>,
    /// Apple Books' `Data/Documents`, where [`Engine::import_apple_books`]
    /// looks: [`apple_books::default_root`] unless a caller said otherwise
    /// through [`Engine::with_apple_books_root`]. Not in [`EngineConfig`] for
    /// the reason `calibre_bin_dir` is: nobody is asked to fill it in, and it
    /// exists so a test can point elsewhere **without mutating `HOME`**
    /// (process-global) and without ever reading the real container.
    apple_books_root: Option<PathBuf>,
}

/// Point every cover row into this library's own `images_dir`
/// ([`images::rerooted`] says when and why). Runs on every open, so a library
/// copied, moved or once opened with a relative data dir heals the first time
/// it is opened — one cheap `SELECT` and a `stat` per covered book. A failure
/// degrades to a warning: a jacket that stays blank is no reason to refuse
/// the library.
async fn reroot_covers(storage: &Storage, images_dir: &Path) {
    let rows = match storage.cover_paths().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "could not read cover paths to re-root them");
            return;
        }
    };
    let mut moved = 0usize;
    for (id, cover, thumb) in rows {
        let new_cover = cover
            .as_deref()
            .and_then(|c| images::rerooted(c, images_dir));
        let new_thumb = thumb
            .as_deref()
            .and_then(|t| images::rerooted(t, images_dir));
        if new_cover.is_none() && new_thumb.is_none() {
            continue;
        }
        let cover = new_cover.map(|p| p.display().to_string()).or(cover);
        let thumb = new_thumb.map(|p| p.display().to_string()).or(thumb);
        match storage
            .reroot_cover(id, cover.as_deref(), thumb.as_deref())
            .await
        {
            Ok(()) => moved += 1,
            Err(e) => tracing::warn!(book_id = id, error = %e, "could not re-root a cover"),
        }
    }
    if moved > 0 {
        tracing::info!(moved, "covers pointed into this library's images folder");
    }
}

/// One `path` place [`Engine::import_preview_annotations`] looked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPlaceRescan {
    pub copy_id: i64,
    /// The place's reference: the PDF's absolute path.
    pub path: PathBuf,
    pub outcome: preview::RescanOutcome,
    /// Empty unless `outcome` is `Reimported`.
    pub summary: HighlightImportSummary,
}

/// What [`Engine::import_preview_annotations`] did across a book's PDFs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewBookImport {
    /// One per PDF `path` place of the book's copies, oldest copy first.
    pub places: Vec<PreviewPlaceRescan>,
    /// The sum of every place's counts. Its `diagnostics` is empty: they stay
    /// with the place they are about.
    pub totals: HighlightImportSummary,
}

/// A file a caller named, re-checked here rather than trusted: it exists
/// (`NotFound`), is a regular file, and has one of `extensions`, compared
/// case-insensitively (`InvalidInput`, naming `what` it should have been).
fn named_file(path: &Path, extensions: &[&str], what: &str) -> Result<()> {
    existing_file(path)?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !ext.is_some_and(|e| extensions.contains(&e.as_str())) {
        return Err(EngineError::InvalidInput(format!(
            "{} is not {what}",
            path.display()
        )));
    }
    Ok(())
}

/// [`named_file`]'s first half, for a file of any kind: it exists
/// (`NotFound`) and is a regular file (`InvalidInput`).
fn existing_file(path: &Path) -> Result<()> {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(EngineError::NotFound(format!("{}", path.display())));
        }
        Err(e) => {
            return Err(EngineError::InvalidInput(format!(
                "cannot read {}: {e}",
                path.display()
            )));
        }
    };
    if !meta.is_file() {
        return Err(EngineError::InvalidInput(format!(
            "{} is not a file",
            path.display()
        )));
    }
    Ok(())
}

/// What [`Engine::readable_file`] found.
pub(crate) struct ReadableFile {
    pub path: PathBuf,
    pub sha256: String,
}

impl Engine {
    pub async fn open(config: EngineConfig) -> Result<Engine> {
        std::fs::create_dir_all(&config.images_dir)?;
        std::fs::create_dir_all(&config.files_dir)?;
        std::fs::create_dir_all(&config.vault_dir)?;
        if let Some(db_path) = config.db_url.strip_prefix("sqlite://")
            && let Some(parent) = Path::new(db_path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let client = Client::builder()
            .user_agent(concat!("bookmole/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let storage = Storage::connect(&config.db_url).await?;
        reroot_covers(&storage, &config.images_dir).await;
        // Forget, at open, the searches older than thirty days and the records
        // past their term, so a library nobody searches in still
        // forgets on schedule.
        if let Err(e) = storage.purge_search_cache(storage::now_unix()).await {
            tracing::warn!(class = ?ErrorClass::from(&e), "could not purge kept searches");
        }
        let key = config.google_api_key.clone();
        let manners = ProviderManners {
            open_library: Arc::new(Manners::open_library()),
            google_books: Arc::new(Manners::google_books()),
        };
        let provider_choices = storage.provider_settings().await?;
        let providers = build_providers(&client, key.clone(), &provider_choices, &manners);
        // Once, here — not once per book on a library import, which would be a
        // PATH sweep per book for an answer that cannot change mid-run.
        let calibre = Calibre::detect(config.calibre_bin_dir.as_deref());
        let wellknown = wellknown_loader(&config.db_url);
        Ok(Engine {
            storage,
            config,
            providers: RwLock::new(Arc::new(providers)),
            google_api_key: RwLock::new(key),
            client,
            manners,
            provider_choices: RwLock::new(provider_choices),
            searches: search::session::Registry::default(),
            wellknown: RwLock::new(WellKnownCell::new(wellknown)),
            calibre,
            wireless: Arc::new(wireless::Listener::new()),
            apple_books_root: apple_books::default_root(),
        })
    }

    // ---- the seam: storage, config, and what is deliberately not exposed ----

    /// The storage boundary, for the engine's **own** integration tests.
    ///
    /// Behind a feature so it cannot be a frontend's escape hatch. `tests/` is a
    /// separate crate, so it cannot reach a `pub(crate)` field, and giving every
    /// `Storage` method a facade twin purely to satisfy a test would bloat the
    /// facade with surface no product ever calls — the opposite of keeping the
    /// facade the one narrow seam. The feature is the honest middle: the door
    /// exists, and only a test build has the key. A plain `cargo check
    /// --workspace` builds without the feature, which is what keeps shipped
    /// frontends from using it.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

    /// Where the database is. A frontend shows this on a settings screen; it is
    /// not a handle to anything.
    pub fn db_url(&self) -> &str {
        &self.config.db_url
    }

    pub fn images_dir(&self) -> &Path {
        &self.config.images_dir
    }

    pub fn vault_dir(&self) -> &Path {
        &self.config.vault_dir
    }

    pub fn files_dir(&self) -> &Path {
        &self.config.files_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.config.log_dir
    }

    /// Where the database is, and whether that folder is inside a
    /// cloud-synced location (`cloud_folder.rs` says which and why).
    ///
    /// Judged on the database's folder and never the vault's: the vault is
    /// plain markdown and safe in a synced folder. A fact for a frontend to
    /// word, once per launch; the library opens either way.
    pub fn library_location(&self) -> LibraryLocation {
        let mut location = LibraryLocation::of(&self.config.db_url, cloud_folder::cloud_folder);
        location.database_dir = location
            .database_dir
            .map(|d| std::path::absolute(&d).unwrap_or(d));
        location
    }

    /// The Google Books key in force **now**, which after a runtime change is
    /// not the one `EngineConfig` was built with.
    ///
    /// Six narrow accessors rather than one `config()` returning `&EngineConfig`
    /// precisely because of this field: a struct getter would hand out the stale
    /// copy beside the live one and there would be no way to tell them apart at
    /// the call site.
    pub fn google_api_key(&self) -> Option<String> {
        read(&self.google_api_key).clone()
    }

    /// Swap in a new Google Books API key (or clear it) and rebuild the
    /// provider list so the change is live for the next search — frontends set
    /// this when the user enters a key at runtime.
    ///
    /// `&self`, not `&mut self`: the API transport hands the same engine to
    /// several connections at once through an `Arc`, and `&mut` on the facade is
    /// a method no shared owner can ever call.
    pub fn set_google_api_key(&self, key: Option<String>) {
        *write(&self.google_api_key) = key.clone();
        self.rebuild_providers();
    }

    /// Build the provider list again from the live key and the reader's
    /// choices.
    fn rebuild_providers(&self) {
        let key = read(&self.google_api_key).clone();
        let chosen = read(&self.provider_choices).clone();
        *write(&self.providers) =
            Arc::new(build_providers(&self.client, key, &chosen, &self.manners));
    }

    /// Replace the provider list, for the engine's **own** tests.
    ///
    /// Behind `internals` for [`Engine::storage`]'s reason. The search
    /// benchmark (`tests/search_bench.rs`) measures [`Engine::search`] exactly
    /// as a frontend calls it, with every provider pointed at a loopback server
    /// replaying recorded responses — and the list is otherwise built inside
    /// [`Engine::open`] against the live hosts. A later
    /// [`Engine::set_google_api_key`] rebuilds the list and undoes this.
    #[cfg(feature = "internals")]
    #[doc(hidden)]
    pub fn replace_providers(&self, providers: Vec<Box<dyn MetadataProvider>>) {
        *write(&self.providers) = Arc::new(providers);
    }

    /// The provider list as of this instant. Cloned out of the lock so nothing
    /// holds a guard across an await.
    fn providers(&self) -> Arc<Vec<Box<dyn MetadataProvider>>> {
        read(&self.providers).clone()
    }

    // ---- metadata search ---------------------------------------------------

    /// Federated search: fan out to all providers, dedup, rank.
    ///
    /// **Free text alone** (what *Add a book* sends) is read first
    /// (`search::query`): `dune by frank herbert` is a title and an author, an
    /// ISBN in any spacing is an ISBN, and the results are ranked against every
    /// reading of the text (`search::ranking`). **A fielded request** keeps
    /// the meaning it always had. Same types out either way.
    // The query itself is the user's private reading interest — `skip` keeps it
    // out of the span, and only its presence is recorded.
    ///
    /// **Free text is a search session run to its end**: the
    /// same session `start_search` begins — kept answers, the providers that
    /// are switched on, works and their editions — flattened back to one
    /// record per work (its default edition, filled from the work), so
    /// callers of this older, flat request (the TUI, `Request::Search`) get
    /// the shape they always had and the better results with it.
    ///
    /// **`shelf_id` is the work's [`Held::Certain`]** on that path:
    /// any edition's ISBN or a provider id matching a book the app holds,
    /// read from the library prepared once for the whole search rather than
    /// one `find_book_by_isbn` per row. It is never [`Held::Likely`] — the
    /// old field means *Enter opens this book*, and a likely answer must not.
    /// A fielded request keeps [`Engine::shelf_of`]'s ISBN join.
    #[tracing::instrument(skip_all, fields(has_isbn = req.isbn.is_some()))]
    pub async fn search(&self, req: &SearchRequest) -> Result<SearchOutcome> {
        let mut req = req.clone();
        if let Some(raw) = &req.isbn {
            req.isbn =
                Some(normalize_isbn(raw).ok_or_else(|| EngineError::InvalidIsbn(raw.clone()))?);
        }
        match search::free_text(&req) {
            Some(text) => {
                // Never registered: the caller holds it, and it ends here.
                let session = self
                    .begin_search(
                        "search-direct".into(),
                        text,
                        SearchScope::Everything,
                        req.limit,
                        search::session::Mode::ToDone,
                    )
                    .await?;
                Ok(flatten(session.finish().await))
            }
            None => {
                let mut outcome = search::federated_search(&self.providers(), &req).await?;
                for r in &mut outcome.results {
                    r.shelf_id = self.shelf_of(&r.book).await?;
                }
                Ok(outcome)
            }
        }
    }

    async fn begin_search(
        &self,
        id: String,
        text: &str,
        scope: SearchScope,
        limit: u32,
        mode: search::session::Mode,
    ) -> Result<search::session::SearchSession> {
        self.begin_search_as(id, text, scope, limit, mode, false)
            .await
    }

    async fn begin_search_as(
        &self,
        id: String,
        text: &str,
        scope: SearchScope,
        limit: u32,
        mode: search::session::Mode,
        literal: bool,
    ) -> Result<search::session::SearchSession> {
        // The list is what is asked; the roster says why the others are not.
        // A provider in the list is ready whatever the settings say, so a list
        // replaced in a test is asked as given.
        let providers = self.providers();
        let availability = roster_of(
            &read(&self.provider_choices),
            read(&self.google_api_key).is_some(),
        )
        .into_iter()
        .map(|(id, a)| {
            let built = providers.iter().any(|p| p.id() == id);
            let a = match (built, a) {
                (true, _) => providers::ProviderAvailability::Ready,
                (false, providers::ProviderAvailability::Ready) => {
                    providers::ProviderAvailability::Off
                }
                (false, other) => other,
            };
            (id, a)
        })
        .collect();
        let roster = search::session::Roster {
            providers,
            availability,
        };
        let (wellknown, missing) = match self.wellknown().await {
            Ok(w) => (Some(w), None),
            Err(why) => (None, Some(why)),
        };
        search::session::SearchSession::start_with(
            id,
            text,
            scope,
            limit,
            roster,
            Some(self.storage.clone()),
            mode,
            search::session::Extras {
                wellknown,
                missing,
                literal,
            },
        )
        .await
    }

    /// The offline index, opened (and built, the first time) on first use.
    async fn wellknown(
        &self,
    ) -> std::result::Result<Arc<search::wellknown::WellKnown>, search::wellknown::Unavailable>
    {
        let cell = read(&self.wellknown).clone();
        cell.open
            .get_or_init(|| async {
                search::wellknown::WellKnown::open(&cell.loader)
                    .await
                    .map(Arc::new)
            })
            .await
            .clone()
    }

    /// Point the offline index somewhere else, for the engine's **own**
    /// tests and benchmark: a fixture built in code, or the embedded file
    /// built in a directory of the caller's. Behind `internals` for
    /// [`Engine::storage`]'s reason.
    #[cfg(feature = "internals")]
    #[doc(hidden)]
    pub fn use_wellknown(&self, loader: search::wellknown::Loader) {
        *write(&self.wellknown) = WellKnownCell::new(loader);
    }

    /// Suggestions as you type: books you have that begin with
    /// or hold what was typed, then well-known works whose title or author
    /// begins with it, best known first. **No provider is ever asked** — this
    /// has no network path at all, which a test proves with providers that
    /// panic. Without an offline index the library still answers, and the
    /// warnings say the index was not there.
    // `skip_all`: what is being typed is the reader's private interest.
    #[tracing::instrument(skip_all)]
    pub async fn suggest_works(&self, prefix: &str, limit: usize) -> Result<WorkSuggestions> {
        let limit = limit.clamp(1, 20);
        let mut out = WorkSuggestions::default();
        if prefix.trim().is_empty() {
            return Ok(out);
        }
        let mine = self
            .storage
            .search_books_ranked(prefix, &BookFilter::default(), LibraryScope::All, limit)
            .await?;
        for (book, _) in mine {
            let Some(book_id) = book.id else { continue };
            let title = book.title.clone().unwrap_or_default();
            let author = book.authors_in_display_order().into_iter().next();
            out.suggestions.push(WorkSuggestion {
                query: suggestion_query(&title, author.as_deref()),
                title,
                author,
                year: book.publish_year,
                source: SuggestionSource::Library { book_id },
            });
        }
        match self.wellknown().await {
            Ok(wk) => {
                for h in wk.suggest(prefix, limit).await? {
                    if out.suggestions.len() >= limit * 2 {
                        break;
                    }
                    out.suggestions.push(WorkSuggestion {
                        query: suggestion_query(&h.title, h.author.as_deref()),
                        title: h.title,
                        author: h.author,
                        year: h.year.map(i64::from),
                        source: SuggestionSource::WellKnown {
                            wikidata: format!("Q{}", h.wikidata),
                            ol_works: h.ol_works,
                        },
                    });
                }
            }
            Err(
                why @ (search::wellknown::Unavailable::NotInThisBuild
                | search::wellknown::Unavailable::Failed(_)),
            ) => {
                out.warnings
                    .push(Diagnostic::offline_index_unavailable(why));
            }
            Err(search::wellknown::Unavailable::NotConfigured) => {}
        }
        Ok(out)
    }

    /// A search session for the text **as typed**: never corrected. What a
    /// corrected search's *Search for "…" instead* runs: a search that was
    /// corrected says so, and this is the literal query, one press away.
    #[tracing::instrument(skip_all)]
    pub async fn start_search_as_typed(
        &self,
        text: &str,
        scope: SearchScope,
        limit: u32,
    ) -> Result<SearchSnapshot> {
        let limit = limit.clamp(1, 40);
        let session = self
            .begin_search_as(
                self.searches.mint(),
                text,
                scope,
                limit,
                search::session::Mode::Progressive,
                true,
            )
            .await?;
        Ok(self.searches.insert(session).snapshot())
    }

    /// [`Engine::search`] for free text as typed: never corrected.
    #[tracing::instrument(skip_all)]
    pub async fn search_as_typed(&self, req: &SearchRequest) -> Result<SearchOutcome> {
        match search::free_text(req) {
            Some(text) => {
                let session = self
                    .begin_search_as(
                        "search-direct".into(),
                        text,
                        SearchScope::Everything,
                        req.limit,
                        search::session::Mode::ToDone,
                        true,
                    )
                    .await?;
                Ok(flatten(session.finish().await))
            }
            None => self.search(req).await,
        }
    }

    /// Start a search session: read the query, answer from what is
    /// kept **in this call**, start asking the providers that are switched on,
    /// and return the first snapshot. Poll it with [`Engine::poll_search`];
    /// end it with [`Engine::end_search`] or let it go idle.
    ///
    /// `limit` is the most works a snapshot carries, at most 40.
    /// Starting a search does not end any other.
    #[tracing::instrument(skip_all)]
    pub async fn start_search(
        &self,
        text: &str,
        scope: SearchScope,
        limit: u32,
    ) -> Result<SearchSnapshot> {
        let limit = limit.clamp(1, 40);
        let session = self
            .begin_search(
                self.searches.mint(),
                text,
                scope,
                limit,
                search::session::Mode::Progressive,
            )
            .await?;
        Ok(self.searches.insert(session).snapshot())
    }

    /// The next snapshot of a search after `after_revision`, waiting up to
    /// `wait` (capped at 1.5 s) for one. `NotFound` for a session that was
    /// ended, let go idle, or never started.
    pub async fn poll_search(
        &self,
        session: &str,
        after_revision: u64,
        wait: std::time::Duration,
    ) -> Result<SearchSnapshot> {
        let s = self.searches.get(session)?;
        Ok(s.poll(after_revision, wait).await)
    }

    /// Stop a search and let it go. `false` when there was none; ending one
    /// twice is not an error.
    pub fn end_search(&self, session: &str) -> bool {
        self.searches.end(session)
    }

    /// The Library's search: the books **in the library** that
    /// answer `query`, best first, as ids and scores — at most `limit`.
    ///
    /// **Nothing leaves the machine.** It reads the books index and nothing
    /// else; no provider is in reach of it, which a test asserts with a
    /// provider list that panics when asked. Typo-tolerant and infix
    /// (`possess` finds *The Dispossessed*), ranked by the scorer *Add a book*
    /// uses. The Library's chips stay the caller's: it applies them to the ids.
    // `skip_all`: the query is the reader's private interest.
    #[tracing::instrument(skip_all)]
    pub async fn search_library(&self, query: &str, limit: usize) -> Result<Vec<BookMatch>> {
        self.storage
            .search_books(
                query,
                &BookFilter::default(),
                LibraryScope::InLibrary,
                limit,
            )
            .await
    }

    /// Every provider this build knows, and what it is now: ready, switched
    /// off, or without the key it needs.
    pub fn search_providers(&self) -> Vec<(ProviderId, providers::ProviderAvailability)> {
        roster_of(
            &read(&self.provider_choices),
            read(&self.google_api_key).is_some(),
        )
    }

    /// Switch a provider on or off (`provider_settings`). Live for the next
    /// search; a provider that is off is sent nothing.
    pub async fn set_search_provider_enabled(
        &self,
        provider: ProviderId,
        enabled: bool,
    ) -> Result<()> {
        self.storage.set_provider_enabled(provider, enabled).await?;
        write(&self.provider_choices).insert(provider, enabled);
        self.rebuild_providers();
        Ok(())
    }

    /// Whether anything is kept, and the oldest kept record's date. No count
    /// of searches.
    pub async fn search_cache_status(&self) -> Result<storage::SearchCacheStatus> {
        self.storage.search_cache_status().await
    }

    /// *Forget searches*: the kept searches, and with `records` the kept
    /// provider records too. No book is touched.
    pub async fn forget_searches(&self, records: bool) -> Result<()> {
        self.storage.forget_searches(records).await
    }

    /// The shelf's copy of a provider's book, if the library already holds one.
    ///
    /// A row-state join, and a derived fact, so it lives here: every frontend
    /// wants to know whether *Enter* on a search result adds a book or opens
    /// one, and they must all agree on the answer. Without it a frontend asks
    /// `resolve_books` once per result and discards the title fall-through
    /// itself — one derivation above the seam, and one more round trip per
    /// row.
    ///
    /// Matches on ISBN-13, then ISBN-10, against either stored column. A book
    /// the providers return without an ISBN is `None`: nothing else about a
    /// provider record is stable enough to say *this is that one*, and a
    /// false "on the shelf" would make Enter open the wrong book.
    pub async fn shelf_of(&self, book: &Book) -> Result<Option<i64>> {
        for isbn in [book.isbn_13.as_deref(), book.isbn_10.as_deref()]
            .into_iter()
            .flatten()
        {
            if let Some(found) = self.storage.find_book_by_isbn(isbn).await? {
                return Ok(found.id);
            }
        }
        Ok(None)
    }

    /// Direct edition lookup by ISBN, merging fields across providers.
    ///
    /// The providers are asked **concurrently**, each inside the search's
    /// five-second budget. One that fails is logged and left out; use
    /// [`Engine::lookup_isbn_reported`] to hear about it.
    // `skip_all`: a typed ISBN is a query, and a query is never in a span.
    #[tracing::instrument(skip_all)]
    pub async fn lookup_isbn(&self, raw: &str) -> Result<Option<Book>> {
        Ok(self.lookup_isbn_reported(raw).await?.book)
    }

    /// [`Engine::lookup_isbn`], with a diagnostic for every provider that did
    /// not answer — so *Open Library timed out* is not mistaken for *no such
    /// book*.
    #[tracing::instrument(skip_all)]
    pub async fn lookup_isbn_reported(&self, raw: &str) -> Result<IsbnLookup> {
        let isbn = normalize_isbn(raw).ok_or_else(|| EngineError::InvalidIsbn(raw.to_string()))?;
        let providers = self.providers();
        let (found, warnings) = search::lookup_isbn_all(&providers, &isbn).await;
        Ok(IsbnLookup {
            book: search::merge_provider_books(found),
            warnings,
        })
    }

    // ---- library -----------------------------------------------------------

    /// Save (insert-or-merge) and return the stored copy.
    ///
    /// **Writes no field provenance, and that is a refusal rather than an
    /// omission.** The `Book` reaching here has already been through
    /// `search::merge_provider_books`, which keeps the winning value per field
    /// and throws away which provider supplied it — so this path genuinely
    /// cannot say where `page_count` came from, and stamping the whole record
    /// with whichever provider is guessed at would be inventing exactly the
    /// thing `field_provenance` exists to record.
    /// [`Engine::enrich_book_from_providers`] keeps each field's supplier and
    /// can answer honestly; a book added through here is unattributed, the
    /// same state every book that predates migration `0012` is in.
    pub async fn save_book(&self, book: &Book) -> Result<Book> {
        let id = self.storage.upsert_book(book, None).await?;
        self.storage
            .get_book(id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))
    }

    /// One page of the library, newest-touched first unless told otherwise.
    ///
    /// `BookQuery::new(limit, sort)` is the whole of what this used to take.
    /// [`BookQuery`] adds the filter and the offset; see its module for why the
    /// page is an offset rather than a cursor.
    pub async fn list_books(&self, query: &BookQuery) -> Result<Vec<Book>> {
        self.storage.list_books(query).await
    }

    /// How many books match — the number a shelf needs *before* it needs the
    /// rows, and the one thing `list_books` could not answer without returning
    /// the whole library.
    ///
    /// The same `WHERE` the page is built from, so the count and the pages
    /// cannot disagree. See [`BookFilter`].
    pub async fn count_books(&self, filter: &BookFilter) -> Result<i64> {
        self.storage.count_books(filter).await
    }

    /// What is behind each of these books — highlights, notes, owned files —
    /// in one call for a whole page.
    ///
    /// The detail screen makes four queries for one book. A list of eight
    /// hundred cannot, which is why nothing in a list could show this before.
    pub async fn book_summaries(&self, book_ids: &[i64]) -> Result<Vec<BookSummary>> {
        self.storage.book_summaries(book_ids).await
    }

    /// One book by its internal id. [`Engine::resolve_books`] is what a
    /// user-typed selector goes through; this is the id path.
    pub async fn get_book(&self, id: i64) -> Result<Option<Book>> {
        self.storage.get_book(id).await
    }

    /// The books behind these ids, one per id **in the order asked**, an id
    /// naming no book left out — so a page of passage search hits gets its
    /// book names in one call. `BookSummaries` does not answer this: it
    /// carries counts and no name.
    pub async fn book_faces(&self, book_ids: &[i64]) -> Result<Vec<Book>> {
        let mut seen: std::collections::HashMap<i64, Option<Book>> = Default::default();
        let mut out = Vec::with_capacity(book_ids.len());
        for id in book_ids {
            if !seen.contains_key(id) {
                seen.insert(*id, self.storage.get_book(*id).await?);
            }
            if let Some(Some(b)) = seen.get(id) {
                out.push(b.clone());
            }
        }
        Ok(out)
    }

    // ---- the settings store (migration 0024) --------------------------------

    /// The JSON text a frontend stored under `key`. See `storage/settings.rs`:
    /// the engine stores and never interprets, and no secret belongs here.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.storage.get_setting(key).await
    }

    /// Store a frontend's choice. Refuses a key outside `[a-z0-9._-]{1,64}`
    /// and a value that is not JSON (`InvalidInput`).
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.storage.set_setting(key, value).await
    }

    /// Every setting under `prefix` (a literal prefix; `""` is all), by key.
    pub async fn list_settings(&self, prefix: &str) -> Result<Vec<Setting>> {
        self.storage.list_settings(prefix).await
    }

    /// The shelf names another system minted for this book. **Inert
    /// provenance** — nothing reads these to decide anything. Three systems
    /// (Goodreads, calibre, KOReader) minting shelves is a merge problem with
    /// no good default, so the raw names are kept rather than turned into
    /// anything, and a grouping design can be made against real shelf names.
    pub async fn book_tags(&self, book_id: i64) -> Result<Vec<BookTag>> {
        self.storage.book_tags(book_id).await
    }

    /// What you are reading: one row per **open** reading, most-recently-touched
    /// first.
    ///
    /// The [`Reading`] comes back beside the [`Book`] because the two are not
    /// interchangeable here: `Book`'s progress fields are projections of the
    /// *current* reading, which for a finished book is a closed one, while this
    /// row is specifically the open one and carries its own `status`, `source`
    /// and device mirror.
    pub async fn currently_reading(&self, limit: i64) -> Result<Vec<(Book, Reading)>> {
        self.storage.list_open_readings(limit).await
    }

    /// Resolve a user-supplied selector: numeric id, ISBN, or title fragment.
    /// Returns all candidates (empty = nothing matched, >1 = ambiguous).
    pub async fn resolve_books(&self, selector: &str) -> Result<Vec<Book>> {
        self.resolve_books_in(selector, LibraryScope::All).await
    }

    /// [`Engine::resolve_books`] over the books **in your library** only (at
    /// least one copy not ended, or any copy and touched): the same ladder,
    /// each rung narrowed, so a copy-less book that is only on a list never
    /// answers. What an add field's *In your library*
    /// group asks.
    pub async fn resolve_library_books(&self, selector: &str) -> Result<Vec<Book>> {
        self.resolve_books_in(selector, LibraryScope::InLibrary)
            .await
    }

    /// The one resolver, narrowed by `scope`. A rung whose hits the scope
    /// removes falls through to the next, as a miss does.
    async fn resolve_books_in(&self, selector: &str, scope: LibraryScope) -> Result<Vec<Book>> {
        // ISBN before id, deliberately. A bare ISBN-13 is thirteen digits and
        // therefore *also* parses as an i64, so trying the id first meant
        // `show 9781900000253` looked up row 9781900000253, found nothing, and
        // reported no match — plain ISBN selectors never resolved at all, while
        // hyphenated ones did.
        if let Some(isbn) = normalize_isbn(selector) {
            let hit = self.storage.find_book_by_isbn(&isbn).await?;
            if let Some(book) = self.in_scope(hit, scope).await? {
                return Ok(vec![book]);
            }
        }
        if let Ok(id) = selector.parse::<i64>() {
            let hit = self.storage.get_book(id).await?;
            if let Some(book) = self.in_scope(hit, scope).await? {
                return Ok(vec![book]);
            }
        }
        // Each branch falls through rather than returning early on a miss, so a
        // selector that looks like one kind but matches another still resolves.
        self.storage.find_books_by_title_in(selector, scope).await
    }

    /// `book`, if it is inside `scope`.
    async fn in_scope(&self, book: Option<Book>, scope: LibraryScope) -> Result<Option<Book>> {
        let Some(book) = book else { return Ok(None) };
        let inside = match (scope, book.id) {
            (LibraryScope::All, _) => true,
            (_, None) => false,
            (LibraryScope::InLibrary, Some(id)) => self.storage.in_library(id).await?,
            (LibraryScope::NotInLibrary, Some(id)) => !self.storage.in_library(id).await?,
        };
        Ok(inside.then_some(book))
    }

    /// Delete a book, its cover image, and the ebook files it owned.
    ///
    /// The rows go by cascade; the bytes are the engine's to remove, the same
    /// contract the cover has had since the beginning. They are listed *before*
    /// the delete because after it there is nothing left to list them by — and
    /// they are safe to remove unconditionally: `book_files` is keyed on the
    /// sha256 alone, so no other book can be holding this content.
    ///
    /// The **cover** no longer gets that guarantee for free — content-addressed
    /// names mean two books can share one jacket — so `Storage::delete_book`
    /// hands back a path only when nothing else references it. The shelf tier
    /// goes with it, derived from the cover's own name rather than read back
    /// from a row that no longer exists.
    pub async fn delete_book(&self, id: i64) -> Result<()> {
        let files = self.storage.book_files(id).await?;
        if let Some(cover) = self.storage.delete_book(id).await? {
            let cover = PathBuf::from(cover);
            std::fs::remove_file(images::thumb_path_of(&cover)).ok();
            std::fs::remove_file(cover).ok();
        }
        for file in &files {
            std::fs::remove_file(self.file_path(file)).ok();
        }
        Ok(())
    }

    /// Download `cover_url` into the images dir and persist the cover.
    pub async fn download_cover(&self, book: &mut Book) -> Result<Option<PathBuf>> {
        let Some(url) = book.cover_url.clone() else {
            return Ok(None);
        };
        let cover = self.download_cover_file(&url).await?;
        write_cover_onto(book, &cover);
        match book.id {
            // **Not `upsert_book`.** Its third branch — no `isbn_10`, no
            // `isbn_13` — is a plain unconditional insert that ignores
            // `Book::id`, so a stored book with an id and no ISBN got a
            // *duplicate row* here instead of a cover. That is every book this
            // path matters most for: a sidecar-seeded one. Found by running
            // provider enrichment against exactly that case, and reachable
            // through `Engine::fetch_cover` as well.
            //
            // **And not `enrich_book` either.** This used to
            // write the *whole* record back to move one field — which is why it
            // could not name a source without claiming fifteen columns it had
            // nothing to do with — and, worse, it went through the no-clobber
            // merge, so a re-fetch that produced a *different* file left the row
            // pointing at the old one. `set_cover` writes the path and the four
            // measurements together and replaces rather than fills.
            Some(id) => self.storage.set_cover(id, &cover, None).await?,
            None if book.isbn_10.is_some() || book.isbn_13.is_some() => {
                let id = self.storage.upsert_book(book, None).await?;
                self.storage.set_cover(id, &cover, None).await?;
            }
            // An unsaved candidate with no ISBN: the caller is holding it and
            // the cover has been written onto it, which is all it asked for.
            None => {}
        }
        Ok(Some(cover.path))
    }

    /// Fetch the bytes at a cover URL. The half of [`Engine::download_cover`]
    /// that is not about *which* row to write, so enrichment can persist the
    /// file it fetched without writing the whole record back.
    async fn download_cover_file(&self, url: &str) -> Result<images::CoverFile> {
        images::image_from_url(&self.client, url, &self.config.images_dir).await
    }

    /// Measure the covers already on disk: the back-fill for covers stored
    /// before their dimensions and border colour were recorded.
    ///
    /// **A command and not a migration**, and it could not have been a
    /// migration: `cover_width` is the result of decoding a PNG and SQLite
    /// cannot decode one. `Storage::unmeasured_covers` states the work list
    /// (`cover_path IS NOT NULL AND cover_width IS NULL`) and this reads each
    /// file, measures it, writes the shelf tier where one is worth having, and
    /// persists all five columns through the same `set_cover` the download path
    /// uses — so a back-filled row and a freshly-downloaded one are the same
    /// row, which is the property a second code path would have quietly given
    /// up.
    ///
    /// Files are **not renamed**. A cover written before content addressing is
    /// not named after its own hash, but the stored `cover_path` is what a
    /// GUI client resolves and loads as it stands; renaming every
    /// image would be a destructive change dressed as a measurement, and buys
    /// nothing — the collision it would fix is in the *write* path, which is
    /// already fixed.
    ///
    /// Returns `(measured, unreadable)`. A file that has gone missing or will
    /// not decode is counted and skipped, never an error: one bad image must not
    /// stop the other two hundred, and the next run retries it for free.
    pub async fn measure_stored_covers(&self) -> Result<(usize, usize)> {
        let mut measured = 0;
        let mut unreadable = 0;
        for (id, path) in self.storage.unmeasured_covers().await? {
            match images::measure_stored(Path::new(&path)) {
                Ok(cover) if cover.metrics.is_some() => {
                    self.storage.set_cover(id, &cover, None).await?;
                    measured += 1;
                }
                _ => unreadable += 1,
            }
        }
        Ok((measured, unreadable))
    }

    /// Compute the filing keys of every book that has never had them: the
    /// back-fill for books stored before `sort_title`/`sort_author` existed.
    ///
    /// **A command and not a migration**, and like
    /// [`Engine::measure_stored_covers`] it could not have been one: `sort_title`
    /// drops a leading article and `sort_author` is
    /// [`crate::names::sort_key`]'s parse of a human name, and SQLite can do
    /// neither. `Storage::stale_sort_keys` states the work list
    /// (`sort_author IS NULL`, which is *never computed* and never *no author*)
    /// and every row goes through the same `refresh_sort_keys` that every live
    /// write goes through, so a back-filled row and a freshly-written one are
    /// the same row.
    ///
    /// Returns how many books it filed. **Idempotent** — a second run finds an
    /// empty work list — which is what lets a seeding script run it
    /// unconditionally and what makes the CLI's already-done wording the line a user usually
    /// sees.
    pub async fn rebuild_sort_keys(&self) -> Result<usize> {
        self.storage.rebuild_sort_keys().await
    }

    // ---- looking a book up again ------------------------------------------

    /// Ask the providers about a book we already have, and merge what they say.
    ///
    /// The gap this closes: for a long time `Storage::enrich_book` was called
    /// only by the calibre importer, so every book created without an ISBN — from
    /// a sidecar, or from a filename stem — had no cover, no description and no
    /// page count, permanently.
    ///
    /// An **explicit action on one book**. Not the device pull path, which
    /// stays fully offline by design (a reader plugged in is imported, and
    /// nobody on the network is asked anything because of it);
    /// nothing automatic, nothing periodic, and no count of books that have not
    /// had it done to them. There is deliberately no bulk form: the per-book cost
    /// is a provider fan-out, and a loop over the shelf is a rate-limit policy
    /// nobody has decided yet.
    ///
    /// Everything else — the one matcher, the refusal band, the per-field
    /// attribution, the user guard — is argued in [`enrich`].
    pub async fn enrich_book_from_providers(&self, book_id: i64) -> Result<EnrichReport> {
        let providers = self.providers();
        let mut report = enrich::enrich_metadata(&self.storage, &providers, book_id).await?;

        // The cover last, and only where there is no file. The merge above may
        // have just supplied the URL — which is the ordinary case, since a book
        // with no cover usually had no `cover_url` either — and a book that
        // already has an image does not re-fetch one.
        let book = self
            .storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
        if book.cover_path.is_none()
            && let Some(url) = book.cover_url.as_deref()
        {
            match self.download_cover_file(url).await {
                Ok(cover) => {
                    // One field, one claim: whoever supplied the URL is the
                    // origin of the file fetched from it. `None` when the URL
                    // predates this run, which is the honest answer — we did not
                    // learn it here and cannot say who did. The claim is on
                    // `cover_path` alone; the measurements beside it have no
                    // origin to name.
                    let source = report.source_of("cover_url");
                    self.storage.set_cover(book_id, &cover, source).await?;
                    report.cover = Some(cover.path);
                }
                // A cover that will not download degrades; it does not fail the
                // call. The metadata landed and is worth keeping, and the next
                // run tries again for free.
                Err(e) => report.warnings.push(Diagnostic::cover_unavailable(&e)),
            }
        }
        Ok(report)
    }

    /// Write fields the **user** supplied, and record that they did.
    ///
    /// This is the one way a field becomes the user's — the `user` rank in
    /// `field_provenance` exists so that a hand correction survives the next
    /// provider merge, and without a writer that stamps it the protection
    /// could never be triggered. It matters in both directions for
    /// [`Engine::enrich_book_from_providers`]: enrichment is the writer that
    /// would otherwise silently overwrite a correction, and correcting a title
    /// or supplying an ISBN is the **next move** its refusal offers. A refusal
    /// whose remedy did not exist would be a dead end.
    ///
    /// The merge is the ordinary partial-record one — a field the record is
    /// silent about is left alone — so this **sets, and cannot clear**. Clearing
    /// a field needs a statement that can write NULL and therefore a way to say
    /// "I mean it", which no caller has asked for; saying so is better than
    /// implying it works.
    pub async fn set_book_fields(&self, book_id: i64, fields: &Book) -> Result<Book> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        self.storage
            .enrich_book(book_id, fields, Some(Source::User))
            .await?;
        self.storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))
    }

    /// Where each field of a book came from, and when.
    ///
    /// On the facade because a frontend showing "page count: 512
    /// (openlibrary)" needs it, and reaching through `Engine::storage` for it
    /// would reopen the seam the facade exists to close. **An absent field
    /// means nobody has claimed it** — every
    /// book predating migration `0012` reports an empty list however
    /// well-populated it is.
    pub async fn field_provenance(&self, book_id: i64) -> Result<Vec<FieldSource>> {
        self.storage.field_provenance(book_id).await
    }

    /// [`Engine::download_cover`] for a book that is already stored.
    ///
    /// The `&mut Book` version is the one the CLI's add-flow wants: it is
    /// holding an unsaved candidate and needs the path written back onto it.
    /// Every *other* caller has an id and wants the stored row updated — and a
    /// `&mut` domain type cannot cross a transport (the API is request and
    /// reply), since the mutation is the return value.
    pub async fn fetch_cover(&self, book_id: i64) -> Result<Option<PathBuf>> {
        let mut book = self
            .storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
        self.download_cover(&mut book).await
    }

    // ---- readings ----------------------------------------------------------

    /// Every reading of this book, oldest first. A reread is a row, not a flag,
    /// so this is a history rather than a count.
    pub async fn list_readings(&self, book_id: i64) -> Result<Vec<Reading>> {
        self.storage.list_readings(book_id).await
    }

    /// One reading by id.
    pub async fn get_reading(&self, id: i64) -> Result<Option<Reading>> {
        self.storage.get_reading(id).await
    }

    /// Every reading of this book, oldest first, **each with its own
    /// progress**.
    ///
    /// The pairing is the derivation, and it is why this is a facade method and
    /// not a `From` impl above the seam. `readings` has no page count — length
    /// is bibliographic and lives on `books` — so a caller holding only a
    /// `Reading` cannot reach [`Progress::of_reading`] at all, and a caller
    /// holding both has to know which book's length goes with which read. That
    /// is where it bites: a frontend showing a reread's second read would
    /// otherwise print `BookDto::progress`, which `progress.rs` warns in so many
    /// words "will show the current read's numbers under an older read's
    /// heading". The alternative it was reaching for is `current_page /
    /// page_count` computed above the API — a derivation a frontend should
    /// never make, and one that walks into all three hazards [`Progress`]
    /// documents.
    ///
    /// One book read once returns one pair, and it agrees with
    /// [`Progress::of_book`] — the two forms differ only where there is more
    /// than one read to disagree about.
    pub async fn readings_with_progress(&self, book_id: i64) -> Result<Vec<(Reading, Progress)>> {
        let length = self.length_of(book_id).await?;
        Ok(self
            .storage
            .list_readings(book_id)
            .await?
            .into_iter()
            .map(|r| {
                let p = Progress::of_reading(&r, length);
                (r, p)
            })
            .collect())
    }

    /// One named reading with its own progress. See
    /// [`Engine::readings_with_progress`].
    pub async fn reading_with_progress(&self, id: i64) -> Result<Option<(Reading, Progress)>> {
        let Some(r) = self.storage.get_reading(id).await? else {
            return Ok(None);
        };
        let length = self.length_of(r.book_id).await?;
        let p = Progress::of_reading(&r, length);
        Ok(Some((r, p)))
    }

    /// The book's open reading with its own progress, if it has one.
    pub async fn active_reading_with_progress(
        &self,
        book_id: i64,
    ) -> Result<Option<(Reading, Progress)>> {
        let Some(r) = self.storage.active_reading(book_id).await? else {
            return Ok(None);
        };
        let length = self.length_of(book_id).await?;
        let p = Progress::of_reading(&r, length);
        Ok(Some((r, p)))
    }

    /// A book's length, or absence.
    ///
    /// A missing *book* answers `None` rather than erroring: the three callers
    /// above are already holding a reading, whose foreign key guarantees the
    /// book exists, so the only way to reach the `None` arm is a race — and a
    /// read with no denominator is a state [`Progress`] already handles.
    async fn length_of(&self, book_id: i64) -> Result<Option<i64>> {
        Ok(self
            .storage
            .get_book(book_id)
            .await?
            .and_then(|b| b.page_count))
    }

    /// One page of the library's readings, each with its book, its own
    /// progress, its read number and its card passage.
    ///
    /// The library-wide list of readings. Every other reading method here is
    /// scoped to one book except [`Engine::currently_reading`], which is
    /// filtered to the open ones — so without this a *finished* reading could
    /// be reached only by already knowing its book, while
    /// `ActivitySummary::books_finished` counts exactly those rows.
    ///
    /// **One call answers a page**, which is the point: the alternatives are a
    /// `get_book` and a `card_passage` per row, an N+1 query pattern, on a
    /// list whose whole purpose is to be long. See
    /// [`Storage::list_reading_rows`] for the plan and for why the read number
    /// is counted over the book rather than over the page.
    pub async fn list_reading_rows(&self, query: &ReadingQuery) -> Result<Vec<ReadingRow>> {
        self.storage.list_reading_rows(query).await
    }

    /// Which years a filter's readings **ended** in, newest first, and whether
    /// any of them is still open.
    ///
    /// The question a year picker asks, and the reason it is a request rather
    /// than a client-side pass over the rows: deriving it above the seam means
    /// pulling every reading in the library — a book and a private passage per
    /// row — to draw six controls, and then extracting a year in a second
    /// dialect. `finished_in` is UTC by [`DayRange`], and a local-time year in
    /// a frontend files a New Year's Eve read under one year in the picker and
    /// the other in the list for every reader west of Greenwich.
    pub async fn reading_years(&self, filter: &ReadingFilter) -> Result<ReadingYears> {
        self.storage.reading_years(filter).await
    }

    /// How many readings match — the number a wall needs before it needs the
    /// rows. Its own call for the reason [`Engine::count_books`] is.
    pub async fn count_readings(&self, filter: &ReadingFilter) -> Result<i64> {
        self.storage.count_readings(filter).await
    }

    /// The book's open reading, if it has one. At most one can exist —
    /// `idx_readings_one_open` makes that an invariant.
    pub async fn active_reading(&self, book_id: i64) -> Result<Option<Reading>> {
        self.storage.active_reading(book_id).await
    }

    /// Write reading progress and return the book with its projections
    /// refreshed. Opens a reading when none is open.
    pub async fn update_progress(
        &self,
        book_id: i64,
        page: Option<i64>,
        finished: Option<bool>,
    ) -> Result<Book> {
        self.storage.update_progress(book_id, page, finished).await
    }

    /// Finish the book's **open** reading on a day the reader names, and return
    /// the book with its projections refreshed (a book page's *Finish the
    /// book*).
    ///
    /// Beside [`Engine::update_progress`]'s `finished: Some(true)`, which closes
    /// at `now()` and — the reason this exists — **opens a reading first when
    /// none is open**, so a stale *Finish* press on a book already closed would
    /// mint an empty read and finish that. This refuses instead: finishing is
    /// about the read you are in, and with none open there is nothing to close.
    ///
    /// `on` is a `YYYY-MM-DD` day, UTC like every day in this engine, or `None`
    /// for now. The instant is [`finish_instant`]'s, which is where the three
    /// edges are decided and tested.
    pub async fn finish_reading_on(&self, book_id: i64, on: Option<&str>) -> Result<Book> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        let open = self.storage.active_reading(book_id).await?.ok_or_else(|| {
            EngineError::InvalidInput(format!("book id {book_id} has no open reading to finish"))
        })?;
        let now = storage::now_unix();
        let day_start = match on {
            None => None,
            Some(day) => Some(DayRange::new(day, day)?.unix_bounds().0),
        };
        let at = finish_instant(day_start, now, open.started_at)?;
        self.storage
            .close_reading_at(open.id, at, storage::STATUS_FINISHED)
            .await?;
        self.storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))
    }

    /// Record how many pages this edition has, as the reader says it — the
    /// second box of the book page's *Page [N] of [M]*.
    ///
    /// [`Engine::set_book_fields`] with a record that speaks to `page_count`
    /// alone, so the field is stamped `user` and held against every later
    /// provider merge, exactly as `rb set` would hold it. Its own method because
    /// a frontend building a whole `Book` to move one number is a frontend
    /// that will one day send the rest of the row back with it, claiming every
    /// field it echoed.
    ///
    /// A page count below 1 is refused: a book has no page zero, and
    /// `Progress` already reads a stored `0` as *no length* — writing one here
    /// would be a field the user owns that every surface ignores.
    pub async fn set_page_count(&self, book_id: i64, page_count: i64) -> Result<Book> {
        if page_count < 1 {
            return Err(EngineError::InvalidInput(format!(
                "a page count is at least 1, got {page_count}"
            )));
        }
        let fields = Book {
            page_count: Some(page_count),
            ..Book::default()
        };
        self.set_book_fields(book_id, &fields).await
    }

    /// Close the open reading and start a fresh one. Returns its id.
    pub async fn reread(&self, book_id: i64) -> Result<i64> {
        self.storage.reread(book_id).await
    }

    // ---- reading events ----------------------------------------------------

    /// Rebuild the activity log from everything already in the database:
    /// highlight stamps, note timestamps, reading endpoints.
    ///
    /// Explicit, and called by nothing here. An import that also refilled would
    /// make the log a side effect of whichever importer ran last, and a filler
    /// that needs a device (KOReader's statistics database) or a typed page
    /// writes through
    /// [`Storage::record_reading_events`] instead.
    ///
    /// Idempotent: a second call with nothing changed upstream reports zero
    /// inserted and zero updated.
    pub async fn refill_reading_events(&self) -> Result<RefillReport> {
        self.storage.refill_reading_events().await
    }

    /// One book's activity log, oldest day first.
    pub async fn reading_events(&self, book_id: i64) -> Result<Vec<ReadingEvent>> {
        self.storage.reading_events(book_id).await
    }

    /// What is known about a period — books finished, days with activity, notes
    /// and links written, and minutes and pages **where they were measured**.
    ///
    /// `minutes` and `pages` come back `None` when nothing in the period
    /// measured them. That is not the same answer as zero and must not be
    /// rendered as one: a reader whose library came from a Goodreads CSV has no
    /// minutes at all, and a screen that shows them `0` has told them something
    /// false about their own reading.
    pub async fn activity_summary(&self, range: &DayRange) -> Result<ActivitySummary> {
        self.storage.activity_summary(range).await
    }

    /// The days of a period that carry an event. The set behind
    /// `ActivitySummary::activity_days`, for a caller that wants to show them.
    pub async fn activity_by_day(&self, range: &DayRange) -> Result<Vec<DayActivity>> {
        self.storage.activity_by_day(range).await
    }

    /// The same period one grain up, for a caller drawing years rather than
    /// weeks. Only months carrying an event come back.
    ///
    /// It exists because a month is **not** a bucket of days a frontend can
    /// fold for itself: `minutes: None` collapses to `0` on the first `reduce`,
    /// and `books` — distinct over the whole month — cannot be recovered from
    /// the days at all.
    pub async fn activity_by_month(&self, range: &DayRange) -> Result<Vec<MonthActivity>> {
        self.storage.activity_by_month(range).await
    }

    // ---- moments -----------------------------------------------------------

    /// Everything worth noticing that has not been shown yet, newest first.
    ///
    /// Derived on every call from rows other features wrote — a reading that
    /// closed, the first mark on a book, a reflection that reached across, a
    /// run of days that ended. **Nothing about a moment is stored**, so this
    /// cannot go stale and there is nothing here to accumulate.
    ///
    /// Two things a caller has to know. There is **no push channel** in this
    /// codebase and this is not one: poll it on launch and after a write that
    /// could mint one. And there is deliberately **no count** — not here, not
    /// on the wire — because a number of things waiting is a badge, and the
    /// app never greets the reader with a count of what they have not done.
    /// `limit` is the only lever, and it
    /// takes from the newest end.
    pub async fn pending_moments(&self, limit: Option<i64>) -> Result<Vec<Moment>> {
        self.storage
            .pending_moments(storage::now_unix(), limit)
            .await
    }

    /// Record that a moment was surfaced, so the ceremony does not replay.
    ///
    /// Idempotent — acknowledging twice, or from two frontends, writes one row
    /// and keeps the first time. The id is a [`Moment::id`] and is opaque: a
    /// frontend hands back what it was given and never builds one.
    pub async fn acknowledge_moment(&self, id: &str) -> Result<()> {
        self.storage.acknowledge_moment(id).await
    }

    // ---- highlights --------------------------------------------------------

    /// This book's highlights, device-owned fields and all.
    ///
    /// Every reading of the book, in one list. Each row carries the
    /// `reading_id` it was attributed to — `None` where no reading's window
    /// holds it — so grouping by read is the caller's to do, and the rows that
    /// belong to no read stay reachable.
    pub async fn list_highlights(&self, book_id: i64) -> Result<Vec<Highlight>> {
        self.storage.list_highlights(book_id).await
    }

    /// Groups of this book's highlight ids that are the same passage seen by
    /// more than one row (usually two apps) — see
    /// [`highlight_origin::same_passages`]. A derived fact: rows are never
    /// merged.
    pub async fn same_passages(&self, book_id: i64) -> Result<Vec<Vec<i64>>> {
        Ok(highlight_origin::same_passages(
            &self.storage.list_highlights(book_id).await?,
        ))
    }

    /// What was highlighted during one reading.
    ///
    /// The reading-scoped half of [`Engine::list_highlights`], and the reason
    /// `highlights.reading_id` exists: reflections and reviews already anchor to
    /// a reading, so "what did I mark the second time through?" is a question
    /// the schema could answer well before anything could ask it.
    pub async fn highlights_for_reading(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        self.storage.highlights_for_reading(reading_id).await
    }

    /// The passage a reading's card shows — the one the reader chose — or
    /// `None` when none is chosen.
    ///
    /// **Which passage is a selection predicate, so it stays below the seam**:
    /// every frontend shows the *same* passage for one reading, which two
    /// frontends each reaching for `highlights[0]` would not. The engine used
    /// to decide by a rule (the longest mark), and that rule selected for the
    /// longest drag rather than the best passage; now
    /// [`Engine::set_card_passage`] records the reader's choice and this reads
    /// it back. `None` is ordinary and means *not chosen*; a card
    /// draws that absence as an absence and does not invite.
    ///
    /// Always a member of [`Engine::card_passage_candidates`] for the same
    /// reading, which is what the write enforces.
    pub async fn card_passage(&self, reading_id: i64) -> Result<Option<Highlight>> {
        self.storage.card_passage(reading_id).await
    }

    /// Choose the passage a reading's card shows, or clear it with `None`.
    /// The highlight must be one of
    /// [`Engine::card_passage_candidates`] for the reading — of the reading's
    /// book, and not marked during another read of it — or this is
    /// `InvalidInput`; a reading or highlight that does not exist is `NotFound`.
    /// The argument for the rule is on `Storage::set_card_passage`.
    pub async fn set_card_passage(&self, reading_id: i64, highlight_id: Option<i64>) -> Result<()> {
        self.storage
            .set_card_passage(reading_id, highlight_id)
            .await
    }

    /// The passages a reading's card may choose from: the book's
    /// highlights attributed to this reading or to none, in
    /// [`Engine::list_highlights`]'s order. A predicate, so it is answered here
    /// once rather than as a filter each frontend writes over the book's list.
    pub async fn card_passage_candidates(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        self.storage.card_passage_candidates(reading_id).await
    }

    /// Write **our** annotation on a highlight — the column beside `origin_note`
    /// that an import never touches.
    ///
    /// Migration `0004` split a highlight's note into the source app's
    /// (`origin_note`, refreshed by every import) and the reader's own
    /// (`annotation`, which no import writes); this is how a frontend writes
    /// the reader's.
    pub async fn set_annotation(&self, highlight_id: i64, annotation: Option<&str>) -> Result<()> {
        self.storage.set_annotation(highlight_id, annotation).await
    }

    // ---- epub import -------------------------------------------------------

    /// Import a local .epub: extract its ISBN, enrich via providers, extract
    /// the embedded cover, save. Falls back to epub metadata alone when the
    /// file has no usable ISBN or the providers are unreachable.
    #[tracing::instrument(skip(self), fields(path = %path.display()))]
    pub async fn import_epub(&self, path: &Path) -> Result<Book> {
        // One open, one parse: the metadata now, the cover after the lookup —
        // the same reads in the same order as two separate opens made.
        let mut file = epub::EpubFile::open(path)?;
        let info = file.info();
        let mut seed = match &info.isbn {
            Some(isbn) => self.lookup_isbn(isbn).await?.unwrap_or_default(),
            None => Book::default(),
        };

        // Two records with two origins, merged by the same statement every other
        // partial record goes through — where this used to fold the file's
        // metadata onto the provider's field by field, with a hand-written
        // `is_none()` per column. That chain *was* `MERGE_RULES`'s no-clobber
        // merge, spelled a second time, and it produced one `Book` with two
        // origins that no single `field_provenance` stamp could describe
        // honestly. The precedence is unchanged: the provider wins, the file
        // fills the gaps, which is what `Storage::fill_book` means.
        let mut from_epub = Book {
            title: info.title.clone(),
            authors: info.authors.clone(),
            language: info.language.clone(),
            ..Default::default()
        };
        if seed.isbn_10.is_none()
            && seed.isbn_13.is_none()
            && let Some(isbn) = &info.isbn
        {
            match isbn.len() {
                10 => from_epub.isbn_10 = Some(isbn.clone()),
                _ => from_epub.isbn_13 = Some(isbn.clone()),
            }
            // The seed carries the ISBN too, and *only* as a conflict key: with
            // no ISBN on it `upsert_book` takes its third branch, a plain
            // insert, and re-importing the same file would make a second book.
            // The claim on the column is stamped by the fill below, which is
            // where it belongs — the file is what said it.
            seed.isbn_10.clone_from(&from_epub.isbn_10);
            seed.isbn_13.clone_from(&from_epub.isbn_13);
        }
        let cover = file.cover(&self.config.images_dir)?;
        drop(file);
        if let Some(cover) = &cover {
            from_epub.cover_path = Some(cover.path.display().to_string());
        }

        let id = self.storage.upsert_book(&seed, None).await?;
        self.storage
            .fill_book(id, &from_epub, Some(storage::Source::Epub))
            .await?;
        // The measurements go in beside the path the fill just wrote — but only
        // when the fill actually took *this* cover. `fill_book` lets the stored
        // row win, so a book that already had a jacket keeps it, and writing
        // the epub's dimensions over that row would describe an image it is not
        // pointing at. Read back rather than predicted: the merge is the
        // authority on which path survived.
        if let Some(cover) = &cover
            && let Some(stored) = self.storage.get_book(id).await?
            && stored.cover_path.as_deref() == Some(cover.path.display().to_string().as_str())
        {
            self.storage
                .set_cover(id, cover, Some(storage::Source::Epub))
                .await?;
        }
        let saved = self
            .get_book(id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))?;

        // Record the file's KOReader identity now, while we are holding the
        // file. The payoff arrives later and elsewhere: when this book's
        // sidecar comes off the device, `match_book` takes its `md5` branch and
        // links it outright, instead of guessing at the title — and that branch
        // is the only one that works when KOReader is configured to keep
        // sidecars in `dir` or `hash` mode, away from the book.
        //
        // `link_device_book`, never `set_device_link`: this is a scan, and a
        // scan must not relabel a link the user made by hand.
        if let Some(id) = saved.id {
            let md5 = partial_md5::partial_md5(path)?;
            self.storage
                .link_device_book(&md5, id, storage::LinkedBy::Auto)
                .await?;
        }
        Ok(saved)
    }

    // ---- owned files -------------------------------------------------------

    /// Take ownership of an ebook file: work out whose it is, copy the bytes
    /// into the content store, and attach the row.
    ///
    /// Creates a book only when nothing in the library plausibly claims the
    /// file. A near-miss title comes back as [`FileOutcome::Unmatched`] with the
    /// candidates and **nothing written**, which `FileImportOptions { new: true }`
    /// overrides — the same refusal-with-a-next-move shape `ko pull` has.
    #[tracing::instrument(skip(self), fields(path = %path.display(), new = opts.new))]
    pub async fn import_file(
        &self,
        path: &Path,
        opts: files::ImportOptions,
    ) -> Result<FileImportReport> {
        files::import(self, path, opts).await
    }

    /// Attach a file to a book the caller has already decided on — dedup level
    /// 2, the epub-and-azw3 case. No matching and no creation.
    pub async fn add_file_to_book(&self, book_id: i64, path: &Path) -> Result<FileImportReport> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        files::attach(&self.storage, &self.config.files_dir, book_id, path).await
    }

    /// What a file is and what it looks like a copy of. Read-only — no bytes
    /// are copied and no row is written, so a frontend can show the answer
    /// before the user commits to it.
    pub async fn identify_file(&self, path: &Path) -> Result<FileIdentity> {
        files::identify(&self.storage, path).await
    }

    /// Paths as the book files they hold: a file passes through, a folder is
    /// walked (bounded, hidden entries and linked folders skipped), and a
    /// calibre library is named rather than walked. Reads directory listings
    /// only; see [`folders`] for the bounds and why each is there.
    pub async fn expand_book_paths(&self, paths: Vec<PathBuf>) -> Result<ExpandedPaths> {
        tokio::task::spawn_blocking(move || folders::expand(&paths, folders::Bounds::SHIPPED))
            .await
            .map_err(|e| EngineError::Other(format!("folder walk task: {e}")))
    }

    /// The files this book owns.
    pub async fn book_files(&self, book_id: i64) -> Result<Vec<BookFile>> {
        self.storage.book_files(book_id).await
    }

    /// The chapter list of the epub this book owns — **read from the file, not
    /// from the database**.
    ///
    /// `None` means there is no file here this can read: no owned epub at all,
    /// or only formats it cannot parse. `Some(entries)` with an empty list
    /// means the epub itself carries no navigable TOC, which is a different
    /// answer and an ordinary one. Neither is an error; a file we own that will
    /// not parse *is*, because those bytes are ours and were verified on the
    /// way in.
    ///
    /// Why nothing is stored is argued at [`epub::table_of_contents`]. The
    /// short of it: the file is content-addressed, so it is always the current
    /// answer, and a chapter list has no origin to attribute or user to correct
    /// it — which is what every other column migration `0013` added does have.
    ///
    /// The **first** epub, when a book owns several. Two epubs on one book is
    /// legitimate (a re-download, a second edition) and choosing between them is
    /// a question no caller has asked yet; `book_files` order is stable, so the
    /// answer at least does not flicker.
    pub async fn table_of_contents(&self, book_id: i64) -> Result<Option<TableOfContents>> {
        let Some(file) = self.readable_file(book_id, "epub").await? else {
            return Ok(None);
        };
        let entries = epub::table_of_contents(&file.path)?;
        Ok(Some(TableOfContents {
            sha256: file.sha256,
            entries,
        }))
    }

    /// **A readable file for this book**, of one format: a kept store file
    /// first, else the first `path` place (of any of the book's copies) whose
    /// file is on disk and whose copy is not `unreadable`. Every "read the
    /// book's file" lookup goes through here, because an imported file is
    /// *pointed to* where it lies (a `path` place on a copy), not copied in,
    /// and `book_files` holds only the ones the reader asked us to keep.
    ///
    /// A pointed file that is gone is skipped, never an error: its place is
    /// stamped `missing_since` (earliest kept) and un-missed when it is found
    /// again. The copy and `have_now` are never touched.
    pub(crate) async fn readable_file(
        &self,
        book_id: i64,
        format: &str,
    ) -> Result<Option<ReadableFile>> {
        if let Some(file) = self
            .storage
            .book_files(book_id)
            .await?
            .into_iter()
            .find(|f| f.format == format)
        {
            return Ok(Some(ReadableFile {
                path: self.file_path(&file),
                sha256: file.sha256,
            }));
        }
        for copy in self.storage.copies_for_book(book_id).await? {
            if copy.unreadable {
                continue;
            }
            for place in copy.places.iter().filter(|p| p.kind == PlaceKind::Path) {
                let path = PathBuf::from(&place.reference);
                if files::format_of(&path) != format {
                    continue;
                }
                if !path.is_file() {
                    self.storage
                        .mark_place_missing(PlaceKind::Path, &place.reference, storage::now_unix())
                        .await?;
                    continue;
                }
                if place.missing_since.is_some() {
                    self.storage
                        .unmiss_place(PlaceKind::Path, &place.reference)
                        .await?;
                }
                let sha256 = match &place.last_hash {
                    Some(h) => h.clone(),
                    None => files::sha256_of(&path)?,
                };
                return Ok(Some(ReadableFile { path, sha256 }));
            }
        }
        Ok(None)
    }

    /// One owned file by its content address. `sha256` is the primary key, so
    /// this is the whole of "who has these bytes" — at most one book can.
    pub async fn book_file(&self, sha256: &str) -> Result<Option<BookFile>> {
        self.storage.book_file(sha256).await
    }

    /// Where a file's bytes are. Derived from the row, never stored — the whole
    /// point of a content address is that the path is not a second fact that can
    /// disagree with the first.
    pub fn file_path(&self, file: &BookFile) -> PathBuf {
        files::content_path(&self.config.files_dir, &file.sha256, &file.format)
    }

    /// Give up a file: forget the row and remove the bytes.
    ///
    /// Returns false when the sha was not ours, which makes a repeat call a
    /// no-op rather than an error.
    pub async fn remove_file(&self, sha256: &str) -> Result<bool> {
        let Some(file) = self.storage.book_file(sha256).await? else {
            return Ok(false);
        };
        self.storage.delete_book_file(sha256).await?;
        // The copy stays, and so do its other places; only this one is gone.
        self.storage.delete_place(PlaceKind::Store, sha256).await?;
        std::fs::remove_file(self.file_path(&file)).ok();
        Ok(true)
    }

    // ---- koreader ----------------------------------------------------------

    /// Import KOReader highlights/notes from a sidecar file, .sdr dir, or
    /// library root. Idempotent; single-word highlights become flashcard
    /// candidates.
    #[tracing::instrument(skip(self), fields(path = %path.display()))]
    pub async fn import_koreader(&self, path: &Path, dry_run: bool) -> Result<ImportReport> {
        koreader::import(&self.storage, path, dry_run).await
    }

    /// Pull a book in from the reader: create it from the sidecar's own
    /// metadata, then import its highlights. Offline — no provider enrichment.
    #[tracing::instrument(skip(self), fields(path = %sidecar.display()))]
    pub async fn pull_book_from_sidecar(&self, sidecar: &Path) -> Result<PullReport> {
        koreader::import_book_from_sidecar(&self.storage, sidecar).await
    }

    // ---- device ------------------------------------------------------------

    /// Walk a mounted reader and report the state of every book on it.
    /// Read-only, and pre-filtered on each sidecar's size and mtime so a
    /// re-scan does not re-evaluate several hundred Lua files.
    ///
    /// Exposed here so a frontend never reaches into [`koreader`] directly.
    #[tracing::instrument(skip(self), fields(root = %root.display()))]
    pub async fn scan_device(&self, root: &Path) -> Result<device::DeviceScan> {
        device::scan_device(&self.storage, root).await
    }

    /// Pull a selection of the device in: one report per book.
    ///
    /// **Deliberately does not import statistics.** See
    /// [`Engine::import_device_statistics`].
    pub async fn sync_device(&self, paths: &[PathBuf]) -> Result<Vec<PullReport>> {
        device::sync_device(&self.storage, paths).await
    }

    // ---- the plugin --------------------------------------------------------

    /// What bookmole's plugin looks like on this reader, and whether we
    /// know the reader it says it is paired with.
    ///
    /// Read-only **about the mount**, and that is the whole of the promise
    /// made about arrival: nothing here writes to somebody else's
    /// hardware. It does write one row of ours — see below.
    ///
    /// The one thing it adds over [`plugin::inspect`] is `paired`, which is a
    /// fact about *our* database rather than about the mount: a `pairing.lua`
    /// naming a device we have no row for is a reader that was paired with some
    /// other copy of bookmole.
    ///
    /// **Recognising a paired reader stamps `last_seen_at`**. Seeing
    /// a device is an event and the only one that can record it is whoever
    /// looked, so this is where it belongs rather than in a
    /// `note_device_seen` every frontend has to remember to call — a stamp one
    /// frontend made and another did not would be a column meaning something
    /// different depending on which app you had been using. If it moved only
    /// on install, so *last connected* was really
    /// *last time you installed the plugin*.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn plugin_status(&self, mount: &Path) -> Result<plugin::PluginStatus> {
        let mut status = plugin::inspect(mount)?;
        // A reader can be paired with several computers, so *which* of its ids
        // is ours is a question only this side can answer — `plugin::inspect`
        // guessed the file's first entry. `touch_device_seen` answers `false`
        // for a reader we have no row for, which is exactly the `paired` we are
        // about to report, so the loop is one statement per candidate rather
        // than a read followed by a conditional write.
        status.paired = false;
        status.device_id = None;
        for pairing in &status.pairings {
            if self
                .storage
                .touch_device_seen(&pairing.device_id, mount.to_str())
                .await?
            {
                status.paired = true;
                status.device_id = Some(pairing.device_id.clone());
                break;
            }
        }
        Ok(status)
    }

    /// Install (or upgrade) the plugin on a mounted reader, and pair with it.
    ///
    /// **Never called automatically.** Mount → import is automatic and
    /// read-only precisely so that mount → *write* can be an explicit act, and
    /// a caller that wired this to the device watcher would be undoing that
    /// rule rather than adding a convenience.
    ///
    /// The identity is minted here and nowhere else, so that the value written
    /// into the device's `pairing.lua` and the value stored in
    /// `paired_devices` cannot drift. A reader that is already paired keeps its
    /// id **and its token**: reinstalling a newer plugin is an upgrade, not a
    /// re-pairing.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn install_plugin(&self, mount: &Path) -> Result<plugin::InstallReport> {
        self.install_plugin_at(mount, storage::now_unix()).await
    }

    /// [`Engine::install_plugin`] with the clock supplied.
    ///
    /// Private, so it adds no product surface — the reason it exists is that
    /// "a reinstall keeps the pairing it already had" is a claim about *which
    /// timestamp is chosen*, and a test that calls the public method twice
    /// reads the same second twice and passes whether or not the rule holds.
    /// That is exactly how the rule came to be broken while looking tested.
    async fn install_plugin_at(&self, mount: &Path, now: i64) -> Result<plugin::InstallReport> {
        let seen = plugin::inspect(mount)?;
        // **Our** entry is the one we have a row for, and finding it is the
        // whole of what stopped a second bookmole stealing a reader. The
        // old code read the file's single `device_id`, found no row for it
        // because that id belonged to somebody else's install, minted a fresh
        // identity and overwrote the file — leaving the first machine holding a
        // `paired_devices` row for a token the device no longer had.
        let mut existing = None;
        for pairing in &seen.pairings {
            if let Some(d) = self.storage.paired_device(&pairing.device_id).await? {
                existing = Some(d);
                break;
            }
        }
        // `paired_at` is carried over too, not just the id and the token. It is
        // the *pairing's* timestamp, and stamping `now` on an upgrade made the
        // device's `pairing.lua` disagree with `paired_devices.installed_at`,
        // which `record_pairing` correctly leaves alone — found by reinstalling
        // onto a real Kindle and reading the file back.
        let (device_id, token, paired_at) = match existing {
            Some(d) => (d.device_id, d.token, d.installed_at),
            None => (plugin::mint_id(16)?, plugin::mint_id(32)?, now),
        };

        let report = plugin::install(
            mount,
            &device_id,
            &token,
            paired_at,
            plugin::this_computer().as_deref(),
            // The hint, not a fact: where this computer would be reached from
            // the network it is on *now*. The device overwrites it with what it
            // actually reached, and `None` — a laptop installing with the wifi
            // off — simply writes nothing.
            plugin::this_lan_address().as_deref(),
        )?;
        self.storage
            .record_pairing(
                &device_id,
                mount.file_name().and_then(|n| n.to_str()),
                &token,
                report.version,
                mount.to_str(),
                paired_at,
            )
            .await?;
        Ok(report)
    }

    /// Remove the plugin and forget the pairing.
    ///
    /// Both halves, because a pairing row whose device no longer holds the
    /// token is a link that exists only on our side. Uninstall is exact: it
    /// leaves neither our files on the reader nor a pairing that can no
    /// longer work.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn uninstall_plugin(&self, mount: &Path) -> Result<plugin::UninstallReport> {
        let mut report = plugin::uninstall(mount)?;
        // `forgot_device` arrives holding the file's first entry, which
        // `plugin::uninstall` cannot do better than. Taking the plugin off ends
        // every computer's pairing — the file went with it, which is what
        // `removed_pairings` says — but the only row *we* may drop is our own,
        // and a reader paired to another install leaves us nothing to forget.
        report.forgot_device = None;
        for id in &report.removed_pairings {
            if self.storage.forget_pairing(id).await? {
                report.forgot_device = Some(id.clone());
                break;
            }
        }
        Ok(report)
    }

    /// Every reader we have paired with, whether or not it is plugged in.
    pub async fn paired_devices(&self) -> Result<Vec<PairedDevice>> {
        self.storage.list_paired_devices().await
    }

    // ---- the wireless listener ----

    /// Whether a paired reader could reach this computer over the LAN.
    ///
    /// `Off` is the default and the answer for every host that has not asked,
    /// which is what makes the whole design fail closed: with nothing bound
    /// there is no service to find, nothing to fingerprint and nothing to leak.
    pub async fn listener_status(&self) -> Result<wireless::ListenerStatus> {
        Ok(self.wireless.status(storage::now_unix()).await)
    }

    /// Open the door, for `minutes` (default five; `Some(0)` means until asked
    /// to stop).
    ///
    /// **Never on an automatic path**, and for `install_plugin`'s reason one
    /// layer out: arrival is kept read-only precisely so that anything
    /// reaching outward is an explicit act. A host that called this on
    /// startup would have made the toggle a lie.
    ///
    /// The window also closes on the **first completed push**, because one tap
    /// on the reader is one session and the door has then done its job. A
    /// session is every book that tap carried, not one file — closing after the
    /// first would strand the rest.
    #[tracing::instrument(skip(self))]
    pub async fn start_listening(&self, minutes: Option<u32>) -> Result<wireless::ListenerStatus> {
        // `0.0.0.0` and the real socket: this is the one call in the subsystem
        // that cannot run in CI, and it is deliberately the *only* one — see
        // `wireless::UdpBeacon`, which is `watch.rs`'s `watch_mounts` in a
        // different costume.
        let bind = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        let beacon = wireless::UdpBeacon::bind(bind).await?;
        self.wireless
            .start(
                self.storage.clone(),
                plugin::this_computer().unwrap_or_else(|| "bookmole".into()),
                bind,
                Arc::new(beacon),
                minutes,
                storage::now_unix(),
            )
            .await
    }

    /// Close it. Idempotent: stopping a listener that is already off is not an
    /// error, because a frontend cannot know the window did not expire between
    /// drawing the button and the user pressing it.
    pub async fn stop_listening(&self) -> Result<wireless::ListenerStatus> {
        Ok(self.wireless.stop().await)
    }

    /// Fetch from a paired reader whose window is open (a *pull*).
    ///
    /// **The same rendezvous, dialled the other way.** The desktop broadcasts
    /// the same `HELLO`, the reader answers the same `HERE`, and this verifies
    /// the same MAC before connecting — so a rogue that answers first cannot
    /// make us open a session with it. Once connected the entries travel reader
    /// → desktop exactly as they do in a push, which is why the transfer is
    /// literally the same function: **wireless is read-only toward us** is the
    /// shape of the protocol rather than a rule somebody enforces.
    ///
    /// It does **not** need [`Engine::start_listening`]: a seeker sends first,
    /// so the reply comes back to the ephemeral port it sent from, and pulling
    /// while the door is shut is the ordinary case rather than a special one.
    ///
    /// Refuses with `ReaderNotFound` when nothing answers — the reader's window
    /// is shut, it is on another subnet, or the AP dropped the broadcast, and a
    /// caller must not guess between them.
    #[tracing::instrument(skip(self))]
    pub async fn pull_from_reader(&self, device_id: &str) -> Result<Vec<koreader::PullReport>> {
        let device = self
            .storage
            .paired_device(device_id)
            .await?
            .ok_or(wireless::WirelessRefusal::UnknownDevice)?;
        let bind = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        let beacon = wireless::UdpBeacon::bind_ephemeral(bind)
            .await?
            // Three seconds, which is `calibre.koplugin/wireless.lua`'s own
            // figure for the same exchange on the same class of device. Not a
            // knob, for `RENDEZVOUS_PORT`'s reason.
            .deadline(std::time::Duration::from_secs(3));
        let report = wireless::pull_from(
            &self.storage,
            &device,
            &beacon,
            &Self::rendezvous_targets(),
            &plugin::mint_id(16)?,
        )
        .await?;
        Ok(report.pulled)
    }

    /// Where a probe for *any reader nearby* is sent: the rendezvous port, at
    /// every broadcast address this machine has. See
    /// [`plugin::broadcast_addresses`] for why it is more than one.
    fn rendezvous_targets() -> Vec<std::net::SocketAddr> {
        plugin::broadcast_addresses()
            .into_iter()
            .map(|a| std::net::SocketAddr::new(a.into(), wireless::RENDEZVOUS_PORT))
            .collect()
    }

    /// This computer's address on the network a reader would reach it over,
    /// when there is one.
    ///
    /// For a person to read off a screen and type into a reader — the plugin
    /// keeps a typed address for the networks where it cannot find this
    /// computer by itself, and the address has to come from somewhere. `None`
    /// with the wifi off, which is ordinary. **A hint and never a handle**:
    /// nothing may store it or join on it, since a lease moves.
    pub fn lan_address(&self) -> Option<String> {
        plugin::this_lan_address()
    }

    /// Pair with a reader **over wifi**, using the code it is showing.
    ///
    /// The reader must already carry the plugin — a cable put it there once,
    /// from this computer or another — and somebody has chosen *Link a computer
    /// over wifi* on it. `code` is what its screen says, typed as it looks:
    /// case, spaces and hyphens do not matter.
    ///
    /// **The code is a secret and is never logged**, which is why it is
    /// skipped below rather than recorded as a field; it joins the token under
    /// the tracing rule that is stricter than the one for highlight text.
    ///
    /// `host` is for the network where a broadcast does not arrive — the
    /// reader shows its own address beside the code for exactly that case. It
    /// is an *extra* place to send the request, never the only one.
    ///
    /// Like [`Engine::pull_from_reader`] it needs no open door: a seeker sends
    /// first and is answered where it sent from. Refuses with
    /// `PairingNotAnswered` when nothing proves the code, and a caller must not
    /// guess whether the code was wrong or the reader out of reach — the
    /// reader is deliberately silent in both cases.
    #[tracing::instrument(skip(self, code))]
    pub async fn pair_with_reader(&self, code: &str, host: Option<&str>) -> Result<PairedDevice> {
        // Checked before a socket exists, so a typo is answered at once.
        if wireless::normalise_code(code).is_none() {
            return Err(wireless::WirelessRefusal::NotAPairingCode.into());
        }
        let port = wireless::RENDEZVOUS_PORT;
        let mut targets = Self::rendezvous_targets();
        if let Some(host) = host.map(str::trim).filter(|h| !h.is_empty()) {
            let not_found = || wireless::WirelessRefusal::AddressNotFound {
                host: host.to_string(),
            };
            let resolved = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| not_found())?
                // IPv4 only, as every other part of this protocol is: the
                // reader's socket is `udp4`.
                .find(std::net::SocketAddr::is_ipv4)
                .ok_or_else(not_found)?;
            targets.push(resolved);
        }

        let bind = std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED);
        let beacon = wireless::UdpBeacon::bind_ephemeral(bind).await?;
        wireless::pair_with(
            &self.storage,
            &beacon,
            &targets,
            code,
            &plugin::this_computer().unwrap_or_else(|| "bookmole".into()),
            &plugin::mint_id(16)?,
            storage::now_unix(),
        )
        .await
    }

    /// The listener itself, for a host that wants to drive it directly.
    ///
    /// Behind `internals` for [`Engine::storage`]'s reason — it is how this
    /// crate's own tests reach a `serve_push` without a real broadcast socket,
    /// and it is not a door a frontend may use to grow a second protocol.
    #[cfg(feature = "internals")]
    pub fn wireless(&self) -> &Arc<wireless::Listener> {
        &self.wireless
    }

    /// Forget a pairing **without the reader in hand**.
    ///
    /// Deliberately not a second door onto [`Engine::uninstall_plugin`], and the
    /// difference is the whole reason it exists: uninstall is *exact* — it takes
    /// the files off the device and then drops our row — and it therefore needs
    /// the mount. This drops our row and nothing else, because a reader that has
    /// been sold, lost or reformatted cannot be reached and a list you can only
    /// leave by plugging something in is a list with no exit.
    ///
    /// **A caller must say so.** The plugin is still on that device and it still
    /// holds the token; if it ever comes back, installing again mints a fresh
    /// identity rather than resuming this one. Copy that says *removed* without
    /// saying *from here* is the failure mode.
    pub async fn forget_device(&self, device_id: &str) -> Result<bool> {
        self.storage.forget_pairing(device_id).await
    }

    /// Name a reader.
    ///
    /// The default is the mount's directory name, which is a fact about a
    /// filesystem — `KOBOeReader`, `Kindle` — and not about the object on the
    /// bedside table. Blank clears it; see [`Storage::set_device_label`].
    pub async fn rename_device(&self, device_id: &str, label: &str) -> Result<bool> {
        self.storage.set_device_label(device_id, label).await
    }

    /// Bring across everything one mounted reader has to offer.
    ///
    /// Scan, then sync every book the scan calls syncable, then stamp
    /// `last_synced_at` if the mount turns out to be a reader we know.
    ///
    /// **A verb of its own rather than an argument to [`Engine::sync_device`]**,
    /// for two reasons that are really one. `sync_device` takes sidecar *paths*
    /// and so cannot know whose they are — there is no device to stamp — and a
    /// caller that scanned, held the paths, and sent them back is holding a
    /// handle across a round trip that the volume may have changed underneath.
    /// This re-scans below the seam: the API's rule that handles do not cross
    /// it, applied to a filesystem.
    ///
    /// It does **not** import measured reading time: that is
    /// [`Engine::import_device_statistics`], for the reason that method's own
    /// doc gives.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn sync_mount(&self, mount: &Path) -> Result<device::MountSync> {
        let scan = self.scan_device(mount).await?;
        let paths: Vec<PathBuf> = scan.syncable().map(|b| b.path.clone()).collect();

        // The pairing is read before the sync so the copies it creates can
        // name their reader, but *stamped* only after it: a sync that failed
        // should not have stamped, and `?` below is what says so. Only a
        // device we hold a row for is named on a copy.
        let device_id = match plugin::inspect(mount) {
            Ok(status) => status.device_id,
            // Not a KOReader mount, a symlink, an unreadable `_meta.lua`. None
            // of them is a reason to fail a sync -- the import path has never
            // needed a pairing, and a library tree on a disk is a legitimate
            // argument to this method.
            Err(_) => None,
        };
        let known = match &device_id {
            Some(id) => self.storage.paired_device(id).await?.map(|d| d.device_id),
            None => None,
        };
        let reports = device::sync_device_for(&self.storage, &paths, known.as_deref()).await?;
        let device_id = match device_id {
            Some(id) if self.storage.stamp_device_sync(&id).await? => Some(id),
            // A `pairing.lua` we have no row for. The books came across; the
            // reader is somebody else's pairing and we record nothing about it.
            _ => None,
        };

        Ok(device::MountSync {
            mount: mount.to_path_buf(),
            device_id,
            found: scan.books.len(),
            synced: paths.len(),
            reports,
            warnings: scan.warnings,
        })
    }

    /// Import measured reading time from a mounted device's
    /// `statistics.sqlite3` into the activity log.
    ///
    /// **A verb of its own, and not part of [`Engine::sync_device`].**
    /// Arrival is read-only, and a device scan that
    /// silently began importing months of timing data would not be read-only in
    /// spirit even though every byte written is ours. The user asks for this by
    /// name.
    ///
    /// Absence is ordinary: a device on which the statistics plugin was never
    /// enabled returns an empty report carrying a `Diagnostic`, not an error.
    pub async fn import_device_statistics(&self, mount: &Path) -> Result<StatsImportReport> {
        ko_statistics::import_device_statistics(&self.storage, mount).await
    }

    /// Library books that look like this sidecar's book but not enough to link
    /// unasked.
    pub async fn sidecar_candidates(&self, sidecar: &Path) -> Result<Vec<MatchCandidate>> {
        let sc = koreader::parse_sidecar(&std::fs::read_to_string(sidecar)?)?;
        koreader::match_candidates(&self.storage, &sc).await
    }

    /// Record that this sidecar is that book, so it is never re-guessed.
    pub async fn link_sidecar(&self, sidecar: &Path, book_id: i64) -> Result<String> {
        let sc = koreader::parse_sidecar(&std::fs::read_to_string(sidecar)?)?;
        let Some(md5) = sc.partial_md5 else {
            return Err(EngineError::InvalidInput(format!(
                "{} has no partial_md5_checksum, so there is nothing to link it by",
                sidecar.display()
            )));
        };
        koreader::link_sidecar(&self.storage, &md5, book_id).await?;
        Ok(md5)
    }

    /// Fold one book into another, deleting `src`. Removes `src`'s cover file
    /// when `dst` kept its own — storage reports the orphan, the caller (this)
    /// owns the filesystem.
    pub async fn merge_books(&self, src: i64, dst: i64) -> Result<MergeReport> {
        let report = self.storage.merge_books(src, dst).await?;
        if let Some(cover) = &report.orphaned_cover {
            // Same resolution as `delete_book`: the stored path as written, and
            // the shelf tier derived from it. `merge_books` has already checked
            // that nothing else points at this file — two books sharing one
            // content-addressed jacket is ordinary.
            let cover = PathBuf::from(cover);
            std::fs::remove_file(images::thumb_path_of(&cover)).ok();
            std::fs::remove_file(cover).ok();
        }
        Ok(report)
    }

    // ---- notes -------------------------------------------------------------

    pub async fn create_note(&self, input: NewNoteInput) -> Result<CreatedNote> {
        let book = match input.book_id {
            Some(id) => self.storage.get_book(id).await?,
            None => None,
        };
        notes::create_note(&self.storage, &self.config.vault_dir, book.as_ref(), input).await
    }

    /// Notes, newest first. `limit` selects along `created_at`; `None` is every
    /// note and is for callers walking the whole graph rather than filling a
    /// viewport — see [`Storage::list_notes`].
    ///
    /// [`NoteScope`] is the narrowing, and [`NoteScope::Reading`] is applied in
    /// the statement, so the limit cuts a list that
    /// is already about the reading asked for.
    pub async fn list_notes(
        &self,
        scope: NoteScope,
        limit: Option<i64>,
    ) -> Result<Vec<NoteRecord>> {
        self.storage.list_notes(scope, limit).await
    }

    /// Everything the reader wrote or kept, matching one query, as **one**
    /// ranked list.
    ///
    /// This replaced `search_notes`, which answered half the question. Two
    /// lists a frontend interleaves is a relevance ordering invented above the
    /// seam — it cannot say a note outranks a highlight, so it interleaves by
    /// source order — so the ranking happens once, here.
    ///
    /// `source` narrows *which* indexes are asked and never how the answer is
    /// ordered; `None` asks both. `book_id` narrows which book the marks are
    /// about; `None` is the whole library. An empty query is no hits and no
    /// error. The ordering rule, and why it is deliberately not bm25 across the
    /// two indexes, is in [`crate::storage`]'s `fts` module header.
    ///
    /// **`book_id` is not a convenience**. `limit` cuts the global
    /// ranked list, so a frontend that searches the library and then keeps one
    /// book's hits gets nothing at all whenever the top `limit` marks live in
    /// other books — which, in a real library, is most queries.
    pub async fn search_marks(
        &self,
        query: &str,
        source: Option<SearchSource>,
        book_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<SearchHit>> {
        self.storage
            .search_marks(query, source, book_id, limit)
            .await
    }

    /// One note by id.
    pub async fn get_note(&self, note_id: i64) -> Result<Option<NoteRecord>> {
        self.storage.get_note(note_id).await
    }

    /// One older note to offer back on `today` — the caller's **local** date,
    /// since the engine has no clock zone of its own and "the same note all
    /// day" means the reader's day. See [`Storage::resurface_note`] for the
    /// rule; `None` is the ordinary answer for a young or empty notebook.
    pub async fn resurface_note(&self, today: time::Date) -> Result<Option<Resurfaced>> {
        self.storage.resurface_note(today).await
    }

    /// One older passage to offer back on `today`, the caller's **local**
    /// date — [`Self::resurface_note`]'s twin and rule; see
    /// [`Storage::resurface_passage`].
    pub async fn resurface_passage(&self, today: time::Date) -> Result<Option<ResurfacedPassage>> {
        self.storage.resurface_passage(today).await
    }

    /// This reading's note of that kind, if it has one. `kind` is
    /// [`NoteKind::as_str`]; the pair `idx_one_reflection`/`idx_one_review`
    /// makes "one of each per reading" an invariant, so this is at most one row.
    pub async fn note_for_reading(
        &self,
        reading_id: i64,
        kind: &str,
    ) -> Result<Option<NoteRecord>> {
        self.storage.note_for_reading(reading_id, kind).await
    }

    /// Where a note's markdown lives. Derived from the row against the vault
    /// root — `NoteRecord::file_path` is relative, and every caller that joined
    /// it by hand had to reach `config.vault_dir` to do so.
    pub fn note_path(&self, note: &NoteRecord) -> PathBuf {
        self.config.vault_dir.join(&note.file_path)
    }

    /// What this note links **out** to: each `[[wikilink]]` its body wrote, and
    /// the note it resolves to when one exists.
    ///
    /// A target with no note is kept as text rather than dropped — a forward
    /// reference to something not written yet is how a zettelkasten is built,
    /// and the edge resolves itself the moment that note appears.
    pub async fn outgoing_links(&self, note_id: i64) -> Result<Vec<OutgoingLink>> {
        self.storage.outgoing_links(note_id).await
    }

    /// What links **in** to this note.
    ///
    /// The facade had no link method at all before this pair: edges were
    /// written by `create_note` / `update_note_body` and read only inside
    /// `open_anchored`, so the graph could be built but never walked.
    pub async fn backlinks(&self, note_id: i64) -> Result<Vec<NoteRecord>> {
        self.storage.backlinks(note_id).await
    }

    /// Every link of a note, both directions, with the other note's book and —
    /// for a link in — the paragraph that holds it (proposal 8, model A).
    ///
    /// Incoming first, newest-kept source first ([`Storage::incoming_links`]);
    /// then outgoing in body order, forward references included. A self-link
    /// appears in neither half. Each book is fetched once however many notes
    /// share it, which is the per-book lookup this replaces in a frontend.
    ///
    /// The paragraphs are the reader's own writing: nothing here logs them,
    /// and nothing may above `trace!`. An unreadable source file degrades to
    /// `paragraph: None` rather than failing the whole answer — the edge is
    /// still true, only its sentence is missing.
    pub async fn link_contexts(&self, note_id: i64) -> Result<Vec<LinkContext>> {
        let mut books: std::collections::HashMap<i64, Option<Book>> =
            std::collections::HashMap::new();
        let mut out = Vec::new();

        for (target_title, source) in self.storage.incoming_links(note_id).await? {
            let paragraph = match self.note_body(&source) {
                Ok(body) => notes::link_paragraph(&body, &target_title),
                Err(e) => {
                    tracing::debug!(note_id = source.id, error = %e, "link source body unreadable");
                    None
                }
            };
            let book = self.book_face(&mut books, source.book_id).await?;
            out.push(LinkContext {
                direction: LinkDirection::Incoming,
                target_title,
                note: Some(source),
                book,
                paragraph,
            });
        }

        for link in self.storage.outgoing_links(note_id).await? {
            if link.to.as_ref().is_some_and(|n| n.id == note_id) {
                continue;
            }
            let book_id = link.to.as_ref().and_then(|n| n.book_id);
            let book = self.book_face(&mut books, book_id).await?;
            out.push(LinkContext {
                direction: LinkDirection::Outgoing,
                target_title: link.target_title,
                note: link.to,
                book,
                paragraph: None,
            });
        }
        Ok(out)
    }

    async fn book_face(
        &self,
        cache: &mut std::collections::HashMap<i64, Option<Book>>,
        book_id: Option<i64>,
    ) -> Result<Option<Book>> {
        let Some(id) = book_id else { return Ok(None) };
        if let Some(b) = cache.get(&id) {
            return Ok(b.clone());
        }
        let b = self.storage.get_book(id).await?;
        cache.insert(id, b.clone());
        Ok(b)
    }

    /// The body text of a note (its markdown minus the frontmatter header).
    pub fn note_body(&self, note: &NoteRecord) -> Result<String> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (_, body) = notes::frontmatter_and_body(&content);
        Ok(body.trim_end().to_string())
    }

    /// Replace a note's body, preserving its frontmatter header, and reindex it
    /// — FTS **and** its wikilink edges. Used by the in-house editor.
    ///
    /// Re-indexing the edges is what makes a reflection the hub it is meant to
    /// be: it is opened empty and written afterwards, so edges computed only at
    /// creation would leave it with none.
    pub async fn update_note_body(&self, note: &NoteRecord, body: &str) -> Result<()> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (header, _) = notes::frontmatter_and_body(&content);
        std::fs::write(&file, format!("{header}{}\n", body.trim_end()))?;
        self.storage
            .refresh_note_body(note.id, &note.title, body)
            .await?;
        let links = notes::extract_wikilinks(body);
        self.storage
            .set_note_links(note.id, &note.title, &links)
            .await
    }

    /// Delete a note: remove its markdown file from the vault, then its DB row
    /// and FTS entry. A missing file is not an error (the DB row still goes).
    pub async fn delete_note(&self, note: &NoteRecord) -> Result<()> {
        let file = self.note_path(note);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.storage.delete_note(note.id).await
    }

    /// Re-read one note file from disk and bring its index in line with it —
    /// FTS body **and** wikilink edges, since an outside edit is exactly where
    /// a new `[[wikilink]]` appears.
    ///
    /// The single-note form of what [`Engine::reconcile_vault`] does to all of
    /// them, and worth keeping on the facade and on the wire beside it: a
    /// frontend that knows which note it just handed to `$EDITOR` should not
    /// have to sweep a whole vault to learn what it already knows.
    pub async fn refresh_note_from_disk(&self, note: &NoteRecord) -> Result<()> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (_, body) = notes::frontmatter_and_body(&content);
        notes::reindex_from_body(&self.storage, note.id, &note.title, body).await?;
        Ok(())
    }

    // ---- vault coherence ---------------------------------------------------

    /// Follow the vault, so a note edited in Obsidian is a note search can
    /// still find.
    ///
    /// A [`VaultWatcher`] does nothing until it is polled and spawns nothing
    /// when it is: the caller drives it from its own loop, and the re-index
    /// happens on the caller's task. Fails when the platform cannot watch,
    /// which is a thing to degrade around rather than abort on — the vault
    /// still works, and [`Engine::reconcile_vault`] is the whole answer for a
    /// machine that cannot watch at all.
    pub fn watch_vault(&self) -> Result<VaultWatcher> {
        watch::watch_vault(&self.config.vault_dir, self.storage.clone())
    }

    /// Bring every note's index in line with its file, once.
    ///
    /// **The half a watcher structurally cannot do.** A watcher sees only the
    /// present, and the ordinary case is a note edited in another program while
    /// bookmole was not running. Cheap on the common path — a `stat` per
    /// note, and a read only for the ones whose file is newer than their index.
    ///
    /// A note whose file is missing is left exactly as it is. Absence is not a
    /// deletion here, for the reasons [`VaultWatcher`] sets out.
    pub async fn reconcile_vault(&self) -> Result<VaultReconcile> {
        notes::reconcile_vault(&self.storage, &self.config.vault_dir).await
    }

    // ---- reflection + review -----------------------------------------------

    /// Open this reading's reflection, creating it on the first call and
    /// returning the same note ever after. Private, and the hub of the graph.
    ///
    /// `reading_id: None` means the book's current reading — and opens one when
    /// the book has none, because a reflection is written *mid-book* and that is
    /// the normal case, not an edge one.
    pub async fn open_reflection(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<CreatedNote> {
        self.open_anchored(book_id, reading_id, NoteKind::Reflection)
            .await
    }

    /// Open this reading's review: public prose, and the only note kind that
    /// carries a rating.
    ///
    /// **Never derived from the reflection.** A review is a rewrite for a
    /// different audience, not a subset of private thinking — no `public:`
    /// frontmatter key, no divider, no shared body.
    pub async fn open_review(&self, book_id: i64, reading_id: Option<i64>) -> Result<CreatedNote> {
        self.open_anchored(book_id, reading_id, NoteKind::Review)
            .await
    }

    /// [`Engine::open_reflection`], as the [`NoteRecord`] an editor needs.
    ///
    /// `CreatedNote` is what the *creating* caller wants — a path to hand to
    /// `$EDITOR` and the links it started with. Everything that then edits the
    /// note wants a `NoteRecord` instead: `note_body`, `update_note_body`,
    /// `delete_note` and the TUI's `TextEditor` all take one. The CLI already
    /// patches over the gap with a follow-up `storage.get_note(note.id)`, and
    /// the home screen's whole action is "open the reflection", so it meets the
    /// same gap on its first keypress.
    ///
    /// A wrapper rather than a change of return type: `open_reflection` is what
    /// the CLI calls, and widening its signature is a diff in files this has no
    /// other reason to touch. Both go through the same `open_anchored`, so
    /// there is one definition of which note this is.
    pub async fn open_reflection_record(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<NoteRecord> {
        self.open_anchored_record(book_id, reading_id, NoteKind::Reflection)
            .await
    }

    /// [`Engine::open_review`], as a [`NoteRecord`]. The twin of
    /// [`Engine::open_reflection_record`], and it exists for the same reason.
    pub async fn open_review_record(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<NoteRecord> {
        self.open_anchored_record(book_id, reading_id, NoteKind::Review)
            .await
    }

    /// Open the note, then read the row back.
    ///
    /// `NotFound` rather than an `expect`: the row was written or found a
    /// statement ago, so its absence means the database changed underneath us,
    /// and the engine is a library — it must not take a frontend down over it.
    async fn open_anchored_record(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
        kind: NoteKind,
    ) -> Result<NoteRecord> {
        let note = self.open_anchored(book_id, reading_id, kind).await?;
        self.storage
            .get_note(note.id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("note id {}", note.id)))
    }

    /// The one implementation behind both. They differ in the `kind` they
    /// carry and in nothing else — which is the point of reusing `notes`
    /// instead of building a parallel vault.
    async fn open_anchored(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
        kind: NoteKind,
    ) -> Result<CreatedNote> {
        let book = self
            .storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;

        let reading_id = match reading_id {
            Some(id) => {
                let reading = self
                    .storage
                    .get_reading(id)
                    .await?
                    .ok_or_else(|| EngineError::NotFound(format!("reading id {id}")))?;
                if reading.book_id != book_id {
                    return Err(EngineError::InvalidInput(format!(
                        "reading {id} belongs to book {}, not {book_id}",
                        reading.book_id
                    )));
                }
                id
            }
            // `ensure_reading`, not `open_reading`: the current reading is the
            // one being reflected on, open or not, and a second reading only
            // ever starts because the user said so (`progress --reread`).
            None => {
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                self.storage
                    .ensure_reading(book_id, Some(now), "manual")
                    .await?
            }
        };

        // Accretion is exactly this: the second call finds the first call's
        // note. The partial unique indexes are what make "the" reflection of a
        // reading a fact rather than a hope.
        if let Some(existing) = self
            .storage
            .note_for_reading(reading_id, kind.as_str())
            .await?
        {
            let links = self
                .storage
                .note_links(existing.id)
                .await?
                .into_iter()
                .map(|(target, _)| target)
                .collect();
            return Ok(CreatedNote {
                id: existing.id,
                title: existing.title,
                file: self.config.vault_dir.join(&existing.file_path),
                links,
            });
        }

        let readings = self.storage.list_readings(book_id).await?;
        let nth = readings
            .iter()
            .position(|r| r.id == reading_id)
            .map(|i| i + 1);
        let label = match kind {
            NoteKind::Review => "Review",
            _ => "Reflection",
        };
        // The title is a wikilink target, so a reread's pair must not collide
        // with the first reading's.
        let title = match nth {
            Some(n) if n > 1 => format!("{label}: {} ({n})", book.display_title()),
            _ => format!("{label}: {}", book.display_title()),
        };

        notes::create_note(
            &self.storage,
            &self.config.vault_dir,
            Some(&book),
            NewNoteInput {
                book_id: Some(book_id),
                reading_id: Some(reading_id),
                kind,
                title: Some(title),
                body: String::new(),
                ..Default::default()
            },
        )
        .await
    }

    /// Rate a review on the active scale.
    ///
    /// The **raw value and the scale id** are stored, never only what it maps
    /// to: the Goodreads map is user-editable, so the mapping has to stay
    /// re-derivable from what the user actually said.
    pub async fn set_rating(&self, note_id: i64, value: f64) -> Result<()> {
        let note = self
            .storage
            .get_note(note_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("note id {note_id}")))?;
        // A reflection that wants a private score can have one later; it is not
        // the same number and must not share this column.
        if note.kind != NoteKind::Review.as_str() {
            return Err(EngineError::InvalidInput(format!(
                "note {note_id} is a {}; a rating belongs to a review",
                note.kind
            )));
        }
        let scale = self.storage.active_rating_scale().await?.ok_or_else(|| {
            EngineError::InvalidInput(
                "no rating scale defined — `bookmole rating scale --min --max --step`".into(),
            )
        })?;
        self.storage.set_review_rating(note_id, &scale, value).await
    }

    /// This review's rating on Goodreads' 0–5 in quarters, `None` when it has
    /// no rating at all.
    ///
    /// A rating the user has never mapped is [`EngineError::UnmappedRating`],
    /// not a rounded one: the seeded scales map one-to-one (migration `0026`),
    /// but a scale the user made may have points Goodreads cannot hold, and
    /// quietly picking one would put a number the user never chose on a public
    /// shelf.
    pub async fn goodreads_rating(&self, note_id: i64) -> Result<Option<f64>> {
        let Some(rating) = self.storage.review_rating(note_id).await? else {
            return Ok(None);
        };
        match self
            .storage
            .goodreads_for(&rating.scale, rating.value)
            .await?
        {
            Some(g) => Ok(Some(g)),
            None => Err(EngineError::UnmappedRating {
                value: rating.value,
                scale: rating.scale.name,
            }),
        }
    }

    /// This review's rating, raw value and scale both — never only the value,
    /// which without its scale means nothing.
    pub async fn review_rating(&self, note_id: i64) -> Result<Option<Rating>> {
        self.storage.review_rating(note_id).await
    }

    /// Unrate a review. False when it had no rating, so a repeat call is a
    /// no-op rather than an error.
    pub async fn clear_review_rating(&self, note_id: i64) -> Result<bool> {
        self.storage.clear_review_rating(note_id).await
    }

    // ---- rating scales -----------------------------------------------------
    //
    // Scale administration goes through the facade like everything else: the
    // CLI's `rating scale|map|show` used to be written entirely against
    // `engine.storage`, which is exactly the reach past the seam the private
    // field exists to prevent.

    /// Define a scale, or redefine an existing one by name. A **new** scale
    /// becomes the default; redefining one leaves the flag alone.
    pub async fn put_rating_scale(
        &self,
        name: &str,
        min: f64,
        max: f64,
        step: f64,
    ) -> Result<RatingScale> {
        self.storage.put_rating_scale(name, min, max, step).await
    }

    /// Every scale the library knows.
    pub async fn rating_scales(&self) -> Result<Vec<RatingScale>> {
        self.storage.list_rating_scales().await
    }

    /// The scale a new rating is given on — the one flagged `is_default`, with
    /// deliberately no ordering fallback.
    pub async fn active_rating_scale(&self) -> Result<Option<RatingScale>> {
        self.storage.active_rating_scale().await
    }

    pub async fn rating_scale_by_name(&self, name: &str) -> Result<Option<RatingScale>> {
        self.storage.rating_scale_by_name(name).await
    }

    /// What this scale's values mean on Goodreads' 0–5 in quarters. An
    /// **explicit lookup table**; a point with no entry is reported, never
    /// rounded.
    pub async fn rating_map(&self, scale_id: i64) -> Result<Vec<(f64, f64)>> {
        self.storage.rating_map(scale_id).await
    }

    /// Add one entry to that table.
    pub async fn map_rating(&self, scale: &RatingScale, value: f64, goodreads: f64) -> Result<()> {
        self.storage.map_rating(scale, value, goodreads).await
    }

    // ---- citations ---------------------------------------------------------

    /// Cite a highlight from a note — by reference, so the citation stays live
    /// when a device refresh rewrites the highlight's device-owned fields.
    pub async fn cite(&self, note_id: i64, highlight_id: i64) -> Result<()> {
        self.storage.add_citation(note_id, highlight_id).await?;
        Ok(())
    }

    /// Drop a citation. False when it was not there.
    pub async fn uncite(&self, note_id: i64, highlight_id: i64) -> Result<bool> {
        self.storage.remove_citation(note_id, highlight_id).await
    }

    pub async fn citations_for(&self, note_id: i64) -> Result<Vec<Highlight>> {
        self.storage.citations_for(note_id).await
    }

    /// Which passages each of these notes cites, in one call.
    ///
    /// The question "is this highlight already quoted somewhere?" is asked
    /// about a *page* of notes, and [`Engine::citations_for`] answers it one
    /// note at a time — an N+1 a frontend should never have to build. This is
    /// the call that makes a *quoted* mark on each highlight affordable.
    ///
    /// One entry per requested id, in the order asked, empties included; and it
    /// carries **ids rather than rows**, because the surface asking already
    /// holds the highlights and does not need the reader's text sent back once
    /// per citing note. The two calls cannot disagree about order — they share
    /// one `ORDER BY`.
    pub async fn citations_for_notes(&self, note_ids: &[i64]) -> Result<Vec<NoteCitations>> {
        self.storage.citations_for_notes(note_ids).await
    }

    // ---- goodreads ---------------------------------------------------------

    /// Import a Goodreads CSV export. `dry_run` reports what would change and
    /// writes nothing.
    ///
    /// Lossless because three things already exist for it to land on: the
    /// title-and-author matcher (so a row finds the book it already is),
    /// `readings` (so `Read Count` is history rather than a flag), and reviews
    /// with ratings (so `My Review` and `My Rating` have somewhere true to land).
    #[tracing::instrument(skip(self), fields(path = %path.display(), dry_run = opts.dry_run))]
    pub async fn import_goodreads(
        &self,
        path: &Path,
        opts: goodreads::ImportOptions,
    ) -> Result<GoodreadsReport> {
        goodreads::import(self, path, opts).await
    }

    /// Build a Goodreads-importable CSV of the library, plus every honest
    /// failure along the way. Same shape as [`Engine::export_flashcards`]:
    /// the payload comes back, the caller owns the file.
    pub async fn export_goodreads(&self) -> Result<(String, Vec<Diagnostic>)> {
        goodreads::export(self).await
    }

    /// Record that a Goodreads row is that book, so it is never re-guessed.
    ///
    /// The [`Engine::link_sidecar`] of the CSV importer, and it exists for the
    /// same reason: an unmatched row has to be a decision rather than a dead
    /// end, and until now the only escape hatch was `create_ambiguous` followed
    /// by [`Engine::merge_books`] — which creates a duplicate on purpose in
    /// order to fold it back in, and leaves the far side's id pointing at
    /// whichever of the two the merge happened to delete.
    ///
    /// Takes Goodreads' own `Book Id` because that is what
    /// [`UnmatchedRow::external_id`](goodreads::UnmatchedRow) carries and what
    /// the `ExternalId` rung of the matcher reads. A row without one cannot be
    /// linked — there is nothing durable to link *by*, the CSV having no other
    /// stable key — and says so rather than linking by title.
    pub async fn link_goodreads_row(&self, external_id: &str, book_id: i64) -> Result<()> {
        self.link_foreign_record(goodreads::SOURCE, external_id, book_id)
            .await
    }

    // ---- calibre -----------------------------------------------------------

    /// Which calibre tools this machine has. Feature detection, resolved once
    /// at [`Engine::open`] — a frontend asks this to decide whether the feature
    /// exists at all, and must never be told to install anything.
    pub fn calibre(&self) -> &Calibre {
        &self.calibre
    }

    /// Tier (i): convert a book between formats through `ebook-convert`.
    /// Calibre reads both formats off the two extensions.
    ///
    /// Refuses to overwrite unless told: the output path is typed by hand, and
    /// losing a file is the one outcome with no undo.
    #[tracing::instrument(skip(self), fields(input = %input.display(), output = %output.display()))]
    pub async fn convert_ebook(
        &self,
        input: &Path,
        output: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        calibre::convert(&self.calibre, input, output, overwrite).await
    }

    /// Tier (ii): what a calibre library holds, read-only.
    pub async fn calibre_library(&self, library: Option<&Path>) -> Result<Vec<CalibreBook>> {
        calibre::list_library(&self.calibre, library).await
    }

    /// Tier (ii): import a calibre library. `dry_run` reports what would change
    /// and writes nothing.
    ///
    /// The onboarding win: a library the user has already curated, with
    /// covers, ISBNs and tags, without typing one ISBN.
    #[tracing::instrument(skip(self), fields(dry_run = opts.dry_run))]
    pub async fn import_calibre_library(
        &self,
        opts: &calibre::ImportOptions,
    ) -> Result<CalibreReport> {
        calibre::import(self, opts).await
    }

    /// Record that a calibre book is that book of ours. The calibre twin of
    /// [`Engine::link_goodreads_row`].
    ///
    /// Keyed on calibre's **uuid, never its `id`**, for the reason
    /// `external_ids` is: calibre ids are per-library and reused after a delete,
    /// and this table has no library column to tell two libraries' id 4 apart.
    /// A row calibre gave no uuid therefore cannot be linked, which is the same
    /// row `CalibreRowNotIdentified` already warns about.
    pub async fn link_calibre_book(&self, uuid: &str, book_id: i64) -> Result<()> {
        self.link_foreign_record(calibre::SOURCE, uuid, book_id)
            .await
    }

    /// The one implementation behind the two link methods above.
    ///
    /// Shared so the two cannot drift, and separate from
    /// [`Storage::link_external_id`] because that is a bare upsert: `book_id`
    /// references `books(id)`, so an id that does not exist comes back as a raw
    /// foreign-key error naming a constraint. A frontend offering a candidate
    /// list can pass a book deleted in another pane, and every caller branches
    /// on the answer, so it gets [`EngineError::NotFound`] instead.
    async fn link_foreign_record(
        &self,
        source: &str,
        external_id: &str,
        book_id: i64,
    ) -> Result<()> {
        if external_id.trim().is_empty() {
            return Err(EngineError::InvalidInput(format!(
                "that {source} record has no id, so there is nothing to link it by"
            )));
        }
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book {book_id}")));
        }
        self.storage
            .link_external_id(source, external_id, book_id)
            .await
    }

    // ---- flashcards --------------------------------------------------------

    /// Capture a flashcard from a book, optionally anchored to the passage the
    /// word came from.
    ///
    /// The one way a frontend mints a card; the other production caller of
    /// [`Storage::insert_flashcard`] is the KOReader import's auto-capture of
    /// single-word highlights.
    ///
    /// Returns whether a card was created. `false` is *you already had this
    /// one*, not a failure: `UNIQUE(book_id, word)` dedupes and the existing
    /// card is left untouched, so the two answers are different facts and a
    /// caller drawing a confirmation has to be able to tell them apart.
    ///
    /// **The pair is checked, not trusted.** `book_id` and `highlight_id` are
    /// two handles a client supplies independently, and nothing in the schema
    /// stops them naming different books — after which the card sits, for ever,
    /// beside a passage from somewhere else. A write path takes ids and
    /// re-reads them below the seam rather than trusting a client's pairing; `link_foreign_record`
    /// is the precedent, down to the reason a raw foreign-key error is not an
    /// acceptable answer to a frontend offering a list that another pane may
    /// have deleted from under it.
    pub async fn create_flashcard(
        &self,
        book_id: i64,
        highlight_id: Option<i64>,
        word: &str,
        context: Option<&str>,
    ) -> Result<bool> {
        // Trimmed rather than taken as given: `UNIQUE(book_id, word)` is the
        // dedup, and " mot" and "mot" are one word wearing two spellings.
        let word = word.trim();
        if word.is_empty() {
            return Err(EngineError::InvalidInput(
                "a flashcard needs a word".to_string(),
            ));
        }
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book {book_id}")));
        }
        if let Some(hid) = highlight_id {
            match self.storage.highlight_book(hid).await? {
                None => return Err(EngineError::NotFound(format!("highlight {hid}"))),
                Some(owner) if owner != book_id => {
                    return Err(EngineError::InvalidInput(format!(
                        "highlight {hid} belongs to book {owner}, not book {book_id}"
                    )));
                }
                Some(_) => {}
            }
        }
        self.storage
            .insert_flashcard(book_id, highlight_id, word, context)
            .await
    }

    pub async fn list_flashcards(&self, include_exported: bool) -> Result<Vec<FlashcardRow>> {
        self.storage.list_flashcards(include_exported).await
    }

    pub async fn list_flashcards_for_book(&self, book_id: i64) -> Result<Vec<FlashcardRow>> {
        self.storage.list_flashcards_for_book(book_id).await
    }

    /// Build the Anki TSV and mark the exported cards. Returns (tsv, count).
    pub async fn export_flashcards(&self, include_exported: bool) -> Result<(String, usize)> {
        let cards = self.storage.list_flashcards(include_exported).await?;
        let tsv = flashcards::export_tsv(&cards);
        let ids: Vec<i64> = cards.iter().map(|c| c.id).collect();
        self.storage.mark_flashcards_exported(&ids).await?;
        Ok((tsv, ids.len()))
    }

    /// Anki's text-import format, with a stable GUID per card so a re-import
    /// updates notes rather than duplicating them.
    ///
    /// `book_id: None` is the whole library; `Some` is one book, and a book
    /// that is not there is `NotFound` rather than an empty file. The
    /// `exported` bookkeeping is exactly [`Engine::export_flashcards`]'s: what
    /// went out is marked, so `include_exported: false` keeps meaning *only the
    /// cards made since the last file*. Because the GUID is stable, sending a
    /// card again with `include_exported: true` is harmless — Anki updates it.
    pub async fn export_anki(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> Result<AnkiExport> {
        let (export, ids) = self.build_anki(book_id, include_exported).await?;
        // The caller owns the text from here, so handing it over is what
        // "went out" means.
        self.storage.mark_flashcards_exported(&ids).await?;
        Ok(export)
    }

    /// [`Engine::export_anki`] without the bookkeeping: the file's text and
    /// the ids of the cards in it, **none of them marked**. Marking is the
    /// caller's, after the cards have actually gone somewhere.
    async fn build_anki(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> Result<(AnkiExport, Vec<i64>)> {
        let (cards, scope) = match book_id {
            None => (self.storage.list_flashcards(include_exported).await?, None),
            Some(id) => {
                let book = self
                    .storage
                    .get_book(id)
                    .await?
                    .ok_or_else(|| EngineError::NotFound(format!("book {id}")))?;
                let cards = self
                    .storage
                    .list_flashcards_for_book(id)
                    .await?
                    .into_iter()
                    .filter(|c| include_exported || !c.exported)
                    .collect();
                (cards, book.title)
            }
        };

        // Authors once per book, not once per card.
        let mut authors: std::collections::HashMap<i64, Vec<String>> =
            std::collections::HashMap::new();
        for c in &cards {
            if let std::collections::hash_map::Entry::Vacant(slot) = authors.entry(c.book_id) {
                let names = self
                    .storage
                    .get_book(c.book_id)
                    .await?
                    .map(|b| b.authors_in_display_order())
                    .unwrap_or_default();
                slot.insert(names);
            }
        }
        let notes: Vec<flashcards::AnkiCard<'_>> = cards
            .iter()
            .map(|card| flashcards::AnkiCard {
                card,
                authors: authors.get(&card.book_id).map_or(&[][..], Vec::as_slice),
            })
            .collect();
        let text = flashcards::export_anki(&notes);

        let now = time::OffsetDateTime::now_utc();
        let stamp = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            now.year(),
            u8::from(now.month()),
            now.day(),
            now.hour(),
            now.minute(),
            now.second()
        );
        let filename = flashcards::anki_filename(scope.as_deref(), &stamp);

        let ids: Vec<i64> = cards.iter().map(|c| c.id).collect();
        Ok((
            AnkiExport {
                text,
                count: ids.len(),
                filename,
            },
            ids,
        ))
    }

    /// Where [`Engine::save_anki_export`] writes: [`EngineConfig::exports_dir`].
    pub fn exports_dir(&self) -> &Path {
        &self.config.exports_dir
    }

    /// [`Engine::export_anki`], written to [`Engine::exports_dir`] by the
    /// engine — for a frontend that has no way to write a file of its own (the
    /// GUI client without a save dialog). Nothing is written when no
    /// card went out, so the folder never fills with header-only files.
    ///
    /// The cards are marked only **after** the write lands: a write that fails
    /// leaves them unmarked, so the next *only new* file still carries them.
    pub async fn save_anki_export(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> Result<SavedAnkiExport> {
        let (export, ids) = self.build_anki(book_id, include_exported).await?;
        if export.count == 0 {
            return Ok(SavedAnkiExport {
                path: None,
                count: 0,
            });
        }
        let dir = self.exports_dir();
        std::fs::create_dir_all(dir)?;
        // The name carries a seconds stamp; a second export in the same second
        // gets a suffix rather than overwriting the first (whose cards are now
        // marked and would not come out again).
        let mut path = dir.join(&export.filename);
        let mut n = 2;
        while path.exists() {
            let stem = export.filename.trim_end_matches(".txt");
            path = dir.join(format!("{stem}-{n}.txt"));
            n += 1;
        }
        std::fs::write(&path, &export.text)?;
        self.storage.mark_flashcards_exported(&ids).await?;
        Ok(SavedAnkiExport {
            path: Some(path),
            count: export.count,
        })
    }

    // ---- lists (migration 0028; rules in storage/lists.rs) ------------------

    pub async fn list_lists(&self, sort: ListSort, preview: usize) -> Result<Vec<ListSummary>> {
        self.storage.list_summaries(sort, preview).await
    }

    pub async fn get_list(&self, id: i64) -> Result<BookList> {
        self.storage.get_list(id).await
    }

    pub async fn create_list(&self, name: &str, description: Option<&str>) -> Result<BookList> {
        self.storage.create_list(name, description).await
    }

    pub async fn update_list(
        &self,
        id: i64,
        name: &str,
        description: Option<&str>,
    ) -> Result<BookList> {
        self.storage.update_list(id, name, description).await
    }

    pub async fn delete_list(&self, id: i64) -> Result<()> {
        self.storage.delete_list(id).await
    }

    pub async fn list_entries(
        &self,
        list_id: i64,
        show: EntryShow,
        sort: EntrySort,
    ) -> Result<Vec<ListEntry>> {
        self.storage.list_entries(list_id, show, sort).await
    }

    pub async fn add_to_list(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> Result<ListEntry> {
        self.storage.add_to_list(list_id, book_id, note).await
    }

    /// **Lists stage B**: put a search result on a list without owning it.
    ///
    /// The candidate is first run through the matching every importer uses, so
    /// a book already here is reused and never duplicated: (1) its ISBNs, the
    /// way [`Engine::save_book`] decides create-vs-update, then (2) the title
    /// and authors through `koreader::scores_for`, the one fuzzy matcher
    /// (`matching.rs`; "no second matcher"), taking only a match it may link
    /// without asking. No match creates the book from the record with **no
    /// copy**, through `save_book` (provenance exactly as a `SaveBook`), so it
    /// is on the list and not in the library. Idempotent, as
    /// [`Engine::add_to_list`].
    pub async fn add_candidate_to_list(
        &self,
        list_id: i64,
        candidate: &Book,
        note: Option<&str>,
    ) -> Result<ListEntry> {
        // A list that is not there is `NotFound` before a book is created for it.
        self.storage.get_list(list_id).await?;
        let mut found = None;
        for raw in [&candidate.isbn_13, &candidate.isbn_10]
            .into_iter()
            .flatten()
        {
            let isbn = normalize_isbn(raw).unwrap_or_else(|| raw.clone());
            if let Some(b) = self.storage.find_book_by_isbn(&isbn).await? {
                found = b.id;
                break;
            }
        }
        if found.is_none() {
            let query = matching::Query::new(candidate.title.as_deref(), &candidate.authors);
            found = koreader::scores_for(&self.storage, &query)
                .await?
                .into_iter()
                .next()
                .filter(|s| s.can_auto)
                .and_then(|s| s.book.id);
        }
        let book_id = match found {
            Some(id) => id,
            None => {
                let mut fresh = candidate.clone();
                fresh.id = None;
                self.save_book(&fresh)
                    .await?
                    .id
                    .ok_or_else(|| EngineError::Other("a stored book has an id".into()))?
            }
        };
        self.storage.add_to_list(list_id, book_id, note).await
    }

    pub async fn remove_from_list(&self, list_id: i64, book_id: i64) -> Result<()> {
        self.storage.remove_from_list(list_id, book_id).await
    }

    pub async fn move_in_list(&self, list_id: i64, book_id: i64, to: usize) -> Result<()> {
        self.storage.move_in_list(list_id, book_id, to).await
    }

    pub async fn set_list_note(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> Result<()> {
        self.storage.set_list_note(list_id, book_id, note).await
    }

    pub async fn lists_for_book(&self, book_id: i64) -> Result<Vec<ListMembership>> {
        self.storage.lists_for_book(book_id).await
    }

    pub async fn list_timeline(&self, list_id: i64) -> Result<ListTimeline> {
        self.storage.list_timeline(list_id).await
    }

    pub async fn shelf_suggestions(&self, preview: usize) -> Result<Vec<ShelfSuggestion>> {
        self.storage.shelf_suggestions(preview).await
    }

    pub async fn keep_shelf(&self, source: &str, tag: &str) -> Result<BookList> {
        self.storage.keep_shelf(source, tag).await
    }

    pub async fn dismiss_shelf(&self, source: &str, tag: &str) -> Result<()> {
        self.storage.dismiss_shelf(source, tag).await
    }

    // ---- copies (migration 0027) ---------------------------------------------
    //
    // A copy is one way the reader has (or had) a book: its form (a file, in
    // print, in an app), its tenure (own, borrowed, rented, subscription) and
    // the places it is found. A book is **in the library** when it has at
    // least one copy not explicitly ended (`have_now` is not `false`), or any
    // copy at all and has been touched. Only the reader ever changes
    // `have_now`; nothing about copies deletes a book.

    /// Record a copy of a book.
    pub async fn add_copy(&self, new: &NewCopy) -> Result<Copy> {
        self.storage.create_copy(new).await
    }

    /// One copy with its places.
    pub async fn get_copy(&self, id: i64) -> Result<Option<Copy>> {
        self.storage.get_copy(id).await
    }

    /// Every copy of a book, oldest first.
    pub async fn copies_for_book(&self, book_id: i64) -> Result<Vec<Copy>> {
        self.storage.copies_for_book(book_id).await
    }

    /// Write a copy's user-editable fields. The only way `have_now` changes.
    pub async fn update_copy(&self, copy: &Copy) -> Result<bool> {
        self.storage.update_copy(copy).await
    }

    /// Delete a copy: an explicit user act.
    pub async fn delete_copy(&self, id: i64) -> Result<bool> {
        self.storage.delete_copy(id).await
    }

    /// The copy holding this place, if any.
    pub async fn copy_for_place(&self, kind: PlaceKind, reference: &str) -> Result<Option<Copy>> {
        self.storage.copy_for_place(kind, reference).await
    }

    /// Attach a place to a copy, merging a same-book copy that already held it.
    pub async fn attach_place(&self, copy_id: i64, place: &NewPlace) -> Result<PlaceAttach> {
        self.storage.attach_place(copy_id, place).await
    }

    /// For a frontend with no "how do you have it" step (the CLI and the TUI):
    /// a book it just **created** must not vanish from the shelf, so record one
    /// `unrecorded` copy (`learned_from = user`, `have_now` unsaid, `form`
    /// unsaid). Does nothing, returning `None`, when the book already has any
    /// copy. Call it only on a save that created the book.
    pub async fn ensure_unrecorded_copy(&self, book_id: i64) -> Result<Option<Copy>> {
        if !self.storage.copies_for_book(book_id).await?.is_empty() {
            return Ok(None);
        }
        let new = NewCopy::new(book_id, None, Tenure::Unrecorded, LearnedFrom::User);
        self.storage.create_copy(&new).await.map(Some)
    }

    /// [`Engine::save_book`] for a frontend with no "how do you have it" step
    /// (the CLI and the TUI): when the save **created** the book (nothing with
    /// its ISBN was there before), record an `unrecorded` copy so it stays on
    /// the shelf. An update to an existing book adds nothing.
    pub async fn save_book_unrecorded(&self, book: &Book) -> Result<Book> {
        let mut existed = false;
        for isbn in [&book.isbn_13, &book.isbn_10].into_iter().flatten() {
            existed |= self.storage.find_book_by_isbn(isbn).await?.is_some();
        }
        let saved = self.save_book(book).await?;
        if !existed && let Some(id) = saved.id {
            self.ensure_unrecorded_copy(id).await?;
        }
        Ok(saved)
    }

    /// **Keep a copy here**: store this copy's file in `database/files/` and
    /// attach a `store` place to it. An explicit act; import never does it.
    pub async fn keep_copy_here(&self, copy_id: i64) -> Result<KeptCopy> {
        files::keep_copy_here(&self.storage, &self.config.files_dir, copy_id).await
    }

    /// The derived facts about a copy that a frontend must not guess: its
    /// format, whether *Keep a copy here* would work, and the name of the
    /// paired reader each `reader` place sits on.
    pub async fn copy_facts(&self, copy: &Copy) -> Result<CopyFacts> {
        let has = |k: PlaceKind| copy.places.iter().any(|p| p.kind == k);
        let can_keep_here = !has(PlaceKind::Store)
            && copy.places.iter().any(|p| {
                p.kind == PlaceKind::Path
                    && p.missing_since.is_none()
                    && Path::new(&p.reference).is_file()
            });
        let stored = if has(PlaceKind::Store) {
            self.storage.book_files(copy.book_id).await?
        } else {
            Vec::new()
        };
        let mut reader_devices = Vec::new();
        for p in copy.places.iter().filter(|p| p.kind == PlaceKind::Reader) {
            if let Some(id) = &p.detail
                && let Some(name) = self.storage.paired_device(id).await?.and_then(|d| d.label)
            {
                reader_devices.push((p.reference.clone(), name));
            }
        }
        Ok(CopyFacts {
            format: copies::derive_format(&copy.places, &stored),
            can_keep_here,
            reader_devices,
        })
    }

    /// Where each of this book's highlights came from: its origin, the copy it
    /// was made in, the reader that copy sits on and whether the copy is on
    /// this computer. Facts only; see [`HighlightOriginFact`].
    pub async fn highlight_origins(&self, book_id: i64) -> Result<Vec<HighlightOriginFact>> {
        let copies = self.storage.copies_for_book(book_id).await?;
        let mut by_copy = std::collections::HashMap::new();
        for copy in &copies {
            let facts = self.copy_facts(copy).await?;
            let device = facts.reader_devices.into_iter().next().map(|(_, n)| n);
            let local = copy.places.iter().any(|p| {
                matches!(
                    p.kind,
                    PlaceKind::Path | PlaceKind::Store | PlaceKind::Calibre | PlaceKind::AppleBooks
                )
            });
            by_copy.insert(copy.id, (device, local));
        }
        Ok(self
            .storage
            .list_highlights(book_id)
            .await?
            .into_iter()
            .map(|h| {
                let (device_name, on_this_computer) = h
                    .copy_id
                    .and_then(|c| by_copy.get(&c).cloned())
                    .unwrap_or((None, false));
                HighlightOriginFact {
                    highlight_id: h.id,
                    origin: h.origin,
                    copy_id: h.copy_id,
                    device_name,
                    on_this_computer,
                    pos_kind: h.pos_kind,
                    text_unavailable: h.text_unavailable,
                }
            })
            .collect())
    }

    /// The files of a book's copies, for the book page's Copies place: one row
    /// per file of a file-bearing place, in copy order then place order.
    /// `reader` and `apple_books` places hold no file here and give no row
    /// (they are on the copy's places). See [`CopyFile`] for each field.
    ///
    /// - `path`: the reference is the path; size and `on_disk` are read now,
    ///   because an epub `path` place is never rescanned and so a moved file
    ///   is not marked missing — the look is the honest fact.
    /// - `store`: the stored file's path and its recorded size; `on_disk` read.
    /// - `calibre`: one row per format path the last scan recorded (`0031`);
    ///   a place with none recorded gives one row with no path, carrying the
    ///   library root so a frontend can still say it is in calibre.
    ///
    /// An unknown book is `NotFound`.
    pub async fn copy_files(&self, book_id: i64) -> Result<Vec<CopyFile>> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        fn look(path: &Path) -> (Option<bool>, Option<i64>) {
            match std::fs::metadata(path) {
                Ok(m) if m.is_file() => (Some(true), Some(m.len() as i64)),
                _ => (Some(false), None),
            }
        }
        let mut out = Vec::new();
        for copy in self.storage.copies_for_book(book_id).await? {
            for place in &copy.places {
                let row = |format: Option<String>,
                           path: Option<PathBuf>,
                           calibre_library: Option<String>,
                           on_disk: Option<bool>,
                           size: Option<i64>| CopyFile {
                    copy_id: copy.id,
                    kind: place.kind,
                    reference: place.reference.clone(),
                    format,
                    path,
                    calibre_library,
                    on_disk,
                    size,
                    missing_since: place.missing_since,
                    last_seen_at: place.last_seen_at,
                };
                match place.kind {
                    PlaceKind::Path => {
                        let path = PathBuf::from(&place.reference);
                        let format = Some(files::format_of(&path)).filter(|f| f != "bin");
                        let (on_disk, size) = look(&path);
                        out.push(row(format, Some(path), None, on_disk, size));
                    }
                    PlaceKind::Store => match self.storage.book_file(&place.reference).await? {
                        Some(file) => {
                            let path = self.file_path(&file);
                            let on_disk = Some(path.is_file());
                            out.push(row(
                                Some(file.format.to_lowercase()),
                                Some(path),
                                None,
                                on_disk,
                                Some(file.size),
                            ));
                        }
                        None => out.push(row(None, None, None, None, None)),
                    },
                    PlaceKind::Calibre => {
                        let library = place
                            .detail
                            .as_deref()
                            .map(|d| copies::calibre_root(d).to_string());
                        let recorded = self
                            .storage
                            .place_files(PlaceKind::Calibre, &place.reference)
                            .await?;
                        if recorded.is_empty() {
                            out.push(row(None, None, library.clone(), None, None));
                        }
                        for f in recorded {
                            let (on_disk, size) = look(&f.path);
                            out.push(row(
                                Some(f.format),
                                Some(f.path),
                                library.clone(),
                                on_disk,
                                size,
                            ));
                        }
                    }
                    PlaceKind::Reader | PlaceKind::AppleBooks => {}
                }
            }
        }
        Ok(out)
    }

    /// Where a book's highlights came from, one row per `(copy, origin)`
    /// (copy id, then origin; history with no copy first), with the reader
    /// the copy sits on and when that source was last read in. **No counts**:
    /// see [`CopyHighlightSource`]. An unknown book is `NotFound`.
    pub async fn copy_highlight_sources(&self, book_id: i64) -> Result<Vec<CopyHighlightSource>> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        let copies = self.storage.copies_for_book(book_id).await?;
        let mut device = std::collections::HashMap::new();
        for copy in &copies {
            let facts = self.copy_facts(copy).await?;
            device.insert(
                copy.id,
                facts.reader_devices.into_iter().next().map(|(_, n)| n),
            );
        }
        let mut groups = std::collections::BTreeMap::new();
        for h in self.storage.list_highlights(book_id).await? {
            groups.insert((h.copy_id, h.origin.as_str()), h.origin);
        }
        // The latest look at any place of `kind` (and `pdf` only, for a path).
        let seen = |copy: &Copy, kind: PlaceKind, pdf_only: bool| {
            copy.places
                .iter()
                .filter(|p| p.kind == kind)
                .filter(|p| !pdf_only || files::format_of(Path::new(&p.reference)) == "pdf")
                .filter_map(|p| p.last_seen_at)
                .max()
        };
        Ok(groups
            .into_iter()
            .map(|((copy_id, _), origin)| {
                let copy = copy_id.and_then(|id| copies.iter().find(|c| c.id == id));
                let last_read_at = copy.and_then(|c| match origin {
                    HighlightOrigin::Koreader => seen(c, PlaceKind::Reader, false),
                    HighlightOrigin::Preview => seen(c, PlaceKind::Path, true),
                    HighlightOrigin::AppleBooks => seen(c, PlaceKind::AppleBooks, false),
                    HighlightOrigin::Kindle
                    | HighlightOrigin::KindleApp
                    | HighlightOrigin::User => None,
                });
                CopyHighlightSource {
                    copy_id,
                    origin,
                    device_name: copy_id.and_then(|id| device.get(&id).cloned().flatten()),
                    last_read_at,
                }
            })
            .collect())
    }

    /// *Find the file…*: this copy's file is at `path` now.
    ///
    /// The path is re-checked rather than trusted (absent is `NotFound`; not a
    /// regular file is `InvalidInput`), and it is refused with `InvalidInput`
    /// when its place or its bytes are already another book's copy. Then, in
    /// one transaction, a `path` place (hash and mtime) is attached to this
    /// copy and the copy's `path` places that are marked missing or no longer
    /// on disk are removed — the file moved. Every other place stays: a
    /// calibre place is calibre's, and a reader or the store are elsewhere.
    /// A place another copy of the same book held merges that copy into this
    /// one, as [`Engine::attach_place`] does. A PDF's annotations are then
    /// read into this copy (a failure there is logged, never the relocation's).
    ///
    /// An unknown copy is `NotFound`. `have_now` is never touched.
    pub async fn relocate_copy_file(&self, copy_id: i64, path: &Path) -> Result<Copy> {
        let copy = self
            .storage
            .get_copy(copy_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("copy id {copy_id}")))?;
        existing_file(path)?;
        let at = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let reference = at.display().to_string();
        let sha256 = files::sha256_of(&at)?;
        let taken = || {
            EngineError::InvalidInput(format!(
                "{} is already a copy of another book",
                at.display()
            ))
        };
        if let Some(c) = self
            .storage
            .copy_for_place(PlaceKind::Path, &reference)
            .await?
            && c.book_id != copy.book_id
        {
            return Err(taken());
        }
        if let Some(c) = self.storage.copy_for_hash(&sha256).await?
            && c.book_id != copy.book_id
        {
            return Err(taken());
        }
        if let Some(f) = self.storage.book_file(&sha256).await?
            && f.book_id != copy.book_id
        {
            return Err(taken());
        }
        let stale: Vec<String> = copy
            .places
            .iter()
            .filter(|p| p.kind == PlaceKind::Path && p.reference != reference)
            .filter(|p| p.missing_since.is_some() || !Path::new(&p.reference).is_file())
            .map(|p| p.reference.clone())
            .collect();
        let mut place = NewPlace::new(PlaceKind::Path, reference.clone());
        place.last_hash = Some(sha256);
        place.last_mtime = files::mtime_of(&at);
        if let PlaceAttach::OtherBook { .. } = self
            .storage
            .relocate_path_place(copy_id, &place, &stale)
            .await?
        {
            return Err(taken());
        }
        tracing::debug!(copy_id, removed = stale.len(), "a copy's file was found");
        // The file's KOReader identity, as adding a file teaches it — never
        // relabelling a link made by hand.
        if let Ok(md5) = partial_md5::partial_md5(&at) {
            self.storage
                .link_device_book(&md5, copy.book_id, storage::LinkedBy::Auto)
                .await?;
        }
        if files::format_of(&at) == "pdf" {
            match preview::import_pdf_annotations(&self.storage, copy.book_id, Some(copy_id), &at)
                .await
            {
                Ok(summary) => {
                    for d in &summary.diagnostics {
                        tracing::warn!(diagnostic = %d, "a found pdf's annotations were not all read");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %at.display(), "a found pdf's annotations could not be stored");
                }
            }
        }
        self.storage
            .get_copy(copy_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("copy id {copy_id}")))
    }

    /// Merge `drop` into `keep` (same book).
    pub async fn merge_copies(&self, keep: i64, drop: i64) -> Result<()> {
        self.storage.merge_copies(keep, drop).await
    }

    /// Record that a place was looked at (new hash and mtime, same copy).
    pub async fn refresh_place(
        &self,
        kind: PlaceKind,
        reference: &str,
        last_hash: Option<&str>,
        last_mtime: Option<i64>,
    ) -> Result<bool> {
        self.storage
            .refresh_place(kind, reference, last_hash, last_mtime)
            .await
    }

    /// Mark a place missing (keeps the earliest instant). The copy is unchanged.
    pub async fn mark_place_missing(
        &self,
        kind: PlaceKind,
        reference: &str,
        since: i64,
    ) -> Result<bool> {
        self.storage
            .mark_place_missing(kind, reference, since)
            .await
    }

    /// A missing place turned up again.
    pub async fn unmiss_place(&self, kind: PlaceKind, reference: &str) -> Result<bool> {
        self.storage.unmiss_place(kind, reference).await
    }

    /// Is this book in the library: a copy not ended, or any copy and touched.
    pub async fn in_library(&self, book_id: i64) -> Result<bool> {
        self.storage.in_library(book_id).await
    }

    /// Has this book been touched: any reading, reading event, highlight, note
    /// (reviews, ratings and citations hang off notes) or flashcard. Being on a
    /// list or having edited metadata does not count.
    pub async fn touched(&self, book_id: i64) -> Result<bool> {
        self.storage.touched(book_id).await
    }

    /// A finished reading and no copy.
    pub async fn read_without_copy(&self, book_id: i64) -> Result<bool> {
        self.storage.read_without_copy(book_id).await
    }

    /// Each in-library book's copy facts and the Library filters' counts.
    /// Definitions on `Storage::library_copies`.
    pub async fn library_copies(&self) -> Result<LibraryCopies> {
        self.storage.library_copies().await
    }

    /// The three derived facts together.
    pub async fn book_standing(&self, book_id: i64) -> Result<BookStanding> {
        self.storage.book_standing(book_id).await
    }

    // ---- reading-app imports (Preview, Apple Books, Kindle) ----
    //
    // Thin over the four importers (`preview`, `apple_books`,
    // `kindle_clippings`, `kindle_notebook`; each module doc is the contract).
    // What is added here is only what a caller naming a file needs: the file
    // is re-checked (exists, is a file, is the right kind) before an importer
    // sees it, so a bad path is `NotFound`/`InvalidInput` rather than a
    // diagnostic about a file nobody meant. Nothing here watches anything: a
    // client asks when it knows a volume appeared (`watch.rs` is a stream).

    /// Point [`Engine::import_apple_books`] at another `Data/Documents`, or
    /// (`None`) at nothing, which reports Apple Books absent. For tests and
    /// tools; a product leaves the default ([`apple_books::default_root`]).
    pub fn with_apple_books_root(mut self, root: Option<PathBuf>) -> Self {
        self.apple_books_root = root;
        self
    }

    /// PDF annotations (as macOS Preview and other viewers write them):
    /// re-read every PDF this book has as a `path` place of one
    /// of its copies, through [`preview::rescan_path_place`]. **mtime-gated**:
    /// a file whose mtime is the one the place recorded is not read, so asking
    /// again is cheap. A file that is gone marks its place missing.
    ///
    /// The gate is the place's own `last_mtime`, which *adding the file*
    /// records too — so adding a PDF ([`Engine::import_file`],
    /// [`Engine::add_file_to_book`]) reads the annotations already in it then,
    /// and this answers `unchanged` for it until the file changes.
    /// [`Engine::import_preview_file`] reads one file unconditionally.
    pub async fn import_preview_annotations(&self, book_id: i64) -> Result<PreviewBookImport> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        let mut out = PreviewBookImport::default();
        for copy in self.storage.copies_for_book(book_id).await? {
            for place in &copy.places {
                if place.kind != PlaceKind::Path
                    || files::format_of(Path::new(&place.reference)) != "pdf"
                {
                    continue;
                }
                let rescan =
                    preview::rescan_path_place(&self.storage, copy.id, &place.reference).await?;
                let t = &mut out.totals;
                t.inserted += rescan.summary.inserted;
                t.updated += rescan.summary.updated;
                t.unchanged += rescan.summary.unchanged;
                t.removed += rescan.summary.removed;
                t.unparsed += rescan.summary.unparsed;
                out.places.push(PreviewPlaceRescan {
                    copy_id: copy.id,
                    path: PathBuf::from(&place.reference),
                    outcome: rescan.outcome,
                    summary: rescan.summary,
                });
            }
        }
        Ok(out)
    }

    /// PDF annotations, for a PDF the reader points at: make it a `path` place of
    /// a copy of `book_id` the way *add a file to this book* does
    /// ([`Engine::add_file_to_book`], the one code path, told to leave the
    /// annotations), then import its annotations into that copy, whatever its
    /// mtime — once, so the summary says what this read found.
    ///
    /// A path that is not there is `NotFound`; one that is not a file, or not
    /// a `.pdf`, is `InvalidInput`; so is a file whose bytes (or place) are
    /// already another book's copy. A PDF that cannot be parsed is `Ok` with
    /// the importer's diagnostic.
    pub async fn import_preview_file(
        &self,
        book_id: i64,
        path: &Path,
    ) -> Result<HighlightImportSummary> {
        named_file(path, &["pdf"], "a PDF")?;
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        let report = files::attach_leaving_annotations(&self.storage, book_id, path).await?;
        if report.book_id != Some(book_id) {
            return Err(EngineError::InvalidInput(format!(
                "{} is already a copy of another book",
                path.display()
            )));
        }
        let at = report
            .stored_path
            .unwrap_or_else(|| std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        let copy = self
            .storage
            .copy_for_place(PlaceKind::Path, &at.display().to_string())
            .await?
            .filter(|c| c.book_id == book_id)
            .ok_or_else(|| {
                EngineError::Other(format!("{} was added but holds no copy", at.display()))
            })?;
        preview::import_pdf_annotations(&self.storage, book_id, Some(copy.id), &at).await
    }

    /// Apple Books from where the app keeps its data on this machine
    /// ([`Engine::with_apple_books_root`]). No such place (not a Mac, no
    /// home) is the report with an `AppleBooksAbsent` diagnostic, never an
    /// error — the same answer as a Mac where Apple Books was never opened.
    pub async fn import_apple_books(&self, dry_run: bool) -> Result<AppleBooksReport> {
        match &self.apple_books_root {
            Some(root) => self.import_apple_books_from(root, dry_run).await,
            None => Ok(AppleBooksReport {
                dry_run,
                diagnostics: vec![Diagnostic::apple_books_absent(PathBuf::new())],
                ..Default::default()
            }),
        }
    }

    /// Apple Books from an explicit `Data/Documents`.
    pub async fn import_apple_books_from(
        &self,
        root: &Path,
        dry_run: bool,
    ) -> Result<AppleBooksReport> {
        apple_books::import_apple_books(&self.storage, root, dry_run).await
    }

    /// The reader's answer to an unmatched Apple Books asset: it is this book.
    /// Returns the copy holding the asset's place.
    pub async fn link_apple_books_asset(&self, asset_id: &str, book_id: i64) -> Result<i64> {
        apple_books::link_apple_books_asset(&self.storage, asset_id, book_id).await
    }

    /// Kindle: every `documents/My Clippings.txt` on a mounted volume.
    /// Read-only; one directory level under each [`mount_roots`] entry and
    /// [`kindle_clippings::clippings_path_on`] below that, nothing deeper.
    pub fn find_kindle_clippings(&self) -> Vec<PathBuf> {
        device::kindle_clippings_on(&mount_roots())
    }

    /// Kindle: import a clippings file the reader named.
    pub async fn import_kindle_clippings(
        &self,
        path: &Path,
        dry_run: bool,
    ) -> Result<kindle_clippings::KindleClippingsReport> {
        named_file(path, &["txt"], "a Kindle clippings file")?;
        kindle_clippings::import_kindle_clippings(&self.storage, path, dry_run).await
    }

    /// Kindle: import every clippings file [`Engine::find_kindle_clippings`]
    /// finds, one report per file, in path order. None mounted is empty.
    pub async fn import_mounted_kindle_clippings(
        &self,
        dry_run: bool,
    ) -> Result<Vec<(PathBuf, kindle_clippings::KindleClippingsReport)>> {
        let mut out = Vec::new();
        for path in self.find_kindle_clippings() {
            let report =
                kindle_clippings::import_kindle_clippings(&self.storage, &path, dry_run).await?;
            out.push((path, report));
        }
        Ok(out)
    }

    /// Kindle: the manual attach of an unmatched title (its `title_line`
    /// as the report gave it) to `book_id`.
    pub async fn attach_kindle_title(
        &self,
        path: &Path,
        title_line: &str,
        book_id: i64,
        dry_run: bool,
    ) -> Result<kindle_clippings::KindleClippingsReport> {
        named_file(path, &["txt"], "a Kindle clippings file")?;
        kindle_clippings::import_kindle_clippings_into(
            &self.storage,
            path,
            title_line,
            book_id,
            dry_run,
        )
        .await
    }

    /// Kindle app: import an Export Notebook file into `book_id`, or the
    /// book its title auto-matches.
    pub async fn import_kindle_notebook(
        &self,
        path: &Path,
        book_id: Option<i64>,
        dry_run: bool,
    ) -> Result<kindle_notebook::KindleNotebookReport> {
        named_file(path, &["html", "htm"], "a Kindle notebook export")?;
        kindle_notebook::import_kindle_notebook(&self.storage, path, book_id, dry_run).await
    }
}

/// The instant a read finished on a named day — [`Engine::finish_reading_on`]'s
/// arithmetic, pure so its edges are tested without a clock.
///
/// `day_start` is midnight UTC opening the day (`None` is *now*). A day is not
/// an instant, so one is chosen, and the choice is **noon**, clamped:
///
/// - never before the read began — a read finished on the day it started is
///   finished at its start, not at a noon that precedes it;
/// - never after `now` — *today* is the ordinary answer and its noon may not
///   have happened yet, and a reader east of Greenwich picking their local
///   today is a day ahead of UTC until their morning.
///
/// Two days are refused rather than clamped, because each is a different
/// question rather than an imprecise answer to this one: a day **before** the
/// read began (the window would be inverted, and `READING_WINDOWS` would then
/// hand this read's highlights to nobody), and a day more than one past UTC
/// today (no timezone makes it today anywhere).
pub fn finish_instant(day_start: Option<i64>, now: i64, started_at: Option<i64>) -> Result<i64> {
    const DAY: i64 = 86_400;
    let Some(start) = day_start else {
        return Ok(match started_at {
            Some(s) if s > now => s,
            _ => now,
        });
    };
    if start > now + DAY {
        return Err(EngineError::InvalidInput(
            "that day has not happened yet".to_string(),
        ));
    }
    if let Some(s) = started_at
        && start + DAY <= s
    {
        return Err(EngineError::InvalidInput(
            "that day is before this read began".to_string(),
        ));
    }
    let noon = (start + DAY / 2).min(now);
    Ok(match started_at {
        Some(s) => noon.max(s),
        None => noon,
    })
}

#[cfg(test)]
mod finish_instant_tests {
    use super::*;

    const DAY: i64 = 86_400;
    /// 2026-11-11T00:00:00Z.
    const TODAY: i64 = 1_794_355_200;

    #[test]
    fn no_day_is_now() {
        assert_eq!(
            finish_instant(None, TODAY + 100, Some(TODAY - DAY)).unwrap(),
            TODAY + 100
        );
    }

    #[test]
    fn a_past_day_finishes_at_its_noon() {
        let at =
            finish_instant(Some(TODAY - 3 * DAY), TODAY + 100, Some(TODAY - 30 * DAY)).unwrap();
        assert_eq!(at, TODAY - 3 * DAY + DAY / 2);
    }

    #[test]
    fn today_before_noon_is_now_not_a_noon_still_to_come() {
        let now = TODAY + 3_600;
        assert_eq!(finish_instant(Some(TODAY), now, None).unwrap(), now);
    }

    #[test]
    fn tomorrow_is_now_and_the_day_after_is_refused() {
        let now = TODAY + 3_600;
        assert_eq!(finish_instant(Some(TODAY + DAY), now, None).unwrap(), now);
        assert!(matches!(
            finish_instant(Some(TODAY + 2 * DAY), now, None),
            Err(EngineError::InvalidInput(_))
        ));
    }

    #[test]
    fn the_day_it_began_finishes_no_earlier_than_it_began() {
        let began = TODAY - 2 * DAY + 20 * 3_600;
        let at = finish_instant(Some(TODAY - 2 * DAY), TODAY, Some(began)).unwrap();
        assert_eq!(at, began);
    }

    #[test]
    fn a_day_before_the_read_began_is_refused() {
        let began = TODAY - 2 * DAY + 3_600;
        assert!(matches!(
            finish_instant(Some(TODAY - 3 * DAY), TODAY, Some(began)),
            Err(EngineError::InvalidInput(_))
        ));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            /// Whatever the day, an accepted instant sits inside the read's
            /// window: not before it began, not after now (unless it began in
            /// the future, which a device clock can do).
            #[test]
            fn an_accepted_instant_is_inside_the_window(
                day in -60i64..3,
                began in proptest::option::of(-90 * DAY..DAY),
                now_off in 0i64..DAY,
            ) {
                let now = TODAY + now_off;
                let start = TODAY + day * DAY;
                let s = began.map(|b| TODAY + b);
                if let Ok(at) = finish_instant(Some(start), now, s) {
                    if let Some(s) = s { prop_assert!(at >= s); }
                    if s.is_none_or(|s| s <= now) { prop_assert!(at <= now); }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An upgrade is not a re-pairing**, and this is the only place it can be
    /// asserted. `plugin::install` is handed the id, the token and the pairing
    /// timestamp and cannot tell a first install from a reinstall; the facade
    /// is where all three are chosen. The clock is supplied here because two
    /// calls to the public method land in the same second and would agree by
    /// accident — which is how a real Kindle came to hold a `pairing.lua`
    /// claiming a `paired_at` its `paired_devices` row disagreed with.
    #[tokio::test]
    async fn reinstalling_the_plugin_keeps_the_pairing_it_already_had() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            db_url: "sqlite::memory:".into(),
            images_dir: tmp.path().join("images"),
            files_dir: tmp.path().join("files"),
            vault_dir: tmp.path().join("vault"),
            log_dir: tmp.path().join("logs"),
            exports_dir: tmp.path().join("exports"),
            google_api_key: None,
            calibre_bin_dir: None,
        })
        .await
        .unwrap();

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());

        let first = engine
            .install_plugin_at(mount.path(), 1_700_000_000)
            .await
            .unwrap();
        assert_eq!(first.upgraded_from, None);
        let pairing = first.plugin_dir.join("pairing.lua");
        let before = std::fs::read_to_string(&pairing).unwrap();
        assert!(before.contains("paired_at = 1700000000"));

        // A day later, and the same plugin.
        let again = engine
            .install_plugin_at(mount.path(), 1_700_086_400)
            .await
            .unwrap();
        assert_eq!(again.device_id, first.device_id, "the id is not re-minted");
        assert!(again.upgraded_from.is_some(), "it landed on our own plugin");

        let after = std::fs::read_to_string(&pairing).unwrap();
        assert_eq!(
            before, after,
            "the token and paired_at both survive a reinstall"
        );

        // And the device's own file agrees with the row we keep about it.
        let devices = engine.paired_devices().await.unwrap();
        let device = devices
            .iter()
            .find(|d| d.device_id == first.device_id)
            .expect("paired");
        assert_eq!(
            device.installed_at, 1_700_000_000,
            "pairing.lua and paired_devices.installed_at must not drift"
        );
    }

    async fn engine_at(tmp: &std::path::Path) -> Engine {
        Engine::open(EngineConfig {
            db_url: "sqlite::memory:".into(),
            images_dir: tmp.join("images"),
            files_dir: tmp.join("files"),
            vault_dir: tmp.join("vault"),
            log_dir: tmp.join("logs"),
            exports_dir: tmp.join("exports"),
            google_api_key: None,
            calibre_bin_dir: None,
        })
        .await
        .unwrap()
    }

    /// Seeing a paired reader stamps `last_seen_at`. If it moved only on
    /// install, so a reader plugged in
    /// every night would still report the date its plugin was put there.
    ///
    /// The clock is not supplied here on purpose — the assertion is about
    /// *which timestamp moved*, not about its value, and `installed_at` being
    /// pinned to a fixed instant is what makes the two distinguishable in the
    /// same second.
    #[tokio::test]
    async fn looking_at_a_paired_reader_records_that_we_saw_it() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());
        engine
            .install_plugin_at(mount.path(), 1_700_000_000)
            .await
            .unwrap();

        // Wipe the install's own stamp, so what we observe can only have come
        // from the status call.
        let id = engine.paired_devices().await.unwrap()[0].device_id.clone();
        sqlx::query("UPDATE paired_devices SET last_seen_at = NULL, last_mount_path = NULL")
            .execute(engine.storage().pool())
            .await
            .unwrap();

        let status = engine.plugin_status(mount.path()).await.unwrap();
        assert!(status.paired);

        let d = &engine.paired_devices().await.unwrap()[0];
        assert!(d.last_seen_at.is_some(), "seeing a reader is an event");
        assert_eq!(d.last_mount_path.as_deref(), mount.path().to_str());
        assert_eq!(
            d.installed_at, 1_700_000_000,
            "and the pairing did not move"
        );
        assert_eq!(
            d.last_synced_at, None,
            "looking at a reader is not syncing with it"
        );
        assert_eq!(d.device_id, id);
    }

    /// A reader carrying somebody else's `pairing.lua`. `paired` is false and —
    /// the part worth pinning — no row is invented for it.
    ///
    /// **`device_id` is `None` here, and deliberately.** The field used to
    /// hold whatever id the file named, so a reader belonging to
    /// another bookmole reported a stranger's id under a heading that reads
    /// *our* id everywhere else — harmless while a reader could hold only one
    /// pairing, and unreadable the moment it can hold several. It now answers
    /// *which of this reader's computers are we*, and the honest answer here is
    /// none; `pairings` is where the rest live.
    #[tokio::test]
    async fn a_reader_paired_with_another_copy_of_bookmole_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());
        engine.install_plugin(mount.path()).await.unwrap();
        // Keep the plugin, drop our half of the pairing — which is exactly the
        // state `forget_device` leaves behind.
        let id = engine.paired_devices().await.unwrap()[0].device_id.clone();
        assert!(engine.forget_device(&id).await.unwrap());
        assert!(!engine.forget_device(&id).await.unwrap());

        let status = engine.plugin_status(mount.path()).await.unwrap();
        assert!(status.installed, "the plugin is still on the reader");
        assert_eq!(
            status.device_id, None,
            "no pairing on this reader is ours any more"
        );
        assert!(!status.paired, "and we no longer know it");
        assert_eq!(
            status
                .pairings
                .iter()
                .map(|p| p.device_id.as_str())
                .collect::<Vec<_>>(),
            vec![id.as_str()],
            "the reader still names the computer it was paired with"
        );
        assert!(engine.paired_devices().await.unwrap().is_empty());

        // And installing again does **not** resume the dead pairing: no row
        // matches, so a fresh identity is minted — and it is *appended*, which
        // is the whole of the fix. The old entry stays where it is, because it
        // is another computer's as far as this one can tell.
        engine.install_plugin(mount.path()).await.unwrap();
        let after = engine.plugin_status(mount.path()).await.unwrap();
        assert_eq!(after.pairings.len(), 2);
        assert!(after.paired);
        assert_ne!(after.device_id.as_deref(), Some(id.as_str()));
        assert_eq!(after.pairings[0].device_id, id, "and it kept the first");
    }

    /// `sync_mount` on a paired reader with nothing on it.
    ///
    /// The empty case is the one worth asserting: `found` and `synced` are both
    /// zero, and the device is still identified — which is what lets a frontend
    /// say *nothing new* rather than *no device*.
    #[tokio::test]
    async fn syncing_a_mount_stamps_the_reader_it_turned_out_to_be() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());
        engine.install_plugin(mount.path()).await.unwrap();
        let id = engine.paired_devices().await.unwrap()[0].device_id.clone();

        let sync = engine.sync_mount(mount.path()).await.unwrap();
        assert_eq!(sync.device_id.as_deref(), Some(id.as_str()));
        assert_eq!(sync.found, 0);
        assert_eq!(sync.synced, 0);
        assert!(sync.reports.is_empty());

        assert!(
            engine.paired_devices().await.unwrap()[0]
                .last_synced_at
                .is_some()
        );
    }

    /// The same verb against a tree that is nobody's reader — a library
    /// directory on a disk. It must work, and it must stamp nothing.
    #[tokio::test]
    async fn syncing_an_unpaired_tree_is_ordinary() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let tree = tempfile::tempdir().unwrap();
        let sync = engine.sync_mount(tree.path()).await.unwrap();
        assert_eq!(sync.device_id, None, "there is no reader here to stamp");
        assert_eq!(sync.found, 0);
        assert!(engine.paired_devices().await.unwrap().is_empty());
    }

    /// A rename is the user's, and a plugin upgrade must not undo it. Asserted
    /// through the facade as well as in `storage::paired_devices` because the
    /// mount's directory name is chosen *here* — `install_plugin_at` passes
    /// `mount.file_name()` — so the two halves of the rule live in two files.
    #[tokio::test]
    async fn renaming_a_reader_survives_the_next_install() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());
        engine
            .install_plugin_at(mount.path(), 1_700_000_000)
            .await
            .unwrap();
        let id = engine.paired_devices().await.unwrap()[0].device_id.clone();

        assert!(
            engine
                .rename_device(&id, "  the bedside Kobo  ")
                .await
                .unwrap()
        );
        engine
            .install_plugin_at(mount.path(), 1_700_086_400)
            .await
            .unwrap();

        assert_eq!(
            engine.paired_devices().await.unwrap()[0].label.as_deref(),
            Some("the bedside Kobo"),
            "the mount's directory name is a default, never an override"
        );
        assert!(!engine.rename_device("nobody", "x").await.unwrap());
    }

    /// The shelf marker on a search result is an ISBN join and nothing else:
    /// a provider record with the shelf's ISBN-13 is that book, one with only
    /// its ISBN-10 is too, and one with no ISBN is not claimed however alike
    /// the title — a false "on the shelf" makes Enter open the wrong book.
    #[tokio::test]
    async fn shelf_of_matches_on_either_isbn_and_claims_nothing_without_one() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig {
            db_url: "sqlite::memory:".into(),
            images_dir: tmp.path().join("images"),
            files_dir: tmp.path().join("files"),
            vault_dir: tmp.path().join("vault"),
            log_dir: tmp.path().join("logs"),
            exports_dir: tmp.path().join("exports"),
            google_api_key: None,
            calibre_bin_dir: None,
        })
        .await
        .unwrap();

        let kept = engine
            .save_book(&Book {
                title: Some("The Long Country".into()),
                isbn_13: Some("9781447268963".into()),
                isbn_10: Some("1447268962".into()),
                ..Book::default()
            })
            .await
            .unwrap();
        let id = kept.id.unwrap();

        let by_13 = Book {
            isbn_13: Some("9781447268963".into()),
            ..Book::default()
        };
        assert_eq!(engine.shelf_of(&by_13).await.unwrap(), Some(id));

        let by_10 = Book {
            isbn_10: Some("1447268962".into()),
            ..Book::default()
        };
        assert_eq!(engine.shelf_of(&by_10).await.unwrap(), Some(id));

        let same_title_no_isbn = Book {
            title: Some("The Long Country".into()),
            ..Book::default()
        };
        assert_eq!(engine.shelf_of(&same_title_no_isbn).await.unwrap(), None);

        let other = Book {
            isbn_13: Some("9780061054884".into()),
            ..Book::default()
        };
        assert_eq!(engine.shelf_of(&other).await.unwrap(), None);
    }

    /// A library whose cover rows point somewhere else — relative, from a
    /// `--data-dir .` open, or into the folder it was copied from — is pointed
    /// into its own `images_dir` when it opens, where the files actually are.
    /// A webview frontend drew those covers blank: the first against whatever
    /// directory it started in, the second outside its asset scope.
    #[tokio::test]
    async fn opening_a_library_points_its_covers_into_its_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let config = || EngineConfig {
            db_url: format!("sqlite://{}", tmp.path().join("app.db").display()),
            images_dir: tmp.path().join("images"),
            files_dir: tmp.path().join("files"),
            vault_dir: tmp.path().join("vault"),
            log_dir: tmp.path().join("logs"),
            exports_dir: tmp.path().join("exports"),
            google_api_key: None,
            calibre_bin_dir: None,
        };
        let images = tmp.path().join("images");
        let (relative, moved, gone) = {
            let engine = Engine::open(config()).await.unwrap();
            let mut ids = Vec::new();
            for title in ["Relative", "Moved", "Gone"] {
                let b = engine
                    .save_book(&Book {
                        title: Some(title.into()),
                        ..Book::default()
                    })
                    .await
                    .unwrap();
                ids.push(b.id.unwrap());
            }
            let s = engine.storage();
            s.reroot_cover(ids[0], Some("./database/images/a.jpg"), None)
                .await
                .unwrap();
            s.reroot_cover(
                ids[1],
                Some("/old/library/database/images/b.jpg"),
                Some("/old/library/database/images/b.thumb.jpg"),
            )
            .await
            .unwrap();
            s.reroot_cover(ids[2], Some("/old/library/database/images/c.jpg"), None)
                .await
                .unwrap();
            (ids[0], ids[1], ids[2])
        };
        for name in ["a.jpg", "b.jpg", "b.thumb.jpg"] {
            std::fs::write(images.join(name), b"x").unwrap();
        }

        let engine = Engine::open(config()).await.unwrap();
        let here = |n: &str| Some(images.join(n).display().to_string());
        let a = engine.get_book(relative).await.unwrap().unwrap();
        assert_eq!(a.cover_path, here("a.jpg"));
        let b = engine.get_book(moved).await.unwrap().unwrap();
        assert_eq!(b.cover_path, here("b.jpg"));
        assert_eq!(b.cover_thumb_path, here("b.thumb.jpg"));
        // No file of that name here: the row is left as it was, never pointed
        // at nothing.
        let c = engine.get_book(gone).await.unwrap().unwrap();
        assert_eq!(
            c.cover_path.as_deref(),
            Some("/old/library/database/images/c.jpg")
        );
    }

    /// Google Books is built only with a key; a provider the
    /// reader switched off is not built at all, so nothing — search, ISBN
    /// lookup, enrichment — can send it anything; and the choice outlives the
    /// engine.
    #[tokio::test]
    async fn providers_are_built_from_the_defaults_the_reader_s_choices_and_the_key() {
        use providers::ProviderAvailability as A;
        let tmp = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            db_url: format!("sqlite://{}", tmp.path().join("app.db").display()),
            ..EngineConfig::rooted_at(tmp.path())
        };
        let config = EngineConfig {
            google_api_key: None,
            ..config
        };
        let engine = Engine::open(config.clone()).await.unwrap();
        let ids = |e: &Engine| e.providers().iter().map(|p| p.id()).collect::<Vec<_>>();
        assert_eq!(
            ids(&engine),
            vec![ProviderId::OpenLibrary],
            "no key, no Google"
        );
        assert_eq!(
            engine.search_providers(),
            vec![
                (ProviderId::OpenLibrary, A::Ready),
                (ProviderId::GoogleBooks, A::NoKey)
            ]
        );
        engine.set_google_api_key(Some("k".into()));
        assert_eq!(
            ids(&engine),
            vec![ProviderId::OpenLibrary, ProviderId::GoogleBooks]
        );
        engine
            .set_search_provider_enabled(ProviderId::OpenLibrary, false)
            .await
            .unwrap();
        assert_eq!(ids(&engine), vec![ProviderId::GoogleBooks]);
        engine
            .set_search_provider_enabled(ProviderId::GoogleBooks, false)
            .await
            .unwrap();
        assert!(ids(&engine).is_empty());
        assert_eq!(
            engine.search_providers(),
            vec![
                (ProviderId::OpenLibrary, A::Off),
                (ProviderId::GoogleBooks, A::Off)
            ]
        );
        drop(engine);

        // Read again at open.
        let engine = Engine::open(config).await.unwrap();
        assert!(ids(&engine).is_empty());
        engine
            .set_search_provider_enabled(ProviderId::OpenLibrary, true)
            .await
            .unwrap();
        assert_eq!(ids(&engine), vec![ProviderId::OpenLibrary]);
    }

    /// The facade's session: a search with every provider off answers at once
    /// from nothing, says why, and can be polled and ended; an unknown session
    /// is `NotFound`.
    #[tokio::test]
    async fn a_search_session_through_the_facade() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        engine
            .set_search_provider_enabled(ProviderId::OpenLibrary, false)
            .await
            .unwrap();
        let first = engine
            .start_search("dune", SearchScope::Everything, 500)
            .await
            .unwrap();
        assert!(first.done && first.works.is_empty() && !first.offline);
        assert_eq!(
            first
                .providers
                .iter()
                .map(|p| p.state.clone())
                .collect::<Vec<_>>(),
            vec![ProviderState::Off, ProviderState::NoKey]
        );
        let again = engine
            .poll_search(
                &first.session,
                first.revision,
                std::time::Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert_eq!(
            again.revision, first.revision,
            "a finished search answers at once"
        );
        assert!(engine.end_search(&first.session));
        assert!(!engine.end_search(&first.session));
        assert!(matches!(
            engine
                .poll_search(&first.session, 0, std::time::Duration::ZERO)
                .await,
            Err(EngineError::NotFound(_))
        ));
        assert!(!engine.search_cache_status().await.unwrap().anything_kept);
        engine.forget_searches(true).await.unwrap();
    }

    /// A provider that fails the test the moment anything asks it anything —
    /// the guard for every path that must not reach out (nothing typed leaves
    /// the machine before the reader presses Enter).
    struct Untouchable;

    #[async_trait::async_trait]
    impl MetadataProvider for Untouchable {
        fn id(&self) -> ProviderId {
            ProviderId::OpenLibrary
        }
        async fn search(&self, _: &SearchRequest) -> Result<Vec<providers::ProviderBook>> {
            panic!("the Library's search asked a provider")
        }
        async fn by_isbn(&self, _: &str) -> Result<Option<providers::ProviderBook>> {
            panic!("the Library's search asked a provider")
        }
    }

    /// The Library's box: the books in the library, ranked, typos forgiven —
    /// and no provider asked, with one in the list that panics if it is.
    #[tokio::test]
    async fn the_library_search_is_local_and_in_the_library_only() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        *write(&engine.providers) = Arc::new(vec![Box::new(Untouchable)]);
        let save = |title: &str| Book {
            title: Some(title.into()),
            authors: vec!["Ursula K. Le Guin".into()],
            ..Book::default()
        };
        let kept = engine.save_book(&save("The Dispossessed")).await.unwrap();
        let listed = engine
            .save_book(&save("The Left Hand of Darkness"))
            .await
            .unwrap();
        let kept = kept.id.unwrap();
        sqlx::query(
            "INSERT INTO copies (book_id, tenure, learned_from, created_at, updated_at)
             VALUES (?, 'own', 'user', 0, 0)",
        )
        .bind(kept)
        .execute(engine.storage.pool())
        .await
        .unwrap();

        let ids = |m: Vec<BookMatch>| m.into_iter().map(|m| m.book_id).collect::<Vec<_>>();
        assert_eq!(
            ids(engine.search_library("possess", 20).await.unwrap()),
            vec![kept]
        );
        assert_eq!(
            ids(engine.search_library("dispossesed", 20).await.unwrap()),
            vec![kept]
        );
        assert_eq!(
            ids(engine.search_library("le guin", 20).await.unwrap()),
            vec![kept],
            "a book with no copy is not on the shelf"
        );
        assert!(listed.id.is_some());
        assert!(engine.search_library("", 20).await.unwrap().is_empty());
    }

    /// Suggestions as you type: your books first, then the
    /// offline index, and **no provider in reach** — a provider list that
    /// panics when asked proves it.
    #[tokio::test]
    async fn suggestions_are_local_and_never_ask_a_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        *write(&engine.providers) = Arc::new(vec![Box::new(Untouchable)]);
        *write(&engine.wellknown) = WellKnownCell::new(search::wellknown::Loader::Works(Arc::new(
            search::wellknown::fixture::works(),
        )));
        let mine = engine
            .save_book(&Book {
                title: Some("Dune Road".into()),
                authors: vec!["A. Neighbour".into()],
                ..Book::default()
            })
            .await
            .unwrap();
        let got = engine.suggest_works("dune", 5).await.unwrap();
        assert!(got.warnings.is_empty());
        let titles: Vec<&str> = got.suggestions.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Dune Road", "Dune", "Dune Messiah"]);
        assert_eq!(
            got.suggestions[0].source,
            SuggestionSource::Library {
                book_id: mine.id.unwrap()
            }
        );
        assert_eq!(got.suggestions[1].query, "Dune by Frank Herbert");
        assert!(matches!(
            &got.suggestions[1].source,
            SuggestionSource::WellKnown { wikidata, .. } if wikidata == "Q190192"
        ));
        assert!(
            engine
                .suggest_works("  ", 5)
                .await
                .unwrap()
                .suggestions
                .is_empty()
        );

        // No index at all (an in-memory database's default): the library
        // still answers, and nothing is said to be missing.
        *write(&engine.wellknown) = WellKnownCell::new(search::wellknown::Loader::None);
        let got = engine.suggest_works("dune", 5).await.unwrap();
        assert_eq!(got.suggestions.len(), 1);
        assert!(got.warnings.is_empty());
    }

    /// Where the offline index goes: beside a database file, and nowhere for
    /// an in-memory one.
    #[test]
    fn the_offline_index_lives_beside_the_database() {
        use search::wellknown::Loader;
        assert!(matches!(wellknown_loader("sqlite::memory:"), Loader::None));
        match wellknown_loader("sqlite:///data/database/app.db") {
            Loader::Embedded { dir } => assert_eq!(dir, PathBuf::from("/data/database")),
            other => panic!("{other:?}"),
        }
        match wellknown_loader("sqlite://./database/app.db?mode=rwc") {
            Loader::Embedded { dir } => assert_eq!(dir, PathBuf::from("./database")),
            other => panic!("{other:?}"),
        }
    }
}
