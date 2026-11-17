//! Apple Books (macOS): highlights and notes out of the app's own databases.
//!
//! Import is one-way. bookmole never writes into Apple Books, and never
//! opens its databases at all: they are **live WAL databases owned by another
//! app**, so each is copied (the db, its `-wal` and its `-shm`, whichever
//! exist) into a private temp dir through [`crate::ko_statistics`]' `copy_out`
//! — the same discipline, for the same reason (RUSTSEC-2026-0459, the vendored
//! SQLite) — and **the copy** is opened read-only, read, closed and deleted.
//! No code path names an original after the copy.
//!
//! ## Where
//!
//! `root` is Apple Books' `Data/Documents` ([`default_root`] on a Mac, always a
//! parameter so tests pass a temp dir). The annotation database is
//! `AEAnnotation/AEAnnotation*.sqlite` and the library database
//! `BKLibrary/BKLibrary*.sqlite`, found **by pattern**: the version suffix is
//! Apple's and changes. Several matches → the most recently modified. None →
//! [`DiagnosticKind::AppleBooksAbsent`], never an error. macOS refusing access
//! (the "access data from other apps" prompt declined) →
//! [`DiagnosticKind::AppleBooksPermissionDenied`], which a frontend explains.
//!
//! ## What is read
//!
//! `ZBKLIBRARYASSET` (`ZASSETID` required; `ZTITLE`, `ZAUTHOR`, `ZPATH`,
//! `ZSTOREID`, `ZCONTENTTYPE`) and `ZAEANNOTATION` (`ZANNOTATIONUUID` and
//! `ZANNOTATIONASSETID` required; the rest optional). A missing table or key
//! column is [`DiagnosticKind::AppleBooksSchemaUnknown`]; a missing optional
//! column is read as NULL for every row and reported once as
//! [`DiagnosticKind::AppleBooksColumnsMissing`]. Values are read by storage
//! class, so a text where a number belongs is NULL, never a guessed number.
//! Dates are Core Data's, seconds since 2001-01-01 UTC: [`core_data_to_unix`]
//! is the one conversion.
//!
//! ## Which annotations
//!
//! `ZANNOTATIONTYPE` 2 is a highlight (with or without a note) and is
//! imported. 1 (a bookmark) and 3 (not a passage: no selected text the reader
//! chose) are **not highlights** and are counted in
//! [`AppleBooksReport::not_highlights`]. Any other value is counted
//! `unparsed`, never guessed at. A NULL type (or no type column) is judged by
//! content. A row with a note and no selected text is a **note-only** row
//! (`text = ""`, as Preview's sticky notes), not `text_unavailable`; a row
//! with neither is `unparsed`.
//!
//! Every highlight is `origin = apple_books`, `origin_id = ZANNOTATIONUUID`
//! ([`IdentityScope::Global`]: the uuid is Apple's and names one highlight
//! wherever it is seen), `origin_note = ZANNOTATIONNOTE`, `pos_kind =
//! epub_cfi` with `pos0 = ZANNOTATIONLOCATION` **verbatim** (never converted,
//! and no page: the source gives none), `made_at` from the creation date and
//! the colour from [`style_color`].
//!
//! ## Which book
//!
//! An asset is matched, in order: by its `apple_books` **place** (an asset we
//! have imported before: identity is the place); by `ZPATH`, when it is a
//! regular file here, through [`crate::files::identify`] — the one matching
//! ladder (bytes a place last saw, ISBN, `partial_md5`, title band), copying no
//! bytes; by `ZSTOREID` through `external_ids` (source [`STORE_ID_SOURCE`],
//! recorded on every match so the next import takes this rung); then by title
//! and author through `koreader::scores_for`'s **auto-match band only**.
//!
//! An asset that matches nothing is **created as a book only when it has a
//! highlight to import**: Apple Books holding a passage the reader marked is
//! evidence they have the book in an app (only an explicit act brings a book
//! into the library: an importer that found a thing, or the reader saying how
//! they have it). One without highlights is
//! skipped and counted, never listed by title. Like `files::import`, it
//! **refuses to create over a candidate**: an asset whose title lands in the
//! candidate band comes back [`AssetOutcome::Unmatched`] with the candidates
//! and nothing written; [`link_apple_books_asset`] is the next move, after
//! which the place rung finds it.
//!
//! ## Copies
//!
//! Find-or-create through the `apple_books` place (`ref` = asset id). A
//! side-loaded file (no `ZSTOREID`, a usable `ZPATH`) is attached to the
//! book's copy that already holds those bytes or that KOReader identity (one
//! file is one copy wherever it sits), else a new `file`/`own` copy; the place
//! records the file's sha256 and mtime. Anything else — store-bought, or a file
//! that is not a regular file here (Apple keeps many epubs as folders) — is a
//! `form = app`, `app = "apple_books"`, `tenure = own` copy. `learned_from =
//! apple_books`, and `have_now` is never set: only the reader says.
//!
//! ## Deletions follow the source
//!
//! `ZANNOTATIONDELETED = 1` removes the row (through
//! [`Storage::remove_imported_highlight`], which keeps one carrying anything
//! of the reader's own; those are counted in
//! [`DiagnosticKind::AppleBooksHighlightsKept`]). A highlight merely **absent**
//! from the database is not removed: absence is not a deletion mark.
//!
//! ## Privacy
//!
//! Highlight text and notes never reach a log field; book titles reach only
//! `trace!`.
//!
//! [`DiagnosticKind::AppleBooksAbsent`]: crate::DiagnosticKind::AppleBooksAbsent
//! [`DiagnosticKind::AppleBooksPermissionDenied`]: crate::DiagnosticKind::AppleBooksPermissionDenied
//! [`DiagnosticKind::AppleBooksSchemaUnknown`]: crate::DiagnosticKind::AppleBooksSchemaUnknown
//! [`DiagnosticKind::AppleBooksColumnsMissing`]: crate::DiagnosticKind::AppleBooksColumnsMissing
//! [`DiagnosticKind::AppleBooksHighlightsKept`]: crate::DiagnosticKind::AppleBooksHighlightsKept

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{Connection, Row, SqliteConnection};

