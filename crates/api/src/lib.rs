//! bookmole's API: the versioned surface, used in-process via [`Api`] or over a transport via
//! [`Api::call`]. `Api::dispatch` is pure fan-out; a rule in an arm would skip in-process callers.
//! Prefer a new request over a new field on an existing one: generated TypeScript requires it.

pub mod dto;
pub mod error;
pub mod protocol;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bookmole::{
    Book, BookFilter, BookQuery, CalibreImportOptions, DayRange, Engine, EngineError, EntryShow,
    EntrySort, FieldClaim, FileImportOptions, GoodreadsImportOptions, LearnedFrom, NewCopy,
    NoteKind, NoteRecord, NoteScope, RatingScale, ReadingFilter, ReadingQuery,
};

pub use bookmole::data_dir;
pub use bookmole::data_dir::{DATA_DIR_ENV, DataDirError, LIBRARY_FOLDER_NAME};
pub use dto::*;
pub use error::{ApiError, ApiResult, ErrorCode};
pub use protocol::{API_VERSION, CRATE_VERSION, Call, Outcome, Reply, Request, Response};

#[derive(Clone)]
pub struct Api {
    engine: Arc<Engine>,
}

impl Api {
    pub fn new(engine: Arc<Engine>) -> Api {
        Api { engine }
    }

    /// Open a library at `data_dir` without the caller naming the engine.
    /// `EngineConfig`'s other knobs are deliberately not parameters here.
    pub async fn open(data_dir: &Path) -> ApiResult<Api> {
        let engine = Engine::open(bookmole::EngineConfig::rooted_at(data_dir)).await?;
        Ok(Api::new(Arc::new(engine)))
    }

    /// The engine underneath, for a host that also drives the mount and vault watchers, which stay
    /// off the wire.
    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    pub fn api_version(&self) -> (u32, String) {
        (API_VERSION, CRATE_VERSION.to_string())
    }

    pub fn paths(&self) -> PathsDto {
        PathsDto {
            db_url: self.engine.db_url().to_string(),
            images_dir: self.engine.images_dir().display().to_string(),
            vault_dir: self.engine.vault_dir().display().to_string(),
            files_dir: self.engine.files_dir().display().to_string(),
            log_dir: self.engine.log_dir().display().to_string(),
            exports_dir: self.engine.exports_dir().display().to_string(),
        }
    }

    pub fn library_location(&self) -> LibraryLocationDto {
        self.engine.library_location().into()
    }

    pub fn has_google_api_key(&self) -> bool {
        self.engine.google_api_key().is_some()
    }

    pub fn set_google_api_key(&self, key: Option<String>) {
        self.engine.set_google_api_key(key);
    }

    pub async fn verify_google_key(&self, key: &str) -> ApiResult<bool> {
        match bookmole::verify_google_key(key).await {
            Ok(()) => Ok(true),
            // A refused key is an answer, not a failure.
            Err(_) => Ok(false),
        }
    }

    pub async fn search(&self, request: SearchRequestDto) -> ApiResult<SearchOutcomeDto> {
        Ok(self.engine.search(&request.into()).await?.into())
    }

    pub async fn lookup_isbn(&self, isbn: &str) -> ApiResult<Option<BookDto>> {
        Ok(self.engine.lookup_isbn(isbn).await?.map(Into::into))
    }

    pub async fn save_book(&self, book: BookDto) -> ApiResult<BookDto> {
        Ok(self.engine.save_book(&book.into()).await?.into())
    }

    pub async fn save_search_result(
        &self,
        book: BookDto,
        claims: Vec<FieldClaimDto>,
    ) -> ApiResult<BookDto> {
        let claims: Vec<FieldClaim> = claims.into_iter().map(Into::into).collect();
        Ok(self
            .engine
            .save_search_result(&book.into(), &claims)
            .await?
            .into())
    }

    pub async fn list_books(&self, query: BookQueryDto) -> ApiResult<Vec<BookDto>> {
        let query: BookQuery = query.into();
        Ok(map(self.engine.list_books(&query).await?))
    }

    pub async fn count_books(&self, filter: Option<BookFilterDto>) -> ApiResult<i64> {
        let filter: BookFilter = filter.unwrap_or_default().into();
        Ok(self.engine.count_books(&filter).await?)
    }

    pub async fn book_summaries(&self, book_ids: &[i64]) -> ApiResult<Vec<BookSummaryDto>> {
        Ok(map(self.engine.book_summaries(book_ids).await?))
    }

    pub async fn get_book(&self, id: i64) -> ApiResult<Option<BookDto>> {
        Ok(self.engine.get_book(id).await?.map(Into::into))
    }

    /// Empty means nothing matched; more than one means ambiguous. Both are answers, not errors.
    pub async fn resolve_books(&self, selector: &str) -> ApiResult<Vec<BookDto>> {
        Ok(map(self.engine.resolve_books(selector).await?))
    }

    pub async fn book_tags(&self, book_id: i64) -> ApiResult<Vec<BookTagDto>> {
        Ok(map(self.engine.book_tags(book_id).await?))
    }

    pub async fn enrich_book(&self, book_id: i64) -> ApiResult<EnrichReportDto> {
        Ok(self
            .engine
            .enrich_book_from_providers(book_id)
            .await?
            .into())
    }

    /// Record a correction as the user's. The reading projections on `fields` are ignored downstream.
    pub async fn set_book_fields(&self, book_id: i64, fields: BookDto) -> ApiResult<BookDto> {
        Ok(self
            .engine
            .set_book_fields(book_id, &Book::from(fields))
            .await?
            .into())
    }

    pub async fn field_provenance(&self, book_id: i64) -> ApiResult<Vec<FieldSourceDto>> {
        Ok(map(self.engine.field_provenance(book_id).await?))
    }

    pub async fn currently_reading(&self, limit: i64) -> ApiResult<Vec<OpenReadingDto>> {
        Ok(map(self.engine.currently_reading(limit).await?))
    }

    pub async fn delete_book(&self, id: i64) -> ApiResult<()> {
        Ok(self.engine.delete_book(id).await?)
    }

    pub async fn fetch_cover(&self, book_id: i64) -> ApiResult<Option<String>> {
        Ok(self
            .engine
            .fetch_cover(book_id)
            .await?
            .map(|p| p.display().to_string()))
    }

    pub async fn merge_books(&self, src: i64, dst: i64) -> ApiResult<MergeReportDto> {
        Ok(self.engine.merge_books(src, dst).await?.into())
    }

    pub async fn list_readings(&self, book_id: i64) -> ApiResult<Vec<ReadingDto>> {
        Ok(self
            .engine
            .readings_with_progress(book_id)
            .await?
            .into_iter()
            .map(|(r, p)| ReadingDto::new(r, p))
            .collect())
    }

    /// An inverted `finished_in` span is refused with `InvalidInput`, not turned into an empty page.
    pub async fn list_reading_rows(&self, query: ReadingQueryDto) -> ApiResult<Vec<ReadingRowDto>> {
        let query: ReadingQuery = query.try_into()?;
        Ok(map(self.engine.list_reading_rows(&query).await?))
    }

    pub async fn count_readings(&self, filter: Option<ReadingFilterDto>) -> ApiResult<i64> {
        let filter: ReadingFilter = filter
            .map(TryInto::try_into)
            .transpose()?
            .unwrap_or_default();
        Ok(self.engine.count_readings(&filter).await?)
    }

    /// An inverted `finished_in` span is `InvalidInput`, as for `list_reading_rows`.
    pub async fn reading_years(
        &self,
        filter: Option<ReadingFilterDto>,
    ) -> ApiResult<ReadingYearsDto> {
        let filter: ReadingFilter = filter
            .map(TryInto::try_into)
            .transpose()?
            .unwrap_or_default();
        Ok(self.engine.reading_years(&filter).await?.into())
    }

    pub async fn get_reading(&self, id: i64) -> ApiResult<Option<ReadingDto>> {
        Ok(self
            .engine
            .reading_with_progress(id)
            .await?
            .map(|(r, p)| ReadingDto::new(r, p)))
    }

    pub async fn active_reading(&self, book_id: i64) -> ApiResult<Option<ReadingDto>> {
        Ok(self
            .engine
            .active_reading_with_progress(book_id)
            .await?
            .map(|(r, p)| ReadingDto::new(r, p)))
    }

    pub async fn update_progress(
        &self,
        book_id: i64,
        page: Option<i64>,
        finished: Option<bool>,
    ) -> ApiResult<BookDto> {
        Ok(self
            .engine
            .update_progress(book_id, page, finished)
            .await?
            .into())
    }

    pub async fn reread(&self, book_id: i64) -> ApiResult<i64> {
        Ok(self.engine.reread(book_id).await?)
    }

    pub async fn finish_reading(
        &self,
        book_id: i64,
        finished_on: Option<String>,
    ) -> ApiResult<BookDto> {
        Ok(self
            .engine
            .finish_reading_on(book_id, finished_on.as_deref())
            .await?
            .into())
    }

    pub async fn put_down_reading(&self, book_id: i64) -> ApiResult<ReadingDto> {
        let (r, p) = self.engine.put_down(book_id).await?;
        Ok(ReadingDto::new(r, p))
    }

    pub async fn pick_up_reading(&self, book_id: i64) -> ApiResult<ReadingDto> {
        let (r, p) = self.engine.pick_up(book_id).await?;
        Ok(ReadingDto::new(r, p))
    }

    pub async fn set_page_count(&self, book_id: i64, page_count: i64) -> ApiResult<BookDto> {
        Ok(self
            .engine
            .set_page_count(book_id, page_count)
            .await?
            .into())
    }

    pub async fn list_highlights(&self, book_id: i64) -> ApiResult<Vec<HighlightDto>> {
        Ok(map(self.engine.list_highlights(book_id).await?))
    }

