use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mlua::{Lua, LuaOptions, StdLib, Table, Value};

use crate::book::Book;
use crate::copies::{CopyForm, LearnedFrom, NewCopy, NewPlace, PlaceAttach, PlaceKind, Tenure};
use crate::diagnostic::Diagnostic;
use crate::error::{EngineError, Result};
use crate::flashcards::single_word;
use crate::matching::{Prepared, Query};
use crate::storage::{LinkedBy, NewHighlight, Source, Storage, ko_datetime_to_unix};

/// Instructions a sidecar chunk may run before it is killed. Kept low so a hostile
/// `while true do end` trips in milliseconds even under sanitizer instrumentation.
const LUA_INSTRUCTION_BUDGET: u32 = 5_000_000;

const MAX_LIBRARY_DEPTH: usize = 32;

#[cfg(test)]
use crate::matching::AUTO_MATCH;
pub(crate) use crate::matching::CANDIDATE_MIN;

#[derive(Debug, Default)]
pub struct KoSidecar {
    pub title: Option<String>,
    pub authors: Option<String>,
    pub language: Option<String>,
    pub partial_md5: Option<String>,
    /// Root `doc_pages`. Kept apart from `stats.pages`, which can differ by one and is
    /// absent on most sidecars.
    pub doc_pages: Option<i64>,
    pub percent_finished: Option<f64>,
    pub summary: Option<KoSummary>,
    pub stats: Option<KoStats>,
    pub highlights: Vec<NewHighlight>,
    /// Count of highlights whose `pos0` is a table (paging documents) and cannot be stored.
    /// A count so a 300-highlight PDF yields one diagnostic per file, not 300.
    pub unsupported_anchors: usize,
}

/// KOReader's per-book reading status. `Other` carries unknown values through so a new
/// status never fails an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KoStatus {
    Reading,
    Abandoned,
    Complete,
    Other(String),
}

impl KoStatus {
    pub fn is_unknown(&self) -> bool {
        matches!(self, KoStatus::Other(_))
    }
}

impl From<&str> for KoStatus {
    fn from(s: &str) -> Self {
        match s {
            "reading" => KoStatus::Reading,
            "abandoned" => KoStatus::Abandoned,
            "complete" => KoStatus::Complete,
            other => KoStatus::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for KoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KoStatus::Reading => write!(f, "reading"),
            KoStatus::Abandoned => write!(f, "abandoned"),
            KoStatus::Complete => write!(f, "complete"),
            KoStatus::Other(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KoSummary {
    pub status: Option<KoStatus>,
    /// 1..=5. Absent means unrated, not zero: KOReader deletes the key when cleared.
    pub rating: Option<i64>,
    pub note: Option<String>,
    /// `"%Y-%m-%d"`. Date only, unlike annotation datetimes.
    pub modified: Option<String>,
}

/// The `stats` subtable: legacy residue. `md5` and `total_time_in_sec` are absent in
/// current files and nothing may depend on them; the book id is `partial_md5_checksum`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KoStats {
    pub title: Option<String>,
    pub authors: Option<String>,
    pub language: Option<String>,
    pub series: Option<String>,
    pub pages: Option<i64>,
    /// Counts annotations *without* a note; `notes` counts those *with* one.
    pub highlights: Option<i64>,
    pub notes: Option<i64>,
    pub md5: Option<String>,
    pub total_time_in_sec: Option<i64>,
}

/// A Lua VM with no stdlib and a bounded instruction count: `StdLib::NONE` still allows
/// infinite loops and the files are untrusted. Shared with `plugin.rs` as the one sandbox.
pub(crate) fn sandboxed_lua() -> Result<Lua> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())
        .map_err(|e| EngineError::Sidecar(format!("lua init: {e}")))?;
    lua.set_hook(
        mlua::HookTriggers::new().every_nth_instruction(LUA_INSTRUCTION_BUDGET),
        |_lua, _debug| {
            Err(mlua::Error::RuntimeError(
                "chunk exceeded its instruction budget (runaway loop?)".to_string(),
            ))
        },
    );
    Ok(lua)
}

pub(crate) fn eval_table(lua: &Lua, src: &str, what: &str) -> Result<mlua::Table> {
    // Name the chunk, or mlua names it after this Rust file and the user cannot tell which
    // of their files is unreadable.
    let value: Value = lua
        .load(src)
        .set_name(what)
        .eval()
        .map_err(|e| EngineError::Sidecar(format!("{what} lua eval: {e}")))?;
    match value {
        Value::Table(t) => Ok(t),
        _ => Err(EngineError::Sidecar(format!(
            "{what} did not return a table"
        ))),
    }
}

pub fn parse_sidecar(src: &str) -> Result<KoSidecar> {
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, src, "sidecar")?;

    let mut sidecar = KoSidecar {
        partial_md5: get_str(&root, "partial_md5_checksum"),
        doc_pages: get_int(&root, "doc_pages"),
        percent_finished: get_f64(&root, "percent_finished"),
        // Root keys, written independently of the annotations layout; read before the layout
        // dispatch so legacy sidecars carry them too.
        summary: get_table(&root, "summary").map(|t| parse_summary(&t)),
        stats: get_table(&root, "stats").map(|t| parse_stats(&t)),
        ..Default::default()
    };
    if let Some(props) = get_table(&root, "doc_props") {
        sidecar.title = get_str(&props, "title");
        sidecar.authors = get_str(&props, "authors");
        sidecar.language = get_str(&props, "language");
    }

    if let Some(annotations) = get_table(&root, "annotations") {
        let parsed = parse_annotations(&annotations)?;
        sidecar.highlights = parsed.highlights;
        sidecar.unsupported_anchors = parsed.unsupported_anchors;
    } else if let Some(highlight) = get_table(&root, "highlight") {
        let notes_by_datetime = get_table(&root, "bookmarks")
            .map(|b| bookmark_notes(&b))
            .unwrap_or_default();
        let parsed = parse_legacy(&highlight, &notes_by_datetime)?;
        sidecar.highlights = parsed.highlights;
        sidecar.unsupported_anchors = parsed.unsupported_anchors;
    }
    Ok(sidecar)
}