use crate::book::Book;
use crate::copies::{CopyForm, LearnedFrom, NewCopy, NewPlace, PlaceAttach, PlaceKind, Tenure};
use crate::diagnostic::Diagnostic;
use crate::error::{EngineError, Result};
use crate::files::{FileIdentity, FileMatch};
use crate::highlight_origin::{
    HighlightImportSummary, HighlightOrigin, IdentityScope, ImportedHighlight, ImportedOutcome,
    PosKind,
};
use crate::koreader::MatchCandidate;
use crate::storage::Storage;

/// The folder under `root` holding the annotation database.
pub const ANNOTATION_DIR: &str = "AEAnnotation";
/// The folder under `root` holding the library database.
pub const LIBRARY_DIR: &str = "BKLibrary";
const ANNOTATION_TABLE: &str = "ZAEANNOTATION";
const ASSET_TABLE: &str = "ZBKLIBRARYASSET";
/// The `external_ids.source` a `ZSTOREID` is recorded under.
pub const STORE_ID_SOURCE: &str = "apple_books";
/// `copies.app` for an Apple Books app copy.
pub const APP: &str = "apple_books";
/// 2001-01-01 00:00:00 UTC as a unix timestamp: Core Data's epoch.
pub const CORE_DATA_EPOCH_UNIX: i64 = 978_307_200;
/// 0001-01-01 00:00:00 UTC and 9999-12-31 23:59:59 UTC: the instants a
/// four-digit `YYYY-MM-DD HH:MM:SS` stamp can name.
const MIN_UNIX: i64 = -62_135_596_800;
const MAX_UNIX: i64 = 253_402_300_799;

/// `ZANNOTATIONTYPE` values (see the module doc, "Which annotations").
const TYPE_BOOKMARK: i64 = 1;
const TYPE_HIGHLIGHT: i64 = 2;
const TYPE_NOT_A_PASSAGE: i64 = 3;

/// `ZANNOTATIONSTYLE` → colour name, the one table. 0 is an underline, which
/// has no colour, and anything not listed is `None`.
const STYLE_COLORS: &[(i64, &str)] = &[
    (1, "green"),
    (2, "blue"),
    (3, "yellow"),
    (4, "pink"),
    (5, "purple"),
];

/// Apple Books' `Data/Documents` for this user, if `HOME` is set. Only ever a
/// default for a caller: the import takes its root as a parameter.
pub fn default_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Containers/com.apple.iBooksX/Data/Documents"))
}

/// A Core Data date (seconds since 2001-01-01 UTC, a REAL) as whole unix
/// seconds, rounded down. `None` for a non-finite value or one outside years
/// 1–9999. **The one conversion**; monotonic (a property test pins it).
pub fn core_data_to_unix(seconds: f64) -> Option<i64> {
    if !seconds.is_finite() {
        return None;
    }
    let unix = seconds.floor() + CORE_DATA_EPOCH_UNIX as f64;
    if !(MIN_UNIX as f64..=MAX_UNIX as f64).contains(&unix) {
        return None;
    }
    Some(unix as i64)
}

/// A Core Data date as the UTC `YYYY-MM-DD HH:MM:SS` text every importer's
/// "when" is stored in (`highlights.ko_datetime`).
pub fn core_data_stamp(seconds: f64) -> Option<String> {
    let t = time::OffsetDateTime::from_unix_timestamp(core_data_to_unix(seconds)?).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    ))
}

/// The colour name of a `ZANNOTATIONSTYLE`, from [`STYLE_COLORS`].
pub fn style_color(style: i64) -> Option<&'static str> {
    STYLE_COLORS
        .iter()
        .find(|(s, _)| *s == style)
        .map(|(_, c)| *c)
}

// ---- the report ----------------------------------------------------------------

/// Which rung matched an asset to a book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetMatch {
    /// We had imported this asset before: its `apple_books` place.
    Place,
    /// Its `ZPATH` file, through the files ladder.
    File(FileMatch),
    /// Its `ZSTOREID`, through `external_ids`.
    StoreId,
    /// Title and author, auto-match band.
    Title,
}

/// What became of one asset.
#[derive(Debug, Clone, PartialEq)]
pub enum AssetOutcome {
    Matched {
        book_id: i64,
        by: AssetMatch,
    },
    /// Nothing matched and nothing was close, and it has highlights: a book
    /// was made from its title and author.
    Created {
        book_id: i64,
    },
    /// `dry_run`: [`AssetOutcome::Created`] would have happened.
    WouldCreate,
    /// Nothing matched outright. With candidates, they are the decision to make
    /// ([`link_apple_books_asset`]); empty when the asset has no title to make
    /// a book from. Nothing was written for this asset.
    Unmatched {
        candidates: Vec<MatchCandidate>,
    },
}

