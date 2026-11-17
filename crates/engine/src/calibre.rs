//! Calibre through its command line: `ebook-convert` for conversion, `calibredb list
//! --for-machine` to import a curated library. Feature-detected once per run
//! ([`Calibre::detect`]), never a hard dependency.
//! Calibre's rating is deliberately not imported: a rating lives on a review of a reading.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::book::{Book, normalize_isbn};
use crate::copies::{CopyForm, LearnedFrom, NewCopy, NewPlace, PlaceKind, Tenure};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::error::{EngineError, Result};
use crate::koreader::{self, MatchCandidate};
use crate::matching::Query;
use crate::providers::normalize_language;
use crate::storage::{LinkedBy, Source, Storage};
use crate::{Engine, partial_md5};

pub const SOURCE: &str = "calibre";

const CONVERT: &str = "ebook-convert";
const CALIBREDB: &str = "calibredb";

/// Where calibre installs itself when not on `PATH`; on macOS `calibre.app` adds nothing to `PATH`.
const KNOWN_DIRS: [&str; 3] = [
    "/Applications/calibre.app/Contents/MacOS",
    "/opt/calibre",
    "/usr/lib/calibre",
];

/// Two separate `Option`s so a partial install degrades to the half that works.
#[derive(Debug, Clone, Default)]
pub struct Calibre {
    convert: Option<PathBuf>,
    calibredb: Option<PathBuf>,
}

impl Calibre {
    /// `extra` is searched first so tests can point at fake binaries without mutating the global
    /// `PATH`.
    pub fn detect(extra: Option<&Path>) -> Calibre {
        Calibre {
            convert: find_tool(CONVERT, extra),
            calibredb: find_tool(CALIBREDB, extra),
        }
    }

    pub fn at(convert: Option<PathBuf>, calibredb: Option<PathBuf>) -> Calibre {
        Calibre { convert, calibredb }
    }

    pub fn can_convert(&self) -> bool {
        self.convert.is_some()
    }

    pub fn can_read_library(&self) -> bool {
        self.calibredb.is_some()
    }

    pub fn convert_path(&self) -> Option<&Path> {
        self.convert.as_deref()
    }

    pub fn calibredb_path(&self) -> Option<&Path> {
        self.calibredb.as_deref()
    }

    fn require(tool: &str, found: Option<&Path>) -> Result<PathBuf> {
        found
            .map(Path::to_path_buf)
            .ok_or_else(|| EngineError::CalibreMissing {
                tool: tool.to_string(),
            })
    }
}