pub(crate) fn get_str(t: &Table, key: &str) -> Option<String> {
    t.get::<Option<String>>(key)
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

pub(crate) fn get_int(t: &Table, key: &str) -> Option<i64> {
    t.get::<Option<i64>>(key).ok().flatten()
}

/// A finished book is the bare integer `1`, not `1.0`; `f64` conversion accepts both.
fn get_f64(t: &Table, key: &str) -> Option<f64> {
    t.get::<Option<f64>>(key).ok().flatten()
}

fn get_table(t: &Table, key: &str) -> Option<Table> {
    t.get::<Option<Table>>(key).ok().flatten()
}

fn parse_summary(t: &Table) -> KoSummary {
    KoSummary {
        status: get_str(t, "status").map(|s| KoStatus::from(s.as_str())),
        rating: get_int(t, "rating"),
        note: get_str(t, "note"),
        modified: get_str(t, "modified"),
    }
}

fn parse_stats(t: &Table) -> KoStats {
    KoStats {
        title: get_str(t, "title"),
        authors: get_str(t, "authors"),
        language: get_str(t, "language"),
        series: get_str(t, "series"),
        pages: get_int(t, "pages"),
        highlights: get_int(t, "highlights"),
        notes: get_int(t, "notes"),
        md5: get_str(t, "md5"),
        total_time_in_sec: get_int(t, "total_time_in_sec"),
    }
}

enum Anchor {
    Xpointer(String),
    Absent,
    Unsupported,
}

fn anchor(item: &Table) -> Anchor {
    if matches!(item.get::<Value>("pos0"), Ok(Value::Table(_))) {
        return Anchor::Unsupported;
    }
    match get_str(item, "pos0") {
        Some(pos0) => Anchor::Xpointer(pos0),
        None => Anchor::Absent,
    }
}

enum Entry {
    Highlight(Box<NewHighlight>),
    NotAHighlight,
    UnsupportedAnchor,
}

fn entry_to_highlight(item: &Table, page: Option<i64>) -> Entry {
    // `text` is tested before the anchor: an entry without text would not import anyway,
    // so it must not count as lost to the anchor.
    let Some(text) = get_str(item, "text") else {
        return Entry::NotAHighlight;
    };
    // Modern `annotations` mixes highlights and bookmarks; a table `pos0` is an
    // unanchorable highlight, not a bookmark.
    let pos0 = match anchor(item) {
        Anchor::Xpointer(pos0) => pos0,
        Anchor::Absent => return Entry::NotAHighlight,
        Anchor::Unsupported => return Entry::UnsupportedAnchor,
    };
    Entry::Highlight(Box::new(NewHighlight {
        text,
        chapter: get_str(item, "chapter"),
        page: get_int(item, "pageno").or(get_int(item, "page")).or(page),
        pos0: Some(pos0),
        pos1: get_str(item, "pos1"),
        ko_datetime: get_str(item, "datetime"),
        ko_datetime_updated: get_str(item, "datetime_updated"),
        color: get_str(item, "color").or_else(|| get_str(item, "drawer")),
        note: get_str(item, "note"),
        source: "koreader".to_string(),
    }))
}

struct Parsed {
    highlights: Vec<NewHighlight>,
    unsupported_anchors: usize,
}

fn parse_annotations(annotations: &Table) -> Result<Parsed> {
    // `sequence_values` stops at the first missing index, dropping the rest after a sync
    // conflict (`{[1]=.., [3]=..}`); iterate the pairs and sort instead.
    let mut indexed: Vec<(i64, NewHighlight)> = Vec::new();
    let mut unsupported_anchors = 0;
    for pair in annotations.pairs::<i64, Table>() {
        let (idx, item) =
            pair.map_err(|e| EngineError::Sidecar(format!("annotation entry: {e}")))?;
        match entry_to_highlight(&item, None) {
            Entry::Highlight(h) => indexed.push((idx, *h)),
            Entry::UnsupportedAnchor => unsupported_anchors += 1,
            Entry::NotAHighlight => {}
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    Ok(Parsed {
        highlights: indexed.into_iter().map(|(_, h)| h).collect(),
        unsupported_anchors,
    })
}

fn parse_legacy(highlight: &Table, notes_by_datetime: &HashMap<String, String>) -> Result<Parsed> {
    let mut out = Vec::new();
    let mut unsupported_anchors = 0;
    for pair in highlight.pairs::<i64, Table>() {
        let (page, items) =
            pair.map_err(|e| EngineError::Sidecar(format!("highlight page: {e}")))?;
        for item in items.pairs::<i64, Table>() {
            let (_, item) =
                item.map_err(|e| EngineError::Sidecar(format!("highlight item: {e}")))?;
            match entry_to_highlight(&item, Some(page)) {
                Entry::Highlight(mut h) => {
                    if h.note.is_none()
                        && let Some(dt) = &h.ko_datetime
                    {
                        h.note = notes_by_datetime.get(dt).cloned();
                    }
                    out.push(*h);
                }
                Entry::UnsupportedAnchor => unsupported_anchors += 1,
                Entry::NotAHighlight => {}
            }
        }
    }
    out.sort_by_key(|h| (h.page, h.ko_datetime.clone()));
    Ok(Parsed {
        highlights: out,
        unsupported_anchors,
    })
}

/// Legacy bookmarks: `text` holds the user's note, `notes` the highlighted passage.
/// Only entries with a real user note are kept.
fn bookmark_notes(bookmarks: &Table) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in bookmarks.pairs::<i64, Table>() {
        let Ok((_, item)) = pair else { continue };
        let (Some(dt), Some(text)) = (get_str(&item, "datetime"), get_str(&item, "text")) else {
            continue;
        };
        let highlighted = get_str(&item, "notes");
        if Some(&text) != highlighted.as_ref() {
            map.insert(dt, text);
        }
    }
    map
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub imported: Vec<BookImportStats>,
    pub unmatched: Vec<UnmatchedSidecar>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMethod {
    Md5,
    Isbn,
    Title,
    /// Created from the sidecar's own metadata; only `import_book_from_sidecar` produces this.
    New,
}

impl std::fmt::Display for MatchMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchMethod::Md5 => write!(f, "md5"),
            MatchMethod::Isbn => write!(f, "isbn"),
            MatchMethod::Title => write!(f, "title"),
            MatchMethod::New => write!(f, "new"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchCandidate {
    pub book_id: i64,
    pub title: String,
    /// Authors as a person says them (`Frank Herbert`). Display order only: name parsing is
    /// the engine's, so a client must not need a raw list to re-derive it.
    pub authors_display: Vec<String>,
    pub publish_year: Option<i64>,
    pub score: f64,
}

#[derive(Debug)]
pub struct BookImportStats {
    pub book_id: i64,
    pub book_title: String,
    pub inserted: usize,
    /// Existing rows whose device-owned payload (`origin_note`, `color`, `chapter`, `page`)
    /// differed and was refreshed toward the device. Never counts our `annotation`.
    pub updated: usize,
    pub skipped: usize,
    pub flashcards: usize,
    pub matched_by: MatchMethod,
    pub percent_finished: Option<f64>,
    pub status: Option<KoStatus>,
    pub rating: Option<i64>,
}

#[derive(Debug)]
pub struct UnmatchedSidecar {
    pub path: PathBuf,
    pub title: Option<String>,
    pub partial_md5: Option<String>,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug)]
pub struct PullReport {
    pub stats: BookImportStats,
    pub warnings: Vec<Diagnostic>,
}

pub fn find_sidecars(path: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    collect_sidecars(path, &mut found)?;
    found.sort();
    Ok(found)
}

fn collect_sidecars(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    collect_sidecars_at(path, out, 0)
}

fn collect_sidecars_at(path: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    if path.is_file() {
        if is_sidecar_file(path) {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }
    // `is_dir()` follows symlinks, so a link to an ancestor recurses forever; cap the depth.
    if depth >= MAX_LIBRARY_DEPTH {
        tracing::warn!(
            path = %path.display(),
            depth,
            "library walk hit its depth cap; not descending further"
        );
        return Ok(());
    }
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let p = entry?.path();
            // Never follow directory symlinks: it cannot reach sidecars the real tree lacks, and it
            // makes cycles impossible.
            let is_link = std::fs::symlink_metadata(&p)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false);
            if p.is_dir() {
                if is_link {
                    tracing::debug!(path = %p.display(), "skipping symlinked directory");
                    continue;
                }
                collect_sidecars_at(&p, out, depth + 1)?;
            } else if is_sidecar_file(&p) {
                out.push(p);
            }
        }
    }
    Ok(())
}

/// Is this one of KOReader's live sidecar files? `metadata.epub.lua.old` (a backup of a
/// previous state) must be excluded or deleted highlights resurrect. Bare `metadata.lua`
/// is excluded too: sidecars are always `metadata.<ext>.lua`, and KOReader ships a
/// `calibre.koplugin/metadata.lua` module that is not one.
pub fn is_sidecar_file(p: &Path) -> bool {
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(ext) = name
        .strip_prefix("metadata.")
        .and_then(|rest| rest.strip_suffix(".lua"))
    else {
        return false;
    };
    !ext.is_empty()
}

/// Match a sidecar to a library book: recorded `device_books` link, sibling ebook ISBN,
/// then the shared title/author matcher. Reads only `partial_md5`, `title` and `authors`
/// off `sc`, so a scan can call it from cached facts.
pub async fn match_book(
    storage: &Storage,
    sidecar_path: &Path,
    sc: &KoSidecar,
) -> Result<Option<(Book, MatchMethod)>> {
    // The recorded link goes first: it is not a guess, and it covers sidecars with no sibling
    // ebook (`hash`/`dir` layouts) and titles edited on either side.
    if let Some(md5) = &sc.partial_md5
        && let Some(book) = storage.find_book_by_partial_md5(md5).await?
    {
        return Ok(Some((book, MatchMethod::Md5)));
    }

    if let Some(sdr_dir) = sidecar_path.parent()
        && sdr_dir.extension().and_then(|e| e.to_str()) == Some("sdr")
        && let (Some(parent), Some(stem)) = (sdr_dir.parent(), sdr_dir.file_stem())
    {
        for ext in ["epub", "EPUB"] {
            let candidate = parent.join(format!("{}.{ext}", stem.to_string_lossy()));
            if candidate.is_file()
                && let Ok(Some(isbn)) = crate::epub::epub_info(&candidate).map(|i| i.isbn)
                && let Some(book) = storage.find_book_by_isbn(&isbn).await?
            {
                return Ok(Some((book, MatchMethod::Isbn)));
            }
        }
    }

    let mut scored = title_scores(storage, sc).await?;
    if scored.first().is_some_and(|s| s.can_auto) {
        return Ok(Some((scored.remove(0).book, MatchMethod::Title)));
    }
    Ok(None)
}

async fn title_scores(storage: &Storage, sc: &KoSidecar) -> Result<Vec<Scored>> {
    let authors = split_authors(sc.authors.as_deref());
    scores_for(storage, &Query::new(sc.title.as_deref(), &authors)).await
}

pub(crate) struct Scored {
    pub score: f64,
    pub can_auto: bool,
    pub book: Book,
}

/// The scan, given whatever the other system knows. The one matcher for every source.
/// Coincidental matches are absent from the result, not present with a low score.
pub(crate) async fn scores_for(storage: &Storage, query: &Query<'_>) -> Result<Vec<Scored>> {
    let Some(prepared) = Prepared::new(query) else {
        return Ok(Vec::new());
    };
    // Every book, uncapped: a book outside the window would get a duplicate, not a match.
    let mut scored: Vec<Scored> = storage
        .list_books(&crate::BookQuery::default())
        .await?
        .into_iter()
        .filter_map(|book| {
            let v = prepared.compare(&book)?;
            Some(Scored {
                score: v.score,
                can_auto: v.can_auto,
                book,
            })
        })
        .collect();
    // `total_cmp`: a NaN must not panic mid-import. Ties break on book id for a stable order.
    scored.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.book.id.cmp(&b.book.id)));
    Ok(scored)
}

