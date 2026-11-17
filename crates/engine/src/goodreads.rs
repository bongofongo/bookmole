//! Goodreads CSV import and export. ISBNs lose Excel armour then go through `normalize_isbn`; `My
//! Rating` is stored raw on the `goodreads` scale.

use std::collections::HashMap;
use std::path::Path;

use time::OffsetDateTime;
use time::format_description::BorrowedFormatItem;
use time::macros::format_description;

use crate::book::{Book, normalize_isbn};
use crate::copies::{LearnedFrom, NewCopy, Tenure};
use crate::diagnostic::{Diagnostic, DiagnosticKind, Severity};
use crate::error::{EngineError, Result};
use crate::koreader::{self, MatchCandidate};
use crate::matching::Query;
use crate::notes::{NewNoteInput, NoteKind};
use crate::spreadsheet::{inert_cell, uninert_cell};
use crate::storage::NoteScope;
use crate::storage::{STATUS_FINISHED, STATUS_READING, Source, Storage};
use crate::{Engine, storage};

/// The `source` recorded on every row this import writes, so one predicate can forget them all.
pub const SOURCE: &str = "goodreads";

pub const SCALE: &str = "goodreads";

const DATE: &[BorrowedFormatItem<'static>] = format_description!("[year]/[month]/[day]");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shelf {
    Read,
    CurrentlyReading,
    ToRead,
    Other(String),
}

impl Shelf {
    fn parse(s: &str) -> Shelf {
        match s.trim() {
            "read" => Shelf::Read,
            "currently-reading" => Shelf::CurrentlyReading,
            "to-read" => Shelf::ToRead,
            other => Shelf::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GoodreadsRow {
    pub row: usize,
    pub external_id: Option<String>,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub isbn_10: Option<String>,
    pub isbn_13: Option<String>,
    /// `None` is an empty cell; `Some(0)` is Goodreads' encoding of unrated. Neither is a rating.
    pub my_rating: Option<f64>,
    pub publisher: Option<String>,
    pub page_count: Option<i64>,
    pub publish_year: Option<i64>,
    pub date_read: Option<i64>,
    /// `Date Started`, which Goodreads' own export does not carry; absent is the ordinary case.
    pub date_started: Option<i64>,
    pub date_added: Option<i64>,
    pub shelves: Vec<String>,
    pub exclusive_shelf: Option<Shelf>,
    pub review: Option<String>,
    pub private_notes: Option<String>,
    /// Defaults to 1 when the column is absent.
    pub read_count: i64,
}

impl GoodreadsRow {
    fn display_title(&self) -> &str {
        self.title.as_deref().unwrap_or("(untitled)")
    }

    /// With no `Exclusive Shelf` (our own export omits it), a `Date Read` means it was read,
    /// so export then import keeps every reading.
    fn wanted_readings(&self) -> i64 {
        match &self.exclusive_shelf {
            Some(Shelf::ToRead) => 0,
            Some(Shelf::CurrentlyReading) => 1,
            Some(Shelf::Read) => self.read_count.max(1),
            Some(Shelf::Other(_)) | None => i64::from(self.date_read.is_some()),
        }
    }

    fn is_currently_reading(&self) -> bool {
        self.exclusive_shelf == Some(Shelf::CurrentlyReading)
    }

    /// The row's start for a reading that ends at `finish` (`None` is open). A start after the finish
    /// is a contradiction the file cannot resolve, so it is dropped rather than repaired.
    fn start_before(&self, finish: Option<i64>) -> Option<i64> {
        self.date_started
            .filter(|start| finish.is_none_or(|end| *start <= end))
    }
}

/// Strips Excel armour (`="0990000028"`). The csv reader leaves it intact, so it must come off
/// before `normalize_isbn`.
fn strip_armour(raw: &str) -> Option<&str> {
    let s = raw.trim();
    let s = s.strip_prefix('=').unwrap_or(s);
    let s = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s);
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

fn cell(raw: &str) -> Option<String> {
    // Undoes `inert_cell`'s guard so our own export re-imports unchanged.
    let s = uninert_cell(raw.trim());
    (!s.is_empty()).then(|| s.to_string())
}

fn parse_date(raw: &str) -> Option<i64> {
    let s = raw.trim().replace('-', "/");
    time::Date::parse(&s, DATE)
        .ok()
        .map(|d| d.midnight().assume_utc().unix_timestamp())
}

fn format_date(unix: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|d| d.date().format(DATE).ok())
}

/// Columns are found by header name and a missing one is not an error; only `Title` is required.
pub fn parse_csv(path: &Path) -> Result<Vec<GoodreadsRow>> {
    let mut reader = csv::ReaderBuilder::new()
        // Flexible: a stray comma in a review makes a short row, not a refused file.
        .flexible(true)
        .from_path(path)
        .map_err(csv_err)?;

    let headers = reader.headers().map_err(csv_err)?.clone();
    let index: HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.trim().to_ascii_lowercase(), i))
        .collect();
    if !index.contains_key("title") {
        return Err(EngineError::InvalidInput(format!(
            "{} has no Title column — is it a Goodreads export?",
            path.display()
        )));
    }

