//! bookmole engine: metadata search, imports, storage and the note vault.
//! Does no terminal I/O; frontends drive it through [`Engine`].
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

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
/// The physical shape of one edition as proportions; height is 1.0 and the frontend picks the unit.
pub mod edition;
pub mod enrich;
pub mod epub;
pub mod error;
pub mod files;
pub mod flashcards;
pub mod folders;
pub mod goodreads;
pub mod highlight_origin;
pub mod history;
pub mod images;
pub mod kindle_clippings;
pub mod kindle_notebook;
pub mod ko_statistics;
pub mod koreader;
pub mod library;
pub mod lists;
/// The one answer to "is this the book I already have"; internal, frontends use import paths.
pub(crate) mod matching;
pub mod names;
pub mod note_graph;
mod notebook;
pub mod notes;
pub mod partial_md5;
pub mod pdf;
pub mod plugin;
pub mod preview;
pub mod progress;
pub mod providers;
pub mod search;
pub mod sort;
pub mod spreadsheet;
pub mod storage;
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
pub use search::{FieldClaim, IsbnLookup, RankedResult, SearchOutcome};
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

struct ProviderManners {
    open_library: Arc<Manners>,
    google_books: Arc<Manners>,
}

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

/// A provider that is off is not built, so nothing can send it anything.
/// Google Books is built only with the user's own key; keyless it answers 429.
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
                claims: row.work.flattened_claims(),
            })
            .collect(),
        warnings: done.warnings,
    }
}