pub(crate) fn band(scored: Vec<Scored>) -> Vec<MatchCandidate> {
    scored
        .into_iter()
        .filter(|s| !s.can_auto && s.score >= CANDIDATE_MIN)
        .filter_map(|s| {
            Some(MatchCandidate {
                book_id: s.book.id?,
                title: s.book.display_title().to_string(),
                authors_display: s.book.authors_in_display_order(),
                publish_year: s.book.publish_year,
                score: s.score,
            })
        })
        .collect()
}

pub async fn match_candidates(storage: &Storage, sc: &KoSidecar) -> Result<Vec<MatchCandidate>> {
    Ok(band(title_scores(storage, sc).await?))
}

pub async fn import(storage: &Storage, path: &Path, dry_run: bool) -> Result<ImportReport> {
    import_for_device(storage, path, dry_run, None).await
}

/// [`import`] for a known paired reader: each copy's `reader` place records the device id
/// in `detail` (filled when empty, never overwritten with `None`).
pub async fn import_for_device(
    storage: &Storage,
    path: &Path,
    dry_run: bool,
    device_id: Option<&str>,
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    let sidecars = find_sidecars(path)?;
    if sidecars.is_empty() {
        tracing::warn!(path = %path.display(), "no KOReader sidecars found");
        report
            .warnings
            .push(Diagnostic::no_sidecars_found(path.to_path_buf()));
        return Ok(report);
    }

    for sidecar_path in sidecars {
        let src = match std::fs::read_to_string(&sidecar_path) {
            Ok(src) => src,
            Err(e) => {
                let err = EngineError::from(e);
                tracing::warn!(path = %sidecar_path.display(), error = %err, "sidecar unreadable");
                report
                    .warnings
                    .push(Diagnostic::sidecar_unreadable(sidecar_path, &err));
                continue;
            }
        };
        let sc = match parse_sidecar(&src) {
            Ok(sc) => sc,
            Err(e) => {
                tracing::warn!(path = %sidecar_path.display(), error = %e, "sidecar unparsable");
                report
                    .warnings
                    .push(Diagnostic::sidecar_unparsable(sidecar_path, &e));
                continue;
            }
        };
        let Some((book, matched_by)) = match_book(storage, &sidecar_path, &sc).await? else {
            let candidates = match_candidates(storage, &sc).await?;
            report.unmatched.push(UnmatchedSidecar {
                path: sidecar_path,
                title: sc.title,
                partial_md5: sc.partial_md5,
                candidates,
            });
            continue;
        };
        let Some(book_id) = book.id else {
            tracing::error!(title = %book.display_title(), "matched book has no id; skipping");
            continue;
        };

        // Record the link so the next import matches on md5, not an editable title. Not in `dry_run`.
        if !dry_run && let Some(md5) = &sc.partial_md5 {
            storage
                .link_device_book(md5, book_id, LinkedBy::Auto)
                .await?;
        }

        let target = ImportTarget {
            book_id,
            book_title: book.display_title().to_string(),
            matched_by,
        };
        let stats = import_into(
            storage,
            target,
            &sc,
            &sidecar_path,
            dry_run,
            device_id,
            &mut report.warnings,
        )
        .await?;
        report.imported.push(stats);
    }
    Ok(report)
}

struct ImportTarget {
    book_id: i64,
    book_title: String,
    matched_by: MatchMethod,
}

/// Write one parsed sidecar's highlights into a chosen book. The single place the
/// insert/refresh/skip decision is made, so both import paths count identically.
async fn import_into(
    storage: &Storage,
    target: ImportTarget,
    sc: &KoSidecar,
    sidecar_path: &Path,
    dry_run: bool,
    device_id: Option<&str>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<BookImportStats> {
    let ImportTarget {
        book_id,
        book_title,
        matched_by,
    } = target;

    let summary = sc.summary.as_ref();
    let status = summary.and_then(|s| s.status.clone());
    if let Some(KoStatus::Other(value)) = &status {
        tracing::warn!(
            path = %sidecar_path.display(),
            status = %value,
            "unknown KOReader status; imported as-is"
        );
        warnings.push(Diagnostic::unknown_device_status(
            sidecar_path.to_path_buf(),
            value,
        ));
    }

    if sc.unsupported_anchors > 0 {
        // A count and a path, never the text: highlights are private reading, `trace!` at most.
        tracing::warn!(
            path = %sidecar_path.display(),
            entries = sc.unsupported_anchors,
            "sidecar highlights are anchored to a page and coordinates; cannot store them"
        );
        warnings.push(Diagnostic::sidecar_anchors_unsupported(
            sidecar_path.to_path_buf(),
            sc.unsupported_anchors,
        ));
    }

    let mut stats = BookImportStats {
        book_id,
        book_title,
        inserted: 0,
        updated: 0,
        skipped: 0,
        flashcards: 0,
        matched_by,
        percent_finished: sc.percent_finished,
        status,
        rating: summary.and_then(|s| s.rating),
    };
    let copy_id = match (&sc.partial_md5, dry_run) {
        (Some(md5), false) => reader_copy(storage, book_id, md5, device_id).await?,
        _ => None,
    };
    for h in &sc.highlights {
        if dry_run {
            if storage.highlight_exists(book_id, h).await? {
                // A dry run must count a device edit as updated, or it disagrees with the import.
                if storage.device_fields_differ(book_id, h).await? {
                    stats.updated += 1;
                } else {
                    stats.skipped += 1;
                }
            } else {
                stats.inserted += 1;
                if single_word(&h.text).is_some() {
                    stats.flashcards += 1;
                }
            }
            continue;
        }
        match storage.insert_highlight_in(book_id, h, copy_id).await? {
            None => {
                if let Some(copy_id) = copy_id {
                    storage.stamp_highlight_copy(book_id, h, copy_id).await?;
                }
                if storage.refresh_device_fields(book_id, h).await? {
                    stats.updated += 1;
                } else {
                    stats.skipped += 1;
                }
            }
            Some(highlight_id) => {
                stats.inserted += 1;
                if let Some(word) = single_word(&h.text) {
                    let context = h.note.as_deref().or(h.chapter.as_deref());
                    if storage
                        .insert_flashcard(book_id, Some(highlight_id), &word, context)
                        .await?
                    {
                        stats.flashcards += 1;
                    }
                }
            }
        }
    }
    if !dry_run {
        persist_device_state(storage, book_id, sc, &stats).await?;
    }

    // `summary.note` is the user's private review: never log it above `trace!`. Status,
    // rating and progress are device state and fine to log.
    tracing::info!(
        book_id,
        inserted = stats.inserted,
        updated = stats.updated,
        skipped = stats.skipped,
        flashcards = stats.flashcards,
        matched_by = %stats.matched_by,
        status = stats.status.as_ref().map(|s| s.to_string()),
        rating = stats.rating,
        percent_finished = stats.percent_finished,
        dry_run,
        "imported sidecar"
    );
    Ok(stats)
}

/// Find or create the copy a KOReader file is, through its `reader` place. `None` when
/// the place belongs to a copy of a different book.
/// One file is one copy: an existing own `file` copy takes the `reader` place, and only a
/// book with none gets a new copy. Idempotent; never touches `have_now`.
async fn reader_copy(
    storage: &Storage,
    book_id: i64,
    partial_md5: &str,
    device_id: Option<&str>,
) -> Result<Option<i64>> {
    if let Some(copy) = storage
        .copy_for_place(PlaceKind::Reader, partial_md5)
        .await?
    {
        if copy.book_id != book_id {
            return Ok(None);
        }
        if let Some(device_id) = device_id
            && copy.places.iter().any(|p| {
                p.kind == PlaceKind::Reader && p.reference == partial_md5 && p.detail.is_none()
            })
        {
            storage
                .set_place_detail(PlaceKind::Reader, partial_md5, Some(device_id))
                .await?;
        }
        storage
            .refresh_place(PlaceKind::Reader, partial_md5, None, None)
            .await?;
        return Ok(Some(copy.id));
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
                    LearnedFrom::Koreader,
                ))
                .await?
                .id
        }
    };
    let mut place = NewPlace::new(PlaceKind::Reader, partial_md5);
    place.detail = device_id.map(str::to_string);
    Ok(match storage.attach_place(copy_id, &place).await? {
        PlaceAttach::OtherBook { .. } => None,
        _ => Some(copy_id),
    })
}