    let mut out = Vec::new();
    for (n, record) in reader.records().enumerate() {
        let record = record.map_err(csv_err)?;
        // Some columns have two spellings (`My Review` from the exporter, `Review` from the importer);
        // our own export uses the importer's, so both must be read.
        let get = |names: &[&str]| -> Option<&str> {
            names
                .iter()
                .find_map(|n| index.get(*n))
                .and_then(|i| record.get(*i))
                .map(str::trim)
        };
        let get = |name: &str| -> Option<&str> {
            match name {
                "my review" => get(&["my review", "review"]),
                "my rating" => get(&["my rating", "rating"]),
                _ => get(&[name]),
            }
        };

        let mut authors: Vec<String> = Vec::new();
        if let Some(a) = get("author").and_then(cell) {
            authors.push(a);
        }
        // Bracketed roles ("(Translator)") are dropped: telling them apart would mean parsing English.
        for extra in get("additional authors").unwrap_or("").split(',') {
            if let Some(a) = cell(extra)
                && !authors.contains(&a)
            {
                authors.push(a);
            }
        }

        let isbn = |name: &str| -> Option<String> {
            get(name).and_then(strip_armour).and_then(normalize_isbn)
        };
        let (mut isbn_10, mut isbn_13) = (isbn("isbn"), isbn("isbn13"));
        // Swapped columns are corrected: a 13-digit value in `isbn_10` would break its UNIQUE lookups.
        if isbn_10.as_ref().is_some_and(|s| s.len() == 13) {
            isbn_13 = isbn_13.or_else(|| isbn_10.take());
        }
        if isbn_13.as_ref().is_some_and(|s| s.len() == 10) {
            isbn_10 = isbn_10.or_else(|| isbn_13.take());
        }

        out.push(GoodreadsRow {
            row: n + 1,
            external_id: get("book id").and_then(cell),
            title: get("title").and_then(cell),
            authors,
            isbn_10,
            isbn_13,
            my_rating: get("my rating")
                .and_then(cell)
                .and_then(|s| s.parse::<f64>().ok())
                .and_then(storage::goodreads_point),
            publisher: get("publisher").and_then(cell),
            page_count: get("number of pages")
                .and_then(cell)
                .and_then(|s| s.parse().ok()),
            publish_year: get("year published")
                .and_then(cell)
                .and_then(|s| s.parse().ok())
                .or_else(|| {
                    get("original publication year")
                        .and_then(cell)
                        .and_then(|s| s.parse().ok())
                }),
            date_read: get("date read").and_then(parse_date),
            date_started: get("date started").and_then(parse_date),
            date_added: get("date added").and_then(parse_date),
            shelves: get("bookshelves")
                .unwrap_or("")
                .split(',')
                .filter_map(cell)
                .collect(),
            exclusive_shelf: get("exclusive shelf")
                .and_then(cell)
                .map(|s| Shelf::parse(&s)),
            review: get("my review").and_then(cell),
            private_notes: get("private notes").and_then(cell),
            read_count: get("read count")
                .and_then(cell)
                .and_then(|s| s.parse().ok())
                .unwrap_or(1),
        });
    }
    Ok(out)
}