/// `PATH`, then known install dirs. Executability is checked: a directory named `calibredb` is
/// not a program.
fn find_tool(name: &str, extra: Option<&Path>) -> Option<PathBuf> {
    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    extra
        .map(Path::to_path_buf)
        .into_iter()
        .chain(path_dirs)
        .chain(KNOWN_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Deliberately bare: formats come from the extensions and no calibre option is sent.
pub fn convert_argv(input: &Path, output: &Path) -> Vec<String> {
    vec![input.display().to_string(), output.display().to_string()]
}

/// Refuses to overwrite an existing output unless `overwrite` is set; `ebook-convert` would
/// replace it silently.
pub async fn convert(
    calibre: &Calibre,
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<PathBuf> {
    let bin = Calibre::require(CONVERT, calibre.convert_path())?;
    if !input.is_file() {
        return Err(EngineError::InvalidInput(format!(
            "{} is not a file to convert",
            input.display()
        )));
    }
    // Calibre infers the format from the extension; without one the failure would surface from inside
    // calibre.
    if output.extension().is_none() {
        return Err(EngineError::InvalidInput(format!(
            "{} has no extension, and that is what calibre reads the output format from",
            output.display()
        )));
    }
    if output.exists() && !overwrite {
        return Err(EngineError::InvalidInput(format!(
            "{} already exists",
            output.display()
        )));
    }

    run(&bin, CONVERT, &convert_argv(input, output)).await?;
    if !output.is_file() {
        return Err(EngineError::Calibre {
            tool: CONVERT.to_string(),
            message: format!("reported success but wrote no {}", output.display()),
        });
    }
    Ok(output.to_path_buf())
}

/// `--fields all` rather than a named subset: the shape of the output changes with the field list.
pub fn list_argv(library: Option<&Path>) -> Vec<String> {
    let mut argv = Vec::new();
    if let Some(lib) = library {
        argv.push("--with-library".to_string());
        argv.push(lib.display().to_string());
    }
    argv.push("list".to_string());
    argv.push("--for-machine".to_string());
    argv.push("--fields".to_string());
    argv.push("all".to_string());
    argv
}

/// `calibredb --with-library /typo` creates a library there and reports `[]` with exit 0, so a path
/// without `metadata.db` is refused before the binary runs.
fn library_root(library: Option<&Path>) -> Result<Option<&Path>> {
    let Some(lib) = library else { return Ok(None) };
    if !is_calibre_library(lib) {
        return Err(EngineError::InvalidInput(format!(
            "{} has no metadata.db, so it is not a calibre library (calibredb would create one \
             there rather than say so)",
            lib.display()
        )));
    }
    Ok(Some(lib))
}

/// A folder holding calibre's `metadata.db`. Shared with `folders.rs`, which must not walk a
/// calibre library.
pub(crate) fn is_calibre_library(dir: &Path) -> bool {
    dir.join("metadata.db").is_file()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalibreBook {
    /// Per-library row id; displayed, never keyed on.
    pub calibre_id: i64,
    /// Stable identity and the `external_ids` key. Not `calibre_id`: ids are per-library and reused
    /// after a delete.
    pub uuid: Option<String>,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub publisher: Option<String>,
    pub publish_year: Option<i64>,
    pub language: Option<String>,
    pub isbn_10: Option<String>,
    pub isbn_13: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub cover: Option<PathBuf>,
    /// Every format calibre holds, as absolute paths, not just the epub: the opened file may be the
    /// azw3.
    pub formats: Vec<PathBuf>,
    pub series: Option<String>,
    pub series_index: Option<f64>,
    pub added: Option<i64>,
}

impl CalibreBook {
    pub fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("(untitled)")
    }
}

/// Calibre emits `authors` as a `&`-joined string but `tags`/`languages`/`formats` as arrays;
/// accept both.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrList {
    One(String),
    Many(Vec<String>),
}

impl StringOrList {
    /// `&` is calibre's author separator; `,` is also split on because hand-edited fields use it.
    fn names(&self) -> Vec<String> {
        match self {
            StringOrList::One(s) => s
                .split('&')
                .flat_map(|part| part.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            StringOrList::Many(v) => v
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    /// Verbatim, for values rather than names: a tag with a comma is one tag.
    fn values(&self) -> Vec<String> {
        match self {
            StringOrList::One(s) => s
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            StringOrList::Many(v) => v
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        }
    }

    fn first(&self) -> Option<String> {
        self.values().into_iter().next()
    }
}

/// Every field is optional: calibre omits a field entirely when it has no value.
#[derive(Debug, Deserialize)]
struct RawRow {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    uuid: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    authors: Option<StringOrList>,
    #[serde(default)]
    publisher: Option<String>,
    #[serde(default)]
    pubdate: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    comments: Option<String>,
    #[serde(default)]
    cover: Option<String>,
    #[serde(default)]
    formats: Option<StringOrList>,
    #[serde(default)]
    identifiers: Option<HashMap<String, String>>,
    #[serde(default)]
    isbn: Option<String>,
    #[serde(default)]
    languages: Option<StringOrList>,
    #[serde(default)]
    tags: Option<StringOrList>,
    #[serde(default)]
    series: Option<String>,
    #[serde(default)]
    series_index: Option<f64>,
}

pub fn parse_library(json: &str) -> Result<Vec<CalibreBook>> {
    let rows: Vec<RawRow> = serde_json::from_str(json)?;
    Ok(rows.into_iter().map(row_to_book).collect())
}

fn row_to_book(r: RawRow) -> CalibreBook {
    let series = r.series.clone().filter(|s| !s.trim().is_empty());
    // The ISBN may sit in the column or the identifiers map; both are free text (ASINs, ISSNs,
    // typos), so both go through `normalize_isbn`.
    let raw_isbn = r
        .identifiers
        .as_ref()
        .and_then(|m| m.get("isbn"))
        .cloned()
        .or(r.isbn)
        .as_deref()
        .and_then(normalize_isbn);
    let (isbn_10, isbn_13) = match raw_isbn {
        Some(i) if i.len() == 10 => (Some(i), None),
        Some(i) => (None, Some(i)),
        None => (None, None),
    };

    CalibreBook {
        calibre_id: r.id,
        uuid: r.uuid.filter(|u| !u.trim().is_empty()),
        title: r.title.filter(|t| !t.trim().is_empty()),
        authors: r
            .authors
            .map(|a| a.names())
            .filter(|names| !is_unknown_author(names))
            .unwrap_or_default(),
        publisher: r.publisher.filter(|p| !p.trim().is_empty()),
        publish_year: r.pubdate.as_deref().and_then(year_of),
        language: r
            .languages
            .and_then(|l| l.first())
            .map(|l| normalize_language(&l)),
        isbn_10,
        isbn_13,
        description: r.comments.filter(|c| !c.trim().is_empty()),
        tags: r.tags.map(|t| t.values()).unwrap_or_default(),
        cover: r.cover.filter(|c| !c.is_empty()).map(PathBuf::from),
        formats: r
            .formats
            .map(|f| f.values().into_iter().map(PathBuf::from).collect())
            .unwrap_or_default(),
        series: series.clone(),
        // `series_index` is 1.0 on a book with no series (calibre's default), so it is kept only where
        // the name exists.
        series_index: series.as_ref().and(r.series_index),
        added: r.timestamp.as_deref().and_then(unix_of),
    }
}

/// Calibre's placeholder author for a book with none; it is not a person, so it is read as no
/// author. Deliberately narrow: only the whole list being exactly this string (the word is
/// locale-translated).
const CALIBRE_UNKNOWN_AUTHOR: &str = "Unknown";

fn is_unknown_author(names: &[String]) -> bool {
    matches!(names, [only] if only == CALIBRE_UNKNOWN_AUTHOR)
}

/// An undated book's `pubdate` is year 101 (`UNDEFINED_DATE`); anything before printing is that
/// sentinel.
const UNDEFINED_BEFORE: i64 = 1000;

fn year_of(iso: &str) -> Option<i64> {
    let year: i64 = iso.get(..4)?.parse().ok()?;
    (year >= UNDEFINED_BEFORE).then_some(year)
}

/// Only the date is used, so an odd timezone offset cannot move it by more than a day.
fn unix_of(iso: &str) -> Option<i64> {
    let date = time::Date::parse(
        iso.get(..10)?,
        time::macros::format_description!("[year]-[month]-[day]"),
    )
    .ok()?;
    let stamp = date.midnight().assume_utc().unix_timestamp();
    (year_of(iso)?.is_positive()).then_some(stamp)
}

pub async fn list_library(calibre: &Calibre, library: Option<&Path>) -> Result<Vec<CalibreBook>> {
    let bin = Calibre::require(CALIBREDB, calibre.calibredb_path())?;
    let library = library_root(library)?;
    let out = run(&bin, CALIBREDB, &list_argv(library)).await?;
    parse_library(&out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalibreMatch {
    /// An `external_ids` row already links this uuid: the only branch that is not a guess.
    Uuid,
    Isbn,
    Md5,
    Title,
    New,
}

impl std::fmt::Display for CalibreMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            CalibreMatch::Uuid => "calibre uuid",
            CalibreMatch::Isbn => "isbn",
            CalibreMatch::Md5 => "file",
            CalibreMatch::Title => "title",
            CalibreMatch::New => "new",
        };
        f.write_str(s)
    }
}

#[derive(Debug)]
pub struct CalibreBookReport {
    /// The calibre row this line is about; `uuid` cannot identify it (optional), and titles can repeat.
    pub calibre_id: i64,
    pub book_id: Option<i64>,
    pub title: String,
    pub matched_by: CalibreMatch,
    pub tags_added: usize,
    pub cover: bool,
    pub files_linked: usize,
}

#[derive(Debug)]
pub struct UnmatchedCalibreBook {
    pub calibre_id: i64,
    pub uuid: Option<String>,
    pub title: String,
    pub authors: Vec<String>,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug, Default)]
pub struct CalibreReport {
    pub dry_run: bool,
    pub rows: usize,
    pub books: Vec<CalibreBookReport>,
    pub unmatched: Vec<UnmatchedCalibreBook>,
    pub warnings: Vec<Diagnostic>,
}

impl CalibreReport {
    pub fn created(&self) -> usize {
        self.books
            .iter()
            .filter(|b| b.matched_by == CalibreMatch::New)
            .count()
    }

    /// What this report says about one calibre row. A row with no title is in neither list and
    /// survives only as a warning, so deriving state from `books` alone would drop it.
    pub fn row_state(&self, calibre_id: i64) -> CalibreRowState {
        if let Some(b) = self.books.iter().find(|b| b.calibre_id == calibre_id) {
            return match b.matched_by {
                CalibreMatch::New => CalibreRowState::New,
                matched_by => CalibreRowState::Linked {
                    matched_by,
                    tags: b.tags_added,
                    cover: b.cover,
                    files: b.files_linked,
                },
            };
        }
        if let Some(u) = self.unmatched.iter().find(|u| u.calibre_id == calibre_id) {
            return CalibreRowState::Candidates(u.candidates.clone());
        }
        // Neither list: the import skipped it. Uses its own diagnostic, never a re-worded one (`Display`
        // is user-visible).
        let warning = self.warnings.iter().find(|w| {
            matches!(
                w.kind,
                DiagnosticKind::CalibreRowSkipped { calibre_id: id } if id == calibre_id
            )
        });
        match warning {
            Some(d) => CalibreRowState::Unreadable(d.clone()),
            None => CalibreRowState::Unreadable(Diagnostic {
                kind: DiagnosticKind::CalibreRowSkipped { calibre_id },
                severity: Severity::Warning,
                detail: "calibre listed it but the import had nothing to say about it".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum CalibreRowState {
    /// Already ours. Carries no `book_id`: nothing that shows this navigates into the book.
    Linked {
        matched_by: CalibreMatch,
        tags: usize,
        cover: bool,
        files: usize,
    },
    New,
    Candidates(Vec<MatchCandidate>),
    /// A row with no title; carries the engine's own diagnostic, not a re-worded one.
    Unreadable(Diagnostic),
}

impl CalibreRowState {
    /// `Candidates` is deliberately not importable: a sweep answering it would create a duplicate.
    pub fn is_importable(&self) -> bool {
        matches!(self, CalibreRowState::New | CalibreRowState::Linked { .. })
    }

    pub fn is_undecided(&self) -> bool {
        matches!(self, CalibreRowState::Candidates(_))
    }

    pub fn label(&self) -> &'static str {
        match self {
            CalibreRowState::Linked { .. } => "in library",
            CalibreRowState::New => "new",
            CalibreRowState::Candidates(_) => "maybe",
            CalibreRowState::Unreadable(_) => "unreadable",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// `None` means calibre's own default library.
    pub library: Option<PathBuf>,
    pub dry_run: bool,
    pub create_ambiguous: bool,
    /// Import only these calibre rows; empty means all. The filter applies to the listing, so report
    /// counts reflect the rows considered.
    pub only: Vec<i64>,
}

pub async fn import(engine: &Engine, opts: &ImportOptions) -> Result<CalibreReport> {
    let root = root_key(opts.library.as_deref());
    // A missing library (unmounted drive, typo) leaves its places missing, not gone; the listing
    // below still refuses.
    if !opts.dry_run
        && let Some(lib) = &opts.library
        && !is_calibre_library(lib)
    {
        mark_missing(&engine.storage, &root, None).await?;
    }
    let mut books = list_library(engine.calibre(), opts.library.as_deref()).await?;
    let listed: std::collections::HashSet<String> =
        books.iter().filter_map(|b| b.uuid.clone()).collect();
    let whole_library = opts.only.is_empty();
    if !opts.only.is_empty() {
        books.retain(|cb| opts.only.contains(&cb.calibre_id));
    }
    let storage = &engine.storage;
    let mut report = CalibreReport {
        dry_run: opts.dry_run,
        rows: books.len(),
        ..Default::default()
    };

    for cb in &books {
        if cb.title.is_none() {
            report.warnings.push(Diagnostic {
                kind: DiagnosticKind::CalibreRowSkipped {
                    calibre_id: cb.calibre_id,
                },
                severity: Severity::Warning,
                detail: "no title, so there is nothing to match on".to_string(),
            });
            continue;
        }
        if cb.uuid.is_none() {
            // The import still happens but cannot be made idempotent, so the duplicate would appear silently
            // later.
            report.warnings.push(Diagnostic {
                kind: DiagnosticKind::CalibreRowNotIdentified {
                    calibre_id: cb.calibre_id,
                },
                severity: Severity::Warning,
                detail: "no uuid; this book will be imported again on a second run".to_string(),
            });
        }

        // Hashed once and shared by `match_book` and `apply`; hashing in each would read every file twice.
        let hashes = file_hashes(cb);
        let (book_id, matched_by) = match match_book(storage, cb, &hashes).await? {
            Matched::Book(id, how) => (Some(id), how),
            Matched::Ambiguous(candidates) if !opts.create_ambiguous => {
                report.unmatched.push(UnmatchedCalibreBook {
                    calibre_id: cb.calibre_id,
                    uuid: cb.uuid.clone(),
                    title: cb.display_title().to_string(),
                    authors: cb.authors.clone(),
                    candidates,
                });
                continue;
            }
            Matched::Ambiguous(_) | Matched::Nothing => (None, CalibreMatch::New),
        };

        if opts.dry_run {
            report
                .books
                .push(preview(storage, cb, book_id, matched_by, &hashes).await?);
            continue;
        }
        report.books.push(
            apply(
                engine,
                cb,
                book_id,
                matched_by,
                &hashes,
                &root,
                &mut report.warnings,
            )
            .await?,
        );
    }
    // Only a scan of the whole library can say a place is missing; a partial one saw only what it was
    // asked for.
    if !opts.dry_run && whole_library {
        mark_missing(&engine.storage, &root, Some(&listed)).await?;
    }
    Ok(report)
}

/// The library root as recorded in `copy_places.detail`; empty is calibre's default library.
fn root_key(library: Option<&Path>) -> String {
    library.map(|p| p.display().to_string()).unwrap_or_default()
}

/// `detail` = `<library root>\t<formats, comma-joined>`.
fn place_detail(root: &str, cb: &CalibreBook) -> String {
    let formats: Vec<String> = cb
        .formats
        .iter()
        .map(|f| crate::files::format_of(f))
        .collect();
    format!("{root}\t{}", formats.join(","))
}

fn root_of(detail: &str) -> &str {
    crate::copies::calibre_root(detail)
}

/// Marks this library's places missing: all when `seen` is `None` (unreachable), else those whose
/// uuid is not in `seen`. A place with no recorded library is left alone. Writes only the place;
/// the copy never changes.
async fn mark_missing(
    storage: &Storage,
    root: &str,
    seen: Option<&std::collections::HashSet<String>>,
) -> Result<()> {
    let now = crate::storage::now_unix();
    for place in storage.places_of_kind(PlaceKind::Calibre).await? {
        let Some(detail) = &place.detail else {
            continue;
        };
        if root_of(detail) != root {
            continue;
        }
        if seen.is_none_or(|s| !s.contains(&place.reference)) {
            storage
                .mark_place_missing(PlaceKind::Calibre, &place.reference, now)
                .await?;
        }
    }
    Ok(())
}

/// Points, copies no bytes. A book with an own file copy takes the place on that copy; `have_now`
/// is never touched, and each scan replaces the place's file paths since calibre owns the format
/// list.
async fn record_copy(storage: &Storage, cb: &CalibreBook, book_id: i64, root: &str) -> Result<()> {
    let detail = place_detail(root, cb);
    if let Some(uuid) = &cb.uuid
        && let Some(copy) = storage.copy_for_place(PlaceKind::Calibre, uuid).await?
    {
        // Held by another book's copy: the external-id link decides whose book this is; the place stays.
        if copy.book_id == book_id {
            storage
                .refresh_place(PlaceKind::Calibre, uuid, None, None)
                .await?;
            storage
                .set_place_detail(PlaceKind::Calibre, uuid, Some(&detail))
                .await?;
            storage.unmiss_place(PlaceKind::Calibre, uuid).await?;
            storage
                .set_place_files(PlaceKind::Calibre, uuid, &place_files(cb))
                .await?;
        }
        return Ok(());
    }
    let copy_id = match storage
        .copies_for_book(book_id)
        .await?
        .into_iter()
        .find(|c| c.form == Some(CopyForm::File) && c.tenure == Tenure::Own)
    {
        Some(c) => c.id,
        None => {
            storage
                .create_copy(&NewCopy::new(
                    book_id,
                    Some(CopyForm::File),
                    Tenure::Own,
                    LearnedFrom::Calibre,
                ))
                .await?
                .id
        }
    };
    if let Some(uuid) = &cb.uuid {
        let mut place = NewPlace::new(PlaceKind::Calibre, uuid.clone());
        place.detail = Some(detail);
        storage.attach_place(copy_id, &place).await?;
        storage
            .set_place_files(PlaceKind::Calibre, uuid, &place_files(cb))
            .await?;
    }
    Ok(())
}

fn place_files(cb: &CalibreBook) -> Vec<crate::copies::PlaceFile> {
    cb.formats
        .iter()
        .map(|f| crate::copies::PlaceFile {
            format: crate::files::format_of(f),
            path: f.clone(),
        })
        .collect()
}

enum Matched {
    Book(i64, CalibreMatch),
    Ambiguous(Vec<MatchCandidate>),
    Nothing,
}

/// uuid, ISBN, `partial_md5`, then the shared title+author scan. No second fuzzy matcher, so the
/// bands agree with sidecars and Goodreads rows.
async fn match_book(storage: &Storage, cb: &CalibreBook, hashes: &[String]) -> Result<Matched> {
    if let Some(uuid) = &cb.uuid
        && let Some(book_id) = storage.book_for_external_id(SOURCE, uuid).await?
    {
        return Ok(Matched::Book(book_id, CalibreMatch::Uuid));
    }
    for isbn in [&cb.isbn_13, &cb.isbn_10].into_iter().flatten() {
        if let Some(book) = storage.find_book_by_isbn(isbn).await?
            && let Some(book_id) = book.id
        {
            return Ok(Matched::Book(book_id, CalibreMatch::Isbn));
        }
    }
    for md5 in hashes {
        if let Some(book) = storage.find_book_by_partial_md5(md5).await?
            && let Some(book_id) = book.id
        {
            return Ok(Matched::Book(book_id, CalibreMatch::Md5));
        }
    }

    // One scan answers both the auto-match and the band; separate calls would read the shelf twice
    // per row.
    let scored =
        koreader::scores_for(storage, &Query::new(cb.title.as_deref(), &cb.authors)).await?;
    if let Some(s) = scored.first()
        && s.can_auto
        && let Some(book_id) = s.book.id
    {
        return Ok(Matched::Book(book_id, CalibreMatch::Title));
    }
    let candidates = koreader::band(scored);
    Ok(if candidates.is_empty() {
        Matched::Nothing
    } else {
        Matched::Ambiguous(candidates)
    })
}

/// An unreadable file is skipped silently: the library may be on an unmounted volume, and the
/// next rung catches it.
fn file_hashes(cb: &CalibreBook) -> Vec<String> {
    cb.formats
        .iter()
        .filter_map(|f| partial_md5::partial_md5(f).ok())
        .collect()
}

async fn preview(
    storage: &Storage,
    cb: &CalibreBook,
    book_id: Option<i64>,
    matched_by: CalibreMatch,
    hashes: &[String],
) -> Result<CalibreBookReport> {
    let (tags_added, cover) = match book_id {
        None => (cb.tags.len(), cb.cover.is_some()),
        Some(id) => {
            let known: Vec<String> = storage
                .book_tags(id)
                .await?
                .into_iter()
                .filter(|t| t.source == SOURCE)
                .map(|t| t.tag)
                .collect();
            let have_cover = storage
                .get_book(id)
                .await?
                .and_then(|b| b.cover_path)
                .is_some();
            (
                cb.tags
                    .iter()
                    .filter(|t| !known.contains(&slug_tag(t)))
                    .count(),
                cb.cover.is_some() && !have_cover,
            )
        }
    };
    Ok(CalibreBookReport {
        calibre_id: cb.calibre_id,
        book_id,
        title: cb.display_title().to_string(),
        matched_by,
        tags_added,
        cover,
        files_linked: unlinked(storage, hashes).await?,
    })
}

/// How many of these files are not already identified: the count is what the import changed, not
/// what it looked at.
async fn unlinked(storage: &Storage, hashes: &[String]) -> Result<usize> {
    let mut n = 0;
    for md5 in hashes {
        if storage.find_book_by_partial_md5(md5).await?.is_none() {
            n += 1;
        }
    }
    Ok(n)
}

async fn apply(
    engine: &Engine,
    cb: &CalibreBook,
    book_id: Option<i64>,
    matched_by: CalibreMatch,
    hashes: &[String],
    root: &str,
    warnings: &mut Vec<Diagnostic>,
) -> Result<CalibreBookReport> {
    let storage = &engine.storage;
    // Counted before the links are written, or every one would look new.
    let files_linked = unlinked(storage, hashes).await?;

    // `upsert_book` (the provider no-clobber merge), not straight assignment: a `calibredb list` row
    // carries no page count and often no ISBN, so assigning it would blank fields calibre has no
    // opinion about.
    let mut book = Book {
        title: cb.title.clone(),
        authors: cb.authors.clone(),
        publisher: cb.publisher.clone(),
        publish_year: cb.publish_year,
        language: cb.language.clone(),
        isbn_10: cb.isbn_10.clone(),
        isbn_13: cb.isbn_13.clone(),
        description: cb.description.clone(),
        // Series and index move together or not at all: an index without its name is not a fact.
        series: cb.series.clone(),
        series_index: cb.series_index,
        // Subjects stay empty: calibre's `tags` are minted shelves (`book_tags`) and must not merge into
        // `subjects`.
        ..Default::default()
    };
    let created = book_id.is_none();
    let mut cover_copied = false;
    // Calibre's cover is often another edition's, so it fills a gap and never replaces a cover.
    let have_cover = match book_id {
        Some(id) => storage
            .get_book(id)
            .await?
            .and_then(|b| b.cover_path)
            .is_some(),
        None => false,
    };
    if !have_cover && let Some(src) = &cb.cover {
        match copy_cover(src, &engine.config.images_dir, cb) {
            Ok(path) => {
                book.cover_path = Some(path.display().to_string());
                cover_copied = true;
            }
            Err(e) => warnings.push(Diagnostic {
                kind: DiagnosticKind::CalibreCoverUnreadable { path: src.clone() },
                severity: Severity::Warning,
                detail: e.to_string(),
            }),
        }
    }

    // `enrich_book` for an identified book, `upsert_book` only when creating: with no ISBN
    // `upsert_book` inserts unconditionally and ignores `Book::id`, duplicating the book on every
    // run.
    let book_id = match book_id {
        Some(id) => {
            storage
                .enrich_book(id, &book, Some(Source::Calibre))
                .await?;
            id
        }
        None => storage.upsert_book(&book, Some(Source::Calibre)).await?,
    };
    // `timestamp` is when it was added to calibre, which is what `created_at` means, but only for a
    // book created here.
    if created && let Some(added) = cb.added {
        storage.set_book_created_at(book_id, added).await?;
    }

    if let Some(uuid) = &cb.uuid {
        storage.link_external_id(SOURCE, uuid, book_id).await?;
    }
    record_copy(storage, cb, book_id, root).await?;
    let tags: Vec<(String, String)> = cb.tags.iter().map(|t| (slug_tag(t), t.clone())).collect();
    let tags_added = storage.add_book_tags(book_id, SOURCE, &tags).await?;

    // `link_device_book`, never `set_device_link`: a scan must not relabel a link the user made by
    // hand.
    for md5 in hashes {
        storage
            .link_device_book(md5, book_id, LinkedBy::Auto)
            .await?;
    }

    Ok(CalibreBookReport {
        calibre_id: cb.calibre_id,
        book_id: Some(book_id),
        title: cb.display_title().to_string(),
        matched_by,
        tags_added,
        cover: cover_copied,
        files_linked,
    })
}

/// Named from the uuid: every calibre cover is called `cover.jpg`, so the source name would
/// collide.
fn copy_cover(src: &Path, images_dir: &Path, cb: &CalibreBook) -> Result<PathBuf> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("jpg");
    let stem = cb
        .uuid
        .clone()
        .unwrap_or_else(|| format!("id-{}", cb.calibre_id));
    std::fs::create_dir_all(images_dir)?;
    let dst = images_dir.join(format!("calibre-{stem}.{ext}"));
    std::fs::copy(src, &dst)?;
    Ok(dst)
}

/// Our normalization of a tag; the raw value is stored beside it, so this can change later.
fn slug_tag(tag: &str) -> String {
    tag.trim().to_lowercase().replace(' ', "-")
}

/// `tokio::process`: `ebook-convert` can run for minutes and must not block the executor.
async fn run(bin: &Path, tool: &str, argv: &[String]) -> Result<String> {
    let out = tokio::process::Command::new(bin)
        .args(argv)
        .output()
        .await
        .map_err(|e| EngineError::Calibre {
            tool: tool.to_string(),
            message: format!("{}: {e}", bin.display()),
        })?;
    if !out.status.success() {
        return Err(EngineError::Calibre {
            tool: tool.to_string(),
            message: tail(&String::from_utf8_lossy(&out.stderr)),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn tail(stderr: &str) -> String {
    let msg: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let n = msg.len().saturating_sub(3);
    let text = msg[n..].join("; ");
    if text.is_empty() {
        "failed with no output".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from real calibre output, not written by hand.
    const RECORDED: &str = include_str!("../tests/fixtures/calibre/recorded/library.json");

    #[test]
    fn the_recorded_library_parses_the_way_calibre_actually_writes_it() {
        let books = parse_library(RECORDED).unwrap();
        assert_eq!(books.len(), 3);

        let p = &books[0];
        assert_eq!(p.title.as_deref(), Some("Grimms' Fairy Tales"));
        assert_eq!(p.authors, vec!["Jacob Grimm", "Wilhelm Grimm"]);
        assert_eq!(
            p.uuid.as_deref(),
            Some("8810c85d-bb14-4a98-a1a9-897508436610")
        );
        assert_eq!(p.isbn_13.as_deref(), Some("9780140447101"));
        assert_eq!(p.isbn_10, None);
        assert_eq!(p.publish_year, Some(2001));
        assert_eq!(
            p.language.as_deref(),
            Some("en"),
            "MARC 'eng' is normalized"
        );
        assert_eq!(p.tags, vec!["fairy-tales", "german-lit"]);
        assert_eq!(p.series.as_deref(), Some("Household Tales"));
        assert_eq!(p.series_index, Some(1.0));
        assert_eq!(p.formats.len(), 2, "every format, not just the epub");
        assert!(p.cover.is_some());
    }

    #[test]
    fn calibres_undefined_date_is_not_a_publication_year() {
        assert_eq!(year_of("0101-01-01T00:00:00+00:00"), None);
        assert_eq!(year_of("2017-02-07T00:00:00+00:00"), Some(2017));
        assert_eq!(year_of(""), None);
        assert_eq!(year_of("not a date"), None);

        let books = parse_library(RECORDED).unwrap();
        let undated = &books[1];
        assert_eq!(undated.publish_year, None, "0101 is a sentinel, not a year");
        assert!(undated.added.is_some());
    }

    #[test]
    fn a_row_with_almost_nothing_on_it_still_parses() {
        let books = parse_library(RECORDED).unwrap();
        let bare = &books[2];
        assert_eq!(bare.title.as_deref(), Some("Untitled Draft"));
        assert!(bare.authors.is_empty());
        assert_eq!(bare.series, None);
        assert_eq!(
            bare.series_index, None,
            "calibre writes series_index 1.0 on a book with no series; that is \
             a default, not a claim that this is book one of something"
        );
        assert_eq!(bare.publisher, None);
        assert_eq!(bare.isbn_10, None);
        assert_eq!(bare.isbn_13, None);
        assert!(bare.tags.is_empty());
        assert!(bare.formats.is_empty());
        assert_eq!(bare.cover, None);
    }

    #[test]
    fn an_empty_library_is_no_books_rather_than_an_error() {
        assert_eq!(parse_library("[]").unwrap().len(), 0);
    }

    #[test]
    fn calibres_unknown_author_is_no_author() {
        for json in [
            r#"[{"id":3,"title":"Untitled Draft","authors":"Unknown"}]"#,
            r#"[{"id":3,"title":"Untitled Draft","authors":["Unknown"]}]"#,
        ] {
            let rows = parse_library(json).unwrap();
            assert!(rows[0].authors.is_empty(), "{json}: {:?}", rows[0].authors);
        }
        let lower = parse_library(r#"[{"id":1,"title":"T","authors":"unknown"}]"#).unwrap();
        assert_eq!(lower[0].authors, vec!["unknown"]);
        let beside = parse_library(r#"[{"id":1,"title":"T","authors":"Unknown & A"}]"#).unwrap();
        assert_eq!(beside[0].authors, vec!["Unknown", "A"]);
    }

    #[test]
    fn authors_parse_from_a_joined_string_or_a_list() {
        let joined = parse_library(r#"[{"id":1,"title":"T","authors":"A & B"}]"#).unwrap();
        assert_eq!(joined[0].authors, vec!["A", "B"]);
        let listed = parse_library(r#"[{"id":1,"title":"T","authors":["A","B"]}]"#).unwrap();
        assert_eq!(listed[0].authors, vec!["A", "B"]);
        let tagged = parse_library(r#"[{"id":1,"title":"T","tags":["Victorian Lit"]}]"#).unwrap();
        assert_eq!(tagged[0].tags, vec!["Victorian Lit"]);
        assert_eq!(slug_tag("Victorian Lit"), "victorian-lit");
    }

    #[test]
    fn an_isbn_is_validated_wherever_calibre_put_it() {
        let from_map =
            parse_library(r#"[{"id":1,"title":"T","identifiers":{"isbn":"0990000028"}}]"#).unwrap();
        assert_eq!(from_map[0].isbn_10.as_deref(), Some("0990000028"));

        let from_col = parse_library(r#"[{"id":1,"title":"T","isbn":"9781900000178"}]"#).unwrap();
        assert_eq!(from_col[0].isbn_13.as_deref(), Some("9781900000178"));

        let junk =
            parse_library(r#"[{"id":1,"title":"T","identifiers":{"isbn":"B01N0Q4B5F"}}]"#).unwrap();
        assert_eq!(junk[0].isbn_10, None);
        assert_eq!(junk[0].isbn_13, None);
    }

    #[test]
    fn the_command_lines_are_what_calibre_documents() {
        assert_eq!(
            list_argv(None),
            ["list", "--for-machine", "--fields", "all"]
        );
        assert_eq!(
            list_argv(Some(Path::new("/books/lib"))),
            [
                "--with-library",
                "/books/lib",
                "list",
                "--for-machine",
                "--fields",
                "all"
            ]
        );
        assert_eq!(
            convert_argv(Path::new("in.epub"), Path::new("out.azw3")),
            ["in.epub", "out.azw3"]
        );
    }

    #[tokio::test]
    async fn an_absent_calibre_is_a_named_error_and_never_a_panic() {
        let none = Calibre::default();
        assert!(!none.can_convert());
        assert!(!none.can_read_library());

        let e = list_library(&none, None).await.unwrap_err();
        assert!(matches!(e, EngineError::CalibreMissing { .. }), "{e:?}");
        let e = convert(&none, Path::new("/x.epub"), Path::new("/x.azw3"), false)
            .await
            .unwrap_err();
        assert!(matches!(e, EngineError::CalibreMissing { .. }), "{e:?}");
    }

    #[test]
    fn a_directory_that_is_not_a_calibre_library_is_refused_before_anything_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let e = library_root(Some(tmp.path())).unwrap_err();
        assert!(matches!(e, EngineError::InvalidInput(_)), "{e:?}");
        assert!(e.to_string().contains("metadata.db"));

        std::fs::write(tmp.path().join("metadata.db"), b"").unwrap();
        assert_eq!(library_root(Some(tmp.path())).unwrap(), Some(tmp.path()));
        assert_eq!(library_root(None).unwrap(), None);
    }

    /// Asserted on `is_executable`, not `find_tool`: `find_tool` falls through to the real `PATH`, so
    /// a machine with calibre installed would pass the negative half for the wrong reason.
    #[cfg(unix)]
    #[test]
    fn detection_wants_an_executable_file_not_a_name() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();

        std::fs::create_dir(tmp.path().join(CALIBREDB)).unwrap();
        assert!(!is_executable(&tmp.path().join(CALIBREDB)), "a directory");
        assert!(!is_executable(&tmp.path().join("absent")));

        let bin = tmp.path().join(CONVERT);
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        assert!(!is_executable(&bin), "present but not executable");

        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(&bin));
        assert_eq!(
            find_tool(CONVERT, Some(tmp.path())),
            Some(bin),
            "the extra directory is searched before PATH"
        );
    }

    #[test]
    fn a_calibre_failure_keeps_the_end_of_what_it_said() {
        let noisy = (0..20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tail(&noisy), "line 17; line 18; line 19");
        assert_eq!(tail("   \n\n"), "failed with no output");
    }
}