    pub async fn highlights_for_reading(&self, reading_id: i64) -> ApiResult<Vec<HighlightDto>> {
        Ok(map(self.engine.highlights_for_reading(reading_id).await?))
    }

    /// The passage chosen for this reading's card, or `None`. Not a `ReadingDto` field, so highlight
    /// text does not ride on every reading row.
    pub async fn card_passage(&self, reading_id: i64) -> ApiResult<Option<HighlightDto>> {
        Ok(self.engine.card_passage(reading_id).await?.map(Into::into))
    }

    pub async fn set_card_passage(
        &self,
        reading_id: i64,
        highlight_id: Option<i64>,
    ) -> ApiResult<()> {
        Ok(self
            .engine
            .set_card_passage(reading_id, highlight_id)
            .await?)
    }

    pub async fn card_passage_candidates(&self, reading_id: i64) -> ApiResult<Vec<HighlightDto>> {
        Ok(map(self.engine.card_passage_candidates(reading_id).await?))
    }

    pub async fn set_annotation(
        &self,
        highlight_id: i64,
        annotation: Option<&str>,
    ) -> ApiResult<()> {
        Ok(self.engine.set_annotation(highlight_id, annotation).await?)
    }

    pub async fn import_epub(&self, path: &Path) -> ApiResult<BookDto> {
        let book = self.engine.import_epub(path).await?;
        if let Some(id) = book.id {
            self.engine.add_file_to_book(id, path).await?;
        }
        Ok(book.into())
    }

    pub async fn import_file(&self, path: &Path, new: bool) -> ApiResult<FileImportReportDto> {
        Ok(self
            .engine
            .import_file(path, FileImportOptions { new })
            .await?
            .into())
    }

    pub async fn add_file_to_book(
        &self,
        book_id: i64,
        path: &Path,
    ) -> ApiResult<FileImportReportDto> {
        Ok(self.engine.add_file_to_book(book_id, path).await?.into())
    }

    pub async fn identify_file(&self, path: &Path) -> ApiResult<FileIdentityDto> {
        Ok(self.engine.identify_file(path).await?.into())
    }

    pub async fn expand_book_paths(&self, paths: &[String]) -> ApiResult<ExpandedPathsDto> {
        let paths = paths.iter().map(PathBuf::from).collect();
        Ok(self.engine.expand_book_paths(paths).await?.into())
    }

    pub async fn book_files(&self, book_id: i64) -> ApiResult<Vec<BookFileDto>> {
        Ok(map(self.engine.book_files(book_id).await?))
    }

    /// The book's chapter list from the first epub it owns. `None`: no readable file; `Some` with no
    /// entries: the epub has no navigable TOC.
    pub async fn table_of_contents(&self, book_id: i64) -> ApiResult<Option<TableOfContentsDto>> {
        Ok(self
            .engine
            .table_of_contents(book_id)
            .await?
            .map(Into::into))
    }