/// A client's claims checked against the book columns; a provider claim never names `user`.
fn claimed_columns(claims: &[FieldClaim]) -> Result<Vec<(&'static str, storage::Source)>> {
    let mut out: Vec<(&'static str, storage::Source)> = Vec::with_capacity(claims.len());
    for claim in claims {
        let field = storage::merge_columns()
            .find(|c| *c == claim.field)
            .ok_or_else(|| {
                EngineError::InvalidInput(format!("{:?} is not a book field", claim.field))
            })?;
        if out.iter().any(|(f, _)| *f == field) {
            return Err(EngineError::InvalidInput(format!(
                "{field} is claimed more than once"
            )));
        }
        out.push((field, claim.provider.into()));
    }
    Ok(out)
}

fn suggestion_query(title: &str, author: Option<&str>) -> String {
    match author {
        Some(a) if !a.is_empty() => format!("{title} by {a}"),
        _ => title.to_string(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct WorkSuggestions {
    pub suggestions: Vec<WorkSuggestion>,
    /// The offline index was not there (it never says what was typed).
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkSuggestion {
    pub title: String,
    pub author: Option<String>,
    pub year: Option<i64>,
    pub source: SuggestionSource,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SuggestionSource {
    Library {
        book_id: i64,
    },
    WellKnown {
        wikidata: String,
        ol_works: Vec<String>,
    },
}

/// Sets every cover field together: a width without the path it measures describes the wrong image.
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
    /// Private so frontends go through the facade; [`Engine::storage`] exists only behind `internals`.
    storage: Storage,
    config: EngineConfig,
    /// Behind a lock so a key change can rebuild it through `&self`.
    /// The `Arc` is cloned out so no guard is held across a slow search.
    providers: RwLock<Arc<Vec<Box<dyn MetadataProvider>>>>,
    /// The live key; `EngineConfig::google_api_key` is only the seed and goes stale after a change.
    google_api_key: RwLock<Option<String>>,
    /// User-Agent is `bookmole/<version>` and nothing that identifies the user to a provider.
    client: Client,
    manners: ProviderManners,
    provider_choices: RwLock<HashMap<ProviderId, bool>>,
    searches: search::session::Registry,
    /// Opened on first use, not at `open`: the first build takes seconds.
    wellknown: RwLock<Arc<WellKnownCell>>,
    calibre: Calibre,
    wireless: Arc<wireless::Listener>,
    apple_books_root: Option<PathBuf>,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPlaceRescan {
    pub copy_id: i64,
    pub path: PathBuf,
    pub outcome: preview::RescanOutcome,
    /// Empty unless `outcome` is `Reimported`.
    pub summary: HighlightImportSummary,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewBookImport {
    pub places: Vec<PreviewPlaceRescan>,
    pub totals: HighlightImportSummary,
}

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
        let storage = Storage::open(&config.db_url).await?;
        reroot_covers(&storage, &config.images_dir).await;
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

    /// For the engine's own integration tests; behind `internals` so shipped frontends cannot use it.
    #[cfg(any(test, feature = "internals"))]
    #[doc(hidden)]
    pub fn storage(&self) -> &Storage {
        &self.storage
    }

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

    /// Judged on the database's folder, never the vault's: the vault is plain markdown, safe to sync.
    pub fn library_location(&self) -> LibraryLocation {
        let mut location = LibraryLocation::of(&self.config.db_url, cloud_folder::cloud_folder);
        location.database_dir = location
            .database_dir
            .map(|d| std::path::absolute(&d).unwrap_or(d));
        location
    }

    /// The live key, which after a runtime change differs from the one `EngineConfig` was built with.
    pub fn google_api_key(&self) -> Option<String> {
        read(&self.google_api_key).clone()
    }

    /// Swaps the key (or clears it) and rebuilds the provider list.
    pub fn set_google_api_key(&self, key: Option<String>) {
        *write(&self.google_api_key) = key.clone();
        self.rebuild_providers();
    }

    fn rebuild_providers(&self) {
        let key = read(&self.google_api_key).clone();
        let chosen = read(&self.provider_choices).clone();
        *write(&self.providers) =
            Arc::new(build_providers(&self.client, key, &chosen, &self.manners));
    }

    /// Replaces the provider list for tests; a later `set_google_api_key` rebuilds it and undoes this.
    #[cfg(feature = "internals")]
    #[doc(hidden)]
    pub fn replace_providers(&self, providers: Vec<Box<dyn MetadataProvider>>) {
        *write(&self.providers) = Arc::new(providers);
    }

    fn providers(&self) -> Arc<Vec<Box<dyn MetadataProvider>>> {
        read(&self.providers).clone()
    }

    /// Federated search. Free text alone is parsed (title, author, ISBN) and ranked against every
    /// reading; a fielded request keeps its literal meaning.
    // The query is the user's private reading: `skip` keeps it out of the span.
    ///
    /// Free text runs a search session to its end, flattened to one record per work. `shelf_id` is only
    /// ever [`Held::Certain`] there: it means *Enter opens this book*, so a likely match is not enough.
    #[tracing::instrument(skip_all, fields(has_isbn = req.isbn.is_some()))]
    pub async fn search(&self, req: &SearchRequest) -> Result<SearchOutcome> {
        let mut req = req.clone();
        if let Some(raw) = &req.isbn {
            req.isbn =
                Some(normalize_isbn(raw).ok_or_else(|| EngineError::InvalidIsbn(raw.clone()))?);
        }
        match search::free_text(&req) {
            Some(text) => {
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
        // A provider in the list is ready whatever the settings say, so a test-replaced list is asked as
        // given.
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

    #[cfg(feature = "internals")]
    #[doc(hidden)]
    pub fn use_wellknown(&self, loader: search::wellknown::Loader) {
        *write(&self.wellknown) = WellKnownCell::new(loader);
    }

    /// Suggestions as you type: held books, then well-known works, best known first.
    /// Never asks a provider; without the offline index only the library answers, with a warning.
    // `skip_all`: what is typed is the reader's private reading.
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

    /// Starts a search session and returns its first snapshot; `limit` is at most 40.
    /// Poll with [`Engine::poll_search`], end with [`Engine::end_search`].
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

    /// The next snapshot after `after_revision`, waiting up to `wait` (capped at 1.5 s).
    /// `NotFound` for a session that ended, went idle or never started.
    pub async fn poll_search(
        &self,
        session: &str,
        after_revision: u64,
        wait: std::time::Duration,
    ) -> Result<SearchSnapshot> {
        let s = self.searches.get(session)?;
        Ok(s.poll(after_revision, wait).await)
    }

    pub fn end_search(&self, session: &str) -> bool {
        self.searches.end(session)
    }

    /// Books in the library that answer `query`, best first, at most `limit`.
    /// Reads only the books index; no provider is reachable from here.
    // `skip_all`: the query is the reader's private reading.
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

    pub fn search_providers(&self) -> Vec<(ProviderId, providers::ProviderAvailability)> {
        roster_of(
            &read(&self.provider_choices),
            read(&self.google_api_key).is_some(),
        )
    }

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

    pub async fn search_cache_status(&self) -> Result<storage::SearchCacheStatus> {
        self.storage.search_cache_status().await
    }

    /// Forgets kept searches, and with `records` the kept provider records too; no book is touched.
    pub async fn forget_searches(&self, records: bool) -> Result<()> {
        self.storage.forget_searches(records).await
    }

    /// The shelf's copy of a provider's book, matched on ISBN-13 then ISBN-10.
    /// A book without an ISBN is `None`: nothing else is stable enough to say it is the same one.
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

    /// Direct edition lookup by ISBN, merging fields across providers asked concurrently.
    /// A failing provider is left out; [`Engine::lookup_isbn_reported`] reports it.
    // `skip_all`: a typed ISBN is a query, and a query is never in a span.
    #[tracing::instrument(skip_all)]
    pub async fn lookup_isbn(&self, raw: &str) -> Result<Option<Book>> {
        Ok(self.lookup_isbn_reported(raw).await?.book)
    }

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

    /// Saves (insert-or-merge) and returns the stored copy.
    /// Writes no field provenance: the merged `Book` no longer says which provider supplied what.
    pub async fn save_book(&self, book: &Book) -> Result<Book> {
        self.save_book_claimed(book, &[]).await
    }

    /// [`Engine::save_book`] for a search result: each field the result's `claims` name is stamped
    /// with its provider. A field the user owns keeps its value and its `user` stamp. A claim naming
    /// no book column, or a column twice, is refused.
    pub async fn save_search_result(&self, book: &Book, claims: &[FieldClaim]) -> Result<Book> {
        let claims = claimed_columns(claims)?;
        self.save_book_claimed(book, &claims).await
    }

    async fn save_book_claimed(
        &self,
        book: &Book,
        claims: &[(&'static str, storage::Source)],
    ) -> Result<Book> {
        let id = self.storage.upsert_book_attributed(book, claims).await?;
        self.storage
            .get_book(id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))
    }

    pub async fn list_books(&self, query: &BookQuery) -> Result<Vec<Book>> {
        self.storage.list_books(query).await
    }

    pub async fn count_books(&self, filter: &BookFilter) -> Result<i64> {
        self.storage.count_books(filter).await
    }

    pub async fn book_summaries(&self, book_ids: &[i64]) -> Result<Vec<BookSummary>> {
        self.storage.book_summaries(book_ids).await
    }

    pub async fn get_book(&self, id: i64) -> Result<Option<Book>> {
        self.storage.get_book(id).await
    }

    /// One book per id, in the order asked; an id naming no book is left out.
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

    /// The JSON text a frontend stored under `key`; stored, never interpreted, and no secret belongs
    /// here.
    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.storage.get_setting(key).await
    }

    /// Refuses a key outside `[a-z0-9._-]{1,64}` and a value that is not JSON (`InvalidInput`).
    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.storage.set_setting(key, value).await
    }

    /// Every setting under `prefix`, a literal prefix where `""` is all, by key.
    pub async fn list_settings(&self, prefix: &str) -> Result<Vec<Setting>> {
        self.storage.list_settings(prefix).await
    }

    /// Shelf names other systems minted for this book: inert provenance, nothing reads them to decide.
    pub async fn book_tags(&self, book_id: i64) -> Result<Vec<BookTag>> {
        self.storage.book_tags(book_id).await
    }

    /// One row per open reading not put down, most recently touched first. The [`Reading`] rides beside
    /// the [`Book`]
    /// because `Book`'s progress fields project the current reading.
    pub async fn currently_reading(&self, limit: i64) -> Result<Vec<(Book, Reading)>> {
        self.storage.list_open_readings(limit).await
    }

    pub async fn resolve_books(&self, selector: &str) -> Result<Vec<Book>> {
        self.resolve_books_in(selector, LibraryScope::All).await
    }

    /// [`Engine::resolve_books`] restricted to books in the library; a copy-less book on a list never
    /// answers.
    pub async fn resolve_library_books(&self, selector: &str) -> Result<Vec<Book>> {
        self.resolve_books_in(selector, LibraryScope::InLibrary)
            .await
    }

    async fn resolve_books_in(&self, selector: &str, scope: LibraryScope) -> Result<Vec<Book>> {
        // ISBN before id: a bare ISBN-13 also parses as an i64, so trying the id first never resolves it.
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
        self.storage.find_books_by_title_in(selector, scope).await
    }

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

    /// Deletes a book, its cover and the ebook files it owned.
    /// Files are listed before the delete; a cover is removed only when no other book references it.
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

    pub async fn download_cover(&self, book: &mut Book) -> Result<Option<PathBuf>> {
        let Some(url) = book.cover_url.clone() else {
            return Ok(None);
        };
        let cover = self.download_cover_file(&url).await?;
        write_cover_onto(book, &cover);
        match book.id {
            // Not `upsert_book`: with no ISBN it inserts a duplicate row, ignoring `Book::id`.
            // Not `enrich_book` either: its no-clobber merge would keep the old file. `set_cover` replaces.
            Some(id) => self.storage.set_cover(id, &cover, None).await?,
            None if book.isbn_10.is_some() || book.isbn_13.is_some() => {
                let id = self.storage.upsert_book(book, None).await?;
                self.storage.set_cover(id, &cover, None).await?;
            }
            None => {}
        }
        Ok(Some(cover.path))
    }

    async fn download_cover_file(&self, url: &str) -> Result<images::CoverFile> {
        images::image_from_url(&self.client, url, &self.config.images_dir).await
    }

    /// Back-fills measurements of covers already on disk; returns `(measured, unreadable)`.
    /// A missing or undecodable file is counted and skipped, and files are never renamed.
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

    pub async fn rebuild_sort_keys(&self) -> Result<usize> {
        self.storage.rebuild_sort_keys().await
    }

    /// Asks the providers about a book we already have and merges what they say.
    /// An explicit per-book action only: never automatic, and there is deliberately no bulk form.
    pub async fn enrich_book_from_providers(&self, book_id: i64) -> Result<EnrichReport> {
        let providers = self.providers();
        let mut report = enrich::enrich_metadata(&self.storage, &providers, book_id).await?;

        // The cover goes last and only where there is no file: the merge may have just supplied the URL.
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
                    // The URL's supplier is the origin of the file fetched from it, `None` if the URL predates this
                    // run. The claim is on `cover_path` alone.
                    let source = report.source_of("cover_url");
                    self.storage.set_cover(book_id, &cover, source).await?;
                    report.cover = Some(cover.path);
                }
                Err(e) => report.warnings.push(Diagnostic::cover_unavailable(&e)),
            }
        }
        Ok(report)
    }

    /// Writes user-supplied fields and records them as the user's, so a hand correction survives the
    /// next provider merge. Sets only; it cannot clear a field.
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

    /// Where each field of a book came from, and when. An absent field means nobody has claimed it.
    pub async fn field_provenance(&self, book_id: i64) -> Result<Vec<FieldSource>> {
        self.storage.field_provenance(book_id).await
    }

    pub async fn fetch_cover(&self, book_id: i64) -> Result<Option<PathBuf>> {
        let mut book = self
            .storage
            .get_book(book_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
        self.download_cover(&mut book).await
    }

    /// Every reading of this book, oldest first; a reread is a row, not a flag.
    pub async fn list_readings(&self, book_id: i64) -> Result<Vec<Reading>> {
        self.storage.list_readings(book_id).await
    }

    pub async fn get_reading(&self, id: i64) -> Result<Option<Reading>> {
        self.storage.get_reading(id).await
    }

    /// Every reading of this book, oldest first, each with its own progress.
    /// Pairs each read with the book's length, since `Reading` has no page count of its own.
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

    pub async fn reading_with_progress(&self, id: i64) -> Result<Option<(Reading, Progress)>> {
        let Some(r) = self.storage.get_reading(id).await? else {
            return Ok(None);
        };
        let length = self.length_of(r.book_id).await?;
        let p = Progress::of_reading(&r, length);
        Ok(Some((r, p)))
    }

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

    async fn length_of(&self, book_id: i64) -> Result<Option<i64>> {
        Ok(self
            .storage
            .get_book(book_id)
            .await?
            .and_then(|b| b.page_count))
    }

    pub async fn list_reading_rows(&self, query: &ReadingQuery) -> Result<Vec<ReadingRow>> {
        self.storage.list_reading_rows(query).await
    }

    /// Which years a filter's readings ended in, newest first, and whether any is still open.
    /// Years are UTC, as `DayRange` is; a frontend's local-time year would disagree with the list.
    pub async fn reading_years(&self, filter: &ReadingFilter) -> Result<ReadingYears> {
        self.storage.reading_years(filter).await
    }

    pub async fn count_readings(&self, filter: &ReadingFilter) -> Result<i64> {
        self.storage.count_readings(filter).await
    }

    /// The book's open reading; at most one exists (`idx_readings_one_open`).
    pub async fn active_reading(&self, book_id: i64) -> Result<Option<Reading>> {
        self.storage.active_reading(book_id).await
    }

    /// Writes reading progress and returns the book with its projections refreshed.
    /// Opens a reading when none is open. A page alone leaves a put-down reading put down;
    /// `finished: Some(false)` picks it up as [`Engine::pick_up`] does, and `Some(true)` finishes it.
    pub async fn update_progress(
        &self,
        book_id: i64,
        page: Option<i64>,
        finished: Option<bool>,
    ) -> Result<Book> {
        self.storage.update_progress(book_id, page, finished).await
    }

    /// Finishes the book's open reading on a UTC `YYYY-MM-DD` day (`None` is now). Unlike
    /// `update_progress(finished: Some(true))` it refuses when no reading is open instead of opening
    /// one.
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

    /// Records the page count as the user's own, held against later provider merges.
    /// Refuses below 1: `Progress` reads a stored `0` as no length.
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

    /// Closes the open reading and opens a new one. A put-down reading stays put down, closed at the
    /// instant it was put down; any other open reading is finished.
    pub async fn reread(&self, book_id: i64) -> Result<i64> {
        self.storage.reread(book_id).await
    }

    /// The reader puts the book's open reading down. It stays open, so picking it up is not a reread,
    /// but it leaves the currently-reading list, reading-now and the timeline. Refused when there
    /// is no open reading or it is already put down.
    pub async fn put_down(&self, book_id: i64) -> Result<(Reading, Progress)> {
        let open = self.open_reading_of(book_id, "put down").await?;
        if ReadingState::from(open.status.as_str()) == ReadingState::Abandoned {
            return Err(EngineError::InvalidInput(format!(
                "book id {book_id}'s open reading is already put down"
            )));
        }
        self.storage.abandon_reading(book_id).await?;
        self.reading_after(open.id).await
    }

    /// The reader picks a put-down reading back up: it is `reading` again and its put-down instant
    /// is cleared. Refused when the open reading is not put down.
    pub async fn pick_up(&self, book_id: i64) -> Result<(Reading, Progress)> {
        let open = self.open_reading_of(book_id, "pick up").await?;
        if !self.storage.pick_up_reading(book_id).await? {
            return Err(EngineError::InvalidInput(format!(
                "book id {book_id}'s open reading is not put down"
            )));
        }
        self.reading_after(open.id).await
    }

    async fn open_reading_of(&self, book_id: i64, verb: &str) -> Result<Reading> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        self.storage.active_reading(book_id).await?.ok_or_else(|| {
            EngineError::InvalidInput(format!("book id {book_id} has no open reading to {verb}"))
        })
    }

    async fn reading_after(&self, reading_id: i64) -> Result<(Reading, Progress)> {
        self.reading_with_progress(reading_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("reading id {reading_id}")))
    }

    /// Rebuilds the activity log from what is already in the database; idempotent.
    /// Explicit only: an import that also refilled would make the log depend on the last importer.
    pub async fn refill_reading_events(&self) -> Result<RefillReport> {
        self.storage.refill_reading_events().await
    }

    pub async fn reading_events(&self, book_id: i64) -> Result<Vec<ReadingEvent>> {
        self.storage.reading_events(book_id).await
    }

    /// What is known about a period. `minutes` and `pages` are `None` when nothing measured them,
    /// which is not zero and must not be shown as zero.
    pub async fn activity_summary(&self, range: &DayRange) -> Result<ActivitySummary> {
        self.storage.activity_summary(range).await
    }

    pub async fn activity_by_day(&self, range: &DayRange) -> Result<Vec<DayActivity>> {
        self.storage.activity_by_day(range).await
    }

    /// The period by month; only months carrying an event come back.
    /// Not foldable from days: `minutes: None` would collapse to 0 and distinct `books` is lost.
    pub async fn activity_by_month(&self, range: &DayRange) -> Result<Vec<MonthActivity>> {
        self.storage.activity_by_month(range).await
    }

    /// Moments not yet shown, newest first, derived on every call; nothing about a moment is stored.
    /// No push channel and deliberately no count: `limit` is the only lever and takes the newest.
    pub async fn pending_moments(&self, limit: Option<i64>) -> Result<Vec<Moment>> {
        self.storage
            .pending_moments(storage::now_unix(), limit)
            .await
    }

    /// Records that a moment was surfaced; idempotent, keeping the first time.
    /// The id is opaque: a frontend hands back what it was given.
    pub async fn acknowledge_moment(&self, id: &str) -> Result<()> {
        self.storage.acknowledge_moment(id).await
    }

    /// This book's highlights across every reading; `reading_id` is `None` where no reading's window
    /// holds one.
    pub async fn list_highlights(&self, book_id: i64) -> Result<Vec<Highlight>> {
        self.storage.list_highlights(book_id).await
    }

    /// Groups of highlight ids that are the same passage seen by more than one row; rows are never
    /// merged.
    pub async fn same_passages(&self, book_id: i64) -> Result<Vec<Vec<i64>>> {
        Ok(highlight_origin::same_passages(
            &self.storage.list_highlights(book_id).await?,
        ))
    }

    pub async fn highlights_for_reading(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        self.storage.highlights_for_reading(reading_id).await
    }

    /// The passage the reader chose for a reading's card, `None` when not chosen.
    /// Always a member of [`Engine::card_passage_candidates`]; selection stays below the seam.
    pub async fn card_passage(&self, reading_id: i64) -> Result<Option<Highlight>> {
        self.storage.card_passage(reading_id).await
    }

    /// Chooses the card passage, or clears it with `None`. Must be a candidate for the reading
    /// (`InvalidInput` otherwise); a missing reading or highlight is `NotFound`.
    pub async fn set_card_passage(&self, reading_id: i64, highlight_id: Option<i64>) -> Result<()> {
        self.storage
            .set_card_passage(reading_id, highlight_id)
            .await
    }

    pub async fn card_passage_candidates(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        self.storage.card_passage_candidates(reading_id).await
    }

    /// Writes the reader's own annotation; `origin_note` is the source app's and imports refresh it.
    pub async fn set_annotation(&self, highlight_id: i64, annotation: Option<&str>) -> Result<()> {
        self.storage.set_annotation(highlight_id, annotation).await
    }

    /// Imports a local .epub: ISBN, provider enrichment, embedded cover.
    /// Falls back to the epub's own metadata when there is no usable ISBN or providers are unreachable.
    #[tracing::instrument(skip(self), fields(path = %path.display()))]
    pub async fn import_epub(&self, path: &Path) -> Result<Book> {
        let mut file = epub::EpubFile::open(path)?;
        let info = file.info();
        let mut seed = match &info.isbn {
            Some(isbn) => self.lookup_isbn(isbn).await?.unwrap_or_default(),
            None => Book::default(),
        };

        // Two records with two origins, merged by the shared statement: the provider wins and the file
        // fills the gaps (`Storage::fill_book`).
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
            // The seed carries the ISBN only as a conflict key: without it `upsert_book` plain-inserts and
            // re-importing the file would make a second book.
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
        // Measurements are written only if the fill kept this cover (the stored row wins), read back
        // rather than predicted, else they would describe an image the row does not point at.
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

        // Record the file's KOReader identity now so the sidecar later links by `md5`, the only branch
        // that works with sidecars kept away from the book. `link_device_book`, not `set_device_link`:
        // a scan must not relabel a link the user made by hand.
        if let Some(id) = saved.id {
            let md5 = partial_md5::partial_md5(path)?;
            self.storage
                .link_device_book(&md5, id, storage::LinkedBy::Auto)
                .await?;
        }
        Ok(saved)
    }

    /// Takes ownership of an ebook file: copies it into the content store and attaches the row. Creates
    /// a book only when nothing plausibly claims the file; a near miss returns
    /// [`FileOutcome::Unmatched`] with nothing written, overridden by `FileImportOptions { new: true
    /// }`.
    #[tracing::instrument(skip(self), fields(path = %path.display(), new = opts.new))]
    pub async fn import_file(
        &self,
        path: &Path,
        opts: files::ImportOptions,
    ) -> Result<FileImportReport> {
        files::import(self, path, opts).await
    }

    /// Attaches a file to a book the caller already chose; no matching, no creation.
    pub async fn add_file_to_book(&self, book_id: i64, path: &Path) -> Result<FileImportReport> {
        if self.storage.get_book(book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {book_id}")));
        }
        files::attach(&self.storage, &self.config.files_dir, book_id, path).await
    }

    pub async fn identify_file(&self, path: &Path) -> Result<FileIdentity> {
        files::identify(&self.storage, path).await
    }

    /// Expands paths into book files: a folder is walked (bounded, skipping hidden entries and linked
    /// folders) and a calibre library is named, not walked. Reads directory listings only.
    pub async fn expand_book_paths(&self, paths: Vec<PathBuf>) -> Result<ExpandedPaths> {
        tokio::task::spawn_blocking(move || folders::expand(&paths, folders::Bounds::SHIPPED))
            .await
            .map_err(|e| EngineError::Other(format!("folder walk task: {e}")))
    }

    pub async fn book_files(&self, book_id: i64) -> Result<Vec<BookFile>> {
        self.storage.book_files(book_id).await
    }

    /// Chapter list of the first epub this book owns, read from the file, not stored.
    /// `None`: no readable epub. `Some` of an empty list: the epub has no navigable TOC.
    /// An owned epub that fails to parse is an error.
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

    /// A readable file of one format: a kept store file first, else the first `path` place whose
    /// file is on disk and whose copy is not `unreadable`. A gone file is skipped, never an error.
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

    pub async fn book_file(&self, sha256: &str) -> Result<Option<BookFile>> {
        self.storage.book_file(sha256).await
    }

    /// Derived from the row, never stored, so the path cannot disagree with the content address.
    pub fn file_path(&self, file: &BookFile) -> PathBuf {
        files::content_path(&self.config.files_dir, &file.sha256, &file.format)
    }

    /// Forgets the row and removes the bytes; `false` when the sha was not ours, so a repeat is a
    /// no-op.
    pub async fn remove_file(&self, sha256: &str) -> Result<bool> {
        let Some(file) = self.storage.book_file(sha256).await? else {
            return Ok(false);
        };
        self.storage.delete_book_file(sha256).await?;
        self.storage.delete_place(PlaceKind::Store, sha256).await?;
        std::fs::remove_file(self.file_path(&file)).ok();
        Ok(true)
    }

    #[tracing::instrument(skip(self), fields(path = %path.display()))]
    pub async fn import_koreader(&self, path: &Path, dry_run: bool) -> Result<ImportReport> {
        koreader::import(&self.storage, path, dry_run).await
    }

    #[tracing::instrument(skip(self), fields(path = %sidecar.display()))]
    pub async fn pull_book_from_sidecar(&self, sidecar: &Path) -> Result<PullReport> {
        koreader::import_book_from_sidecar(&self.storage, sidecar).await
    }

    #[tracing::instrument(skip(self), fields(root = %root.display()))]
    pub async fn scan_device(&self, root: &Path) -> Result<device::DeviceScan> {
        device::scan_device(&self.storage, root).await
    }

    /// Pulls a selection of the device in, one report per book.
    /// Deliberately does not import statistics; see [`Engine::import_device_statistics`].
    pub async fn sync_device(&self, paths: &[PathBuf]) -> Result<Vec<PullReport>> {
        device::sync_device(&self.storage, paths).await
    }

    /// The plugin's state on this reader, and whether the reader it says it is paired with is known
    /// here. Never writes to the mount, but recognising a paired reader stamps `last_seen_at`; it is
    /// stamped here so every frontend records it the same way.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn plugin_status(&self, mount: &Path) -> Result<plugin::PluginStatus> {
        let mut status = plugin::inspect(mount)?;
        // A reader can be paired with several computers; only this side knows which id is ours, so
        // each candidate is tried with one `touch_device_seen` (false for a reader we have no row for).
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

    /// Installs or upgrades the plugin on a mounted reader and pairs with it.
    /// Never called automatically: mount-to-import is read-only so that writing is an explicit act.
    /// An already paired reader keeps its id and token.
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn install_plugin(&self, mount: &Path) -> Result<plugin::InstallReport> {
        self.install_plugin_at(mount, storage::now_unix()).await
    }

    async fn install_plugin_at(&self, mount: &Path, now: i64) -> Result<plugin::InstallReport> {
        let seen = plugin::inspect(mount)?;
        // Our entry is the one we have a row for: a file's single `device_id` may be another install's,
        // and overwriting it would strand that machine's `paired_devices` row.
        let mut existing = None;
        for pairing in &seen.pairings {
            if let Some(d) = self.storage.paired_device(&pairing.device_id).await? {
                existing = Some(d);
                break;
            }
        }
        // `paired_at` is carried over too: stamping `now` would make `pairing.lua` disagree with
        // `paired_devices.installed_at`.
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
            // A hint, not a fact: the address as of now. `None` (wifi off) writes nothing.
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

    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn uninstall_plugin(&self, mount: &Path) -> Result<plugin::UninstallReport> {
        let mut report = plugin::uninstall(mount)?;
        // Uninstalling ends every computer's pairing, but the only row we may drop is our own.
        report.forgot_device = None;
        for id in &report.removed_pairings {
            if self.storage.forget_pairing(id).await? {
                report.forgot_device = Some(id.clone());
                break;
            }
        }
        Ok(report)
    }

    pub async fn paired_devices(&self) -> Result<Vec<PairedDevice>> {
        self.storage.list_paired_devices().await
    }

    pub async fn listener_status(&self) -> Result<wireless::ListenerStatus> {
        Ok(self.wireless.status(storage::now_unix()).await)
    }

    /// Opens the door for `minutes` (default five; `Some(0)` until stopped).
    /// Never on an automatic path: reaching outward is an explicit act. Closes after the first
    /// completed push, which is one whole session of every book a tap carried.
    #[tracing::instrument(skip(self))]
    pub async fn start_listening(&self, minutes: Option<u32>) -> Result<wireless::ListenerStatus> {
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

    /// Closes it; idempotent, since the window may have expired before the user pressed stop.
    pub async fn stop_listening(&self) -> Result<wireless::ListenerStatus> {
        Ok(self.wireless.stop().await)
    }

    /// Fetches from a paired reader whose window is open: the rendezvous dialled the other way, with
    /// the same MAC check so a rogue answering first cannot open a session. Needs no open door.
    /// `ReaderNotFound` when nothing answers; callers must not guess why.
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
            // Three seconds, the plugin's own figure for this exchange; not a knob.
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

    fn rendezvous_targets() -> Vec<std::net::SocketAddr> {
        plugin::broadcast_addresses()
            .into_iter()
            .map(|a| std::net::SocketAddr::new(a.into(), wireless::RENDEZVOUS_PORT))
            .collect()
    }

    /// This computer's LAN address, `None` with wifi off, for a person to type into a reader.
    /// A hint, never a handle: nothing may store it or join on it, since a lease moves.
    pub fn lan_address(&self) -> Option<String> {
        plugin::this_lan_address()
    }

    /// Pairs with a reader over wifi using the code its screen shows (case, spaces, hyphens ignored).
    /// The code is a secret and is never logged. `host` is an extra place to send for networks where
    /// broadcast fails. `PairingNotAnswered` when nothing proves the code; the reader is silent either
    /// way.
    #[tracing::instrument(skip(self, code))]
    pub async fn pair_with_reader(&self, code: &str, host: Option<&str>) -> Result<PairedDevice> {
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
                // IPv4 only: the reader's socket is `udp4`.
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

    /// The listener itself, behind `internals` so the engine's tests can drive `serve_push` without a
    /// broadcast socket.
    #[cfg(feature = "internals")]
    pub fn wireless(&self) -> &Arc<wireless::Listener> {
        &self.wireless
    }

    /// Forgets a pairing without the reader in hand, dropping only our row.
    /// The plugin still holds the token on the device; callers must say "removed from here".
    pub async fn forget_device(&self, device_id: &str) -> Result<bool> {
        self.storage.forget_pairing(device_id).await
    }

    /// Names a reader; blank clears it. The default is the mount's directory name.
    pub async fn rename_device(&self, device_id: &str, label: &str) -> Result<bool> {
        self.storage.set_device_label(device_id, label).await
    }

    /// Scans one mounted reader, syncs every syncable book, and stamps `last_synced_at` for a known
    /// reader. Re-scans below the seam because a handle held across a round trip may be stale. Does not
    /// import reading time; see [`Engine::import_device_statistics`].
    #[tracing::instrument(skip(self), fields(mount = %mount.display()))]
    pub async fn sync_mount(&self, mount: &Path) -> Result<device::MountSync> {
        let scan = self.scan_device(mount).await?;
        let paths: Vec<PathBuf> = scan.syncable().map(|b| b.path.clone()).collect();

        // Read before the sync so created copies can name their reader, stamped only after it succeeds.
        let device_id = match plugin::inspect(mount) {
            Ok(status) => status.device_id,
            // Not a KOReader mount or an unreadable `_meta.lua` is no reason to fail a sync; no pairing is
            // needed.
            Err(_) => None,
        };
        let known = match &device_id {
            Some(id) => self.storage.paired_device(id).await?.map(|d| d.device_id),
            None => None,
        };
        let reports = device::sync_device_for(&self.storage, &paths, known.as_deref()).await?;
        let device_id = match device_id {
            Some(id) if self.storage.stamp_device_sync(&id).await? => Some(id),
            // A `pairing.lua` with no row of ours belongs to another install; record nothing about it.
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

    /// Imports measured reading time from a device's `statistics.sqlite3` into the activity log.
    /// Separate from [`Engine::sync_device`] because arrival is read-only; the user asks for it by
    /// name. A device without the statistics plugin returns an empty report with a `Diagnostic`.
    pub async fn import_device_statistics(&self, mount: &Path) -> Result<StatsImportReport> {
        ko_statistics::import_device_statistics(&self.storage, mount).await
    }

    pub async fn sidecar_candidates(&self, sidecar: &Path) -> Result<Vec<MatchCandidate>> {
        let sc = koreader::parse_sidecar(&std::fs::read_to_string(sidecar)?)?;
        koreader::match_candidates(&self.storage, &sc).await
    }

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

    pub async fn merge_books(&self, src: i64, dst: i64) -> Result<MergeReport> {
        let report = self.storage.merge_books(src, dst).await?;
        if let Some(cover) = &report.orphaned_cover {
            let cover = PathBuf::from(cover);
            std::fs::remove_file(images::thumb_path_of(&cover)).ok();
            std::fs::remove_file(cover).ok();
        }
        Ok(report)
    }

    pub async fn create_note(&self, input: NewNoteInput) -> Result<CreatedNote> {
        let book = match input.book_id {
            Some(id) => self.storage.get_book(id).await?,
            None => None,
        };
        notes::create_note(&self.storage, &self.config.vault_dir, book.as_ref(), input).await
    }

    /// Notes, newest first. `limit` cuts along `created_at`; `None` is every note, for walking the
    /// graph.
    pub async fn list_notes(
        &self,
        scope: NoteScope,
        limit: Option<i64>,
    ) -> Result<Vec<NoteRecord>> {
        self.storage.list_notes(scope, limit).await
    }

    /// Everything the reader wrote or kept matching one query, as one ranked list. `source` narrows
    /// which indexes are asked, never the order; `book_id` must be applied here because `limit` cuts
    /// the global list, so filtering afterwards loses hits. Empty query is no hits.
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

    pub async fn get_note(&self, note_id: i64) -> Result<Option<NoteRecord>> {
        self.storage.get_note(note_id).await
    }

    /// One older note to offer back on `today`, the caller's local date (the engine has no zone).
    /// `None` is ordinary for a young or empty notebook.
    pub async fn resurface_note(&self, today: time::Date) -> Result<Option<Resurfaced>> {
        self.storage.resurface_note(today).await
    }

    /// One older passage to offer back on `today`, the caller's local date; see
    /// [`Self::resurface_note`].
    pub async fn resurface_passage(&self, today: time::Date) -> Result<Option<ResurfacedPassage>> {
        self.storage.resurface_passage(today).await
    }

    /// This reading's note of that kind (`kind` is [`NoteKind::as_str`]); at most one exists.
    pub async fn note_for_reading(
        &self,
        reading_id: i64,
        kind: &str,
    ) -> Result<Option<NoteRecord>> {
        self.storage.note_for_reading(reading_id, kind).await
    }

    pub fn note_path(&self, note: &NoteRecord) -> PathBuf {
        self.config.vault_dir.join(&note.file_path)
    }

    /// Each `[[wikilink]]` this note's body wrote and the note it resolves to, if any.
    /// A target with no note yet is kept as text and resolves when that note appears.
    pub async fn outgoing_links(&self, note_id: i64) -> Result<Vec<OutgoingLink>> {
        self.storage.outgoing_links(note_id).await
    }

    pub async fn backlinks(&self, note_id: i64) -> Result<Vec<NoteRecord>> {
        self.storage.backlinks(note_id).await
    }

    /// Every link of a note in both directions, with the other note's book and, for incoming, the
    /// paragraph. Incoming first, then outgoing in body order; a self-link appears in neither.
    /// Paragraphs are the reader's writing, never above `trace!`. An unreadable source file gives
    /// `paragraph: None`.
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

    pub fn note_body(&self, note: &NoteRecord) -> Result<String> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (_, body) = notes::frontmatter_and_body(&content);
        Ok(body.trim_end().to_string())
    }

    /// Replaces a note's body, keeping its frontmatter, and reindexes FTS and wikilink edges.
    pub async fn update_note_body(&self, note: &NoteRecord, body: &str) -> Result<()> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (header, _) = notes::frontmatter_and_body(&content);
        let body = body.trim_end();
        std::fs::write(&file, format!("{header}{body}\n"))?;
        let links = notes::extract_wikilinks(body);
        self.storage
            .reindex_note(note.id, &note.title, body, &links)
            .await
    }

    /// Removes the markdown file, then the row and FTS entry; a missing file is not an error.
    pub async fn delete_note(&self, note: &NoteRecord) -> Result<()> {
        let file = self.note_path(note);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        self.storage.delete_note(note.id).await
    }

    /// Re-reads one note file and reindexes FTS and wikilink edges, since an outside edit may add a
    /// link.
    pub async fn refresh_note_from_disk(&self, note: &NoteRecord) -> Result<()> {
        let file = self.note_path(note);
        let content = std::fs::read_to_string(&file)?;
        let (_, body) = notes::frontmatter_and_body(&content);
        notes::reindex_from_body(&self.storage, note.id, &note.title, body).await?;
        Ok(())
    }

    /// Follows the vault so outside edits stay searchable. The caller polls it; it spawns nothing.
    /// Fails where the platform cannot watch; degrade to [`Engine::reconcile_vault`].
    pub fn watch_vault(&self) -> Result<VaultWatcher> {
        watch::watch_vault(&self.config.vault_dir, self.storage.clone())
    }

    /// Brings every note's index in line with its file, once: the half a watcher cannot see, edits
    /// made while bookmole was not running. A note whose file is missing is left exactly as it is.
    pub async fn reconcile_vault(&self) -> Result<VaultReconcile> {
        notes::reconcile_vault(&self.storage, &self.config.vault_dir).await
    }

    /// Opens this reading's reflection, creating it once and returning the same note after.
    /// `reading_id: None` is the current reading, opened if the book has none.
    pub async fn open_reflection(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<CreatedNote> {
        self.open_anchored(book_id, reading_id, NoteKind::Reflection)
            .await
    }

    /// Opens this reading's review: public prose, the only note kind that carries a rating.
    /// Never derived from the reflection: no shared body or frontmatter.
    pub async fn open_review(&self, book_id: i64, reading_id: Option<i64>) -> Result<CreatedNote> {
        self.open_anchored(book_id, reading_id, NoteKind::Review)
            .await
    }

    pub async fn open_reflection_record(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<NoteRecord> {
        self.open_anchored_record(book_id, reading_id, NoteKind::Reflection)
            .await
    }

    pub async fn open_review_record(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> Result<NoteRecord> {
        self.open_anchored_record(book_id, reading_id, NoteKind::Review)
            .await
    }

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
            // `ensure_reading`, not `open_reading`: a second reading starts only when the user asks.
            None => {
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                self.storage
                    .ensure_reading(book_id, Some(now), "manual")
                    .await?
            }
        };

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
        // The title is a wikilink target, so a reread's must not collide with the first reading's.
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

    /// Rates a review on the active scale, storing the raw value and scale id so the mapping
    /// stays re-derivable when the user edits it.
    pub async fn set_rating(&self, note_id: i64, value: f64) -> Result<()> {
        let note = self
            .storage
            .get_note(note_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("note id {note_id}")))?;
        // A reflection's private score would not be the same number and must not share this column.
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

    /// The review's rating on Goodreads' 0-5 in quarters, `None` when unrated.
    /// An unmapped rating is [`EngineError::UnmappedRating`], never rounded.
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

    /// The review's raw rating value together with its scale; a value is meaningless without its scale.
    pub async fn review_rating(&self, note_id: i64) -> Result<Option<Rating>> {
        self.storage.review_rating(note_id).await
    }

    /// Unrates a review; `false` when it had no rating, so a repeat is a no-op.
    pub async fn clear_review_rating(&self, note_id: i64) -> Result<bool> {
        self.storage.clear_review_rating(note_id).await
    }

    /// Defines a scale, or redefines one by name. A new scale becomes the default; redefining keeps the
    /// flag.
    pub async fn put_rating_scale(
        &self,
        name: &str,
        min: f64,
        max: f64,
        step: f64,
    ) -> Result<RatingScale> {
        self.storage.put_rating_scale(name, min, max, step).await
    }

    pub async fn rating_scales(&self) -> Result<Vec<RatingScale>> {
        self.storage.list_rating_scales().await
    }

    /// The scale flagged `is_default`, with deliberately no ordering fallback.
    pub async fn active_rating_scale(&self) -> Result<Option<RatingScale>> {
        self.storage.active_rating_scale().await
    }

    pub async fn rating_scale_by_name(&self, name: &str) -> Result<Option<RatingScale>> {
        self.storage.rating_scale_by_name(name).await
    }

    /// What this scale's values mean on Goodreads' 0-5 in quarters: an explicit lookup table;
    /// a point with no entry is reported, never rounded.
    pub async fn rating_map(&self, scale_id: i64) -> Result<Vec<(f64, f64)>> {
        self.storage.rating_map(scale_id).await
    }

    pub async fn map_rating(&self, scale: &RatingScale, value: f64, goodreads: f64) -> Result<()> {
        self.storage.map_rating(scale, value, goodreads).await
    }

    /// Cites a highlight from a note by reference, so the citation survives a device refresh.
    pub async fn cite(&self, note_id: i64, highlight_id: i64) -> Result<()> {
        self.storage.add_citation(note_id, highlight_id).await?;
        Ok(())
    }

    pub async fn uncite(&self, note_id: i64, highlight_id: i64) -> Result<bool> {
        self.storage.remove_citation(note_id, highlight_id).await
    }

    pub async fn citations_for(&self, note_id: i64) -> Result<Vec<Highlight>> {
        self.storage.citations_for(note_id).await
    }

    /// Which passages each note cites, one entry per requested id in the order asked, empties included.
    /// Returns ids, not rows, so the reader's text is not sent once per citing note.
    pub async fn citations_for_notes(&self, note_ids: &[i64]) -> Result<Vec<NoteCitations>> {
        self.storage.citations_for_notes(note_ids).await
    }

    #[tracing::instrument(skip(self), fields(path = %path.display(), dry_run = opts.dry_run))]
    pub async fn import_goodreads(
        &self,
        path: &Path,
        opts: goodreads::ImportOptions,
    ) -> Result<GoodreadsReport> {
        goodreads::import(self, path, opts).await
    }

    /// Builds a Goodreads-importable CSV of the library plus every failure; the caller owns the file.
    pub async fn export_goodreads(&self) -> Result<(String, Vec<Diagnostic>)> {
        goodreads::export(self).await
    }

    /// Records that a Goodreads row is that book, so it is never re-guessed.
    /// Takes Goodreads' own `Book Id`; a row without one cannot be linked, as nothing else is stable.
    pub async fn link_goodreads_row(&self, external_id: &str, book_id: i64) -> Result<()> {
        self.link_foreign_record(goodreads::SOURCE, external_id, book_id)
            .await
    }

    /// Which calibre tools this machine has, resolved once at [`Engine::open`]; never prompt to
    /// install.
    pub fn calibre(&self) -> &Calibre {
        &self.calibre
    }

    /// Converts a book between formats through `ebook-convert`, which reads both off the extensions.
    /// Refuses to overwrite unless told: the output path is typed by hand and a lost file has no undo.
    #[tracing::instrument(skip(self), fields(input = %input.display(), output = %output.display()))]
    pub async fn convert_ebook(
        &self,
        input: &Path,
        output: &Path,
        overwrite: bool,
    ) -> Result<PathBuf> {
        calibre::convert(&self.calibre, input, output, overwrite).await
    }

    pub async fn calibre_library(&self, library: Option<&Path>) -> Result<Vec<CalibreBook>> {
        calibre::list_library(&self.calibre, library).await
    }

    #[tracing::instrument(skip(self), fields(dry_run = opts.dry_run))]
    pub async fn import_calibre_library(
        &self,
        opts: &calibre::ImportOptions,
    ) -> Result<CalibreReport> {
        calibre::import(self, opts).await
    }

    /// Records that a calibre book is that book of ours.
    /// Keyed on calibre's uuid, never its `id`: ids are per-library and reused after a delete.
    pub async fn link_calibre_book(&self, uuid: &str, book_id: i64) -> Result<()> {
        self.link_foreign_record(calibre::SOURCE, uuid, book_id)
            .await
    }

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

    /// Captures a flashcard, optionally anchored to its source passage. Returns `false` when the card
    /// already existed (`UNIQUE(book_id, word)`), which is not a failure. The book and highlight pair
    /// is re-read here, not trusted: a highlight of another book is refused.
    pub async fn create_flashcard(
        &self,
        book_id: i64,
        highlight_id: Option<i64>,
        word: &str,
        context: Option<&str>,
    ) -> Result<bool> {
        // Trimmed because `UNIQUE(book_id, word)` is the dedup and " mot" and "mot" are one word.
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

    pub async fn export_flashcards(&self, include_exported: bool) -> Result<(String, usize)> {
        let cards = self.storage.list_flashcards(include_exported).await?;
        let tsv = flashcards::export_tsv(&cards);
        let ids: Vec<i64> = cards.iter().map(|c| c.id).collect();
        self.storage.mark_flashcards_exported(&ids).await?;
        Ok((tsv, ids.len()))
    }

    /// Anki's text-import format with a stable GUID per card, so a re-import updates rather than
    /// duplicates. `book_id: None` is the whole library; an unknown book is `NotFound`. Exported cards
    /// are marked, so `include_exported: false` means only cards made since the last file.
    pub async fn export_anki(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> Result<AnkiExport> {
        let (export, ids) = self.build_anki(book_id, include_exported).await?;
        self.storage.mark_flashcards_exported(&ids).await?;
        Ok(export)
    }

    /// [`Engine::export_anki`] without marking: the text and card ids; marking is the caller's, after
    /// delivery.
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

    pub fn exports_dir(&self) -> &Path {
        &self.config.exports_dir
    }

    /// [`Engine::export_anki`] written to [`Engine::exports_dir`]; writes nothing when no card went
    /// out. Cards are marked only after the write lands, so a failed write loses none.
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
        // A same-second export gets a suffix, not an overwrite: the first file's cards are already marked.
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

    /// Puts a search result on a list without owning it. An existing book is reused, matched by ISBN
    /// then by `koreader::scores_for`, taking only a match it may link unasked; otherwise the book is
    /// created through `save_book` with no copy, so it is on the list and not in the library.
    /// Idempotent.
    pub async fn add_candidate_to_list(
        &self,
        list_id: i64,
        candidate: &Book,
        note: Option<&str>,
    ) -> Result<ListEntry> {
        self.add_claimed_candidate_to_list(list_id, candidate, &[], note)
            .await
    }

    /// [`Engine::add_candidate_to_list`] for a search result: a book it creates is stamped as
    /// [`Engine::save_search_result`] stamps one. A book already known is reused and not written.
    pub async fn add_search_result_to_list(
        &self,
        list_id: i64,
        candidate: &Book,
        claims: &[FieldClaim],
        note: Option<&str>,
    ) -> Result<ListEntry> {
        let claims = claimed_columns(claims)?;
        self.add_claimed_candidate_to_list(list_id, candidate, &claims, note)
            .await
    }

    async fn add_claimed_candidate_to_list(
        &self,
        list_id: i64,
        candidate: &Book,
        claims: &[(&'static str, storage::Source)],
        note: Option<&str>,
    ) -> Result<ListEntry> {
        // A missing list is `NotFound` before a book is created for it.
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
                self.save_book_claimed(&fresh, claims)
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

    // A book is in the library when it has a copy not ended (`have_now` is not `false`), or any
    // copy and has been touched. Only the reader changes `have_now`; copies never delete a book.

    pub async fn add_copy(&self, new: &NewCopy) -> Result<Copy> {
        self.storage.create_copy(new).await
    }

    pub async fn get_copy(&self, id: i64) -> Result<Option<Copy>> {
        self.storage.get_copy(id).await
    }

    pub async fn copies_for_book(&self, book_id: i64) -> Result<Vec<Copy>> {
        self.storage.copies_for_book(book_id).await
    }

    /// Writes a copy's user-editable fields; the only way `have_now` changes.
    pub async fn update_copy(&self, copy: &Copy) -> Result<bool> {
        self.storage.update_copy(copy).await
    }

    pub async fn delete_copy(&self, id: i64) -> Result<bool> {
        self.storage.delete_copy(id).await
    }

    pub async fn copy_for_place(&self, kind: PlaceKind, reference: &str) -> Result<Option<Copy>> {
        self.storage.copy_for_place(kind, reference).await
    }

    pub async fn attach_place(&self, copy_id: i64, place: &NewPlace) -> Result<PlaceAttach> {
        self.storage.attach_place(copy_id, place).await
    }

    /// For frontends with no "how do you have it" step: records one `unrecorded` copy so a created book
    /// stays on the shelf. Returns `None` if the book has any copy; call only after a creating save.
    pub async fn ensure_unrecorded_copy(&self, book_id: i64) -> Result<Option<Copy>> {
        if !self.storage.copies_for_book(book_id).await?.is_empty() {
            return Ok(None);
        }
        let new = NewCopy::new(book_id, None, Tenure::Unrecorded, LearnedFrom::User);
        self.storage.create_copy(&new).await.map(Some)
    }

    /// [`Engine::save_book`] that records an `unrecorded` copy when the save created the book;
    /// an update to an existing book adds nothing.
    pub async fn save_book_unrecorded(&self, book: &Book) -> Result<Book> {
        let id = self
            .storage
            .upsert_book_with_first_copy(book, |id| {
                NewCopy::new(id, None, Tenure::Unrecorded, LearnedFrom::User)
            })
            .await?;
        self.storage
            .get_book(id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))
    }

    /// Stores this copy's file in `database/files/` and attaches a `store` place; explicit, never by
    /// import.
    pub async fn keep_copy_here(&self, copy_id: i64) -> Result<KeptCopy> {
        files::keep_copy_here(&self.storage, &self.config.files_dir, copy_id).await
    }

    /// Derived facts about a copy a frontend must not guess: format, whether *Keep a copy here* works,
    /// and the paired reader name for each `reader` place.
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

    /// One row per file of a copy's file-bearing places, in copy then place order; `reader` and
    /// `apple_books` places give no row. `path` places read size and `on_disk` now, since an epub
    /// `path` place is never rescanned. A `calibre` place gives a row per recorded format path, or one
    /// pathless row carrying the library root. An unknown book is `NotFound`.
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

    /// One row per `(copy, origin)` (copy id, then origin; copy-less history first), with the reader
    /// and when the source was last read. No counts. An unknown book is `NotFound`.
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

    /// *Find the file*: this copy's file is at `path` now; re-checked (`NotFound`, `InvalidInput` if
    /// not a file or it belongs to another book's copy). One transaction attaches the `path` place and
    /// removes this copy's missing `path` places; a same-book copy holding it is merged. PDF annotation
    /// read failures are only logged. `have_now` is never touched.
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

    pub async fn merge_copies(&self, keep: i64, drop: i64) -> Result<()> {
        self.storage.merge_copies(keep, drop).await
    }

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

    /// Marks a place missing, keeping the earliest instant; the copy is unchanged.
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

    pub async fn unmiss_place(&self, kind: PlaceKind, reference: &str) -> Result<bool> {
        self.storage.unmiss_place(kind, reference).await
    }

    pub async fn in_library(&self, book_id: i64) -> Result<bool> {
        self.storage.in_library(book_id).await
    }

    /// Whether the book has any reading, event, highlight, note or flashcard; a list or metadata edit
    /// does not count.
    pub async fn touched(&self, book_id: i64) -> Result<bool> {
        self.storage.touched(book_id).await
    }

    pub async fn read_without_copy(&self, book_id: i64) -> Result<bool> {
        self.storage.read_without_copy(book_id).await
    }

    pub async fn library_copies(&self) -> Result<LibraryCopies> {
        self.storage.library_copies().await
    }

    pub async fn book_standing(&self, book_id: i64) -> Result<BookStanding> {
        self.storage.book_standing(book_id).await
    }

    // The importers below re-check a named file (exists, is a file, right kind) so a bad path is
    // `NotFound`/`InvalidInput`, not a diagnostic about a file nobody meant.

    pub fn with_apple_books_root(mut self, root: Option<PathBuf>) -> Self {
        self.apple_books_root = root;
        self
    }

    /// Re-reads every PDF this book has as a `path` place, through [`preview::rescan_path_place`].
    /// mtime-gated by the place's `last_mtime`, which adding the file also records; a gone file marks
    /// its place missing.
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

    /// Makes the PDF a `path` place of a copy of `book_id` as [`Engine::add_file_to_book`] does, then
    /// imports its annotations regardless of mtime. `NotFound` if absent; `InvalidInput` if not a
    /// `.pdf` or if its bytes or place belong to another book's copy. An unparseable PDF is `Ok` with a
    /// diagnostic.
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

    /// Imports Apple Books from the configured root. No such place gives a report with an
    /// `AppleBooksAbsent` diagnostic, never an error.
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

    pub async fn import_apple_books_from(
        &self,
        root: &Path,
        dry_run: bool,
    ) -> Result<AppleBooksReport> {
        apple_books::import_apple_books(&self.storage, root, dry_run).await
    }

    pub async fn link_apple_books_asset(&self, asset_id: &str, book_id: i64) -> Result<i64> {
        apple_books::link_apple_books_asset(&self.storage, asset_id, book_id).await
    }

    pub fn find_kindle_clippings(&self) -> Vec<PathBuf> {
        device::kindle_clippings_on(&mount_roots())
    }

    pub async fn import_kindle_clippings(
        &self,
        path: &Path,
        dry_run: bool,
    ) -> Result<kindle_clippings::KindleClippingsReport> {
        named_file(path, &["txt"], "a Kindle clippings file")?;
        kindle_clippings::import_kindle_clippings(&self.storage, path, dry_run).await
    }

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

/// The instant a read finished on a named UTC day (`None` is now): noon, clamped to the read's
/// start and to `now`. A day before the read began, or more than one past UTC today, is refused.
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

    /// Uses `install_plugin_at` so the clock differs between calls; two public calls in one second
    /// agree by accident.
    #[tokio::test]
    async fn reinstalling_the_plugin_keeps_the_pairing_it_already_had() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig::in_memory(tmp.path()))
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
        Engine::open(EngineConfig::in_memory(tmp)).await.unwrap()
    }

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

    /// `device_id` is `None` for another install's pairing: it means which computer we are, and none
    /// is.
    #[tokio::test]
    async fn a_reader_paired_with_another_copy_of_bookmole_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;

        let mount = tempfile::tempdir().unwrap();
        device::install_fake_reader(mount.path());
        engine.install_plugin(mount.path()).await.unwrap();
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

        // Reinstalling appends a fresh identity; the old entry stays, as another computer's.
        engine.install_plugin(mount.path()).await.unwrap();
        let after = engine.plugin_status(mount.path()).await.unwrap();
        assert_eq!(after.pairings.len(), 2);
        assert!(after.paired);
        assert_ne!(after.device_id.as_deref(), Some(id.as_str()));
        assert_eq!(after.pairings[0].device_id, id, "and it kept the first");
    }

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

    #[tokio::test]
    async fn shelf_of_matches_on_either_isbn_and_claims_nothing_without_one() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = Engine::open(EngineConfig::in_memory(tmp.path()))
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

    #[tokio::test]
    async fn opening_a_library_points_its_covers_into_its_own_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let config = || {
            let mut c = EngineConfig::in_memory(tmp.path());
            c.db_url = format!("sqlite://{}", tmp.path().join("app.db").display());
            c
        };
        let images = config().images_dir;
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
        let c = engine.get_book(gone).await.unwrap().unwrap();
        assert_eq!(
            c.cover_path.as_deref(),
            Some("/old/library/database/images/c.jpg")
        );
    }

    #[tokio::test]
    async fn providers_are_built_from_the_defaults_the_reader_s_choices_and_the_key() {
        use providers::ProviderAvailability as A;
        let tmp = tempfile::tempdir().unwrap();
        let mut config = EngineConfig::in_memory(tmp.path());
        config.db_url = format!("sqlite://{}", tmp.path().join("app.db").display());
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

        let engine = Engine::open(config).await.unwrap();
        assert!(ids(&engine).is_empty());
        engine
            .set_search_provider_enabled(ProviderId::OpenLibrary, true)
            .await
            .unwrap();
        assert_eq!(ids(&engine), vec![ProviderId::OpenLibrary]);
    }

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

    /// Fails the test if anything asks it anything; guards paths that must not leave the machine.
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

        *write(&engine.wellknown) = WellKnownCell::new(search::wellknown::Loader::None);
        let got = engine.suggest_works("dune", 5).await.unwrap();
        assert_eq!(got.suggestions.len(), 1);
        assert!(got.warnings.is_empty());
    }

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

    async fn sql(engine: &Engine, statement: &str) {
        sqlx::query(statement)
            .execute(engine.storage.pool())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_body_update_the_index_refuses_leaves_the_index_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        let created = engine
            .create_note(NewNoteInput {
                body: "Plain.".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let note = engine.get_note(created.id).await.unwrap().unwrap();

        sql(
            &engine,
            "CREATE TRIGGER refuse_links BEFORE INSERT ON note_links
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .await;
        let edited = "Now about [[Vocation]].";
        assert!(engine.update_note_body(&note, edited).await.is_err());
        let indexed = engine.storage.indexed_body(note.id).await.unwrap();
        let links = engine.storage.note_links(note.id).await.unwrap();
        assert_eq!(
            (indexed.as_deref().map(str::trim_end), links.len()),
            (Some("Plain."), 0),
            "body and edges are indexed together or not at all"
        );

        sql(&engine, "DROP TRIGGER refuse_links").await;
        engine.update_note_body(&note, edited).await.unwrap();
        assert_eq!(
            engine
                .storage
                .indexed_body(note.id)
                .await
                .unwrap()
                .as_deref(),
            Some(edited)
        );
        assert_eq!(engine.storage.note_links(note.id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_delete_the_database_refuses_can_be_retried() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        let created = engine
            .create_note(NewNoteInput {
                body: "Brief.".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let note = engine.get_note(created.id).await.unwrap().unwrap();

        sql(
            &engine,
            "CREATE TRIGGER refuse_delete BEFORE DELETE ON notes
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .await;
        assert!(engine.delete_note(&note).await.is_err());
        assert!(!created.file.exists());
        assert!(engine.get_note(note.id).await.unwrap().is_some());

        sql(&engine, "DROP TRIGGER refuse_delete").await;
        engine.delete_note(&note).await.unwrap();
        assert!(engine.get_note(note.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_created_book_whose_copy_is_refused_is_kept_on_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = engine_at(tmp.path()).await;
        let book = Book {
            title: Some("Middlemarch".into()),
            isbn_13: Some("9780141439549".into()),
            ..Default::default()
        };

        sql(
            &engine,
            "CREATE TRIGGER refuse_copies BEFORE INSERT ON copies
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .await;
        assert!(engine.save_book_unrecorded(&book).await.is_err());
        let isbn = book.isbn_13.as_deref().unwrap();
        assert!(
            engine
                .storage
                .find_book_by_isbn(isbn)
                .await
                .unwrap()
                .is_none()
        );

        sql(&engine, "DROP TRIGGER refuse_copies").await;
        let saved = engine.save_book_unrecorded(&book).await.unwrap();
        let copies = engine.copies_for_book(saved.id.unwrap()).await.unwrap();
        assert_eq!(copies.len(), 1, "the retry leaves the book in the library");
    }
}
