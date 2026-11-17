//! Apple Books (macOS): imports highlights and notes. The databases are live WAL files owned by
//! another app, so each is copied out and only the copy is opened.
//! Only `ZANNOTATIONDELETED = 1` removes a highlight; absence from the database is not deletion.
//! An unmatched asset becomes a book only if it has a highlight, and never over a candidate.

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

pub const ANNOTATION_DIR: &str = "AEAnnotation";
pub const LIBRARY_DIR: &str = "BKLibrary";
const ANNOTATION_TABLE: &str = "ZAEANNOTATION";
const ASSET_TABLE: &str = "ZBKLIBRARYASSET";
pub const STORE_ID_SOURCE: &str = "apple_books";
pub const APP: &str = "apple_books";
pub const CORE_DATA_EPOCH_UNIX: i64 = 978_307_200;
const MIN_UNIX: i64 = -62_135_596_800;
const MAX_UNIX: i64 = 253_402_300_799;

const TYPE_BOOKMARK: i64 = 1;
const TYPE_HIGHLIGHT: i64 = 2;
const TYPE_NOT_A_PASSAGE: i64 = 3;

/// `ZANNOTATIONSTYLE` to colour name; 0 is an underline, which has no colour.
const STYLE_COLORS: &[(i64, &str)] = &[
    (1, "green"),
    (2, "blue"),
    (3, "yellow"),
    (4, "pink"),
    (5, "purple"),
];

pub fn default_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Containers/com.apple.iBooksX/Data/Documents"))
}

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

pub fn style_color(style: i64) -> Option<&'static str> {
    STYLE_COLORS
        .iter()
        .find(|(s, _)| *s == style)
        .map(|(_, c)| *c)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetMatch {
    Place,
    File(FileMatch),
    StoreId,
    Title,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssetOutcome {
    Matched {
        book_id: i64,
        by: AssetMatch,
    },
    Created {
        book_id: i64,
    },
    WouldCreate,
    /// Nothing matched outright. Candidates, if any, are the decision to make; nothing was written.
    Unmatched {
        candidates: Vec<MatchCandidate>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppleBooksBook {
    pub asset_id: String,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub store_bought: bool,
    pub outcome: AssetOutcome,
    pub copy_id: Option<i64>,
    pub highlights_in_source: usize,
    pub deleted_in_source: usize,
    pub summary: HighlightImportSummary,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppleBooksReport {
    pub dry_run: bool,
    pub annotation_db: Option<PathBuf>,
    pub library_db: Option<PathBuf>,
    pub books: Vec<AppleBooksBook>,
    /// Sum of every book's summary plus annotations placed on no asset; its `diagnostics` is empty.
    pub totals: HighlightImportSummary,
    pub assets_in_library: usize,
    pub assets_skipped: usize,
    pub not_highlights: usize,
    pub orphaned: usize,
    pub diagnostics: Vec<Diagnostic>,
}

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

/// A number column holding text reads as NULL, not SQLite's `CAST('x' AS INTEGER)` zero, which
/// would be a fabricated value.
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
    // Read-only and never created: a missing copy is an error. With `-wal`/`-shm` beside it, SQLite
    // still reads the uncheckpointed rows.
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
    // Every statement in one fallible region: SQLite only notices a non-database file on its first
    // statement.
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

fn is_database_name(name: &str, prefix: &str) -> bool {
    name.starts_with(prefix) && name.ends_with(".sqlite") && name.len() > prefix.len()
}

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

fn usable_file(path: Option<&str>) -> Option<PathBuf> {
    let raw = path?;
    let p = if raw.starts_with("file:") {
        url::Url::parse(raw).ok()?.to_file_path().ok()?
    } else {
        PathBuf::from(raw)
    };
    (p.is_absolute() && std::fs::metadata(&p).is_ok_and(|m| m.is_file())).then_some(p)
}

async fn match_asset(
    storage: &Storage,
    asset: &Asset,
    file: Option<&FileIdentity>,
) -> Result<(Option<(i64, AssetMatch)>, Vec<MatchCandidate>)> {
    let mut candidates = Vec::new();
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
    if let Some(store_id) = &asset.store_id
        && let Some(book_id) = storage
            .book_for_external_id(STORE_ID_SOURCE, store_id)
            .await?
    {
        return Ok((Some((book_id, AssetMatch::StoreId)), Vec::new()));
    }
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

/// Records an app copy with the asset's `apple_books` place (or returns the one holding it), so the
/// next import matches by place. A place held by another book's copy is `InvalidInput`.
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

/// `dry_run` writes nothing at all. Absence, permission and unreadable schemas are diagnostics;
/// `Err` is only a failure of our own database.
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

#[expect(clippy::too_many_lines)]
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
        // Nothing to import and nothing of ours to remove: never a reason to create a book.
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
            report.orphaned += importable;
            report.totals.unparsed += summary.unparsed;
            return Ok(None);
        }
    };
    tracing::trace!(asset_id, title = ?asset.title, "apple books asset");
    let store_bought = asset.store_id.is_some();

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

    // Deletions follow the source: only rows Apple Books marks deleted; merely absent rows stay.
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
        let secs = (1_767_647_648 - CORE_DATA_EPOCH_UNIX) as f64 + 0.75;
        assert_eq!(core_data_to_unix(secs), Some(1_767_647_648), "rounded down");
        assert_eq!(
            core_data_stamp(secs).as_deref(),
            Some("2026-01-05 21:14:08")
        );
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
        assert!(matches!(
            verdict(&ann(None, Some("x"), None), None),
            Verdict::Highlight(_)
        ));
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
        #[test]
        fn core_data_conversion_is_monotonic(a in -1e11f64..1e11, b in -1e11f64..1e11) {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if let (Some(x), Some(y)) = (core_data_to_unix(lo), core_data_to_unix(hi)) {
                prop_assert!(x <= y);
                let (sx, sy) = (core_data_stamp(lo).unwrap(), core_data_stamp(hi).unwrap());
                prop_assert!(sx <= sy, "{} > {}", sx, sy);
            }
        }

        #[test]
        fn whole_seconds_land_on_the_epoch_offset(s in -1_000_000_000i64..4_000_000_000) {
            prop_assert_eq!(core_data_to_unix(s as f64), Some(s + CORE_DATA_EPOCH_UNIX));
        }
    }
}