    pub async fn file_path(&self, sha256: &str) -> ApiResult<String> {
        let file = self.engine.book_file(sha256).await?.ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("no owned file with sha {sha256}"),
            )
        })?;
        Ok(self.engine.file_path(&file).display().to_string())
    }

    pub async fn remove_file(&self, sha256: &str) -> ApiResult<bool> {
        Ok(self.engine.remove_file(sha256).await?)
    }

    pub async fn import_koreader(&self, path: &Path, dry_run: bool) -> ApiResult<ImportReportDto> {
        Ok(self.engine.import_koreader(path, dry_run).await?.into())
    }

    pub async fn pull_book_from_sidecar(&self, path: &Path) -> ApiResult<PullReportDto> {
        Ok(self.engine.pull_book_from_sidecar(path).await?.into())
    }

    pub async fn sidecar_candidates(&self, path: &Path) -> ApiResult<Vec<MatchCandidateDto>> {
        Ok(map(self.engine.sidecar_candidates(path).await?))
    }

    /// Record that this sidecar is that book; returns the `partial_md5` the link was keyed on.
    pub async fn link_sidecar(&self, path: &Path, book_id: i64) -> ApiResult<String> {
        Ok(self.engine.link_sidecar(path, book_id).await?)
    }

    pub fn candidate_mounts(&self) -> Vec<String> {
        bookmole::candidate_mounts()
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    pub fn is_koreader_mount(&self, path: &Path) -> bool {
        bookmole::is_koreader_mount(path)
    }

    pub async fn scan_device(&self, root: &Path) -> ApiResult<DeviceScanDto> {
        Ok(self.engine.scan_device(root).await?.into())
    }

    pub async fn sync_device(&self, paths: &[PathBuf]) -> ApiResult<Vec<PullReportDto>> {
        Ok(map(self.engine.sync_device(paths).await?))
    }

    pub async fn plugin_status(&self, mount: &Path) -> ApiResult<PluginStatusDto> {
        Ok(self.engine.plugin_status(mount).await?.into())
    }

    /// Install or upgrade the plugin and pair with the reader. Never call this on a mount event.
    pub async fn install_plugin(&self, mount: &Path) -> ApiResult<InstallReportDto> {
        Ok(self.engine.install_plugin(mount).await?.into())
    }

    pub async fn uninstall_plugin(&self, mount: &Path) -> ApiResult<UninstallReportDto> {
        Ok(self.engine.uninstall_plugin(mount).await?.into())
    }

    pub async fn paired_devices(&self) -> ApiResult<Vec<PairedDeviceDto>> {
        Ok(map(self.engine.paired_devices().await?))
    }

    pub async fn forget_device(&self, device_id: &str) -> ApiResult<bool> {
        Ok(self.engine.forget_device(device_id).await?)
    }

    /// Name a reader. Blank clears it; `false` is *no such pairing*.
    pub async fn rename_device(&self, device_id: &str, label: &str) -> ApiResult<bool> {
        Ok(self.engine.rename_device(device_id, label).await?)
    }

    pub async fn sync_mount(&self, mount: &Path) -> ApiResult<MountSyncDto> {
        Ok(self.engine.sync_mount(mount).await?.into())
    }

    pub async fn listener_status(&self) -> ApiResult<ListenerStatusDto> {
        let status = self.engine.listener_status().await?;
        Ok(ListenerStatusDto::new(status, self.engine.lan_address()))
    }

    /// Open the door for `minutes`; `Some(0)` means always, not never.
    pub async fn start_listening(&self, minutes: Option<u32>) -> ApiResult<ListenerStatusDto> {
        let status = self.engine.start_listening(minutes).await?;
        Ok(ListenerStatusDto::new(status, self.engine.lan_address()))
    }

    /// Close the listener; idempotent.
    pub async fn stop_listening(&self) -> ApiResult<ListenerStatusDto> {
        let status = self.engine.stop_listening().await?;
        Ok(ListenerStatusDto::new(status, self.engine.lan_address()))
    }

    pub async fn pull_from_reader(&self, device_id: &str) -> ApiResult<Vec<PullReportDto>> {
        Ok(self
            .engine
            .pull_from_reader(device_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub async fn pair_with_reader(
        &self,
        code: &str,
        host: Option<&str>,
    ) -> ApiResult<PairedDeviceDto> {
        Ok(self.engine.pair_with_reader(code, host).await?.into())
    }

    /// Reading time from a device's `statistics.sqlite3`, as activity-log rows. Deliberately not part
    /// of the mount sync; a device without the statistics plugin gives an empty report and a warning.
    pub async fn import_device_statistics(&self, mount: &Path) -> ApiResult<StatsImportReportDto> {
        Ok(self.engine.import_device_statistics(mount).await?.into())
    }

    /// Rebuild the log from what is already stored. Idempotent.
    pub async fn refill_reading_events(&self) -> ApiResult<RefillReportDto> {
        Ok(self.engine.refill_reading_events().await?.into())
    }

    pub async fn reading_events(&self, book_id: i64) -> ApiResult<Vec<ReadingEventDto>> {
        Ok(map(self.engine.reading_events(book_id).await?))
    }

    /// The days are validated by `DayRange`, so an inverted or malformed span is `InvalidInput`.
    pub async fn activity_summary(&self, from: &str, to: &str) -> ApiResult<ActivitySummaryDto> {
        let range = DayRange::new(from, to)?;
        Ok(self.engine.activity_summary(&range).await?.into())
    }

    pub async fn activity_by_day(&self, from: &str, to: &str) -> ApiResult<Vec<DayActivityDto>> {
        let range = DayRange::new(from, to)?;
        Ok(map(self.engine.activity_by_day(&range).await?))
    }

    pub async fn activity_by_month(
        &self,
        from: &str,
        to: &str,
    ) -> ApiResult<Vec<MonthActivityDto>> {
        let range = DayRange::new(from, to)?;
        Ok(map(self.engine.activity_by_month(&range).await?))
    }

    /// What is worth noticing and has not been shown yet, newest first. Derived on every call and
    /// stored nowhere; there is no push channel, so polling is the only way to read it.
    pub async fn pending_moments(&self, limit: Option<i64>) -> ApiResult<Vec<MomentDto>> {
        Ok(map(self.engine.pending_moments(limit).await?))
    }

    /// Record that a moment was surfaced. Idempotent.
    pub async fn acknowledge_moment(&self, id: &str) -> ApiResult<()> {
        Ok(self.engine.acknowledge_moment(id).await?)
    }

    pub async fn create_note(&self, note: NewNoteDto) -> ApiResult<CreatedNoteDto> {
        Ok(self.engine.create_note(note.into()).await?.into())
    }

    /// Notes, newest first; both narrowings absent is every note. `book_id` and `reading_id` are
    /// alternatives, and giving both is refused.
    pub async fn list_notes(
        &self,
        book_id: Option<i64>,
        reading_id: Option<i64>,
        limit: Option<i64>,
    ) -> ApiResult<Vec<NoteDto>> {
        let scope = match (book_id, reading_id) {
            (Some(_), Some(_)) => {
                return Err(ApiError::new(
                    ErrorCode::InvalidInput,
                    "list_notes takes book_id or reading_id, not both — a reading names its book",
                ));
            }
            (_, Some(r)) => NoteScope::Reading(r),
            (b, None) => NoteScope::of_book(b),
        };
        Ok(map(self.engine.list_notes(scope, limit).await?))
    }

    pub async fn search_marks(
        &self,
        query: &str,
        source: Option<SearchSourceDto>,
        book_id: Option<i64>,
        limit: i64,
    ) -> ApiResult<Vec<SearchHitDto>> {
        Ok(map(self
            .engine
            .search_marks(query, source.map(Into::into), book_id, limit)
            .await?))
    }

    pub async fn get_note(&self, id: i64) -> ApiResult<Option<NoteDto>> {
        Ok(self.engine.get_note(id).await?.map(Into::into))
    }

    /// One older note for the caller's local day. `today` must be
    /// `YYYY-MM-DD`; anything else is `invalid_input`, never a guess at UTC.
    pub async fn resurface_note(&self, today: &str) -> ApiResult<Option<ResurfacedDto>> {
        let day = bookmole::parse_local_date(today)?;
        Ok(self.engine.resurface_note(day).await?.map(Into::into))
    }

    /// One older passage for the caller's local day — `resurface_note`'s twin.
    /// `today` must be `YYYY-MM-DD`; anything else is `invalid_input`.
    pub async fn resurface_passage(&self, today: &str) -> ApiResult<Option<ResurfacedPassageDto>> {
        let day = bookmole::parse_local_date(today)?;
        Ok(self.engine.resurface_passage(day).await?.map(Into::into))
    }

    pub async fn note_for_reading(
        &self,
        reading_id: i64,
        kind: NoteKindDto,
    ) -> ApiResult<Option<NoteDto>> {
        let kind: NoteKind = kind.into();
        Ok(self
            .engine
            .note_for_reading(reading_id, kind.as_str())
            .await?
            .map(Into::into))
    }

    pub async fn note_path(&self, note_id: i64) -> ApiResult<String> {
        let note = self.note(note_id).await?;
        Ok(self.engine.note_path(&note).display().to_string())
    }

    pub async fn note_body(&self, note_id: i64) -> ApiResult<String> {
        let note = self.note(note_id).await?;
        Ok(self.engine.note_body(&note)?)
    }

    pub async fn update_note_body(&self, note_id: i64, body: &str) -> ApiResult<()> {
        let note = self.note(note_id).await?;
        Ok(self.engine.update_note_body(&note, body).await?)
    }

    pub async fn delete_note(&self, note_id: i64) -> ApiResult<()> {
        let note = self.note(note_id).await?;
        Ok(self.engine.delete_note(&note).await?)
    }

    pub async fn refresh_note_from_disk(&self, note_id: i64) -> ApiResult<()> {
        let note = self.note(note_id).await?;
        Ok(self.engine.refresh_note_from_disk(&note).await?)
    }

    pub async fn outgoing_links(&self, note_id: i64) -> ApiResult<Vec<OutgoingLinkDto>> {
        Ok(map(self.engine.outgoing_links(note_id).await?))
    }

    pub async fn backlinks(&self, note_id: i64) -> ApiResult<Vec<NoteDto>> {
        Ok(map(self.engine.backlinks(note_id).await?))
    }

    pub async fn link_contexts(&self, note_id: i64) -> ApiResult<Vec<LinkContextDto>> {
        Ok(map(self.engine.link_contexts(note_id).await?))
    }

    /// A page of note rows. The two ids are alternatives, refused together as for `list_notes`.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_note_rows(
        &self,
        kinds: Option<Vec<NoteKindDto>>,
        book_id: Option<i64>,
        reading_id: Option<i64>,
        ids: Option<Vec<i64>>,
        until: Option<i64>,
        limit: i64,
    ) -> ApiResult<Vec<NoteRowDto>> {
        self.note_rows_page(kinds, book_id, reading_id, ids, until, None, limit)
            .await
    }

    /// [`Api::list_note_rows`] with a keyset cursor; `after: None` is the first page.
    #[allow(clippy::too_many_arguments)]
    pub async fn list_note_rows_after(
        &self,
        kinds: Option<Vec<NoteKindDto>>,
        book_id: Option<i64>,
        reading_id: Option<i64>,
        ids: Option<Vec<i64>>,
        until: Option<i64>,
        after: Option<NoteCursorDto>,
        limit: i64,
    ) -> ApiResult<Vec<NoteRowDto>> {
        self.note_rows_page(kinds, book_id, reading_id, ids, until, after, limit)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn note_rows_page(
        &self,
        kinds: Option<Vec<NoteKindDto>>,
        book_id: Option<i64>,
        reading_id: Option<i64>,
        ids: Option<Vec<i64>>,
        until: Option<i64>,
        after: Option<NoteCursorDto>,
        limit: i64,
    ) -> ApiResult<Vec<NoteRowDto>> {
        let scope = match (book_id, reading_id) {
            (Some(_), Some(_)) => {
                return Err(ApiError::new(
                    ErrorCode::InvalidInput,
                    "list_note_rows takes book_id or reading_id, not both — a reading names its book",
                ));
            }
            (_, Some(r)) => NoteScope::Reading(r),
            (b, None) => NoteScope::of_book(b),
        };
        let query = bookmole::NoteRowQuery {
            kinds: kinds.map(|ks| ks.into_iter().map(Into::into).collect()),
            scope,
            ids,
            until,
            after: after.map(|c| (c.at, c.id)),
            limit,
        };
        Ok(map(self.engine.note_rows(&query).await?))
    }

    pub async fn get_highlight(&self, id: i64) -> ApiResult<Option<HighlightDto>> {
        Ok(self.engine.get_highlight(id).await?.map(Into::into))
    }

    /// One face per asked id that names a book, in the order asked.
    pub async fn book_faces(&self, book_ids: &[i64]) -> ApiResult<Vec<BookFaceDto>> {
        Ok(map(self.engine.book_faces(book_ids).await?))
    }

    pub async fn note_books(&self) -> ApiResult<Vec<NoteBookDto>> {
        Ok(self
            .engine
            .note_books()
            .await?
            .into_iter()
            .map(|(book, notes)| NoteBookDto {
                book: book.into(),
                notes,
            })
            .collect())
    }

    pub async fn read_marks(&self, reading_id: i64, limit: i64) -> ApiResult<Vec<ReadMarkDto>> {
        Ok(map(self.engine.read_marks(reading_id, limit).await?))
    }

    pub async fn get_setting(&self, key: &str) -> ApiResult<Option<String>> {
        Ok(self.engine.get_setting(key).await?)
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> ApiResult<()> {
        Ok(self.engine.set_setting(key, value).await?)
    }

    pub async fn list_settings(&self, prefix: &str) -> ApiResult<Vec<SettingDto>> {
        Ok(map(self.engine.list_settings(prefix).await?))
    }

    pub async fn history_timeline(
        &self,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<Vec<HistoryReadDto>> {
        let span = bookmole::HistorySpan::new(since, until)?;
        Ok(map(self.engine.history_timeline(&span).await?))
    }

    pub async fn history_reading_now(&self) -> ApiResult<Vec<HistoryReadDto>> {
        Ok(map(self.engine.history_reading_now().await?))
    }

    pub async fn history_figures(
        &self,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<HistoryFiguresDto> {
        let span = bookmole::HistorySpan::new(since, until)?;
        Ok(self.engine.history_figures(&span).await?.into())
    }

    pub async fn history_records(
        &self,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<HistoryRecordsDto> {
        let span = bookmole::HistorySpan::new(since, until)?;
        Ok(self.engine.history_records(&span).await?.into())
    }

    fn series_query(
        metric: HistoryMetricDto,
        group: HistoryGroupDto,
        split: HistorySplitDto,
        compare: HistoryCompareDto,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<bookmole::SeriesQuery> {
        Ok(bookmole::SeriesQuery {
            metric: metric.into(),
            group: group.into(),
            split: split.into(),
            compare: compare.into(),
            span: bookmole::HistorySpan::new(since, until)?,
        })
    }

    pub async fn history_series(
        &self,
        metric: HistoryMetricDto,
        group: HistoryGroupDto,
        split: HistorySplitDto,
        compare: HistoryCompareDto,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<HistorySeriesDto> {
        let q = Self::series_query(metric, group, split, compare, since, until)?;
        Ok(self.engine.history_series(&q).await?.into())
    }

    pub async fn history_kept(
        &self,
        since: Option<i64>,
        until: Option<i64>,
        limit: i64,
    ) -> ApiResult<Vec<KeptRowDto>> {
        let span = bookmole::HistorySpan::new(since, until)?;
        Ok(map(self.engine.history_kept(&span, limit).await?))
    }

    pub async fn export_history_series(
        &self,
        metric: HistoryMetricDto,
        group: HistoryGroupDto,
        split: HistorySplitDto,
        compare: HistoryCompareDto,
        since: Option<i64>,
        until: Option<i64>,
    ) -> ApiResult<SavedCsvDto> {
        let q = Self::series_query(metric, group, split, compare, since, until)?;
        Ok(self.engine.export_history_series(&q).await?.into())
    }

    pub async fn library_table(&self) -> ApiResult<LibraryTableDto> {
        Ok(self.engine.library_table().await?.into())
    }

    pub async fn note_periods(&self) -> ApiResult<Vec<NotePeriodDto>> {
        Ok(self
            .engine
            .note_periods()
            .await?
            .into_iter()
            .map(|(year, month)| NotePeriodDto { year, month })
            .collect())
    }

    pub async fn note_periods_in(&self, utc_offset_minutes: i32) -> ApiResult<Vec<NotePeriodDto>> {
        Ok(self
            .engine
            .note_periods_in(utc_offset_minutes)
            .await?
            .into_iter()
            .map(|(year, month)| NotePeriodDto { year, month })
            .collect())
    }

    pub async fn matched_parts(&self, note_id: i64, query: &str) -> ApiResult<Vec<i64>> {
        Ok(self.engine.matched_parts(note_id, query).await?)
    }

    pub async fn threads_for(
        &self,
        note_ids: &[i64],
        since: Option<i64>,
    ) -> ApiResult<Vec<ThreadDto>> {
        Ok(map(self.engine.threads_for(note_ids, since).await?))
    }

    pub async fn links_from_notes(&self, note_ids: &[i64]) -> ApiResult<Vec<NoteLinkDto>> {
        Ok(map(self.engine.links_from_notes(note_ids).await?))
    }

    pub async fn note_graph(&self, scope: GraphScopeDto) -> ApiResult<NoteGraphDto> {
        Ok(self.engine.note_graph(scope.into()).await?.into())
    }

    pub async fn note_parts(&self, note_id: i64) -> ApiResult<Vec<NotePartDto>> {
        Ok(map(self.engine.note_parts(note_id).await?))
    }

    pub async fn continue_note(
        &self,
        note_id: i64,
        body: &str,
        page: Option<i64>,
    ) -> ApiResult<Vec<NotePartDto>> {
        Ok(map(self.engine.continue_note(note_id, body, page).await?))
    }

    pub async fn update_note_part(
        &self,
        note_id: i64,
        n: i64,
        body: &str,
    ) -> ApiResult<Vec<NotePartDto>> {
        Ok(map(self.engine.update_note_part(note_id, n, body).await?))
    }

    pub async fn unlinked_mentions(
        &self,
        note_id: i64,
        limit: i64,
    ) -> ApiResult<Vec<SearchHitDto>> {
        Ok(map(self.engine.unlinked_mentions(note_id, limit).await?))
    }

    pub async fn link_mention(&self, from_note_id: i64, to_note_id: i64) -> ApiResult<()> {
        Ok(self.engine.link_mention(from_note_id, to_note_id).await?)
    }

    pub async fn create_highlight(
        &self,
        book_id: i64,
        text: &str,
        page: Option<i64>,
        chapter: Option<&str>,
    ) -> ApiResult<HighlightDto> {
        Ok(self
            .engine
            .create_highlight(book_id, text, page, chapter)
            .await?
            .into())
    }

    pub async fn preview_links(
        &self,
        title: Option<&str>,
        body: &str,
    ) -> ApiResult<LinkPreviewDto> {
        Ok(self.engine.preview_links(title, body).await?.into())
    }

    pub async fn open_reflection(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> ApiResult<CreatedNoteDto> {
        Ok(self
            .engine
            .open_reflection(book_id, reading_id)
            .await?
            .into())
    }

    pub async fn open_review(
        &self,
        book_id: i64,
        reading_id: Option<i64>,
    ) -> ApiResult<CreatedNoteDto> {
        Ok(self.engine.open_review(book_id, reading_id).await?.into())
    }

    pub async fn set_rating(&self, note_id: i64, value: f64) -> ApiResult<()> {
        Ok(self.engine.set_rating(note_id, value).await?)
    }

    pub async fn review_rating(&self, note_id: i64) -> ApiResult<Option<RatingDto>> {
        Ok(self.engine.review_rating(note_id).await?.map(Into::into))
    }

    pub async fn clear_review_rating(&self, note_id: i64) -> ApiResult<bool> {
        Ok(self.engine.clear_review_rating(note_id).await?)
    }

    /// This review on Goodreads' 0–5 in quarters. An unmapped value is `ErrorCode::UnmappedRating`,
    /// never a rounded integer.
    pub async fn goodreads_rating(&self, note_id: i64) -> ApiResult<Option<f64>> {
        Ok(self.engine.goodreads_rating(note_id).await?)
    }

    pub async fn put_rating_scale(
        &self,
        name: &str,
        min: f64,
        max: f64,
        step: f64,
    ) -> ApiResult<RatingScaleDto> {
        Ok(self
            .engine
            .put_rating_scale(name, min, max, step)
            .await?
            .into())
    }

    pub async fn rating_scales(&self) -> ApiResult<Vec<RatingScaleDto>> {
        Ok(map(self.engine.rating_scales().await?))
    }

    pub async fn active_rating_scale(&self) -> ApiResult<Option<RatingScaleDto>> {
        Ok(self.engine.active_rating_scale().await?.map(Into::into))
    }

    pub async fn rating_scale_by_name(&self, name: &str) -> ApiResult<Option<RatingScaleDto>> {
        Ok(self
            .engine
            .rating_scale_by_name(name)
            .await?
            .map(Into::into))
    }

    pub async fn rating_map(&self, scale_id: i64) -> ApiResult<Vec<RatingMapEntryDto>> {
        Ok(self
            .engine
            .rating_map(scale_id)
            .await?
            .into_iter()
            .map(|(value, goodreads)| RatingMapEntryDto { value, goodreads })
            .collect())
    }

    /// Map one point of a scale onto a Goodreads value (0–5 in quarters). The scale is re-read by id,
    /// not echoed by the client.
    pub async fn map_rating(&self, scale_id: i64, value: f64, goodreads: f64) -> ApiResult<()> {
        let scale = self.scale(scale_id).await?;
        Ok(self.engine.map_rating(&scale, value, goodreads).await?)
    }

    pub async fn cite(&self, note_id: i64, highlight_id: i64) -> ApiResult<()> {
        Ok(self.engine.cite(note_id, highlight_id).await?)
    }

    pub async fn uncite(&self, note_id: i64, highlight_id: i64) -> ApiResult<bool> {
        Ok(self.engine.uncite(note_id, highlight_id).await?)
    }

    pub async fn citations_for(&self, note_id: i64) -> ApiResult<Vec<HighlightDto>> {
        Ok(map(self.engine.citations_for(note_id).await?))
    }

    /// Which passages each of these notes cites: one entry per id, in the order asked, empties
    /// included.
    pub async fn citations_for_notes(&self, note_ids: &[i64]) -> ApiResult<Vec<NoteCitationsDto>> {
        Ok(map(self.engine.citations_for_notes(note_ids).await?))
    }

    pub async fn import_goodreads(
        &self,
        path: &Path,
        dry_run: bool,
        create_ambiguous: bool,
    ) -> ApiResult<GoodreadsReportDto> {
        Ok(self
            .engine
            .import_goodreads(
                path,
                GoodreadsImportOptions {
                    dry_run,
                    create_ambiguous,
                },
            )
            .await?
            .into())
    }

    pub async fn export_goodreads(&self) -> ApiResult<(String, Vec<DiagnosticDto>)> {
        let (csv, warnings) = self.engine.export_goodreads().await?;
        Ok((csv, map(warnings)))
    }

    pub async fn link_goodreads_row(&self, external_id: &str, book_id: i64) -> ApiResult<()> {
        Ok(self.engine.link_goodreads_row(external_id, book_id).await?)
    }

    /// Which calibre tools this machine has. Both `None` is not an error: the feature shows as absent.
    pub fn calibre_status(&self) -> CalibreStatusDto {
        self.engine.calibre().into()
    }

    pub async fn convert_ebook(
        &self,
        input: &Path,
        output: &Path,
        overwrite: bool,
    ) -> ApiResult<String> {
        Ok(self
            .engine
            .convert_ebook(input, output, overwrite)
            .await?
            .display()
            .to_string())
    }

    pub async fn calibre_library(&self, library: Option<&Path>) -> ApiResult<Vec<CalibreBookDto>> {
        Ok(map(self.engine.calibre_library(library).await?))
    }

    pub async fn import_calibre_library(
        &self,
        library: Option<PathBuf>,
        dry_run: bool,
        create_ambiguous: bool,
        only: Vec<i64>,
    ) -> ApiResult<CalibreReportDto> {
        Ok(self
            .engine
            .import_calibre_library(&CalibreImportOptions {
                library,
                dry_run,
                create_ambiguous,
                only,
            })
            .await?
            .into())
    }

    pub async fn link_calibre_book(&self, uuid: &str, book_id: i64) -> ApiResult<()> {
        Ok(self.engine.link_calibre_book(uuid, book_id).await?)
    }

    /// Capture a card. `true` created, `false` you already had it. Both ids are re-read first, since
    /// nothing in the schema stops them naming different books.
    pub async fn create_flashcard(
        &self,
        book_id: i64,
        highlight_id: Option<i64>,
        word: &str,
        context: Option<&str>,
    ) -> ApiResult<bool> {
        Ok(self
            .engine
            .create_flashcard(book_id, highlight_id, word, context)
            .await?)
    }

    pub async fn list_flashcards(&self, include_exported: bool) -> ApiResult<Vec<FlashcardDto>> {
        Ok(map(self.engine.list_flashcards(include_exported).await?))
    }

    pub async fn list_flashcards_for_book(&self, book_id: i64) -> ApiResult<Vec<FlashcardDto>> {
        Ok(map(self.engine.list_flashcards_for_book(book_id).await?))
    }

    pub async fn export_flashcards(&self, include_exported: bool) -> ApiResult<(String, usize)> {
        Ok(self.engine.export_flashcards(include_exported).await?)
    }

    pub async fn export_anki(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> ApiResult<AnkiExportDto> {
        Ok(self
            .engine
            .export_anki(book_id, include_exported)
            .await?
            .into())
    }

    pub async fn save_anki_export(
        &self,
        book_id: Option<i64>,
        include_exported: bool,
    ) -> ApiResult<SavedAnkiExportDto> {
        Ok(self
            .engine
            .save_anki_export(book_id, include_exported)
            .await?
            .into())
    }

    async fn note(&self, note_id: i64) -> ApiResult<NoteRecord> {
        self.engine
            .get_note(note_id)
            .await?
            .ok_or_else(|| ApiError::from(EngineError::NotFound(format!("note id {note_id}"))))
    }

    async fn scale(&self, scale_id: i64) -> ApiResult<RatingScale> {
        self.engine
            .rating_scales()
            .await?
            .into_iter()
            .find(|s| s.id == scale_id)
            .ok_or_else(|| ApiError::from(EngineError::NotFound(format!("scale id {scale_id}"))))
    }

    /// Run one call and produce a reply that is always well-formed; never returns `Err`.
    pub async fn call(&self, call: Call) -> Reply {
        match self.dispatch(call.request).await {
            Ok(response) => Reply::ok(call.id, response),
            Err(error) => Reply::err(call.id, error),
        }
    }

    /// One arm per request, and no arm decides anything: a rule here would skip in-process callers.
    #[expect(clippy::too_many_lines)]
    pub async fn dispatch(&self, request: Request) -> ApiResult<Response> {
        use Request as R;
        Ok(match request {
            R::ApiVersion => {
                let (api, crate_version) = self.api_version();
                Response::Version { api, crate_version }
            }
            R::Paths => Response::Where(self.paths()),
            R::LibraryLocation => Response::LibraryLocation(self.library_location()),
            R::GoogleApiKey => Response::Bool(self.has_google_api_key()),
            R::SetGoogleApiKey { key } => {
                self.set_google_api_key(key);
                Response::Unit
            }
            R::VerifyGoogleKey { key } => Response::Bool(self.verify_google_key(&key).await?),

            R::Search { request } => Response::SearchOutcome(self.search(request).await?),
            R::LookupIsbn { isbn } => Response::Book(self.lookup_isbn(&isbn).await?),

            R::SaveBook { book } => Response::Book(Some(self.save_book(book).await?)),
            R::SaveSearchResult { book, claims } => {
                Response::Book(Some(self.save_search_result(book, claims).await?))
            }
            R::ListBooks {
                limit,
                sort,
                offset,
                filter,
            } => Response::Books(
                self.list_books(BookQueryDto {
                    sort,
                    filter,
                    limit,
                    offset,
                })
                .await?,
            ),
            R::CountBooks { filter } => Response::Count(self.count_books(filter).await?),
            R::BookSummaries { book_ids } => {
                Response::BookSummaries(self.book_summaries(&book_ids).await?)
            }
            R::GetBook { id } => Response::Book(self.get_book(id).await?),
            R::ResolveBooks { selector } => Response::Books(self.resolve_books(&selector).await?),
            R::BookTags { book_id } => Response::BookTags(self.book_tags(book_id).await?),
            R::EnrichBook { book_id } => Response::EnrichReport(self.enrich_book(book_id).await?),
            R::SetBookFields { book_id, fields } => {
                Response::Book(Some(self.set_book_fields(book_id, fields).await?))
            }
            R::FieldProvenance { book_id } => {
                Response::FieldProvenance(self.field_provenance(book_id).await?)
            }
            R::CurrentlyReading { limit } => {
                Response::OpenReadings(self.currently_reading(limit).await?)
            }
            R::DeleteBook { id } => {
                self.delete_book(id).await?;
                Response::Unit
            }
            R::FetchCover { book_id } => Response::MaybePath(self.fetch_cover(book_id).await?),
            R::MergeBooks { src, dst } => Response::MergeReport(self.merge_books(src, dst).await?),

            R::ListReadings { book_id } => Response::Readings(self.list_readings(book_id).await?),
            R::ListReadingRows {
                limit,
                sort,
                offset,
                filter,
            } => Response::ReadingRows(
                self.list_reading_rows(ReadingQueryDto {
                    sort,
                    filter,
                    limit,
                    offset,
                })
                .await?,
            ),
            R::CountReadings { filter } => Response::Count(self.count_readings(filter).await?),
            R::ReadingYears { filter } => Response::ReadingYears(self.reading_years(filter).await?),
            R::GetReading { id } => Response::Reading(self.get_reading(id).await?),
            R::ActiveReading { book_id } => Response::Reading(self.active_reading(book_id).await?),
            R::UpdateProgress {
                book_id,
                page,
                finished,
            } => Response::Book(Some(self.update_progress(book_id, page, finished).await?)),
            R::Reread { book_id } => Response::Id(self.reread(book_id).await?),
            R::PutDownReading { book_id } => {
                Response::Reading(Some(self.put_down_reading(book_id).await?))
            }
            R::PickUpReading { book_id } => {
                Response::Reading(Some(self.pick_up_reading(book_id).await?))
            }
            R::FinishReading {
                book_id,
                finished_on,
            } => Response::Book(Some(self.finish_reading(book_id, finished_on).await?)),
            R::SetPageCount {
                book_id,
                page_count,
            } => Response::Book(Some(self.set_page_count(book_id, page_count).await?)),

            R::ListHighlights { book_id } => {
                Response::Highlights(self.list_highlights(book_id).await?)
            }
            R::HighlightsForReading { reading_id } => {
                Response::Highlights(self.highlights_for_reading(reading_id).await?)
            }
            R::CardPassage { reading_id } => {
                Response::Highlight(self.card_passage(reading_id).await?)
            }
            R::SetCardPassage {
                reading_id,
                highlight_id,
            } => {
                self.set_card_passage(reading_id, highlight_id).await?;
                Response::Unit
            }
            R::CardPassageCandidates { reading_id } => {
                Response::Highlights(self.card_passage_candidates(reading_id).await?)
            }
            R::SetAnnotation {
                highlight_id,
                annotation,
            } => {
                self.set_annotation(highlight_id, annotation.as_deref())
                    .await?;
                Response::Unit
            }

            R::ImportEpub { path } => {
                Response::Book(Some(self.import_epub(Path::new(&path)).await?))
            }
            R::ImportFile { path, new } => {
                Response::FileImport(self.import_file(Path::new(&path), new).await?)
            }
            R::AddFileToBook { book_id, path } => {
                Response::FileImport(self.add_file_to_book(book_id, Path::new(&path)).await?)
            }
            R::IdentifyFile { path } => {
                Response::FileIdentity(self.identify_file(Path::new(&path)).await?)
            }
            R::ExpandBookPaths { paths } => {
                Response::ExpandedPaths(self.expand_book_paths(&paths).await?)
            }
            R::BookFiles { book_id } => Response::BookFiles(self.book_files(book_id).await?),
            R::TableOfContents { book_id } => {
                Response::TableOfContents(self.table_of_contents(book_id).await?)
            }
            R::FilePath { sha256 } => Response::Text(self.file_path(&sha256).await?),
            R::RemoveFile { sha256 } => Response::Bool(self.remove_file(&sha256).await?),

            R::ImportKoreader { path, dry_run } => {
                Response::ImportReport(self.import_koreader(Path::new(&path), dry_run).await?)
            }
            R::PullBookFromSidecar { path } => {
                Response::PullReport(self.pull_book_from_sidecar(Path::new(&path)).await?)
            }
            R::SidecarCandidates { path } => {
                Response::Candidates(self.sidecar_candidates(Path::new(&path)).await?)
            }
            R::LinkSidecar { path, book_id } => {
                Response::Text(self.link_sidecar(Path::new(&path), book_id).await?)
            }

            R::CandidateMounts => Response::Paths(self.candidate_mounts()),
            R::IsKoreaderMount { path } => Response::Bool(self.is_koreader_mount(Path::new(&path))),
            R::ScanDevice { root } => {
                Response::DeviceScan(self.scan_device(Path::new(&root)).await?)
            }
            R::PluginStatus { mount } => {
                Response::PluginStatus(self.plugin_status(Path::new(&mount)).await?)
            }
            R::InstallPlugin { mount } => {
                Response::PluginInstalled(self.install_plugin(Path::new(&mount)).await?)
            }
            R::UninstallPlugin { mount } => {
                Response::PluginUninstalled(self.uninstall_plugin(Path::new(&mount)).await?)
            }
            R::PairedDevices => Response::PairedDevices(self.paired_devices().await?),
            R::ForgetDevice { device_id } => Response::Bool(self.forget_device(&device_id).await?),
            R::RenameDevice { device_id, label } => {
                Response::Bool(self.rename_device(&device_id, &label).await?)
            }
            R::ListenerStatus => Response::ListenerStatus(self.listener_status().await?),
            R::StartListening { minutes } => {
                Response::ListenerStatus(self.start_listening(minutes).await?)
            }
            R::StopListening => Response::ListenerStatus(self.stop_listening().await?),
            R::PullFromReader { device_id } => {
                Response::PullReports(self.pull_from_reader(&device_id).await?)
            }
            R::PairWithReader { code, host } => {
                Response::PairedDevice(self.pair_with_reader(&code, host.as_deref()).await?)
            }
            R::SyncMount { mount } => {
                Response::MountSync(self.sync_mount(Path::new(&mount)).await?)
            }
            R::SyncDevice { paths } => {
                let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
                Response::PullReports(self.sync_device(&paths).await?)
            }
            R::ImportDeviceStatistics { mount } => {
                Response::StatsImport(self.import_device_statistics(Path::new(&mount)).await?)
            }

            R::RefillReadingEvents => Response::RefillReport(self.refill_reading_events().await?),
            R::ReadingEvents { book_id } => {
                Response::ReadingEvents(self.reading_events(book_id).await?)
            }
            R::PendingMoments { limit } => Response::Moments(self.pending_moments(limit).await?),
            R::AcknowledgeMoment { id } => {
                self.acknowledge_moment(&id).await?;
                Response::Unit
            }
            R::ActivitySummary { from, to } => {
                Response::ActivitySummary(self.activity_summary(&from, &to).await?)
            }
            R::ActivityByDay { from, to } => {
                Response::ActivityByDay(self.activity_by_day(&from, &to).await?)
            }
            R::ActivityByMonth { from, to } => {
                Response::ActivityByMonth(self.activity_by_month(&from, &to).await?)
            }

            R::CreateNote { note } => Response::CreatedNote(self.create_note(note).await?),
            R::ListNotes {
                book_id,
                reading_id,
                limit,
            } => Response::Notes(self.list_notes(book_id, reading_id, limit).await?),
            R::SearchMarks {
                query,
                source,
                book_id,
                limit,
            } => Response::SearchHits(self.search_marks(&query, source, book_id, limit).await?),
            R::GetNote { id } => Response::Note(self.get_note(id).await?),
            R::ResurfaceNote { today } => Response::Resurfaced(self.resurface_note(&today).await?),
            R::ResurfacePassage { today } => {
                Response::ResurfacedPassage(self.resurface_passage(&today).await?)
            }
            R::NoteForReading { reading_id, kind } => {
                Response::Note(self.note_for_reading(reading_id, kind).await?)
            }
            R::NotePath { note_id } => Response::Text(self.note_path(note_id).await?),
            R::NoteBody { note_id } => Response::Text(self.note_body(note_id).await?),
            R::UpdateNoteBody { note_id, body } => {
                self.update_note_body(note_id, &body).await?;
                Response::Unit
            }
            R::DeleteNote { note_id } => {
                self.delete_note(note_id).await?;
                Response::Unit
            }
            R::RefreshNoteFromDisk { note_id } => {
                self.refresh_note_from_disk(note_id).await?;
                Response::Unit
            }
            R::OutgoingLinks { note_id } => Response::Links(self.outgoing_links(note_id).await?),
            R::Backlinks { note_id } => Response::Notes(self.backlinks(note_id).await?),
            R::LinkContexts { note_id } => {
                Response::LinkContexts(self.link_contexts(note_id).await?)
            }
            R::ListNoteRows {
                kinds,
                book_id,
                reading_id,
                ids,
                until,
                limit,
            } => Response::NoteRows(
                self.list_note_rows(kinds, book_id, reading_id, ids, until, limit)
                    .await?,
            ),
            R::NotePeriods => Response::NotePeriods(self.note_periods().await?),
            R::NotePeriodsIn { utc_offset_minutes } => {
                Response::NotePeriods(self.note_periods_in(utc_offset_minutes).await?)
            }
            R::MatchedParts { note_id, query } => {
                Response::Parts(self.matched_parts(note_id, &query).await?)
            }
            R::HistoryReadingNow => Response::HistoryTimeline(self.history_reading_now().await?),
            R::ListNoteRowsAfter {
                kinds,
                book_id,
                reading_id,
                ids,
                until,
                after,
                limit,
            } => Response::NoteRows(
                self.list_note_rows_after(kinds, book_id, reading_id, ids, until, after, limit)
                    .await?,
            ),
            R::GetHighlight { id } => Response::Highlight(self.get_highlight(id).await?),
            R::BookFaces { book_ids } => Response::BookFaces(self.book_faces(&book_ids).await?),
            R::NoteBooks => Response::NoteBooks(self.note_books().await?),
            R::ReadMarks { reading_id, limit } => {
                Response::ReadMarks(self.read_marks(reading_id, limit).await?)
            }
            R::GetSetting { key } => Response::Setting(self.get_setting(&key).await?),
            R::SetSetting { key, value } => {
                self.set_setting(&key, &value).await?;
                Response::Unit
            }
            R::ListSettings { prefix } => Response::Settings(self.list_settings(&prefix).await?),
            R::HistoryTimeline { since, until } => {
                Response::HistoryTimeline(self.history_timeline(since, until).await?)
            }
            R::HistoryFigures { since, until } => {
                Response::HistoryFigures(self.history_figures(since, until).await?)
            }
            R::HistoryRecords { since, until } => {
                Response::HistoryRecords(self.history_records(since, until).await?)
            }
            R::HistorySeries {
                metric,
                group,
                split,
                compare,
                since,
                until,
            } => Response::HistorySeries(
                self.history_series(metric, group, split, compare, since, until)
                    .await?,
            ),
            R::HistoryKept {
                since,
                until,
                limit,
            } => Response::HistoryKept(self.history_kept(since, until, limit).await?),
            R::ExportHistorySeries {
                metric,
                group,
                split,
                compare,
                since,
                until,
            } => Response::SavedCsv(
                self.export_history_series(metric, group, split, compare, since, until)
                    .await?,
            ),
            R::LibraryTable => Response::LibraryTable(self.library_table().await?),

            R::ThreadsFor { note_ids, since } => {
                Response::Threads(self.threads_for(&note_ids, since).await?)
            }
            R::LinksFromNotes { note_ids } => {
                Response::NoteLinks(self.links_from_notes(&note_ids).await?)
            }
            R::NoteGraph { scope } => Response::NoteGraph(self.note_graph(scope).await?),
            R::NoteParts { note_id } => Response::NoteParts(self.note_parts(note_id).await?),
            R::ContinueNote {
                note_id,
                body,
                page,
            } => Response::NoteParts(self.continue_note(note_id, &body, page).await?),
            R::UpdateNotePart { note_id, n, body } => {
                Response::NoteParts(self.update_note_part(note_id, n, &body).await?)
            }
            R::UnlinkedMentions { note_id, limit } => {
                Response::SearchHits(self.unlinked_mentions(note_id, limit).await?)
            }
            R::LinkMention {
                from_note_id,
                to_note_id,
            } => {
                self.link_mention(from_note_id, to_note_id).await?;
                Response::Unit
            }
            R::CreateHighlight {
                book_id,
                text,
                page,
                chapter,
            } => Response::Highlight(Some(
                self.create_highlight(book_id, &text, page, chapter.as_deref())
                    .await?,
            )),
            R::PreviewLinks { title, body } => {
                Response::LinkPreview(self.preview_links(title.as_deref(), &body).await?)
            }

            R::OpenReflection {
                book_id,
                reading_id,
            } => Response::CreatedNote(self.open_reflection(book_id, reading_id).await?),
            R::OpenReview {
                book_id,
                reading_id,
            } => Response::CreatedNote(self.open_review(book_id, reading_id).await?),

            R::SetRating { note_id, value } => {
                self.set_rating(note_id, value).await?;
                Response::Unit
            }
            R::ReviewRating { note_id } => Response::Rating(self.review_rating(note_id).await?),
            R::ClearReviewRating { note_id } => {
                Response::Bool(self.clear_review_rating(note_id).await?)
            }
            R::GoodreadsRating { note_id } => {
                Response::GoodreadsRating(self.goodreads_rating(note_id).await?)
            }
            R::PutRatingScale {
                name,
                min,
                max,
                step,
            } => Response::RatingScale(Some(self.put_rating_scale(&name, min, max, step).await?)),
            R::RatingScales => Response::RatingScales(self.rating_scales().await?),
            R::ActiveRatingScale => Response::RatingScale(self.active_rating_scale().await?),
            R::RatingScaleByName { name } => {
                Response::RatingScale(self.rating_scale_by_name(&name).await?)
            }
            R::RatingMap { scale_id } => Response::RatingMap(self.rating_map(scale_id).await?),
            R::MapRating {
                scale_id,
                value,
                goodreads,
            } => {
                self.map_rating(scale_id, value, goodreads).await?;
                Response::Unit
            }

            R::Cite {
                note_id,
                highlight_id,
            } => {
                self.cite(note_id, highlight_id).await?;
                Response::Unit
            }
            R::Uncite {
                note_id,
                highlight_id,
            } => Response::Bool(self.uncite(note_id, highlight_id).await?),
            R::CitationsFor { note_id } => Response::Highlights(self.citations_for(note_id).await?),
            R::CitationsForNotes { note_ids } => {
                Response::NoteCitations(self.citations_for_notes(&note_ids).await?)
            }

            R::ImportGoodreads {
                path,
                dry_run,
                create_ambiguous,
            } => Response::GoodreadsReport(
                self.import_goodreads(Path::new(&path), dry_run, create_ambiguous)
                    .await?,
            ),
            R::ExportGoodreads => {
                let (csv, warnings) = self.export_goodreads().await?;
                Response::GoodreadsExport { csv, warnings }
            }
            R::LinkGoodreadsRow {
                external_id,
                book_id,
            } => {
                self.link_goodreads_row(&external_id, book_id).await?;
                Response::Unit
            }

            R::CalibreStatus => Response::CalibreStatus(self.calibre_status()),
            R::ConvertEbook {
                input,
                output,
                overwrite,
            } => Response::Text(
                self.convert_ebook(Path::new(&input), Path::new(&output), overwrite)
                    .await?,
            ),
            R::CalibreLibrary { library } => Response::CalibreLibrary(
                self.calibre_library(library.as_deref().map(Path::new))
                    .await?,
            ),
            R::ImportCalibreLibrary {
                library,
                dry_run,
                create_ambiguous,
                only,
            } => Response::CalibreReport(
                self.import_calibre_library(
                    library.map(PathBuf::from),
                    dry_run,
                    create_ambiguous,
                    only,
                )
                .await?,
            ),
            R::LinkCalibreBook { uuid, book_id } => {
                self.link_calibre_book(&uuid, book_id).await?;
                Response::Unit
            }

            R::CreateFlashcard {
                book_id,
                highlight_id,
                word,
                context,
            } => Response::Bool(
                self.create_flashcard(book_id, highlight_id, &word, context.as_deref())
                    .await?,
            ),
            R::ListFlashcards { include_exported } => {
                Response::Flashcards(self.list_flashcards(include_exported).await?)
            }
            R::ListFlashcardsForBook { book_id } => {
                Response::Flashcards(self.list_flashcards_for_book(book_id).await?)
            }
            R::ExportFlashcards { include_exported } => {
                let (tsv, count) = self.export_flashcards(include_exported).await?;
                Response::FlashcardExport { tsv, count }
            }
            R::ExportAnki {
                book_id,
                include_exported,
            } => Response::AnkiExport(self.export_anki(book_id, include_exported).await?),
            R::SaveAnkiExport {
                book_id,
                include_exported,
            } => Response::AnkiSaved(self.save_anki_export(book_id, include_exported).await?),

            R::ListLists { sort, preview } => {
                Response::ListSummaries(self.list_lists(sort, preview).await?)
            }
            R::GetList { id } => Response::List(self.get_list(id).await?),
            R::CreateList { name, description } => {
                Response::List(self.create_list(&name, description.as_deref()).await?)
            }
            R::UpdateList {
                id,
                name,
                description,
            } => Response::List(self.update_list(id, &name, description.as_deref()).await?),
            R::DeleteList { id } => {
                self.delete_list(id).await?;
                Response::Unit
            }
            R::ListEntries {
                list_id,
                show,
                sort,
            } => Response::ListEntries(self.list_entries(list_id, show, sort).await?),
            R::AddToList {
                list_id,
                book_id,
                note,
            } => Response::ListEntry(self.add_to_list(list_id, book_id, note.as_deref()).await?),
            R::RemoveFromList { list_id, book_id } => {
                self.remove_from_list(list_id, book_id).await?;
                Response::Unit
            }
            R::MoveInList {
                list_id,
                book_id,
                to,
            } => {
                self.move_in_list(list_id, book_id, to).await?;
                Response::Unit
            }
            R::SetListNote {
                list_id,
                book_id,
                note,
            } => {
                self.set_list_note(list_id, book_id, note.as_deref())
                    .await?;
                Response::Unit
            }
            R::AddCandidateToList {
                list_id,
                book,
                note,
            } => Response::ListEntry(
                self.add_candidate_to_list(list_id, book, note.as_deref())
                    .await?,
            ),
            R::AddSearchResultToList {
                list_id,
                book,
                claims,
                note,
            } => Response::ListEntry(
                self.add_search_result_to_list(list_id, book, claims, note.as_deref())
                    .await?,
            ),
            R::KeepCopyHere { copy_id } => Response::Copy(self.keep_copy_here(copy_id).await?),
            R::HighlightOrigins { book_id } => {
                Response::HighlightOrigins(self.highlight_origins(book_id).await?)
            }
            R::SamePassages { book_id } => {
                Response::SamePassages(self.engine.same_passages(book_id).await?)
            }
            R::ListsForBook { book_id } => {
                Response::ListMemberships(self.lists_for_book(book_id).await?)
            }
            R::ListTimeline { list_id } => {
                Response::ListTimeline(self.list_timeline(list_id).await?)
            }
            R::ShelfSuggestions { preview } => {
                Response::ShelfSuggestions(self.shelf_suggestions(preview).await?)
            }
            R::KeepShelf { source, tag } => Response::List(self.keep_shelf(&source, &tag).await?),
            R::DismissShelf { source, tag } => {
                self.dismiss_shelf(&source, &tag).await?;
                Response::Unit
            }

            R::CopiesForBook { book_id } => Response::Copies(self.copies_for_book(book_id).await?),
            R::AddCopy {
                book_id,
                form,
                tenure,
                have_now,
                from_whom,
                app,
                location,
                started_at,
                due_at,
                note,
            } => Response::Copy(
                self.add_copy(NewCopyDto {
                    book_id,
                    form,
                    tenure,
                    have_now,
                    from_whom,
                    app,
                    location,
                    started_at,
                    due_at,
                    note,
                })
                .await?,
            ),
            R::UpdateCopy {
                copy_id,
                form,
                tenure,
                from_whom,
                app,
                location,
                started_at,
                due_at,
                ended_at,
                lent_to,
                unreadable,
                note,
            } => Response::Copy(
                self.update_copy(
                    copy_id,
                    CopyEditDto {
                        form,
                        tenure,
                        from_whom,
                        app,
                        location,
                        started_at,
                        due_at,
                        ended_at,
                        lent_to,
                        unreadable,
                        note,
                    },
                )
                .await?,
            ),
            R::EndCopy { copy_id, ended_at } => {
                Response::Copy(self.end_copy(copy_id, ended_at).await?)
            }
            R::ResumeCopy { copy_id } => Response::Copy(self.resume_copy(copy_id).await?),
            R::DeleteCopy { copy_id } => {
                self.delete_copy(copy_id).await?;
                Response::Unit
            }
            R::BookStanding { book_id } => {
                Response::BookStanding(self.book_standing(book_id).await?)
            }
            R::LibraryCopies => Response::LibraryCopies(self.library_copies().await?),

            R::ImportPreviewAnnotations { book_id } => {
                Response::PreviewImport(self.import_preview_annotations(book_id).await?)
            }
            R::ImportPreviewFile { book_id, path } => {
                Response::HighlightImport(self.import_preview_file(book_id, &path).await?)
            }
            R::ImportAppleBooks { dry_run } => {
                Response::AppleBooksReport(self.import_apple_books(dry_run).await?)
            }
            R::LinkAppleBooksAsset { asset_id, book_id } => {
                Response::Copy(self.link_apple_books_asset(&asset_id, book_id).await?)
            }
            R::FindKindleClippings => Response::Paths(self.find_kindle_clippings()),
            R::ImportKindleClippings { path, dry_run } => {
                Response::KindleClippingsReport(self.import_kindle_clippings(&path, dry_run).await?)
            }
            R::ImportMountedKindleClippings { dry_run } => Response::KindleClippingsReports(
                self.import_mounted_kindle_clippings(dry_run).await?,
            ),
            R::AttachKindleTitle {
                path,
                title_line,
                book_id,
            } => Response::KindleClippingsReport(
                self.attach_kindle_title(&path, &title_line, book_id)
                    .await?,
            ),
            R::ImportKindleNotebook {
                path,
                book_id,
                dry_run,
            } => Response::KindleNotebookReport(
                self.import_kindle_notebook(&path, book_id, dry_run).await?,
            ),

            R::CopyFiles { book_id } => Response::CopyFiles(self.copy_files(book_id).await?),
            R::CopyHighlightSources { book_id } => {
                Response::CopyHighlightSources(self.copy_highlight_sources(book_id).await?)
            }
            R::RelocateCopyFile { copy_id, path } => {
                Response::Copy(self.relocate_copy_file(copy_id, &path).await?)
            }
            R::ResolveLibraryBooks { selector } => {
                Response::Books(self.resolve_library_books(&selector).await?)
            }
        })
    }
}

fn map<T, U: From<T>>(items: Vec<T>) -> Vec<U> {
    items.into_iter().map(U::from).collect()
}

#[derive(Debug, Clone)]
pub struct NewCopyDto {
    pub book_id: i64,
    pub form: Option<CopyFormDto>,
    pub tenure: TenureDto,
    pub have_now: Option<bool>,
    pub from_whom: Option<String>,
    pub app: Option<String>,
    pub location: Option<String>,
    pub started_at: Option<i64>,
    pub due_at: Option<i64>,
    pub note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CopyEditDto {
    pub form: Option<CopyFormDto>,
    pub tenure: TenureDto,
    pub from_whom: Option<String>,
    pub app: Option<String>,
    pub location: Option<String>,
    pub started_at: Option<i64>,
    pub due_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub lent_to: Option<String>,
    pub unreadable: bool,
    pub note: Option<String>,
}

fn copy_not_found(id: i64) -> ApiError {
    ApiError::from(EngineError::NotFound(format!("copy id {id}")))
}

impl Api {
    pub async fn list_lists(
        &self,
        sort: ListSortDto,
        preview: u8,
    ) -> ApiResult<Vec<ListSummaryDto>> {
        Ok(map(self
            .engine
            .list_lists(sort.into(), usize::from(preview))
            .await?))
    }

    async fn list_dto(&self, list: bookmole::BookList) -> ApiResult<ListDto> {
        let entries = self
            .engine
            .list_entries(list.id, EntryShow::All, EntrySort::Yours)
            .await?;
        let in_library = entries.iter().filter(|e| e.in_library).count() as i64;
        let not_in_library = entries.len() as i64 - in_library;
        let ready = self.engine.list_timeline(list.id).await?.ready;
        Ok(ListDto::new(
            list,
            entries.len() as i64,
            not_in_library,
            in_library,
            ready,
        ))
    }

    pub async fn get_list(&self, id: i64) -> ApiResult<ListDto> {
        let list = self.engine.get_list(id).await?;
        self.list_dto(list).await
    }

    pub async fn create_list(&self, name: &str, description: Option<&str>) -> ApiResult<ListDto> {
        let list = self.engine.create_list(name, description).await?;
        self.list_dto(list).await
    }

    pub async fn update_list(
        &self,
        id: i64,
        name: &str,
        description: Option<&str>,
    ) -> ApiResult<ListDto> {
        let list = self.engine.update_list(id, name, description).await?;
        self.list_dto(list).await
    }

    pub async fn delete_list(&self, id: i64) -> ApiResult<()> {
        Ok(self.engine.delete_list(id).await?)
    }

    pub async fn list_entries(
        &self,
        list_id: i64,
        show: EntryShowDto,
        sort: EntrySortDto,
    ) -> ApiResult<Vec<ListEntryDto>> {
        Ok(map(self
            .engine
            .list_entries(list_id, show.into(), sort.into())
            .await?))
    }

    pub async fn add_to_list(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> ApiResult<ListEntryDto> {
        Ok(self
            .engine
            .add_to_list(list_id, book_id, note)
            .await?
            .into())
    }

    pub async fn add_candidate_to_list(
        &self,
        list_id: i64,
        book: BookDto,
        note: Option<&str>,
    ) -> ApiResult<ListEntryDto> {
        Ok(self
            .engine
            .add_candidate_to_list(list_id, &book.into(), note)
            .await?
            .into())
    }

    pub async fn add_search_result_to_list(
        &self,
        list_id: i64,
        book: BookDto,
        claims: Vec<FieldClaimDto>,
        note: Option<&str>,
    ) -> ApiResult<ListEntryDto> {
        let claims: Vec<FieldClaim> = claims.into_iter().map(Into::into).collect();
        Ok(self
            .engine
            .add_search_result_to_list(list_id, &book.into(), &claims, note)
            .await?
            .into())
    }

    pub async fn remove_from_list(&self, list_id: i64, book_id: i64) -> ApiResult<()> {
        Ok(self.engine.remove_from_list(list_id, book_id).await?)
    }

    pub async fn move_in_list(&self, list_id: i64, book_id: i64, to: u32) -> ApiResult<()> {
        Ok(self
            .engine
            .move_in_list(list_id, book_id, to as usize)
            .await?)
    }

    pub async fn set_list_note(
        &self,
        list_id: i64,
        book_id: i64,
        note: Option<&str>,
    ) -> ApiResult<()> {
        Ok(self.engine.set_list_note(list_id, book_id, note).await?)
    }

    pub async fn lists_for_book(&self, book_id: i64) -> ApiResult<Vec<ListMembershipDto>> {
        Ok(map(self.engine.lists_for_book(book_id).await?))
    }

    pub async fn list_timeline(&self, list_id: i64) -> ApiResult<ListTimelineDto> {
        Ok(self.engine.list_timeline(list_id).await?.into())
    }

    pub async fn shelf_suggestions(&self, preview: u8) -> ApiResult<Vec<ShelfSuggestionDto>> {
        Ok(map(self
            .engine
            .shelf_suggestions(usize::from(preview))
            .await?))
    }

    pub async fn keep_shelf(&self, source: &str, tag: &str) -> ApiResult<ListDto> {
        let list = self.engine.keep_shelf(source, tag).await?;
        self.list_dto(list).await
    }

    pub async fn dismiss_shelf(&self, source: &str, tag: &str) -> ApiResult<()> {
        Ok(self.engine.dismiss_shelf(source, tag).await?)
    }
}

impl Api {
    async fn copy_dto(&self, copy: bookmole::Copy) -> ApiResult<CopyDto> {
        let facts = self.engine.copy_facts(&copy).await?;
        Ok(CopyDto::from_parts(copy, facts))
    }

    pub async fn copies_for_book(&self, book_id: i64) -> ApiResult<Vec<CopyDto>> {
        let mut out = Vec::new();
        for copy in self.engine.copies_for_book(book_id).await? {
            out.push(self.copy_dto(copy).await?);
        }
        Ok(out)
    }

    /// Keep a copy here. An engine error (a copy with no file on this computer is `invalid_input`)
    /// passes through.
    pub async fn keep_copy_here(&self, copy_id: i64) -> ApiResult<CopyDto> {
        self.engine.keep_copy_here(copy_id).await?;
        let copy = self.copy(copy_id).await?;
        self.copy_dto(copy).await
    }

    pub async fn highlight_origins(&self, book_id: i64) -> ApiResult<Vec<HighlightOriginFactDto>> {
        Ok(map(self.engine.highlight_origins(book_id).await?))
    }

    /// Record a copy. `learned_from` is always `user` here.
    pub async fn add_copy(&self, new: NewCopyDto) -> ApiResult<CopyDto> {
        // A copy of a missing book is `not_found`, not a foreign-key failure.
        if self.engine.get_book(new.book_id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {}", new.book_id)).into());
        }
        let mut copy = NewCopy::new(
            new.book_id,
            new.form.map(Into::into),
            new.tenure.into(),
            LearnedFrom::User,
        );
        copy.have_now = new.have_now;
        copy.from_whom = new.from_whom;
        copy.app = new.app;
        copy.location = new.location;
        copy.started_at = new.started_at;
        copy.due_at = new.due_at;
        copy.note = new.note;
        let copy = self.engine.add_copy(&copy).await?;
        self.copy_dto(copy).await
    }

    async fn copy(&self, id: i64) -> ApiResult<bookmole::Copy> {
        self.engine
            .get_copy(id)
            .await?
            .ok_or_else(|| copy_not_found(id))
    }

    async fn write_copy(&self, copy: bookmole::Copy) -> ApiResult<CopyDto> {
        if !self.engine.update_copy(&copy).await? {
            return Err(copy_not_found(copy.id));
        }
        let fresh = self.copy(copy.id).await?;
        self.copy_dto(fresh).await
    }

    /// Write a copy's user-entered fields as given; `have_now` is untouched.
    pub async fn update_copy(&self, copy_id: i64, edit: CopyEditDto) -> ApiResult<CopyDto> {
        let mut copy = self.copy(copy_id).await?;
        copy.form = edit.form.map(Into::into);
        copy.tenure = edit.tenure.into();
        copy.from_whom = edit.from_whom;
        copy.app = edit.app;
        copy.location = edit.location;
        copy.started_at = edit.started_at;
        copy.due_at = edit.due_at;
        copy.ended_at = edit.ended_at;
        copy.lent_to = edit.lent_to;
        copy.unreadable = edit.unreadable;
        copy.note = edit.note;
        self.write_copy(copy).await
    }

    pub async fn end_copy(&self, copy_id: i64, ended_at: Option<i64>) -> ApiResult<CopyDto> {
        let mut copy = self.copy(copy_id).await?;
        copy.have_now = Some(false);
        if ended_at.is_some() {
            copy.ended_at = ended_at;
        }
        self.write_copy(copy).await
    }

    pub async fn resume_copy(&self, copy_id: i64) -> ApiResult<CopyDto> {
        let mut copy = self.copy(copy_id).await?;
        copy.have_now = Some(true);
        copy.ended_at = None;
        self.write_copy(copy).await
    }

    pub async fn delete_copy(&self, copy_id: i64) -> ApiResult<()> {
        if self.engine.delete_copy(copy_id).await? {
            Ok(())
        } else {
            Err(copy_not_found(copy_id))
        }
    }

    pub async fn book_standing(&self, book_id: i64) -> ApiResult<BookStandingDto> {
        Ok(self.engine.book_standing(book_id).await?.into())
    }

    pub async fn library_copies(&self) -> ApiResult<LibraryCopiesDto> {
        Ok(self.engine.library_copies().await?.into())
    }
}

// A client's `path` is a string here and the engine re-checks it (exists, is a file, right kind).

impl Api {
    pub async fn import_preview_annotations(&self, book_id: i64) -> ApiResult<PreviewImportDto> {
        Ok(self
            .engine
            .import_preview_annotations(book_id)
            .await?
            .into())
    }

    pub async fn import_preview_file(
        &self,
        book_id: i64,
        path: &str,
    ) -> ApiResult<HighlightImportDto> {
        Ok(self
            .engine
            .import_preview_file(book_id, Path::new(path))
            .await?
            .into())
    }

    pub async fn import_apple_books(&self, dry_run: bool) -> ApiResult<AppleBooksReportDto> {
        Ok(self.engine.import_apple_books(dry_run).await?.into())
    }

    pub async fn link_apple_books_asset(&self, asset_id: &str, book_id: i64) -> ApiResult<CopyDto> {
        let copy_id = self
            .engine
            .link_apple_books_asset(asset_id, book_id)
            .await?;
        let copy = self.copy(copy_id).await?;
        self.copy_dto(copy).await
    }

    pub fn find_kindle_clippings(&self) -> Vec<String> {
        self.engine
            .find_kindle_clippings()
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    pub async fn import_kindle_clippings(
        &self,
        path: &str,
        dry_run: bool,
    ) -> ApiResult<KindleClippingsReportDto> {
        let path = Path::new(path);
        let report = self.engine.import_kindle_clippings(path, dry_run).await?;
        Ok(KindleClippingsReportDto::new(path, report))
    }

    pub async fn import_mounted_kindle_clippings(
        &self,
        dry_run: bool,
    ) -> ApiResult<Vec<KindleClippingsReportDto>> {
        Ok(self
            .engine
            .import_mounted_kindle_clippings(dry_run)
            .await?
            .into_iter()
            .map(|(path, report)| KindleClippingsReportDto::new(&path, report))
            .collect())
    }

    /// Never a dry run: it answers a report the reader has already seen.
    pub async fn attach_kindle_title(
        &self,
        path: &str,
        title_line: &str,
        book_id: i64,
    ) -> ApiResult<KindleClippingsReportDto> {
        let path = Path::new(path);
        let report = self
            .engine
            .attach_kindle_title(path, title_line, book_id, false)
            .await?;
        Ok(KindleClippingsReportDto::new(path, report))
    }

    pub async fn import_kindle_notebook(
        &self,
        path: &str,
        book_id: Option<i64>,
        dry_run: bool,
    ) -> ApiResult<KindleNotebookReportDto> {
        Ok(self
            .engine
            .import_kindle_notebook(Path::new(path), book_id, dry_run)
            .await?
            .into())
    }

    pub async fn resolve_library_books(&self, selector: &str) -> ApiResult<Vec<BookDto>> {
        Ok(map(self.engine.resolve_library_books(selector).await?))
    }
}

impl Api {
    pub async fn copy_files(&self, book_id: i64) -> ApiResult<Vec<CopyFileDto>> {
        Ok(map(self.engine.copy_files(book_id).await?))
    }

    /// Where a book's highlights came from, per (copy, origin). No counts.
    pub async fn copy_highlight_sources(
        &self,
        book_id: i64,
    ) -> ApiResult<Vec<CopyHighlightSourceDto>> {
        Ok(map(self.engine.copy_highlight_sources(book_id).await?))
    }

    pub async fn relocate_copy_file(&self, copy_id: i64, path: &str) -> ApiResult<CopyDto> {
        let copy = self
            .engine
            .relocate_copy_file(copy_id, Path::new(path))
            .await?;
        self.copy_dto(copy).await
    }
}