/// Mirror the sidecar's reading state onto a reading and attribute the imported highlights.
/// Opens a reading (at the earliest `datetime`) only when the sidecar carries device state
/// and the book has none, so re-import stays idempotent. Never called under `dry_run`.
async fn persist_device_state(
    storage: &Storage,
    book_id: i64,
    sc: &KoSidecar,
    stats: &BookImportStats,
) -> Result<()> {
    if stats.status.is_none() && stats.percent_finished.is_none() && stats.rating.is_none() {
        return Ok(());
    }

    let started = sc
        .highlights
        .iter()
        .filter_map(|h| h.ko_datetime.as_deref())
        .filter_map(ko_datetime_to_unix)
        .min();
    storage.ensure_reading(book_id, started, "koreader").await?;
    let last = last_device_report(storage, book_id).await?;
    let report = DeviceReport {
        status: stats.status.clone(),
        percent: stats.percent_finished,
        rating: stats.rating,
    };

    // An unchanged report is not mirrored either: a read the reader opened since (a reread) would
    // take on a stale status and percent.
    if is_new(last.as_ref(), &report) {
        storage
            .set_device_state(
                book_id,
                stats.status.as_ref(),
                stats.percent_finished,
                stats.rating,
            )
            .await?;
        match device_effect(&last.unwrap_or_default(), &report) {
            DeviceEffect::Finish => {
                storage.finish_reading(book_id).await?;
            }
            DeviceEffect::PutDown => {
                storage.abandon_reading(book_id).await?;
            }
            DeviceEffect::PickUp => {
                storage.pick_up_reading(book_id).await?;
            }
            DeviceEffect::Nothing => {}
        }
    }

    storage.attribute_highlights(book_id).await?;
    Ok(())
}

/// What a device said about a book: the sidecar's status, `percent_finished` and rating.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DeviceReport {
    pub(crate) status: Option<KoStatus>,
    pub(crate) percent: Option<f64>,
    pub(crate) rating: Option<i64>,
}

/// The device's last report on the book, across its readings; see `Storage::last_device_report`.
pub(crate) async fn last_device_report(
    storage: &Storage,
    book_id: i64,
) -> Result<Option<DeviceReport>> {
    Ok(storage
        .last_device_report(book_id)
        .await?
        .map(|r| DeviceReport {
            status: r.status.as_deref().map(KoStatus::from),
            percent: r.percent,
            rating: r.rating,
        }))
}

/// Whether a sync would change anything: a report without a percent keeps the last one.
pub(crate) fn is_new(last: Option<&DeviceReport>, now: &DeviceReport) -> bool {
    let Some(last) = last else {
        return now.status.is_some() || now.percent.is_some() || now.rating.is_some();
    };
    last.status != now.status
        || last.rating != now.rating
        || now.percent.is_some_and(|p| last.percent != Some(p))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceEffect {
    Finish,
    PutDown,
    PickUp,
    Nothing,
}

/// A sync changes the reading's state only for what is new on the device since its last report, so a
/// pause, pick-up or reread made in the app stands until the device says something different.
/// Compared with the device's own last report, never the app's page, which the reader may edit.
fn device_effect(last: &DeviceReport, now: &DeviceReport) -> DeviceEffect {
    let was = |s: KoStatus| last.status.as_ref() == Some(&s);
    match &now.status {
        Some(KoStatus::Complete) if was(KoStatus::Complete) => DeviceEffect::Nothing,
        Some(KoStatus::Complete) => DeviceEffect::Finish,
        Some(KoStatus::Abandoned) if was(KoStatus::Abandoned) => DeviceEffect::Nothing,
        Some(KoStatus::Abandoned) => DeviceEffect::PutDown,
        Some(KoStatus::Reading) if was(KoStatus::Abandoned) => DeviceEffect::PickUp,
        Some(KoStatus::Reading) | Some(KoStatus::Other(_)) | None => {
            match (last.percent, now.percent) {
                (Some(was), Some(is)) if was != is => DeviceEffect::PickUp,
                _ => DeviceEffect::Nothing,
            }
        }
    }
}

/// Create a book from a sidecar's own metadata and import its highlights. Fully offline:
/// no ISBN, cover or description.
/// Idempotency comes from `device_books` keyed on `partial_md5`: `upsert_book` would insert
/// a second book each time for an ISBN-less one.
pub async fn import_book_from_sidecar(storage: &Storage, sidecar: &Path) -> Result<PullReport> {
    import_book_from_sidecar_for_device(storage, sidecar, None).await
}

pub async fn import_book_from_sidecar_for_device(
    storage: &Storage,
    sidecar: &Path,
    device_id: Option<&str>,
) -> Result<PullReport> {
    // One file the user chose: an unreadable or unparsable sidecar is an error, not a warning.
    let src = std::fs::read_to_string(sidecar)?;
    import_book_from_sidecar_src_for_device(storage, &src, sidecar, device_id).await
}

/// [`import_book_from_sidecar`] with the bytes supplied, so a wireless push shares the one
/// parser and idempotency rule. `origin` names the source for diagnostics only.
pub async fn import_book_from_sidecar_src(
    storage: &Storage,
    src: &str,
    origin: &Path,
) -> Result<PullReport> {
    import_book_from_sidecar_src_for_device(storage, src, origin, None).await
}

pub async fn import_book_from_sidecar_src_for_device(
    storage: &Storage,
    src: &str,
    origin: &Path,
    device_id: Option<&str>,
) -> Result<PullReport> {
    let sidecar = origin;
    let sc = parse_sidecar(src)?;

    let mut warnings = Vec::new();
    let (book_id, matched_by) = match sc.partial_md5.as_deref() {
        Some(md5) => match storage.find_book_by_partial_md5(md5).await? {
            Some(book) => {
                let id = book
                    .id
                    .ok_or_else(|| EngineError::Other("linked book has no id".into()))?;
                (id, MatchMethod::Md5)
            }
            None => {
                let id = storage
                    .upsert_book(&book_from_sidecar(&sc), Some(Source::KOReader))
                    .await?;
                storage.link_device_book(md5, id, LinkedBy::Auto).await?;
                (id, MatchMethod::New)
            }
        },
        None => {
            tracing::warn!(
                path = %sidecar.display(),
                "sidecar has no partial_md5_checksum; the pulled book cannot be de-duplicated"
            );
            warnings.push(Diagnostic::sidecar_not_identified(sidecar.to_path_buf()));
            (
                storage
                    .upsert_book(&book_from_sidecar(&sc), Some(Source::KOReader))
                    .await?,
                MatchMethod::New,
            )
        }
    };

    let book_title = storage
        .get_book(book_id)
        .await?
        .map(|b| b.display_title().to_string())
        .unwrap_or_default();
    let target = ImportTarget {
        book_id,
        book_title,
        matched_by,
    };
    let stats = import_into(
        storage,
        target,
        &sc,
        sidecar,
        false,
        device_id,
        &mut warnings,
    )
    .await?;
    Ok(PullReport { stats, warnings })
}

/// The book a sidecar describes: `stats` first, then `doc_props`; only `stats` has `pages`.
fn book_from_sidecar(sc: &KoSidecar) -> Book {
    let stats = sc.stats.as_ref();
    let pick = |from_stats: Option<&String>, from_props: Option<&String>| {
        from_stats
            .or(from_props)
            .map(|s| s.as_str())
            .and_then(meaningful)
            .map(str::to_string)
    };
    Book {
        title: pick(stats.and_then(|s| s.title.as_ref()), sc.title.as_ref()),
        authors: split_authors(
            stats
                .and_then(|s| s.authors.as_deref())
                .or(sc.authors.as_deref()),
        ),
        language: pick(
            stats.and_then(|s| s.language.as_ref()),
            sc.language.as_ref(),
        ),
        page_count: stats.and_then(|s| s.pages).or(sc.doc_pages),
        ..Default::default()
    }
}

/// KOReader writes the literal `N/A` where it has no value; treat it as absent.
fn meaningful(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty() && !s.eq_ignore_ascii_case("n/a")).then_some(s)
}