/// One Apple Books asset (a book in the app) that had something to import.
#[derive(Debug, Clone, PartialEq)]
pub struct AppleBooksBook {
    /// `ZASSETID`, the `apple_books` place's `ref`.
    pub asset_id: String,
    /// What Apple Books calls it (`ZTITLE`), when the library knows.
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// `ZSTOREID` present: bought in the Book Store.
    pub store_bought: bool,
    pub outcome: AssetOutcome,
    /// The copy its highlights are stamped with.
    pub copy_id: Option<i64>,
    /// Live highlights (and note-only rows) Apple Books holds for it.
    pub highlights_in_source: usize,
    /// Annotations Apple Books marks deleted.
    pub deleted_in_source: usize,
    /// What the import did (all zero but `unparsed` on a dry run).
    pub summary: HighlightImportSummary,
}

/// One pass over Apple Books.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppleBooksReport {
    pub dry_run: bool,
    /// The annotation database read (the original's path), if one was found.
    pub annotation_db: Option<PathBuf>,
    /// The library database read, if one was found.
    pub library_db: Option<PathBuf>,
    /// One entry per asset that had something to import or remove.
    pub books: Vec<AppleBooksBook>,
    /// The sum of every book's summary, plus annotations that could not be
    /// placed on any asset. Its `diagnostics` is empty: they are in each
    /// book's summary and in [`AppleBooksReport::diagnostics`].
    pub totals: HighlightImportSummary,
    /// Rows in the library table.
    pub assets_in_library: usize,
    /// Assets with nothing to import that matched no book: skipped, counted,
    /// not listed.
    pub assets_skipped: usize,
    /// Bookmarks and other annotations that are not highlights.
    pub not_highlights: usize,
    /// Highlights whose asset neither the library database nor a place of
    /// ours knows, so there was no book to put them on.
    pub orphaned: usize,
    /// About the databases themselves.
    pub diagnostics: Vec<Diagnostic>,
}

// ---- reading ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
struct Asset {
    asset_id: String,
    title: Option<String>,
    author: Option<String>,
    path: Option<String>,
    store_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Annotation {
    uuid: String,
    asset_id: String,
    selected_text: Option<String>,
    note: Option<String>,
    style: Option<i64>,
    kind: Option<i64>,
    location: Option<String>,
    deleted: bool,
    created: Option<f64>,
}

#[derive(Clone, Copy)]
enum Col {
    Text,
    Int,
    Real,
}

struct Spec {
    name: &'static str,
    col: Col,
    required: bool,
}

const fn spec(name: &'static str, col: Col, required: bool) -> Spec {
    Spec {
        name,
        col,
        required,
    }
}

const ASSET_COLUMNS: &[Spec] = &[
    spec("ZASSETID", Col::Text, true),
    spec("ZTITLE", Col::Text, false),
    spec("ZAUTHOR", Col::Text, false),
    spec("ZPATH", Col::Text, false),
    spec("ZSTOREID", Col::Text, false),
    spec("ZCONTENTTYPE", Col::Int, false),
];

const ANNOTATION_COLUMNS: &[Spec] = &[
    spec("ZANNOTATIONUUID", Col::Text, true),
    spec("ZANNOTATIONASSETID", Col::Text, true),
    spec("ZANNOTATIONSELECTEDTEXT", Col::Text, false),
    spec("ZANNOTATIONNOTE", Col::Text, false),
    spec("ZANNOTATIONSTYLE", Col::Int, false),
    spec("ZANNOTATIONTYPE", Col::Int, false),
    spec("ZANNOTATIONLOCATION", Col::Text, false),
    spec("ZANNOTATIONDELETED", Col::Int, false),
    spec("ZANNOTATIONCREATIONDATE", Col::Real, false),
    spec("ZANNOTATIONMODIFICATIONDATE", Col::Real, false),
];

/// The select expression for one column, by storage class: a number column
/// holding text reads as NULL rather than as SQLite's `CAST('x' AS INTEGER)`
/// zero, which would be a fabricated value (a type, a 2001 date).
fn select_expr(s: &Spec) -> String {
    let c = s.name;
    match s.col {
        Col::Text => format!("CAST({c} AS TEXT) AS {c}"),
        Col::Int => format!(
            "CASE WHEN typeof({c}) IN ('integer', 'real') THEN CAST({c} AS INTEGER) END AS {c}"
        ),
        Col::Real => format!(
            "CASE WHEN typeof({c}) IN ('integer', 'real') THEN CAST({c} AS REAL) END AS {c}"
        ),
    }
}

/// Read `table`'s rows with exactly `specs`' columns. `Ok(None)` (with a
/// diagnostic) when the table or a required column is not there.
async fn read_table(
    conn: &mut SqliteConnection,
    db: &Path,
    table: &str,
    specs: &[Spec],
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<Vec<SqliteRow>>> {
    let present: HashSet<String> =
        sqlx::query_scalar::<_, String>("SELECT name FROM pragma_table_info(?1)")
            .bind(table)
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .map(|n| n.to_ascii_uppercase())
            .collect();
    if specs
        .iter()
        .any(|s| s.required && !present.contains(s.name))
    {
        diagnostics.push(Diagnostic::apple_books_schema_unknown(
            db.to_path_buf(),
            table,
        ));
        return Ok(None);
    }
    let missing: Vec<String> = specs
        .iter()
        .filter(|s| !present.contains(s.name))
        .map(|s| s.name.to_string())
        .collect();
    if !missing.is_empty() {
        diagnostics.push(Diagnostic::apple_books_columns_missing(
            db.to_path_buf(),
            table,
            missing,
        ));
    }
    let select = specs
        .iter()
        .map(|s| {
            if present.contains(s.name) {
                select_expr(s)
            } else {
                format!("NULL AS {}", s.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let rows = sqlx::query(&format!("SELECT {select} FROM {table}"))
        .fetch_all(&mut *conn)
        .await?;
    Ok(Some(rows))
}

fn text(row: &SqliteRow, col: &str) -> Option<String> {
    row.try_get::<Option<String>, _>(col)
        .ok()
        .flatten()
        .filter(|s| !s.trim().is_empty())
}

fn int(row: &SqliteRow, col: &str) -> Option<i64> {
    row.try_get::<Option<i64>, _>(col).ok().flatten()
}

fn real(row: &SqliteRow, col: &str) -> Option<f64> {
    row.try_get::<Option<f64>, _>(col).ok().flatten()
}

/// Copy `db` (and its WAL siblings) out, and open **the copy** read-only.
/// `None` with a diagnostic when it cannot be copied or opened.
async fn open_snapshot(
    db: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(tempfile::TempDir, SqliteConnection)> {
    let scratch = match tempfile::tempdir() {
        Ok(s) => s,
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(
                db.to_path_buf(),
                &EngineError::from(e),
            ));
            return None;
        }
    };
    let copy = match crate::ko_statistics::copy_out(db, scratch.path()) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            diagnostics.push(Diagnostic::apple_books_permission_denied(db.to_path_buf()));
            return None;
        }
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(
                db.to_path_buf(),
                &EngineError::from(e),
            ));
            return None;
        }
    };
    // Read-only, and never created: a missing copy is an error, not an empty
    // database. With the `-wal` and `-shm` beside it in a directory we own,
    // SQLite reads the write-ahead log's frames (the uncheckpointed rows) on a
    // read-only connection; `tests/apple_books.rs` holds a writer open across
    // an import to prove those rows arrive.
    let opts = SqliteConnectOptions::new()
        .filename(&copy)
        .read_only(true)
        .create_if_missing(false);
    match SqliteConnection::connect_with(&opts).await {
        Ok(conn) => Some((scratch, conn)),
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(
                db.to_path_buf(),
                &EngineError::from(e),
            ));
            None
        }
    }
}