/// An unreadable file stays `EngineError::Io`; malformed CSV is user input. Kept apart so
/// `ErrorClass::from` classifies without reading the message.
fn csv_err(e: csv::Error) -> EngineError {
    let rendered = e.to_string();
    match e.into_kind() {
        csv::ErrorKind::Io(io) => EngineError::Io(io),
        _ => EngineError::InvalidInput(format!("goodreads csv: {rendered}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoodreadsMatch {
    ExternalId,
    Isbn,
    Title,
    New,
}

impl std::fmt::Display for GoodreadsMatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoodreadsMatch::ExternalId => write!(f, "goodreads id"),
            GoodreadsMatch::Isbn => write!(f, "isbn"),
            GoodreadsMatch::Title => write!(f, "title"),
            GoodreadsMatch::New => write!(f, "new"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOutcome {
    Absent,
    Written,
    Unchanged,
    KeptOurs,
}

#[derive(Debug)]
pub struct GoodreadsBookReport {
    /// `None` in a dry run for a book that does not exist yet.
    pub book_id: Option<i64>,
    pub title: String,
    pub matched_by: GoodreadsMatch,
    pub readings_added: usize,
    pub tags_added: usize,
    pub review: TextOutcome,
    pub private_notes: TextOutcome,
    /// The raw rating written, on the `goodreads` scale; `None` if empty, zero, unchanged or the
    /// user has rated it on their own scale.
    pub rating: Option<f64>,
}

#[derive(Debug)]
pub struct UnmatchedRow {
    pub row: usize,
    pub title: String,
    pub authors: Vec<String>,
    pub external_id: Option<String>,
    pub candidates: Vec<MatchCandidate>,
}

#[derive(Debug, Default)]
pub struct GoodreadsReport {
    pub dry_run: bool,
    pub rows: usize,
    pub books: Vec<GoodreadsBookReport>,
    pub unmatched: Vec<UnmatchedRow>,
    pub warnings: Vec<Diagnostic>,
}

impl GoodreadsReport {
    pub fn created(&self) -> usize {
        self.books
            .iter()
            .filter(|b| b.matched_by == GoodreadsMatch::New)
            .count()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportOptions {
    pub dry_run: bool,
    pub create_ambiguous: bool,
}

/// Takes the whole [`Engine`], not a [`Storage`]: a review is a note (vault file, row, FTS,
/// links), which `Engine::open_review` already handles, including finding the prior note.
pub async fn import(engine: &Engine, path: &Path, opts: ImportOptions) -> Result<GoodreadsReport> {
    let rows = parse_csv(path)?;
    let mut report = GoodreadsReport {
        dry_run: opts.dry_run,
        rows: rows.len(),
        ..Default::default()
    };
    let storage = &engine.storage;
    let scale = storage
        .rating_scale_by_name(SCALE)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("the '{SCALE}' rating scale")))?;

    for row in &rows {
        if row.title.is_none() {
            report.warnings.push(row_skipped(
                row.row,
                "no Title, so there is nothing to match on",
            ));
            continue;
        }

        let matched = match_row(storage, row).await?;
        let (book_id, matched_by) = match matched {
            Matched::Book(id, how) => (Some(id), how),
            Matched::Ambiguous(candidates) if !opts.create_ambiguous => {
                report.unmatched.push(UnmatchedRow {
                    row: row.row,
                    title: row.display_title().to_string(),
                    authors: row.authors.clone(),
                    external_id: row.external_id.clone(),
                    candidates,
                });
                continue;
            }
            Matched::Ambiguous(_) | Matched::Nothing => (None, GoodreadsMatch::New),
        };

        if opts.dry_run {
            report
                .books
                .push(preview(storage, row, book_id, matched_by).await?);
            continue;
        }

        let book_id = match book_id {
            Some(id) => id,
            None => create_book(storage, row).await?,
        };
        report.books.push(
            apply(
                engine,
                row,
                book_id,
                matched_by,
                &scale,
                &mut report.warnings,
            )
            .await?,
        );
    }
    Ok(report)
}

enum Matched {
    Book(i64, GoodreadsMatch),
    Ambiguous(Vec<MatchCandidate>),
    Nothing,
}

/// ISBN-13, then ISBN-10, then the shared title scan: most certain identity first.
async fn match_row(storage: &Storage, row: &GoodreadsRow) -> Result<Matched> {
    if let Some(id) = &row.external_id
        && let Some(book_id) = storage.book_for_external_id(SOURCE, id).await?
    {
        return Ok(Matched::Book(book_id, GoodreadsMatch::ExternalId));
    }
    for isbn in [&row.isbn_13, &row.isbn_10].into_iter().flatten() {
        if let Some(book) = storage.find_book_by_isbn(isbn).await?
            && let Some(book_id) = book.id
        {
            return Ok(Matched::Book(book_id, GoodreadsMatch::Isbn));
        }
    }

    let scored =
        koreader::scores_for(storage, &Query::new(row.title.as_deref(), &row.authors)).await?;
    if let Some(s) = scored.first()
        && s.can_auto
        && let Some(book_id) = s.book.id
    {
        return Ok(Matched::Book(book_id, GoodreadsMatch::Title));
    }
    let candidates = koreader::band(scored);
    Ok(if candidates.is_empty() {
        Matched::Nothing
    } else {
        Matched::Ambiguous(candidates)
    })
}

async fn create_book(storage: &Storage, row: &GoodreadsRow) -> Result<i64> {
    let book_id = storage
        .upsert_book(
            &Book {
                title: row.title.clone(),
                authors: row.authors.clone(),
                publisher: row.publisher.clone(),
                publish_year: row.publish_year,
                page_count: row.page_count,
                isbn_10: row.isbn_10.clone(),
                isbn_13: row.isbn_13.clone(),
                ..Default::default()
            },
            Some(Source::Goodreads),
        )
        .await?;
    // `Date Added` sets `created_at` only for a book this import just created.
    if let Some(added) = row.date_added {
        storage.set_book_created_at(book_id, added).await?;
    }
    Ok(book_id)
}

async fn preview(
    storage: &Storage,
    row: &GoodreadsRow,
    book_id: Option<i64>,
    matched_by: GoodreadsMatch,
) -> Result<GoodreadsBookReport> {
    let (readings_added, tags_added) = match book_id {
        None => (row.wanted_readings().max(0) as usize, row.shelves.len()),
        Some(id) => {
            let have = storage.readings_from_source(id, SOURCE).await?.len() as i64;
            let known: Vec<String> = storage
                .book_tags(id)
                .await?
                .into_iter()
                .filter(|t| t.source == SOURCE)
                .map(|t| t.tag)
                .collect();
            (
                (row.wanted_readings() - have).max(0) as usize,
                row.shelves
                    .iter()
                    .filter(|s| !known.contains(&slug_tag(s)))
                    .count(),
            )
        }
    };
    Ok(GoodreadsBookReport {
        book_id,
        title: row.display_title().to_string(),
        matched_by,
        readings_added,
        tags_added,
        review: match row.review {
            Some(_) => TextOutcome::Written,
            None => TextOutcome::Absent,
        },
        private_notes: match row.private_notes {
            Some(_) => TextOutcome::Written,
            None => TextOutcome::Absent,
        },
        rating: row.my_rating.filter(|r| *r > 0.0),
    })
}

async fn apply(
    engine: &Engine,
    row: &GoodreadsRow,
    book_id: i64,
    matched_by: GoodreadsMatch,
    scale: &storage::RatingScale,
    warnings: &mut Vec<Diagnostic>,
) -> Result<GoodreadsBookReport> {
    let storage = &engine.storage;

    if let Some(id) = &row.external_id {
        storage.link_external_id(SOURCE, id, book_id).await?;
    }
    let tags: Vec<(String, String)> = row
        .shelves
        .iter()
        .map(|s| (slug_tag(s), s.clone()))
        .collect();
    let tags_added = storage.add_book_tags(book_id, SOURCE, &tags).await?;

    let readings_added = reconcile_readings(storage, row, book_id, warnings).await?;

    // `to-read` gets no copy. Otherwise a book with none gets one with form, tenure and have_now
    // unset and `learned_from = user` (there is no Goodreads variant).
    if row.wanted_readings() > 0 && storage.copies_for_book(book_id).await?.is_empty() {
        storage
            .create_copy(&NewCopy::new(
                book_id,
                None,
                Tenure::Unrecorded,
                LearnedFrom::User,
            ))
            .await?;
    }
    let reading_id = storage
        .readings_from_source(book_id, SOURCE)
        .await?
        .last()
        .map(|r| r.id);

    // A rating lives on a review, so a rating-only row opens an empty one; otherwise it is lost.
    let mut review = TextOutcome::Absent;
    let mut rating = None;
    let rated = row.my_rating.is_some_and(|r| r > 0.0);
    if row.review.is_some() || rated {
        match reading_id {
            Some(reading_id) => {
                let note = engine.open_review(book_id, Some(reading_id)).await?;
                if let Some(text) = &row.review {
                    review = write_body(engine, note.id, text).await?;
                    if review == TextOutcome::KeptOurs {
                        warnings.push(kept_ours(
                            DiagnosticKind::GoodreadsReviewDiverged {
                                title: row.display_title().to_string(),
                            },
                            "the review here differs from the one in the CSV; yours was kept",
                        ));
                    }
                }
                rating = apply_rating(engine, note.id, row, scale, warnings).await?;
            }
            // A review anchors to a reading and `to-read` has none, so the prose becomes an ordinary note.
            None => {
                if let Some(text) = &row.review {
                    let title = format!("Goodreads review: {}", row.display_title());
                    review = write_side_note(engine, book_id, &title, text).await?;
                }
                warnings.push(kept_ours(
                    DiagnosticKind::GoodreadsUnanchoredReview {
                        title: row.display_title().to_string(),
                    },
                    "a review needs a reading to anchor to; kept as a plain note",
                ));
            }
        }
    }

    // Private notes are neither review nor reflection; folding them in would put words in the
    // user's reflection they did not write.
    let mut private_notes = TextOutcome::Absent;
    if let Some(text) = &row.private_notes {
        let title = format!("Goodreads notes: {}", row.display_title());
        private_notes = write_side_note(engine, book_id, &title, text).await?;
    }

    Ok(GoodreadsBookReport {
        book_id: Some(book_id),
        title: row.display_title().to_string(),
        matched_by,
        readings_added,
        tags_added,
        review,
        private_notes,
        rating,
    })
}

/// `Read Count` becomes readings; only one may be open (`idx_readings_one_open`), so earlier ones
/// close at the row's `Date Read`. Only the first reading recorded takes a `Date Started`, since the
/// file gives one start for all of them; the rest have none. With no date, one open reading.
async fn reconcile_readings(
    storage: &Storage,
    row: &GoodreadsRow,
    book_id: i64,
    warnings: &mut Vec<Diagnostic>,
) -> Result<usize> {
    let want = row.wanted_readings();
    let have = storage.readings_from_source(book_id, SOURCE).await?.len() as i64;
    let mut missing = (want - have).max(0);
    let stamp = row.date_read.or(row.date_added);
    if missing == 0 {
        let Some(last) = storage.readings_from_source(book_id, SOURCE).await?.pop() else {
            return Ok(0);
        };
        let mut finished_at = last.finished_at;
        // A book that was `currently-reading` and is now `read` has the right count but wrong end date.
        if let Some(when) = row.date_read
            && !row.is_currently_reading()
            && finished_at.is_none()
        {
            storage
                .close_reading_at(last.id, when, STATUS_FINISHED)
                .await?;
            finished_at = Some(when);
        }
        // Only the reading this row's dates describe gains its start, and a start already known stays.
        let described = match finished_at {
            None => row.is_currently_reading(),
            Some(end) => Some(end) == stamp,
        };
        if described && let Some(start) = row.start_before(finished_at) {
            storage.fill_reading_start(last.id, start).await?;
        }
        return Ok(0);
    }

    if row.is_currently_reading() {
        // An existing active reading is left alone, whoever recorded it.
        if storage.active_reading(book_id).await?.is_some() {
            return Ok(0);
        }
        storage
            .record_reading(
                book_id,
                row.start_before(None),
                None,
                STATUS_READING,
                SOURCE,
            )
            .await?;
        return Ok(1);
    }

    let mut added = 0;
    match stamp {
        Some(when) => {
            let mut start = row.start_before(Some(when));
            while missing > 0 {
                storage
                    .record_reading(book_id, start.take(), Some(when), STATUS_FINISHED, SOURCE)
                    .await?;
                added += 1;
                missing -= 1;
            }
        }
        None => {
            // Only one undated reading is representable, and only with no other open reading.
            if storage.active_reading(book_id).await?.is_none() {
                storage
                    .record_reading(
                        book_id,
                        row.start_before(None),
                        None,
                        STATUS_FINISHED,
                        SOURCE,
                    )
                    .await?;
                added += 1;
                missing -= 1;
            }
            if missing > 0 {
                warnings.push(Diagnostic {
                    kind: DiagnosticKind::GoodreadsUndatableRereads {
                        title: row.display_title().to_string(),
                        dropped: missing as usize,
                    },
                    severity: Severity::Warning,
                    detail: format!(
                        "Read Count {} but no Date Read or Date Added, and a reading with no end \
                         date is an open one — {missing} not recorded",
                        row.read_count
                    ),
                });
            }
        }
    }
    Ok(added)
}

async fn apply_rating(
    engine: &Engine,
    note_id: i64,
    row: &GoodreadsRow,
    scale: &storage::RatingScale,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Option<f64>> {
    // 0 means unrated and empty says nothing; neither is stored as a zero-star rating.
    let Some(value) = row.my_rating.filter(|r| *r > 0.0) else {
        return Ok(None);
    };
    // Refreshed only while ours: a rating on the user's own scale is a different axis, and the
    // map is many-to-one so the two cannot be compared.
    if let Some(existing) = engine.storage.review_rating(note_id).await? {
        if existing.scale.name != scale.name {
            warnings.push(kept_ours(
                DiagnosticKind::GoodreadsRatingDiverged {
                    title: row.display_title().to_string(),
                },
                &format!(
                    "already rated {} on the '{}' scale; the CSV's {value} was not written",
                    existing.value, existing.scale.name
                ),
            ));
            return Ok(None);
        }
        if existing.value == value {
            return Ok(None);
        }
    }
    engine
        .storage
        .set_review_rating(note_id, scale, value)
        .await?;
    Ok(Some(value))
}

async fn write_body(engine: &Engine, note_id: i64, text: &str) -> Result<TextOutcome> {
    let note = engine
        .storage
        .get_note(note_id)
        .await?
        .ok_or_else(|| EngineError::NotFound(format!("note id {note_id}")))?;
    let body = engine.note_body(&note)?;
    if body.trim() == text.trim() {
        return Ok(TextOutcome::Unchanged);
    }
    if !body.trim().is_empty() {
        return Ok(TextOutcome::KeptOurs);
    }
    engine.update_note_body(&note, text).await?;
    Ok(TextOutcome::Written)
}

async fn write_side_note(
    engine: &Engine,
    book_id: i64,
    title: &str,
    text: &str,
) -> Result<TextOutcome> {
    if let Some(existing) = engine
        .storage
        // All notes on the book, unlimited: a viewport limit could miss the match and write a duplicate.
        .list_notes(NoteScope::Book(book_id), None)
        .await?
        .into_iter()
        .find(|n| n.title == title)
    {
        return write_body(engine, existing.id, text).await;
    }
    engine
        .create_note(NewNoteInput {
            book_id: Some(book_id),
            kind: NoteKind::Note,
            title: Some(title.to_string()),
            body: text.to_string(),
            ..Default::default()
        })
        .await?;
    Ok(TextOutcome::Written)
}

fn slug_tag(shelf: &str) -> String {
    shelf.trim().to_lowercase().replace(' ', "-")
}

fn row_skipped(row: usize, why: &str) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::GoodreadsRowSkipped { row },
        severity: Severity::Warning,
        detail: why.to_string(),
    }
}

fn kept_ours(kind: DiagnosticKind, detail: &str) -> Diagnostic {
    Diagnostic {
        kind,
        severity: Severity::Warning,
        detail: detail.to_string(),
    }
}

const EXPORT_COLUMNS: [&str; 8] = [
    "Title",
    "Author",
    "ISBN",
    "My Rating",
    "Date Read",
    "Date Added",
    "Bookshelves",
    "Review",
];

#[derive(Debug, Default, PartialEq)]
pub struct ExportRow {
    pub title: String,
    pub author: String,
    pub isbn: Option<String>,
    pub my_rating: Option<f64>,
    pub date_read: Option<String>,
    pub date_added: Option<String>,
    pub bookshelves: String,
    pub review: String,
}

/// ISBNs are written armoured (`="0990000028"`) so a spreadsheet keeps the leading zero.
pub fn write_csv(rows: &[ExportRow]) -> String {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(EXPORT_COLUMNS).expect("in-memory write");
    for r in rows {
        // Free-text cells go through `inert_cell` so a leading `=`/`+`/`-`/`@` is not a formula;
        // the armoured ISBN below is the one deliberate `=`.
        w.write_record([
            &*inert_cell(&r.title),
            &*inert_cell(&r.author),
            &r.isbn
                .as_ref()
                .map(|i| format!("=\"{i}\""))
                .unwrap_or_default(),
            &r.my_rating.map(|v| v.to_string()).unwrap_or_default(),
            r.date_read.as_deref().unwrap_or(""),
            r.date_added.as_deref().unwrap_or(""),
            &*inert_cell(&r.bookshelves),
            &*inert_cell(&r.review),
        ])
        .expect("in-memory write");
    }
    String::from_utf8(w.into_inner().expect("flush")).expect("csv is utf-8")
}

/// Counts csv records, not lines: a review with a newline is one record over two lines.
pub fn row_count(csv: &str) -> usize {
    csv::Reader::from_reader(csv.as_bytes()).records().count()
}

/// Returns the payload and any diagnostics; the caller owns the file. An unmapped rating skips its
/// row (no rounding), and rereads export only the most recent (the CSV has no read count).
pub async fn export(engine: &Engine) -> Result<(String, Vec<Diagnostic>)> {
    let storage = &engine.storage;
    let mut warnings = Vec::new();
    let mut rows = Vec::new();

    let whole_library = crate::BookQuery {
        sort: storage::BookSort::Title,
        ..Default::default()
    };
    for book in storage.list_books(&whole_library).await? {
        let Some(book_id) = book.id else { continue };
        let readings = storage.list_readings(book_id).await?;
        if readings.len() > 1 {
            warnings.push(Diagnostic {
                kind: DiagnosticKind::GoodreadsRereadsDropped {
                    title: book.display_title().to_string(),
                    dropped: readings.len() - 1,
                },
                severity: Severity::Warning,
                detail: format!(
                    "read {} times; Goodreads' importable CSV has no read-count column, so only \
                     the most recent is exported",
                    readings.len()
                ),
            });
        }
        let latest = readings.last();

        let review_note = match latest {
            Some(r) => {
                storage
                    .note_for_reading(r.id, NoteKind::Review.as_str())
                    .await?
            }
            None => None,
        };
        let my_rating = match &review_note {
            Some(note) => match engine.goodreads_rating(note.id).await {
                Ok(r) => r,
                Err(EngineError::UnmappedRating { value, scale }) => {
                    warnings.push(Diagnostic {
                        kind: DiagnosticKind::GoodreadsRatingUnmapped {
                            title: book.display_title().to_string(),
                        },
                        severity: Severity::Warning,
                        detail: format!(
                            "{value} on the '{scale}' scale has no Goodreads mapping, so the row \
                             was skipped (`bookmole rating map {value} <0-5 in quarters>`)"
                        ),
                    });
                    continue;
                }
                Err(e) => return Err(e),
            },
            None => None,
        };
        let review = match &review_note {
            Some(note) => engine.note_body(note)?,
            None => String::new(),
        };

        let bookshelves = storage
            .book_tags(book_id)
            .await?
            .into_iter()
            .map(|t| t.raw.unwrap_or(t.tag))
            .collect::<Vec<_>>()
            .join(", ");

        rows.push(ExportRow {
            title: book.display_title().to_string(),
            author: book.authors.first().cloned().unwrap_or_default(),
            isbn: book.any_isbn().map(str::to_string),
            my_rating,
            date_read: latest.and_then(|r| r.finished_at).and_then(format_date),
            date_added: book
                .created_at
                .map(|d| d.unix_timestamp())
                .and_then(format_date),
            bookshelves,
            review,
        });
    }

    // Ordered by the data, never row id: re-import into an empty library assigns fresh ids and
    // would otherwise reorder rows.
    rows.sort_by(|a, b| {
        (a.title.to_lowercase(), &a.author, &a.isbn).cmp(&(
            b.title.to_lowercase(),
            &b.author,
            &b.isbn,
        ))
    });
    Ok((write_csv(&rows), warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excel_armour_comes_off_and_an_empty_cell_stays_empty() {
        assert_eq!(strip_armour("=\"0990000028\""), Some("0990000028"));
        assert_eq!(strip_armour("=\"\""), None);
        assert_eq!(strip_armour("0990000028"), Some("0990000028"));
        assert_eq!(
            strip_armour("  =\"9780316769488\"  "),
            Some("9780316769488")
        );
        assert_eq!(strip_armour(""), None);
        assert_eq!(strip_armour("="), None);
    }

    #[test]
    fn dates_round_trip_through_goodreads_own_format() {
        let read = parse_date("2019/03/12").expect("parses");
        assert_eq!(format_date(read).as_deref(), Some("2019/03/12"));
        assert_eq!(parse_date("2019-03-12"), Some(read));
        assert_eq!(parse_date(""), None);
        assert_eq!(parse_date("March 2019"), None);
    }

    #[test]
    fn the_three_shelves_are_named_and_the_rest_are_carried() {
        assert_eq!(Shelf::parse("read"), Shelf::Read);
        assert_eq!(Shelf::parse("currently-reading"), Shelf::CurrentlyReading);
        assert_eq!(Shelf::parse("to-read"), Shelf::ToRead);
        assert_eq!(Shelf::parse("dnf"), Shelf::Other("dnf".into()));
    }

    #[test]
    fn a_shelf_decides_how_many_readings_a_row_describes() {
        let row = |shelf: Option<Shelf>, count: i64, read: Option<i64>| GoodreadsRow {
            exclusive_shelf: shelf,
            read_count: count,
            date_read: read,
            ..Default::default()
        };
        assert_eq!(row(Some(Shelf::ToRead), 0, None).wanted_readings(), 0);
        assert_eq!(
            row(Some(Shelf::CurrentlyReading), 0, None).wanted_readings(),
            1
        );
        assert_eq!(row(Some(Shelf::Read), 3, Some(1)).wanted_readings(), 3);
        assert_eq!(row(Some(Shelf::Read), 0, None).wanted_readings(), 1);
        assert_eq!(row(None, 1, Some(1)).wanted_readings(), 1);
        assert_eq!(row(None, 1, None).wanted_readings(), 0);
    }

    #[test]
    fn the_export_writes_only_the_columns_goodreads_reads() {
        let csv = write_csv(&[ExportRow {
            title: "Middlemarch".into(),
            author: "George Eliot".into(),
            isbn: Some("9781900000178".into()),
            my_rating: Some(5.0),
            date_read: Some("2019/03/12".into()),
            date_added: Some("2018/12/01".into()),
            bookshelves: "victorian-lit, favourites".into(),
            review: "A river of a book, and a comma, and a \"quote\".".into(),
        }]);
        let mut lines = csv.lines();
        assert_eq!(
            lines.next().unwrap(),
            "Title,Author,ISBN,My Rating,Date Read,Date Added,Bookshelves,Review"
        );
        let row = lines.next().unwrap();
        // Armoured and properly quoted; Goodreads writes a bare field, which is not strictly valid CSV.
        assert!(row.contains(r#""=""9781900000178""""#), "armoured: {row}");
        assert!(row.contains("\"victorian-lit, favourites\""));
        assert!(row.contains("\"\"quote\"\""), "quotes doubled: {row}");
    }

    #[test]
    fn a_written_row_reads_back_as_itself() {
        let csv = write_csv(&[ExportRow {
            title: "Middlemarch, and \"other\" stories".into(),
            author: "George Eliot".into(),
            isbn: Some("0990000028".into()),
            my_rating: Some(4.0),
            date_read: Some("2019/03/12".into()),
            date_added: Some("2018/12/01".into()),
            bookshelves: "victorian-lit, favourites".into(),
            review: "First line.\nSecond line.".into(),
        }]);
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out.csv");
        std::fs::write(&path, &csv).unwrap();

        let rows = parse_csv(&path).unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            r.title.as_deref(),
            Some("Middlemarch, and \"other\" stories")
        );
        assert_eq!(r.isbn_10.as_deref(), Some("0990000028"));
        assert_eq!(r.my_rating, Some(4.0));
        assert_eq!(r.date_read, parse_date("2019/03/12"));
        assert_eq!(r.shelves, vec!["victorian-lit", "favourites"]);
        assert_eq!(r.review.as_deref(), Some("First line.\nSecond line."));
    }

    #[test]
    fn a_formula_goes_out_inert_and_reads_back_as_itself() {
        let title = r#"=HYPERLINK("https://example.invalid","click")"#;
        let csv = write_csv(&[ExportRow {
            title: title.into(),
            author: "+Plus Author".into(),
            isbn: None,
            my_rating: None,
            date_read: None,
            date_added: None,
            bookshelves: "@shelf".into(),
            review: "-minus".into(),
        }]);
        let mut reader = csv::Reader::from_reader(csv.as_bytes());
        let rec = reader.records().next().unwrap().unwrap();
        assert!(rec.get(0).unwrap().starts_with("'="), "{csv}");
        assert!(rec.get(1).unwrap().starts_with("'+"));
        assert!(rec.get(6).unwrap().starts_with("'@"));
        assert!(rec.get(7).unwrap().starts_with("'-"));

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("out.csv");
        std::fs::write(&path, &csv).unwrap();
        let r = &parse_csv(&path).unwrap()[0];
        assert_eq!(r.title.as_deref(), Some(title));
        assert_eq!(r.authors, vec!["+Plus Author".to_string()]);
        assert_eq!(r.shelves, vec!["@shelf".to_string()]);
        assert_eq!(r.review.as_deref(), Some("-minus"));
    }
}