fn split_authors(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.lines()
            .filter_map(meaningful)
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Record a sidecar-book decision as `Manual`, repointing any existing link; a library
/// scan must never do either.
pub async fn link_sidecar(storage: &Storage, partial_md5: &str, book_id: i64) -> Result<()> {
    if storage.get_book(book_id).await?.is_none() {
        return Err(EngineError::NotFound(format!("book id {book_id}")));
    }
    storage
        .set_device_link(partial_md5, book_id, LinkedBy::Manual)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODERN: &str = r#"
return {
    ["annotations"] = {
        [1] = {
            ["chapter"] = "Chapter One",
            ["datetime"] = "2026-01-05 21:14:08",
            ["drawer"] = "lighten",
            ["color"] = "yellow",
            ["page"] = "/body/DocFragment[8]/body/p[12]/text().0",
            ["pageno"] = 42,
            ["pos0"] = "/body/DocFragment[8]/body/p[12]/text().0",
            ["pos1"] = "/body/DocFragment[8]/body/p[12]/text().57",
            ["text"] = "We are all of us born in moral stupidity.",
            ["note"] = "Opening line - sets the whole register.",
        },
        [2] = {
            ["chapter"] = "Chapter Two",
            ["datetime"] = "2026-01-06 08:02:11",
            ["drawer"] = "lighten",
            ["pageno"] = 55,
            ["pos0"] = "/body/DocFragment[9]/body/p[4]/text().10",
            ["pos1"] = "/body/DocFragment[9]/body/p[4]/text().19",
            ["text"] = "middlemarch",
        },
        [3] = {
            -- plain bookmark, no pos0: must be skipped
            ["datetime"] = "2026-01-06 09:00:00",
            ["pageno"] = 60,
            ["text"] = "dogear",
        },
    },
    ["doc_props"] = {
        ["authors"] = "George Eliot",
        ["title"] = "Middlemarch",
        ["language"] = "en",
    },
    ["partial_md5_checksum"] = "0d6ba6c47caf63b8b3d1a2b3c4d5e6f7",
}
"#;

    const LEGACY: &str = r#"
return {
    ["highlight"] = {
        [42] = {
            [1] = {
                ["datetime"] = "2024-03-01 10:00:00",
                ["chapter"] = "I",
                ["text"] = "Someone must have been telling lies about Josef K.",
                ["pos0"] = "/body/DocFragment[3]/body/p[1]/text().0",
                ["pos1"] = "/body/DocFragment[3]/body/p[1]/text().49",
                ["drawer"] = "lighten",
            },
        },
        [77] = {
            [1] = {
                ["datetime"] = "2024-03-02 22:30:00",
                ["chapter"] = "III",
                ["text"] = "verdict",
                ["pos0"] = "/body/DocFragment[5]/body/p[9]/text().4",
                ["pos1"] = "/body/DocFragment[5]/body/p[9]/text().11",
            },
        },
    },
    ["bookmarks"] = {
        [1] = {
            ["datetime"] = "2024-03-01 10:00:00",
            ["notes"] = "Someone must have been telling lies about Josef K.",
            ["text"] = "Famous opening, guilt before act.",
        },
    },
    ["doc_props"] = {
        ["authors"] = "Franz Kafka",
        ["title"] = "The Trial",
    },
}
"#;

    #[test]
    fn parses_modern_annotations() {
        let sc = parse_sidecar(MODERN).unwrap();
        assert_eq!(sc.title.as_deref(), Some("Middlemarch"));
        assert_eq!(sc.authors.as_deref(), Some("George Eliot"));
        assert_eq!(
            sc.partial_md5.as_deref(),
            Some("0d6ba6c47caf63b8b3d1a2b3c4d5e6f7")
        );
        assert_eq!(sc.highlights.len(), 2);
        let h = &sc.highlights[0];
        assert_eq!(h.text, "We are all of us born in moral stupidity.");
        assert_eq!(h.page, Some(42));
        assert_eq!(
            h.note.as_deref(),
            Some("Opening line - sets the whole register.")
        );
        assert_eq!(sc.highlights[1].text, "middlemarch");
        assert_eq!(sc.unsupported_anchors, 0);
    }

    #[test]
    fn a_table_shaped_pos0_is_counted_rather_than_dropped_in_silence() {
        const PDF: &str = r#"
return {
    ["annotations"] = {
        [1] = {
            ["datetime"] = "2026-05-02 11:15:00",
            ["pageno"] = 3,
            ["pos0"] = { ["page"] = 3, ["x"] = 96.5, ["y"] = 220.0 },
            ["pos1"] = { ["page"] = 3, ["x"] = 402.0, ["y"] = 236.5 },
            ["text"] = "a passage on a scanned page",
        },
        [2] = {
            ["datetime"] = "2026-05-02 11:22:41",
            ["note"] = "a note goes with it",
            ["pageno"] = 4,
            ["pos0"] = { ["page"] = 4, ["x"] = 72.0, ["y"] = 118.25 },
            ["text"] = "a second passage",
        },
        [3] = {
            -- a plain bookmark: no pos0 at all, and not a lost highlight
            ["datetime"] = "2026-05-03 09:40:00",
            ["pageno"] = 10,
            ["text"] = "in III",
        },
        [4] = {
            -- no text: it would not have imported whatever its anchor was, so
            -- counting it would overstate what the anchor cost
            ["datetime"] = "2026-05-03 09:41:00",
            ["pos0"] = { ["page"] = 11, ["x"] = 1.0, ["y"] = 2.0 },
        },
    },
    ["doc_props"] = { ["title"] = "A Scanned Monograph" },
}
"#;
        let sc = parse_sidecar(PDF).unwrap();
        assert!(sc.highlights.is_empty(), "nothing here can be anchored");
        assert_eq!(
            sc.unsupported_anchors, 2,
            "the bookmark and the text-less entry are not lost highlights"
        );
    }

    #[test]
    fn a_legacy_paging_sidecar_counts_the_same_way() {
        const LEGACY_PDF: &str = r#"
return {
    ["highlight"] = {
        [7] = {
            [1] = {
                ["datetime"] = "2021-05-02 22:30:00",
                ["text"] = "an older passage on a scanned page",
                ["pos0"] = { ["page"] = 7, ["x"] = 10.0, ["y"] = 20.0 },
            },
        },
    },
    ["doc_props"] = { ["title"] = "An Old Scan" },
}
"#;
        let sc = parse_sidecar(LEGACY_PDF).unwrap();
        assert!(sc.highlights.is_empty());
        assert_eq!(sc.unsupported_anchors, 1);
    }

    #[test]
    fn only_a_table_anchor_changed_behaviour() {
        const ODD: &str = r#"
return {
    ["annotations"] = {
        [1] = { ["text"] = "numeric pos0", ["pos0"] = 7, ["pageno"] = 7 },
        [2] = { ["text"] = "empty pos0",   ["pos0"] = "", ["pageno"] = 8 },
    },
    ["doc_props"] = { ["title"] = "An Odd Book" },
}
"#;
        let sc = parse_sidecar(ODD).unwrap();
        assert_eq!(sc.unsupported_anchors, 0, "neither of these is a table");
        assert_eq!(sc.highlights.len(), 1, "the empty pos0 is still no anchor");
        assert_eq!(sc.highlights[0].pos0.as_deref(), Some("7"));
    }

    #[test]
    fn parses_legacy_highlight_map_with_bookmark_notes() {
        let sc = parse_sidecar(LEGACY).unwrap();
        assert_eq!(sc.title.as_deref(), Some("The Trial"));
        assert_eq!(sc.highlights.len(), 2);
        let first = &sc.highlights[0];
        assert_eq!(first.page, Some(42));
        assert_eq!(
            first.note.as_deref(),
            Some("Famous opening, guilt before act.")
        );
        assert_eq!(sc.highlights[1].text, "verdict");
        assert_eq!(sc.highlights[1].page, Some(77));
        assert_eq!(sc.highlights[1].note, None);
    }

    const DEVICE_STATE: &str = r#"
return {
    ["annotations"] = {
        [1] = {
            ["chapter"] = "Chapter 2",
            ["color"] = "gray",
            ["datetime"] = "2026-01-10 10:00:00",
            ["datetime_updated"] = "2026-01-10 10:00:11",
            ["drawer"] = "lighten",
            ["note"] = "typed 11 seconds after the highlight was made",
            ["page"] = "/body/DocFragment[12]/body/p[40]/text().120",
            ["pageno"] = 52,
            ["pos0"] = "/body/DocFragment[12]/body/p[40]/text().120",
            ["pos1"] = "/body/DocFragment[12]/body/p[40]/text().310",
            ["text"] = "a passage",
        },
    },
    ["doc_pages"] = 880,
    ["doc_props"] = { ["title"] = "Middlemarch", ["authors"] = "George Eliot" },
    ["partial_md5_checksum"] = "7d2a9e4c1b6f3058c2e4a6b8d0f1a3c5",
    ["percent_finished"] = 0.99772727272727,
    ["stats"] = {
        ["authors"] = "George Eliot",
        ["highlights"] = 12,
        ["language"] = "en",
        ["notes"] = 9,
        ["pages"] = 880,
        ["performance_in_pages"] = {},
        ["series"] = "N/A",
        ["title"] = "Middlemarch",
    },
    ["summary"] = {
        ["modified"] = "2026-01-20",
        ["rating"] = 5,
        ["status"] = "complete",
    },
}
"#;

    #[test]
    fn parses_the_devices_own_reading_state() {
        let sc = parse_sidecar(DEVICE_STATE).unwrap();

        assert_eq!(sc.percent_finished, Some(0.99772727272727));

        let summary = sc.summary.expect("summary");
        assert_eq!(summary.status, Some(KoStatus::Complete));
        assert_eq!(summary.rating, Some(5));
        assert_eq!(summary.modified.as_deref(), Some("2026-01-20"));
        assert_eq!(summary.note, None);

        let stats = sc.stats.expect("stats");
        assert_eq!(stats.title.as_deref(), Some("Middlemarch"));
        assert_eq!(stats.pages, Some(880));
        assert_eq!(stats.highlights, Some(12));
        assert_eq!(stats.notes, Some(9));
        assert_eq!(stats.md5, None);
        assert_eq!(stats.total_time_in_sec, None);
        assert_eq!(
            sc.partial_md5.as_deref(),
            Some("7d2a9e4c1b6f3058c2e4a6b8d0f1a3c5")
        );
    }

    /// KOReader never rewrites `datetime` (creation) and records edits in `datetime_updated`,
    /// which must stay out of the identity hash.
    #[test]
    fn parses_the_edit_timestamp_separately_from_the_creation_one() {
        let sc = parse_sidecar(DEVICE_STATE).unwrap();
        let h = &sc.highlights[0];
        assert_eq!(h.ko_datetime.as_deref(), Some("2026-01-10 10:00:00"));
        assert_eq!(
            h.ko_datetime_updated.as_deref(),
            Some("2026-01-10 10:00:11")
        );
    }

    #[test]
    fn a_completed_book_writes_percent_finished_as_an_integer() {
        let sc = parse_sidecar(r#"return { ["percent_finished"] = 1 }"#).unwrap();
        assert_eq!(sc.percent_finished, Some(1.0));
    }

    #[test]
    fn an_unknown_status_degrades_rather_than_failing() {
        let sc = parse_sidecar(r#"return { ["summary"] = { ["status"] = "tbr" } }"#)
            .expect("an unknown status must not fail the parse");
        let status = sc.summary.unwrap().status.unwrap();
        assert_eq!(status, KoStatus::Other("tbr".into()));
        assert!(status.is_unknown());
        assert_eq!(status.to_string(), "tbr");
        assert_eq!(KoStatus::from(status.to_string().as_str()), status);
    }

    #[test]
    fn every_known_status_round_trips() {
        for s in ["reading", "abandoned", "complete"] {
            let parsed = KoStatus::from(s);
            assert!(!parsed.is_unknown(), "{s} should be a known status");
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn legacy_sidecars_still_carry_device_state() {
        let src = r#"
return {
    ["highlight"] = {
        [7] = { [1] = { ["datetime"] = "2021-05-02 22:30:00", ["text"] = "old",
                        ["pos0"] = "/body/p[7]/text().0" } },
    },
    ["doc_props"] = { ["title"] = "An Old Book" },
    ["percent_finished"] = 0.5,
    ["summary"] = { ["status"] = "abandoned", ["rating"] = 3 },
    ["stats"] = { ["md5"] = "deadbeef", ["total_time_in_sec"] = 7200 },
}
"#;
        let sc = parse_sidecar(src).unwrap();
        assert_eq!(sc.highlights.len(), 1, "legacy branch must still have run");
        assert_eq!(sc.percent_finished, Some(0.5));
        let summary = sc.summary.expect("summary");
        assert_eq!(summary.status, Some(KoStatus::Abandoned));
        assert_eq!(summary.rating, Some(3));
        let stats = sc.stats.expect("stats");
        assert_eq!(stats.md5.as_deref(), Some("deadbeef"));
        assert_eq!(stats.total_time_in_sec, Some(7200));
    }

    #[tokio::test]
    async fn a_recorded_md5_link_wins_over_a_fuzzy_title_match() {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), DEVICE_STATE).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    authors: vec!["George Eliot".into()],
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        let first = import(&s, tmp.path(), false).await.unwrap();
        assert_eq!(first.imported[0].matched_by, MatchMethod::Title);

        sqlx::query("UPDATE books SET title = ? WHERE id = ?")
            .bind("Something Else Entirely")
            .bind(book_id)
            .execute(s.pool())
            .await
            .unwrap();

        let second = import(&s, tmp.path(), false).await.unwrap();
        assert_eq!(second.imported.len(), 1, "the link must survive a rename");
        assert_eq!(second.imported[0].matched_by, MatchMethod::Md5);
        assert_eq!(second.imported[0].book_id, book_id);
        assert_eq!(second.imported[0].inserted, 0, "still idempotent");
    }

    #[tokio::test]
    async fn a_dry_run_records_no_device_link() {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), DEVICE_STATE).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.upsert_book(
            &Book {
                title: Some("Middlemarch".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        import(&s, tmp.path(), true).await.unwrap();

        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM device_books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    async fn import_one(src: &str, title: &str, dry_run: bool) -> (Storage, i64) {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join(format!("{title}.sdr"));
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), src).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some(title.into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        import(&s, tmp.path(), dry_run).await.unwrap();
        (s, id)
    }

    fn sidecar_with_status(status: &str, when: &str) -> String {
        format!(
            r#"
return {{
    ["annotations"] = {{
        [1] = {{
            ["datetime"] = "{when}",
            ["pos0"] = "/body/DocFragment[1]/body/p[1]/text().0",
            ["pos1"] = "/body/DocFragment[1]/body/p[1]/text().9",
            ["text"] = "a passage",
        }},
    }},
    ["doc_props"] = {{ ["title"] = "Middlemarch" }},
    ["percent_finished"] = 0.5,
    ["summary"] = {{ ["status"] = "{status}", ["rating"] = 4 }},
}}
"#
        )
    }

    #[tokio::test]
    async fn an_import_opens_a_reading_and_mirrors_the_device() {
        let (s, id) = import_one(DEVICE_STATE, "Middlemarch", false).await;

        let readings = s.list_readings(id).await.unwrap();
        assert_eq!(readings.len(), 1);
        let r = &readings[0];
        assert_eq!(r.source, "koreader");
        assert_eq!(r.ko_status.as_deref(), Some("complete"));
        assert_eq!(r.ko_percent, Some(0.99772727272727));
        assert_eq!(r.ko_rating, Some(5));
        // The earliest annotation starts the reading: KOReader does not record when a book was opened.
        assert_eq!(
            r.started_at,
            crate::storage::ko_datetime_to_unix("2026-01-10 10:00:00")
        );
        assert!(r.finished_at.is_some());
        assert_eq!(r.status, crate::storage::STATUS_FINISHED);

        let attributed: Option<i64> =
            sqlx::query_scalar("SELECT reading_id FROM highlights WHERE book_id = ?")
                .bind(id)
                .fetch_one(s.pool())
                .await
                .unwrap();
        assert_eq!(attributed, Some(r.id));

        let b = s.get_book(id).await.unwrap().unwrap();
        assert!(b.finished);
    }

    #[tokio::test]
    async fn every_device_status_maps_as_specified() {
        for (device, ours, closed) in [
            ("reading", crate::storage::STATUS_READING, false),
            ("abandoned", crate::storage::STATUS_ABANDONED, false),
            ("complete", crate::storage::STATUS_FINISHED, true),
            ("tbr", crate::storage::STATUS_READING, false),
        ] {
            let src = sidecar_with_status(device, "2026-01-05 21:14:08");
            let (s, id) = import_one(&src, "Middlemarch", false).await;
            let r = &s.list_readings(id).await.unwrap()[0];
            assert_eq!(r.status, ours, "device status {device}");
            assert_eq!(
                r.finished_at.is_some(),
                closed,
                "device status {device} closed the reading wrongly"
            );
            assert_eq!(r.ko_status.as_deref(), Some(device));
            assert_eq!(r.ko_rating, Some(4));
        }
    }

    #[tokio::test]
    async fn abandoned_marks_the_reading_without_closing_it() {
        let src = sidecar_with_status("abandoned", "2026-01-05 21:14:08");
        let (s, id) = import_one(&src, "Middlemarch", false).await;
        assert!(s.active_reading(id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_sidecar_with_no_device_state_opens_no_reading() {
        let (s, id) = import_one(MODERN, "Middlemarch", false).await;
        assert!(s.list_readings(id).await.unwrap().is_empty());
        let unattributed: Option<i64> =
            sqlx::query_scalar("SELECT reading_id FROM highlights WHERE book_id = ? LIMIT 1")
                .bind(id)
                .fetch_one(s.pool())
                .await
                .unwrap();
        assert_eq!(unattributed, None, "unattributed is correct, not a gap");
    }

    #[tokio::test]
    async fn a_dry_run_persists_no_reading_state() {
        let (s, id) = import_one(DEVICE_STATE, "Middlemarch", true).await;
        assert!(s.list_readings(id).await.unwrap().is_empty());
    }

    /// Re-importing must not add a reading: `complete` closes the reading it opened, so an
    /// "open one when none is open" rule would grow the history on every import.
    #[tokio::test]
    async fn re_importing_never_adds_a_reading() {
        use crate::book::Book;

        for status in ["reading", "abandoned", "complete"] {
            let tmp = tempfile::tempdir().unwrap();
            let sdr = tmp.path().join("Middlemarch.sdr");
            std::fs::create_dir_all(&sdr).unwrap();
            let src = sidecar_with_status(status, "2026-01-05 21:14:08");
            std::fs::write(sdr.join("metadata.epub.lua"), &src).unwrap();

            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s
                .upsert_book(
                    &Book {
                        title: Some("Middlemarch".into()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();

            import(&s, tmp.path(), false).await.unwrap();
            let first = s.list_readings(id).await.unwrap();
            assert_eq!(first.len(), 1, "device status {status}");
            import(&s, tmp.path(), false).await.unwrap();
            import(&s, tmp.path(), false).await.unwrap();
            assert_eq!(
                s.list_readings(id).await.unwrap(),
                first,
                "device status {status} grew the reading history"
            );
        }
    }

    #[tokio::test]
    async fn an_import_after_a_reread_writes_to_the_open_reading() {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(
            sdr.join("metadata.epub.lua"),
            sidecar_with_status("complete", "2026-01-05 21:14:08"),
        )
        .unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        import(&s, tmp.path(), false).await.unwrap();
        let first = s.list_readings(id).await.unwrap().remove(0);

        let second = s.reread(id).await.unwrap();
        import(&s, tmp.path(), false).await.unwrap();
        let reread = |readings: &[crate::storage::Reading]| {
            readings
                .iter()
                .find(|r| r.id == second)
                .expect("reread")
                .clone()
        };
        let readings = s.list_readings(id).await.unwrap();
        assert_eq!(readings.len(), 2, "the reread is the user's, not ours");
        let untouched = reread(&readings);
        assert_eq!(
            (untouched.ko_status, untouched.finished_at),
            (None, None),
            "the device said nothing new, so the reread stays as the reader opened it"
        );

        for status in ["reading", "complete"] {
            std::fs::write(
                sdr.join("metadata.epub.lua"),
                sidecar_with_status(status, "2026-01-05 21:14:08"),
            )
            .unwrap();
            import(&s, tmp.path(), false).await.unwrap();
        }
        let readings = s.list_readings(id).await.unwrap();
        assert_eq!(readings.len(), 2);
        let touched = reread(&readings);
        assert_eq!(touched.ko_status.as_deref(), Some("complete"));
        assert!(touched.finished_at.is_some(), "a new complete closes it");
        assert_eq!(
            readings.iter().find(|r| r.id == first.id),
            Some(&first),
            "the device writes to the open reading, never the closed one"
        );
    }

    /// The walker must not see `metadata.*.lua.old` backups: importing them would resurrect
    /// deleted highlights.
    #[test]
    fn the_walker_ignores_the_old_backup_koreader_leaves_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), DEVICE_STATE).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua.old"), DEVICE_STATE).unwrap();

        let found = find_sidecars(tmp.path()).unwrap();
        assert_eq!(found.len(), 1, "the .old backup must not be imported");
        assert!(found[0].ends_with("metadata.epub.lua"));
    }

    /// `plugins/calibre.koplugin/metadata.lua` is a Lua module, not a sidecar: sidecars are
    /// always `metadata.<ext>.lua` with a non-empty extension.
    #[test]
    fn the_walker_ignores_koreaders_own_metadata_module() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = tmp.path().join("plugins/calibre.koplugin");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("metadata.lua"), "return require(\"x\")").unwrap();
        let sdr = tmp.path().join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), DEVICE_STATE).unwrap();

        let found = find_sidecars(tmp.path()).unwrap();
        assert_eq!(found.len(), 1, "metadata.lua is not a sidecar");
        assert!(found[0].ends_with("metadata.epub.lua"));

        assert!(!is_sidecar_file(Path::new("metadata.lua")));
        assert!(is_sidecar_file(Path::new("metadata.epub.lua")));
        assert!(is_sidecar_file(Path::new("metadata.pdf.lua")));
        assert!(!is_sidecar_file(Path::new("metadata.epub.lua.old")));
    }

    #[test]
    fn rejects_non_table_sidecars() {
        assert!(parse_sidecar("return 42").is_err());
        assert!(parse_sidecar("this is not lua").is_err());
    }

    #[test]
    fn sandbox_has_no_stdlib() {
        assert!(parse_sidecar("return { x = os.time() }").is_err());
    }

    #[tokio::test]
    async fn import_is_idempotent_and_extracts_flashcards() {
        use crate::book::Book;

        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let sdr = dir.join("Middlemarch.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), MODERN).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.upsert_book(
            &Book {
                title: Some("Middlemarch".into()),
                authors: vec!["George Eliot".into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        let report = import(&s, dir, false).await.unwrap();
        assert_eq!(report.imported.len(), 1);
        let stats = &report.imported[0];
        assert_eq!(stats.inserted, 2);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.flashcards, 1);

        let report2 = import(&s, dir, false).await.unwrap();
        assert_eq!(report2.imported[0].inserted, 0);
        assert_eq!(report2.imported[0].skipped, 2);
    }

    #[tokio::test]
    async fn unmatched_sidecar_reported_not_fatal() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let sdr = dir.join("Nothing.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), LEGACY).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let report = import(&s, dir, false).await.unwrap();
        assert!(report.imported.is_empty());
        assert_eq!(report.unmatched.len(), 1);
        assert_eq!(report.unmatched[0].title.as_deref(), Some("The Trial"));
    }

    fn sdr(dir: &Path, name: &str, src: &str) -> PathBuf {
        let d = dir.join(format!("{name}.sdr"));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("metadata.epub.lua");
        std::fs::write(&f, src).unwrap();
        f
    }

    #[tokio::test]
    async fn pulling_an_unmatched_sidecar_creates_the_book_from_its_own_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sdr(tmp.path(), "Middlemarch", DEVICE_STATE);
        let s = Storage::connect("sqlite::memory:").await.unwrap();

        let report = import_book_from_sidecar(&s, &path).await.unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(report.stats.matched_by, MatchMethod::New);
        assert_eq!(report.stats.inserted, 1);

        let book = s.get_book(report.stats.book_id).await.unwrap().unwrap();
        assert_eq!(book.title.as_deref(), Some("Middlemarch"));
        assert_eq!(book.authors, ["George Eliot"]);
        assert_eq!(book.language.as_deref(), Some("en"));
        assert_eq!(book.page_count, Some(880));
        assert_eq!(book.isbn_13, None);
        assert_eq!(book.cover_path, None);

        assert_eq!(s.list_highlights(book.id.unwrap()).await.unwrap().len(), 1);
        assert_eq!(
            s.device_links_for_book(book.id.unwrap()).await.unwrap(),
            ["7d2a9e4c1b6f3058c2e4a6b8d0f1a3c5"],
            "the mapping is what makes a second pull a no-op"
        );
    }

    #[tokio::test]
    async fn pulling_the_same_sidecar_twice_makes_one_book_not_two() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sdr(tmp.path(), "Middlemarch", DEVICE_STATE);
        let s = Storage::connect("sqlite::memory:").await.unwrap();

        let first = import_book_from_sidecar(&s, &path).await.unwrap();
        let second = import_book_from_sidecar(&s, &path).await.unwrap();

        assert_eq!(second.stats.book_id, first.stats.book_id);
        assert_eq!(second.stats.matched_by, MatchMethod::Md5);
        assert_eq!(second.stats.inserted, 0);
        let books: i64 = sqlx::query_scalar("SELECT count(*) FROM books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(books, 1);
    }

    #[tokio::test]
    async fn a_sidecar_with_no_md5_still_imports_and_says_it_cannot_be_deduped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sdr(tmp.path(), "Middlemarch", MODERN_WITHOUT_MD5);
        let s = Storage::connect("sqlite::memory:").await.unwrap();

        let report = import_book_from_sidecar(&s, &path).await.unwrap();
        assert_eq!(report.stats.inserted, 2, "the highlights still land");
        assert!(
            report.warnings.iter().any(|d| matches!(
                d.kind,
                crate::diagnostic::DiagnosticKind::SidecarNotIdentified { .. }
            )),
            "expected the not-identified diagnostic, got {:?}",
            report.warnings
        );

        import_book_from_sidecar(&s, &path).await.unwrap();
        let books: i64 = sqlx::query_scalar("SELECT count(*) FROM books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(books, 2);
    }

    #[tokio::test]
    async fn match_candidates_returns_the_near_miss_and_nothing_else() {
        use crate::book::Book;

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for title in [
            "Middlemarch",
            "Middlemarch: A Study of Provincial Life",
            "The Brothers Karamazov",
        ] {
            s.upsert_book(
                &Book {
                    title: Some(title.into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        }

        let sc = parse_sidecar(MODERN).unwrap();
        let got = match_candidates(&s, &sc).await.unwrap();
        let titles: Vec<&str> = got.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Middlemarch: A Study of Provincial Life"]);
        assert!(got[0].score >= CANDIDATE_MIN && got[0].score < AUTO_MATCH);
    }

    #[tokio::test]
    async fn a_candidate_carries_who_wrote_it_and_when() {
        use crate::book::Book;

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.upsert_book(
            &Book {
                title: Some("Middlemarch: A Study of Provincial Life".into()),
                authors: vec!["Eliot, George".into()],
                publish_year: Some(2017),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        let sc = parse_sidecar(MODERN).unwrap();
        let got = match_candidates(&s, &sc).await.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].authors_display, ["George Eliot"]);
        assert_eq!(got[0].publish_year, Some(2017));

        let bare = Storage::connect("sqlite::memory:").await.unwrap();
        bare.upsert_book(
            &Book {
                title: Some("Middlemarch: A Study of Provincial Life".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        let got = match_candidates(&bare, &sc).await.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].authors_display.is_empty());
        assert_eq!(got[0].publish_year, None);
    }

    #[tokio::test]
    async fn linking_a_sidecar_makes_the_next_pull_find_the_book_we_chose() {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        let path = sdr(tmp.path(), "Middlemarch", DEVICE_STATE);
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("Nineteen Eighty-Four Times Two".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        link_sidecar(&s, "7d2a9e4c1b6f3058c2e4a6b8d0f1a3c5", book_id)
            .await
            .unwrap();

        let report = import_book_from_sidecar(&s, &path).await.unwrap();
        assert_eq!(report.stats.book_id, book_id);
        assert_eq!(report.stats.matched_by, MatchMethod::Md5);
        let books: i64 = sqlx::query_scalar("SELECT count(*) FROM books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(books, 1, "the link must prevent the duplicate");

        let bulk = import(&s, tmp.path(), false).await.unwrap();
        assert_eq!(bulk.imported[0].matched_by, MatchMethod::Md5);
        assert!(bulk.unmatched.is_empty());
    }

    #[tokio::test]
    async fn linking_to_a_book_that_does_not_exist_is_not_found_not_a_constraint_error() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        assert!(matches!(
            link_sidecar(&s, "abc", 9999).await,
            Err(EngineError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn an_unmatched_sidecar_reports_its_candidates_and_its_device_key() {
        use crate::book::Book;

        let tmp = tempfile::tempdir().unwrap();
        sdr(tmp.path(), "Middlemarch", MODERN);
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.upsert_book(
            &Book {
                title: Some("Middlemarch: A Study of Provincial Life".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        let report = import(&s, tmp.path(), false).await.unwrap();
        assert_eq!(report.unmatched.len(), 1);
        let u = &report.unmatched[0];
        assert_eq!(
            u.partial_md5.as_deref(),
            Some("0d6ba6c47caf63b8b3d1a2b3c4d5e6f7")
        );
        assert_eq!(u.candidates.len(), 1);
        assert_eq!(
            u.candidates[0].title,
            "Middlemarch: A Study of Provincial Life"
        );
    }

    #[tokio::test]
    async fn a_pull_of_an_unparsable_sidecar_errors_rather_than_degrading() {
        let tmp = tempfile::tempdir().unwrap();
        let path = sdr(tmp.path(), "Bad", "return 42");
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        assert!(matches!(
            import_book_from_sidecar(&s, &path).await,
            Err(EngineError::Sidecar(_))
        ));
    }

    #[test]
    fn sidecar_metadata_is_cleaned_before_it_becomes_a_book() {
        let sc = parse_sidecar(
            "return { [\"doc_props\"] = { [\"title\"] = \"Two Authors\",
                       [\"authors\"] = \"Ann Writer\\nBo Writer\", [\"language\"] = \"N/A\" },
                       [\"doc_pages\"] = 311 }",
        )
        .unwrap();
        let book = book_from_sidecar(&sc);
        assert_eq!(book.authors, ["Ann Writer", "Bo Writer"]);
        assert_eq!(book.language, None, "\"N/A\" is not a language");
        assert_eq!(
            book.page_count,
            Some(311),
            "doc_pages is the only page count a stats-less sidecar has"
        );
    }

    const MODERN_WITHOUT_MD5: &str = r#"
return {
    ["annotations"] = {
        [1] = {
            ["datetime"] = "2026-01-05 21:14:08",
            ["pageno"] = 42,
            ["pos0"] = "/body/DocFragment[8]/body/p[12]/text().0",
            ["text"] = "We are all of us born in moral stupidity.",
        },
        [2] = {
            ["datetime"] = "2026-01-06 08:02:11",
            ["pageno"] = 55,
            ["pos0"] = "/body/DocFragment[9]/body/p[4]/text().10",
            ["text"] = "middlemarch",
        },
    },
    ["doc_props"] = { ["title"] = "Middlemarch", ["authors"] = "George Eliot" },
}
"#;

    #[test]
    fn a_runaway_loop_is_killed_rather_than_hanging_the_import() {
        let started = std::time::Instant::now();
        let err = parse_sidecar("return (function() while true do end end)()")
            .expect_err("a non-terminating chunk must not be allowed to return");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "budget did not bite: took {:?}",
            started.elapsed()
        );
        assert!(
            err.to_string().contains("instruction budget"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_hole_in_the_annotations_table_does_not_truncate_the_import() {
        let holey = r#"
return {
    ["annotations"] = {
        [1] = { ["text"] = "first",  ["pos0"] = "/a.0", ["datetime"] = "2026-01-01 00:00:00" },
        [3] = { ["text"] = "third",  ["pos0"] = "/c.0", ["datetime"] = "2026-01-03 00:00:00" },
        [4] = { ["text"] = "fourth", ["pos0"] = "/d.0", ["datetime"] = "2026-01-04 00:00:00" },
    },
    ["doc_props"] = { ["title"] = "Holey" },
}
"#;
        let sc = parse_sidecar(holey).unwrap();
        let texts: Vec<&str> = sc.highlights.iter().map(|h| h.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["first", "third", "fourth"],
            "entries after the gap were dropped"
        );
    }

    #[tokio::test]
    async fn one_unreadable_sidecar_does_not_abort_the_whole_import() {
        let tmp = tempfile::tempdir().unwrap();

        let bad = tmp.path().join("Bad.sdr");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(bad.join("metadata.epub.lua"), [0xff, 0xfe, 0x00, 0x01]).unwrap();

        let good = tmp.path().join("The Trial.sdr");
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(good.join("metadata.epub.lua"), LEGACY).unwrap();

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        s.upsert_book(
            &Book {
                title: Some("The Trial".into()),
                authors: vec!["Franz Kafka".into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();

        let report = import(&s, tmp.path(), false)
            .await
            .expect("an unreadable file must degrade, not abort");

        assert_eq!(
            report.imported.len(),
            1,
            "the readable sidecar should still have imported"
        );
        assert!(report.imported[0].inserted > 0);
        assert!(
            report.warnings.iter().any(|d| matches!(
                d.kind,
                crate::diagnostic::DiagnosticKind::SidecarUnreadable { .. }
            )),
            "expected an unreadable-sidecar diagnostic, got {:?}",
            report.warnings
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_does_not_hang_the_library_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let sdr = tmp.path().join("Book.sdr");
        std::fs::create_dir_all(&sdr).unwrap();
        std::fs::write(sdr.join("metadata.epub.lua"), LEGACY).unwrap();

        let nested = tmp.path().join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::os::unix::fs::symlink(tmp.path(), nested.join("loop")).unwrap();

        let found = find_sidecars(tmp.path()).expect("walk must terminate");
        assert_eq!(
            found.len(),
            1,
            "expected exactly the one real sidecar, got {found:?}"
        );
    }
}