async fn read_assets(db: &Path, diagnostics: &mut Vec<Diagnostic>) -> Option<Vec<Asset>> {
    let (scratch, mut conn) = open_snapshot(db, diagnostics).await?;
    // Every statement inside one fallible region: SQLite only notices a file
    // is not a database on the first statement (`ko_statistics`' lesson).
    let read = read_table(&mut conn, db, ASSET_TABLE, ASSET_COLUMNS, diagnostics).await;
    conn.close().await.ok();
    drop(scratch);
    let rows = match read {
        Ok(rows) => rows?,
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(db.to_path_buf(), &e));
            return None;
        }
    };
    Some(
        rows.iter()
            .filter_map(|r| {
                Some(Asset {
                    asset_id: text(r, "ZASSETID")?,
                    title: text(r, "ZTITLE"),
                    author: text(r, "ZAUTHOR"),
                    path: text(r, "ZPATH"),
                    store_id: text(r, "ZSTOREID").map(|s| s.trim().to_string()),
                })
            })
            .collect(),
    )
}

/// The annotations, and how many rows had no uuid or asset id to key them.
async fn read_annotations(
    db: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Vec<Annotation>, usize)> {
    let (scratch, mut conn) = open_snapshot(db, diagnostics).await?;
    let read = read_table(
        &mut conn,
        db,
        ANNOTATION_TABLE,
        ANNOTATION_COLUMNS,
        diagnostics,
    )
    .await;
    conn.close().await.ok();
    drop(scratch);
    let rows = match read {
        Ok(rows) => rows?,
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(db.to_path_buf(), &e));
            return None;
        }
    };
    let mut out = Vec::with_capacity(rows.len());
    let mut unkeyed = 0;
    for r in &rows {
        let (Some(uuid), Some(asset_id)) =
            (text(r, "ZANNOTATIONUUID"), text(r, "ZANNOTATIONASSETID"))
        else {
            unkeyed += 1;
            continue;
        };
        out.push(Annotation {
            uuid,
            asset_id,
            selected_text: text(r, "ZANNOTATIONSELECTEDTEXT"),
            note: text(r, "ZANNOTATIONNOTE"),
            style: int(r, "ZANNOTATIONSTYLE"),
            kind: int(r, "ZANNOTATIONTYPE"),
            location: text(r, "ZANNOTATIONLOCATION"),
            deleted: int(r, "ZANNOTATIONDELETED").is_some_and(|d| d != 0),
            created: real(r, "ZANNOTATIONCREATIONDATE"),
        });
    }
    Some((out, unkeyed))
}

/// Where a database matching `prefix*.sqlite` is in `dir`: the most recently
/// modified one (ties broken by name, so the answer is deterministic).
fn locate(dir: &Path, prefix: &str, diagnostics: &mut Vec<Diagnostic>) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            diagnostics.push(Diagnostic::apple_books_absent(dir.to_path_buf()));
            return None;
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            diagnostics.push(Diagnostic::apple_books_permission_denied(dir.to_path_buf()));
            return None;
        }
        Err(e) => {
            diagnostics.push(Diagnostic::apple_books_db_unreadable(
                dir.to_path_buf(),
                &EngineError::from(e),
            ));
            return None;
        }
    };
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_database_name(name, prefix) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let better = best
            .as_ref()
            .is_none_or(|(t, p)| modified > *t || (modified == *t && path > *p));
        if better {
            best = Some((modified, path));
        }
    }
    if best.is_none() {
        diagnostics.push(Diagnostic::apple_books_absent(dir.to_path_buf()));
    }
    best.map(|(_, p)| p)
}

/// `AEAnnotation_v10312011_1727_local.sqlite` for prefix `AEAnnotation`: the
/// prefix, anything, `.sqlite`. Its `-wal`/`-shm` siblings do not match.
fn is_database_name(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix) && name.ends_with(".sqlite") && name.len() > prefix.len()
}

// ---- turning rows into highlights ----------------------------------------------

/// What one live annotation is.
#[derive(Debug, Clone, PartialEq)]
enum Verdict {
    Highlight(Box<ImportedHighlight>),
    NotAHighlight,
    Unparsed,
}

fn verdict(a: &Annotation, copy_id: Option<i64>) -> Verdict {
    match a.kind {
        Some(TYPE_BOOKMARK) | Some(TYPE_NOT_A_PASSAGE) => return Verdict::NotAHighlight,
        Some(TYPE_HIGHLIGHT) | None => {}
        Some(_) => return Verdict::Unparsed,
    }
    if a.selected_text.is_none() && a.note.is_none() {
        return Verdict::Unparsed;
    }
    let mut h = ImportedHighlight::new(
        HighlightOrigin::AppleBooks,
        a.uuid.clone(),
        a.selected_text.clone().unwrap_or_default(),
    );
    h.copy_id = copy_id;
    h.identity_scope = IdentityScope::Global;
    h.origin_id = Some(a.uuid.clone());
    h.origin_note = a.note.clone();
    h.color = a.style.and_then(style_color).map(str::to_string);
    h.pos0 = a.location.clone();
    h.pos_kind = h.pos0.as_ref().map(|_| PosKind::EpubCfi);
    h.made_at = a.created.and_then(core_data_stamp);
    Verdict::Highlight(Box::new(h))
}

/// `ZAUTHOR` is one display string; "A & B" and "A; B" are two people.
fn authors_of(author: Option<&str>) -> Vec<String> {
    author
        .map(|a| {
            a.split(['&', ';'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ---- matching ----------------------------------------------------------------------

/// `ZPATH` as a regular file on this computer, if it is one.
fn usable_file(path: Option<&str>) -> Option<PathBuf> {
    let raw = path?;
    let p = if raw.starts_with("file:") {
        url::Url::parse(raw).ok()?.to_file_path().ok()?
    } else {
        PathBuf::from(raw)
    };
    (p.is_absolute() && std::fs::metadata(&p).is_ok_and(|m| m.is_file())).then_some(p)
}

/// The ladder for one asset that no place of ours holds. Read-only.
async fn match_asset(
    storage: &Storage,
    asset: &Asset,
    file: Option<&FileIdentity>,
) -> Result<(Option<(i64, AssetMatch)>, Vec<MatchCandidate>)> {
    let mut candidates = Vec::new();
    // ZPATH: a place that last saw these bytes, else the files ladder.
    if let Some(f) = file {
        if let Some(c) = storage.copy_for_hash(&f.sha256).await? {
            return Ok((
                Some((c.book_id, AssetMatch::File(FileMatch::Sha256))),
                Vec::new(),
            ));
        }
        if let Some((book_id, by)) = f.matched {
            return Ok((Some((book_id, AssetMatch::File(by))), Vec::new()));
        }
        candidates = f.candidates.clone();
    }
    // ZSTOREID.
    if let Some(store_id) = &asset.store_id
        && let Some(book_id) = storage
            .book_for_external_id(STORE_ID_SOURCE, store_id)
            .await?
    {
        return Ok((Some((book_id, AssetMatch::StoreId)), Vec::new()));
    }
    // Title and author: the one matcher, auto band only.
    let authors = authors_of(asset.author.as_deref());
    let scored = crate::koreader::scores_for(
        storage,
        &crate::matching::Query::new(asset.title.as_deref(), &authors),
    )
    .await?;
    if let Some(best) = scored.first()
        && best.can_auto
        && let Some(id) = best.book.id
    {
        return Ok((Some((id, AssetMatch::Title)), Vec::new()));
    }
    let band = crate::koreader::band(scored);
    if !band.is_empty() {
        candidates = band;
    }
    Ok((None, candidates))
}

// ---- copies ------------------------------------------------------------------------

/// Find or create the copy this asset is, through its `apple_books` place.
/// `None` when the place is held by another book's copy.
async fn copy_for_asset(
    storage: &Storage,
    book_id: i64,
    asset_id: &str,
    store_bought: bool,
    file: Option<&FileIdentity>,
) -> Result<Option<i64>> {
    if let Some(copy) = storage
        .copy_for_place(PlaceKind::AppleBooks, asset_id)
        .await?
    {
        if copy.book_id != book_id {
            return Ok(None);
        }
        let place = copy
            .places
            .iter()
            .find(|p| p.kind == PlaceKind::AppleBooks && p.reference == asset_id);
        let hash = file
            .map(|f| f.sha256.clone())
            .or_else(|| place.and_then(|p| p.last_hash.clone()));
        let mtime = file
            .and_then(|f| crate::files::mtime_of(&f.path))
            .or_else(|| place.and_then(|p| p.last_mtime));
        storage
            .refresh_place(PlaceKind::AppleBooks, asset_id, hash.as_deref(), mtime)
            .await?;
        return Ok(Some(copy.id));
    }
    let mut place = NewPlace::new(PlaceKind::AppleBooks, asset_id);
    let side_loaded = file.filter(|_| !store_bought);
    let copy_id = match side_loaded {
        Some(f) => {
            place.last_hash = Some(f.sha256.clone());
            place.last_mtime = crate::files::mtime_of(&f.path);
            // One file is one copy wherever it sits.
            let same_bytes = storage
                .copy_for_hash(&f.sha256)
                .await?
                .filter(|c| c.book_id == book_id);
            let same_reader_file = match same_bytes {
                Some(_) => None,
                None => storage
                    .copy_for_place(PlaceKind::Reader, &f.partial_md5)
                    .await?
                    .filter(|c| c.book_id == book_id),
            };
            match same_bytes.or(same_reader_file) {
                Some(c) => c.id,
                None => {
                    storage
                        .create_copy(&NewCopy::new(
                            book_id,
                            Some(CopyForm::File),
                            Tenure::Own,
                            LearnedFrom::AppleBooks,
                        ))
                        .await?
                        .id
                }
            }
        }
        None => create_app_copy(storage, book_id).await?,
    };
    Ok(match storage.attach_place(copy_id, &place).await? {
        PlaceAttach::OtherBook { .. } => None,
        _ => Some(copy_id),
    })
}

/// A `form = app`, `app = apple_books`, `tenure = own` copy. `have_now` unsaid.
async fn create_app_copy(storage: &Storage, book_id: i64) -> Result<i64> {
    let mut new = NewCopy::new(
        book_id,
        Some(CopyForm::App),
        Tenure::Own,
        LearnedFrom::AppleBooks,
    );
    new.app = Some(APP.to_string());
    Ok(storage.create_copy(&new).await?.id)
}

/// The reader's answer to an [`AssetOutcome::Unmatched`]: this asset is that
/// book. Records an app copy with the asset's `apple_books` place (or returns
/// the copy already holding it), so the next import matches it by place. A
/// place held by another book's copy is `InvalidInput`; an unknown book is
/// `NotFound`. Returns the copy id.
pub async fn link_apple_books_asset(
    storage: &Storage,
    asset_id: &str,
    book_id: i64,
) -> Result<i64> {
    if let Some(copy) = storage
        .copy_for_place(PlaceKind::AppleBooks, asset_id)
        .await?
    {
        if copy.book_id == book_id {
            return Ok(copy.id);
        }
        return Err(EngineError::InvalidInput(format!(
            "Apple Books asset {asset_id} is already a copy of another book"
        )));
    }
    if storage.get_book(book_id).await?.is_none() {
        return Err(EngineError::NotFound(format!("book id {book_id}")));
    }
    let copy_id = create_app_copy(storage, book_id).await?;
    storage
        .attach_place(copy_id, &NewPlace::new(PlaceKind::AppleBooks, asset_id))
        .await?;
    Ok(copy_id)
}

// ---- the import --------------------------------------------------------------------

/// Import Apple Books' highlights and notes from the databases under `root`
/// (its `Data/Documents`; see [`default_root`]). `dry_run` writes nothing at
/// all — no book, copy, external id or highlight — and reports what it read
/// and how each asset would match.
///
/// Never fails on absence, permission, or a schema it cannot read: each is a
/// [`Diagnostic`] in the report. `Err` is only a failure of our own database.
#[tracing::instrument(skip(storage), fields(root = %root.display()))]
pub async fn import_apple_books(
    storage: &Storage,
    root: &Path,
    dry_run: bool,
) -> Result<AppleBooksReport> {
    let mut report = AppleBooksReport {
        dry_run,
        ..Default::default()
    };
    let annotation_db = locate(
        &root.join(ANNOTATION_DIR),
        ANNOTATION_DIR,
        &mut report.diagnostics,
    );
    let library_db = locate(
        &root.join(LIBRARY_DIR),
        LIBRARY_DIR,
        &mut report.diagnostics,
    );
    report.annotation_db = annotation_db.clone();
    report.library_db = library_db.clone();
    let Some(annotation_db) = annotation_db else {
        return Ok(report);
    };
    let Some((annotations, unkeyed)) =
        read_annotations(&annotation_db, &mut report.diagnostics).await
    else {
        return Ok(report);
    };
    report.totals.unparsed += unkeyed;
    let assets: HashMap<String, Asset> = match &library_db {
        Some(db) => read_assets(db, &mut report.diagnostics)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| (a.asset_id.clone(), a))
            .collect(),
        None => HashMap::new(),
    };
    report.assets_in_library = assets.len();

    let mut by_asset: BTreeMap<String, Vec<Annotation>> = BTreeMap::new();
    for a in annotations {
        by_asset.entry(a.asset_id.clone()).or_default().push(a);
    }
    report.assets_skipped += assets
        .keys()
        .filter(|id| !by_asset.contains_key(*id))
        .count();

    for (asset_id, mut anns) in by_asset {
        // Creation order, then uuid: the same snapshot imports in the same order.
        anns.sort_by(|a, b| {
            a.created
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&b.created.unwrap_or(f64::NEG_INFINITY))
                .then_with(|| a.uuid.cmp(&b.uuid))
        });
        let asset = assets.get(&asset_id).cloned();
        if let Some(entry) =
            import_asset(storage, &asset_id, asset, &anns, dry_run, &mut report).await?
        {
            let t = &mut report.totals;
            t.inserted += entry.summary.inserted;
            t.updated += entry.summary.updated;
            t.unchanged += entry.summary.unchanged;
            t.removed += entry.summary.removed;
            t.unparsed += entry.summary.unparsed;
            report.books.push(entry);
        }
    }
    tracing::debug!(
        books = report.books.len(),
        inserted = report.totals.inserted,
        updated = report.totals.updated,
        unchanged = report.totals.unchanged,
        removed = report.totals.removed,
        unparsed = report.totals.unparsed,
        skipped = report.assets_skipped,
        dry_run,
        "imported apple books"
    );
    Ok(report)
}

/// One asset's annotations. `None` when it is skipped (counted in `report`).
async fn import_asset(
    storage: &Storage,
    asset_id: &str,
    asset: Option<Asset>,
    anns: &[Annotation],
    dry_run: bool,
    report: &mut AppleBooksReport,
) -> Result<Option<AppleBooksBook>> {
    let mut summary = HighlightImportSummary::default();
    let mut importable = 0usize;
    let mut not_highlights = 0usize;
    let deleted = anns.iter().filter(|a| a.deleted).count();
    for a in anns.iter().filter(|a| !a.deleted) {
        match verdict(a, None) {
            Verdict::Highlight(_) => importable += 1,
            Verdict::NotAHighlight => not_highlights += 1,
            Verdict::Unparsed => summary.unparsed += 1,
        }
    }
    report.not_highlights += not_highlights;

    let place_copy = storage
        .copy_for_place(PlaceKind::AppleBooks, asset_id)
        .await?;
    if place_copy.is_none() && importable == 0 {
        // Nothing to bring in, and nothing of ours to remove from: never a
        // reason to make a book. Counted, not listed.
        report.assets_skipped += 1;
        report.totals.unparsed += summary.unparsed;
        return Ok(None);
    }
    let asset = match (asset, &place_copy) {
        (Some(a), _) => a,
        (None, Some(_)) => Asset {
            asset_id: asset_id.to_string(),
            ..Default::default()
        },
        (None, None) => {
            // The library database does not know it and neither do we: there is
            // no title, path or store id to find a book by.
            report.orphaned += importable;
            report.totals.unparsed += summary.unparsed;
            return Ok(None);
        }
    };
    tracing::trace!(asset_id, title = ?asset.title, "apple books asset");
    let store_bought = asset.store_id.is_some();

    // The file, for matching and for the copy, only when no place decides.
    let file = match &place_copy {
        Some(_) => None,
        None => match usable_file(asset.path.as_deref()) {
            Some(p) => crate::files::identify(storage, &p).await.ok(),
            None => None,
        },
    };
    let (matched, candidates) = match &place_copy {
        Some(c) => (Some((c.book_id, AssetMatch::Place)), Vec::new()),
        None => match_asset(storage, &asset, file.as_ref()).await?,
    };

    let mut entry = AppleBooksBook {
        asset_id: asset_id.to_string(),
        title: asset.title.clone(),
        authors: authors_of(asset.author.as_deref()),
        store_bought,
        outcome: AssetOutcome::WouldCreate,
        copy_id: place_copy.as_ref().map(|c| c.id),
        highlights_in_source: importable,
        deleted_in_source: deleted,
        summary: HighlightImportSummary::default(),
    };
    let book_id = match matched {
        Some((book_id, by)) => {
            entry.outcome = AssetOutcome::Matched { book_id, by };
            Some(book_id)
        }
        None if !candidates.is_empty() || asset.title.is_none() => {
            entry.outcome = AssetOutcome::Unmatched { candidates };
            None
        }
        None if dry_run => None,
        None => {
            let book_id = storage
                .upsert_book(
                    &Book {
                        title: asset.title.clone(),
                        authors: entry.authors.clone(),
                        ..Default::default()
                    },
                    None,
                )
                .await?;
            entry.outcome = AssetOutcome::Created { book_id };
            Some(book_id)
        }
    };
    let (Some(book_id), false) = (book_id, dry_run) else {
        entry.summary = summary;
        return Ok(Some(entry));
    };

    if let Some(store_id) = &asset.store_id {
        storage
            .link_external_id(STORE_ID_SOURCE, store_id, book_id)
            .await?;
    }
    let copy_id = copy_for_asset(storage, book_id, asset_id, store_bought, file.as_ref()).await?;
    entry.copy_id = copy_id;

    for a in anns.iter().filter(|a| !a.deleted) {
        let Verdict::Highlight(h) = verdict(a, copy_id) else {
            continue;
        };
        match storage.upsert_imported_highlight(book_id, &h).await {
            Ok(ImportedOutcome::Inserted(_)) => summary.inserted += 1,
            Ok(ImportedOutcome::Updated(_)) => summary.updated += 1,
            Ok(ImportedOutcome::Unchanged(_)) => summary.unchanged += 1,
            Err(EngineError::InvalidInput(_)) => summary.unparsed += 1,
            Err(e) => return Err(e),
        }
    }

    // Deletions follow the source: only rows Apple Books marks deleted.
    if deleted > 0 {
        let held: HashMap<String, i64> = storage
            .imported_highlights(book_id, HighlightOrigin::AppleBooks)
            .await?
            .into_iter()
            .filter_map(|h| Some((h.origin_id?, h.id)))
            .collect();
        let mut kept = 0usize;
        for a in anns.iter().filter(|a| a.deleted) {
            let Some(&id) = held.get(&a.uuid) else {
                continue;
            };
            if storage.imported_highlight_kept(id).await? {
                kept += 1;
            } else if storage
                .remove_imported_highlight(book_id, HighlightOrigin::AppleBooks, &a.uuid)
                .await?
            {
                summary.removed += 1;
            }
        }
        if kept > 0 {
            summary
                .diagnostics
                .push(Diagnostic::apple_books_highlights_kept(asset_id, kept));
        }
    }
    entry.summary = summary;
    Ok(Some(entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_data_dates_are_seconds_since_2001_utc() {
        assert_eq!(core_data_to_unix(0.0), Some(978_307_200));
        assert_eq!(core_data_stamp(0.0).as_deref(), Some("2001-01-01 00:00:00"));
        // 2026-01-05 21:14:08 UTC.
        let secs = (1_767_647_648 - CORE_DATA_EPOCH_UNIX) as f64 + 0.75;
        assert_eq!(core_data_to_unix(secs), Some(1_767_647_648), "rounded down");
        assert_eq!(
            core_data_stamp(secs).as_deref(),
            Some("2026-01-05 21:14:08")
        );
        // Before the epoch is negative, and still a date.
        assert_eq!(
            core_data_stamp(-86_400.0).as_deref(),
            Some("2000-12-31 00:00:00")
        );
        assert_eq!(core_data_to_unix(f64::NAN), None);
        assert_eq!(core_data_to_unix(f64::INFINITY), None);
        assert_eq!(core_data_to_unix(1e300), None);
    }

    #[test]
    fn styles_map_to_named_colours_and_unknown_is_none() {
        assert_eq!(style_color(3), Some("yellow"));
        assert_eq!(style_color(1), Some("green"));
        assert_eq!(style_color(0), None, "an underline has no colour");
        assert_eq!(style_color(42), None);
    }

    #[test]
    fn database_names_match_by_pattern_not_by_version() {
        assert!(is_database_name(
            "AEAnnotation_v10312011_1727_local.sqlite",
            "AEAnnotation"
        ));
        assert!(is_database_name(
            "BKLibrary-1-091020131601.sqlite",
            "BKLibrary"
        ));
        assert!(!is_database_name(
            "AEAnnotation_v10312011_1727_local.sqlite-wal",
            "AEAnnotation"
        ));
        assert!(!is_database_name("BKLibrary.sqlite-shm", "BKLibrary"));
        assert!(!is_database_name("Other.sqlite", "BKLibrary"));
    }

    fn ann(kind: Option<i64>, text: Option<&str>, note: Option<&str>) -> Annotation {
        Annotation {
            uuid: "U-1".into(),
            asset_id: "A".into(),
            selected_text: text.map(str::to_string),
            note: note.map(str::to_string),
            style: Some(3),
            kind,
            location: Some("epubcfi(/6/4!/4/2/1:0)".into()),
            deleted: false,
            created: Some(0.0),
        }
    }

    #[test]
    fn a_highlight_carries_its_cfi_verbatim_and_no_page() {
        let Verdict::Highlight(h) = verdict(&ann(Some(2), Some("words"), Some("mine")), Some(7))
        else {
            panic!("a highlight");
        };
        assert_eq!(h.origin, HighlightOrigin::AppleBooks);
        assert_eq!(h.origin_id.as_deref(), Some("U-1"));
        assert_eq!(h.identity_scope, IdentityScope::Global);
        assert_eq!(h.copy_id, Some(7));
        assert_eq!(h.pos_kind, Some(PosKind::EpubCfi));
        assert_eq!(h.pos0.as_deref(), Some("epubcfi(/6/4!/4/2/1:0)"));
        assert_eq!(h.page, None);
        assert_eq!(h.color.as_deref(), Some("yellow"));
        assert_eq!(h.made_at.as_deref(), Some("2001-01-01 00:00:00"));
        assert!(!h.text_unavailable);
    }

    #[test]
    fn types_decide_what_is_a_highlight() {
        assert_eq!(
            verdict(&ann(Some(1), Some("x"), None), None),
            Verdict::NotAHighlight
        );
        assert_eq!(
            verdict(&ann(Some(3), Some("x"), None), None),
            Verdict::NotAHighlight
        );
        assert_eq!(
            verdict(&ann(Some(9), Some("x"), None), None),
            Verdict::Unparsed
        );
        assert_eq!(verdict(&ann(Some(2), None, None), None), Verdict::Unparsed);
        // A NULL type is judged by content.
        assert!(matches!(
            verdict(&ann(None, Some("x"), None), None),
            Verdict::Highlight(_)
        ));
        // Note-only: empty text, not text_unavailable.
        let Verdict::Highlight(h) = verdict(&ann(Some(2), None, Some("a thought")), None) else {
            panic!("note-only");
        };
        assert_eq!(h.text, "");
        assert!(!h.text_unavailable);
        assert_eq!(h.origin_note.as_deref(), Some("a thought"));
    }

    #[test]
    fn authors_split_on_ampersands_and_semicolons() {
        assert_eq!(
            authors_of(Some("Ursula K. Le Guin")),
            vec!["Ursula K. Le Guin"]
        );
        assert_eq!(authors_of(Some("A & B; C")), vec!["A", "B", "C"]);
        assert!(authors_of(None).is_empty());
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Conversion preserves order: a later Core Data date is never an
        /// earlier unix second or an earlier stamp. Stamps compare as text, so
        /// this is also what keeps `ko_datetime` sortable for Apple rows.
        #[test]
        fn core_data_conversion_is_monotonic(a in -1e11f64..1e11, b in -1e11f64..1e11) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if let (Some(x), Some(y)) = (core_data_to_unix(lo), core_data_to_unix(hi)) {
                prop_assert!(x <= y);
                let (sx, sy) = (core_data_stamp(lo).unwrap(), core_data_stamp(hi).unwrap());
                prop_assert!(sx <= sy, "{} > {}", sx, sy);
            }
        }

        /// Whole seconds round-trip exactly.
        #[test]
        fn whole_seconds_land_on_the_epoch_offset(s in -1_000_000_000i64..4_000_000_000) {
            prop_assert_eq!(core_data_to_unix(s as f64), Some(s + CORE_DATA_EPOCH_UNIX));
        }
    }
}
