use std::collections::HashMap;

use sqlx::Row;
use sqlx::query::Query;
use sqlx::sqlite::{Sqlite, SqliteArguments, SqliteRow};
use time::OffsetDateTime;

use super::copies;
use super::field_provenance::{self, Source};
use super::highlights::identity_hash_of;
use super::query::{Bind, BookFilter, BookQuery, BookSummary, LibraryScope};
use super::{Storage, now_unix};
use crate::book::{Book, series_index_text};
use crate::copies::NewCopy;
use crate::error::{EngineError, Result};

/// `limit` selects along the sort key in every arm; every arm is an indexed `ORDER BY` except
/// `Progress`,
/// which cannot be indexed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookSort {
    LastModified,
    Title,
    Progress,
    /// By the first author's last name, authorless last; ordered by the write-time `sort_author` key.
    Author,
    /// Newest first, undated last.
    Year,
}

/// What `merge_books` moved; the dropped counts tell a clean merge from one that lost duplicates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    /// False when `src` was already gone; every other field is then zero, so a repeat merge is a no-op.
    pub src_existed: bool,
    pub highlights_moved: usize,
    /// Highlights that collided on identity with one on `dst`; dropped after their dependents were
    /// repointed.
    pub highlights_dropped: usize,
    pub notes_moved: usize,
    /// Readings repointed at `dst`; when both had one open, the older is closed as `abandoned`, not
    /// deleted.
    pub readings_moved: usize,
    pub flashcards_moved: usize,
    /// A word both books captured cannot move (`UNIQUE(book_id, word)`).
    pub flashcards_dropped: usize,
    pub device_links_moved: usize,
    /// No `dropped` twin: `book_files` is keyed on the sha256, so a collision cannot occur.
    pub files_moved: usize,
    /// `src`'s cover file when `dst` kept its own; now unreferenced, so the caller deletes it.
    pub orphaned_cover: Option<String>,
}

/// The `books` columns plus the reading-state projections of the current reading and the cover
/// metrics.
/// Every column is qualified because `readings` has its own `id`, `created_at` and `last_modified`.
pub(super) const BOOK_COLUMNS: &str = "books.id, books.title, books.sort_title, books.authors, \
     books.translators, books.publisher, books.publish_year, books.first_published, books.language, books.isbn_10, \
     books.isbn_13, books.openlibrary_key, books.googlebooks_id, books.cover_url, \
     books.cover_path, books.page_count, books.description, books.first_sentence, \
     books.subjects, books.series, books.series_index, \
     books.cover_width, books.cover_height, books.cover_accent, books.cover_thumb_path, \
     cur.current_page AS current_page, \
     CASE WHEN cur.status = 'finished' THEN 1 ELSE 0 END AS finished, \
     cur.status AS reading_status, cur.ko_percent AS ko_percent, \
     cur.started_at AS date_started, cur.finished_at AS date_finished, \
     books.created_at, books.last_modified";

/// Joins the current reading (the open one, else the most recent): one definition of "current" for
/// every column.
/// A macro because `concat!` cannot take a `const`, and both drivers (`books`, `readings`) need
/// identical text.
macro_rules! book_current_join {
    () => {
        "LEFT JOIN readings cur ON cur.id = (
         SELECT r.id FROM readings r WHERE r.book_id = books.id
          ORDER BY (r.finished_at IS NULL) DESC,
                   COALESCE(r.started_at, r.created_at) DESC, r.id DESC
          LIMIT 1)"
    };
}
pub(super) use book_current_join;

pub(super) const BOOK_FROM: &str = concat!("FROM books ", book_current_join!());

/// `ORDER BY` for one sort key, ending in `books.id` so every arm is a total order and paging is
/// stable.
/// Each leading term must match its index's expression and collation exactly, or SQLite skips the
/// index.
fn order_by(sort: BookSort) -> &'static str {
    match sort {
        BookSort::LastModified => "books.last_modified DESC, books.id DESC",
        // `COALESCE`: `sort_title` is NULL for a row not yet back-filled, so fall back to the raw title.
        // Must match `idx_books_sort_title`'s expression, `COLLATE NOCASE` included.
        BookSort::Title => {
            "COALESCE(books.sort_title, books.title) COLLATE NOCASE ASC, books.id ASC"
        }
        // A computed ratio across a LEFT JOIN: no index is possible, hence offset paging.
        BookSort::Progress => {
            "CAST(cur.current_page AS REAL) / NULLIF(books.page_count, 0) DESC NULLS LAST, \
             books.id ASC"
        }
        // Undated last, so the NULL placement sits outside the descending reversal.
        BookSort::Year => "books.publish_year DESC NULLS LAST, books.id DESC",
        // Plain BINARY comparison: `sort_author` is a packed key whose `memcmp` order is
        // `names::sort_key`'s, so
        // `COLLATE NOCASE` would break it. Rows not yet back-filled (NULL) sort first.
        BookSort::Author => "books.sort_author ASC, books.id ASC",
    }
}

/// Applies a `Predicate`'s values in the order its clause names them; shared with `readings`.
pub(super) fn bind_all<'q>(
    mut q: Query<'q, Sqlite, SqliteArguments<'q>>,
    binds: &[Bind],
) -> Query<'q, Sqlite, SqliteArguments<'q>> {
    for b in binds {
        q = match b {
            Bind::Text(s) => q.bind(s.clone()),
            Bind::Int(i) => q.bind(*i),
        };
    }
    q
}

pub(super) fn row_to_book(row: &SqliteRow) -> Result<Book> {
    let authors: String = row.try_get("authors")?;
    let translators: String = row.try_get("translators")?;
    let subjects: String = row.try_get("subjects")?;
    let created: i64 = row.try_get("created_at")?;
    let modified: i64 = row.try_get("last_modified")?;
    Ok(Book {
        id: row.try_get("id")?,
        title: row.try_get("title")?,
        sort_title: row.try_get("sort_title")?,
        authors: serde_json::from_str(&authors).unwrap_or_default(),
        translators: serde_json::from_str(&translators).unwrap_or_default(),
        publisher: row.try_get("publisher")?,
        publish_year: row.try_get("publish_year")?,
        first_published: row.try_get("first_published")?,
        language: row.try_get("language")?,
        isbn_10: row.try_get("isbn_10")?,
        isbn_13: row.try_get("isbn_13")?,
        openlibrary_key: row.try_get("openlibrary_key")?,
        googlebooks_id: row.try_get("googlebooks_id")?,
        cover_url: row.try_get("cover_url")?,
        cover_path: row.try_get("cover_path")?,
        page_count: row.try_get("page_count")?,
        description: row.try_get("description")?,
        first_sentence: row.try_get("first_sentence")?,
        subjects: serde_json::from_str(&subjects).unwrap_or_default(),
        series: row.try_get("series")?,
        series_index: row.try_get("series_index")?,
        cover_width: row.try_get("cover_width")?,
        cover_height: row.try_get("cover_height")?,
        cover_accent: row.try_get("cover_accent")?,
        cover_thumb_path: row.try_get("cover_thumb_path")?,
        current_page: row.try_get("current_page")?,
        finished: row.try_get::<i64, _>("finished")? != 0,
        reading_status: row.try_get("reading_status")?,
        ko_percent: row.try_get("ko_percent")?,
        date_started: row.try_get("date_started")?,
        date_finished: row.try_get("date_finished")?,
        created_at: OffsetDateTime::from_unix_timestamp(created).ok(),
        last_modified: OffsetDateTime::from_unix_timestamp(modified).ok(),
    })
}

/// How one column merges when a partial record arrives. `title` and the JSON list columns are NOT
/// NULL with
/// an empty sentinel, so `COALESCE` would let an empty value overwrite a real one.
#[derive(Clone, Copy)]
enum Merge {
    NonEmptyText,
    NonEmptyList,
    Coalesce,
}

type Take = fn(&mut Book, &Book);

/// How the federated search merges a column between providers, in memory; `Merge` is the SQL side
/// against a
/// stored row. Every variant but `Local` carries its own `Take`.
#[derive(Clone, Copy)]
enum Federated {
    /// Ours, never a provider's (a derived key, a local path): nothing to merge or attribute.
    Local,
    /// The first provider to speak owns it; a later one fills only a gap.
    Fill(Take),
    /// This source outranks a value already merged in: OpenLibrary for editions, Google Books for
    /// prose.
    Prefer(Source, Take),
    /// Moves with `Rule::pair` and is never taken alone, or a row would say *Dune #2* about another
    /// series.
    WithPair(Take),
}

/// One column's entry in `MERGE_RULES`. `says` must agree with the SQL's own definition of empty,
/// or
/// provenance stamps drift from what is written.
struct Rule {
    col: &'static str,
    merge: Merge,
    says: fn(&Book) -> bool,
    /// Printable text of what the record says; `Some` exactly when `says` is true.
    show: fn(&Book) -> Option<String>,
    /// The column this one only means anything beside (`series`/`series_index`); owning either half
    /// holds both.
    pair: Option<&'static str>,
    federated: Federated,
}

/// The provider no-clobber merge, defined once: `upsert_book`, `enrich_book`, `merge_books` and the
/// provenance stamp all generate from it so they cannot disagree about a column.
/// Records are partial, so a missing value means unknown, unlike a device sidecar's complete state.
/// `sort_title`, `sort_author` and the cover metrics are deliberately absent: each has one writer.
const MERGE_RULES: [Rule; 19] = [
    Rule {
        col: "title",
        merge: Merge::NonEmptyText,
        says: |b| b.title.as_deref().is_some_and(|t| !t.is_empty()),
        show: |b| b.title.clone().filter(|t| !t.is_empty()),
        pair: None,
        federated: Federated::Fill(|d, s| d.title = s.title.clone()),
    },
    Rule {
        col: "authors",
        merge: Merge::NonEmptyList,
        says: |b| !b.authors.is_empty(),
        show: |b| (!b.authors.is_empty()).then(|| b.authors.join(", ")),
        pair: None,
        federated: Federated::Fill(|d, s| d.authors = s.authors.clone()),
    },
    Rule {
        col: "translators",
        merge: Merge::NonEmptyList,
        says: |b| !b.translators.is_empty(),
        show: |b| (!b.translators.is_empty()).then(|| b.translators.join(", ")),
        pair: None,
        federated: Federated::Fill(|d, s| d.translators = s.translators.clone()),
    },
    Rule {
        col: "publisher",
        merge: Merge::Coalesce,
        says: |b| b.publisher.is_some(),
        show: |b| b.publisher.clone(),
        pair: None,
        federated: Federated::Fill(|d, s| d.publisher = s.publisher.clone()),
    },
    Rule {
        col: "publish_year",
        merge: Merge::Coalesce,
        says: |b| b.publish_year.is_some(),
        show: |b| b.publish_year.map(|y| y.to_string()),
        pair: None,
        federated: Federated::Fill(|d, s| d.publish_year = s.publish_year),
    },
    Rule {
        col: "first_published",
        merge: Merge::Coalesce,
        says: |b| b.first_published.is_some(),
        show: |b| b.first_published.map(|y| y.to_string()),
        pair: None,
        // Prefer, not Fill: only OpenLibrary reports the work-level year, and a second source must not
        // displace it.
        federated: Federated::Prefer(Source::OpenLibrary, |d, s| {
            d.first_published = s.first_published
        }),
    },
    Rule {
        col: "language",
        merge: Merge::Coalesce,
        says: |b| b.language.is_some(),
        show: |b| b.language.clone(),
        pair: None,
        federated: Federated::Prefer(Source::GoogleBooks, |d, s| d.language = s.language.clone()),
    },
    Rule {
        col: "isbn_10",
        merge: Merge::Coalesce,
        says: |b| b.isbn_10.is_some(),
        show: |b| b.isbn_10.clone(),
        // Paired with `isbn_13` like `series`: owning one holds both, or two editions' ISBNs mix in one
        // row.
        pair: Some("isbn_13"),
        // Editions are OpenLibrary's; Google's volumes often carry another printing's ISBN.
        federated: Federated::Prefer(Source::OpenLibrary, |d, s| d.isbn_10 = s.isbn_10.clone()),
    },
    Rule {
        col: "isbn_13",
        merge: Merge::Coalesce,
        says: |b| b.isbn_13.is_some(),
        show: |b| b.isbn_13.clone(),
        pair: Some("isbn_10"),
        federated: Federated::Prefer(Source::OpenLibrary, |d, s| d.isbn_13 = s.isbn_13.clone()),
    },
    Rule {
        col: "openlibrary_key",
        merge: Merge::Coalesce,
        says: |b| b.openlibrary_key.is_some(),
        show: |b| b.openlibrary_key.clone(),
        pair: None,
        federated: Federated::Fill(|d, s| d.openlibrary_key = s.openlibrary_key.clone()),
    },
    Rule {
        col: "googlebooks_id",
        merge: Merge::Coalesce,
        says: |b| b.googlebooks_id.is_some(),
        show: |b| b.googlebooks_id.clone(),
        pair: None,
        federated: Federated::Fill(|d, s| d.googlebooks_id = s.googlebooks_id.clone()),
    },
    Rule {
        col: "cover_url",
        merge: Merge::Coalesce,
        says: |b| b.cover_url.is_some(),
        show: |b| b.cover_url.clone(),
        pair: None,
        federated: Federated::Fill(|d, s| d.cover_url = s.cover_url.clone()),
    },
    Rule {
        col: "cover_path",
        merge: Merge::Coalesce,
        says: |b| b.cover_path.is_some(),
        show: |b| b.cover_path.clone(),
        pair: None,
        // Local: written by `download_cover` after any merge, never supplied by a provider.
        federated: Federated::Local,
    },
    Rule {
        col: "page_count",
        merge: Merge::Coalesce,
        says: |b| b.page_count.is_some(),
        show: |b| b.page_count.map(|n| n.to_string()),
        pair: None,
        federated: Federated::Prefer(Source::OpenLibrary, |d, s| d.page_count = s.page_count),
    },
    Rule {
        col: "description",
        merge: Merge::Coalesce,
        says: |b| b.description.is_some(),
        show: |b| b.description.clone(),
        pair: None,
        federated: Federated::Prefer(Source::GoogleBooks, |d, s| {
            d.description = s.description.clone()
        }),
    },
    Rule {
        col: "first_sentence",
        merge: Merge::Coalesce,
        says: |b| b.first_sentence.is_some(),
        show: |b| b.first_sentence.clone(),
        pair: None,
        federated: Federated::Fill(|d, s| d.first_sentence = s.first_sentence.clone()),
    },
    Rule {
        // A non-empty list replaces the stored set whole; `[]` means "record is silent", not "no subjects".
        // Not a union: source vocabularies differ and provenance holds one source per field.
        col: "subjects",
        merge: Merge::NonEmptyList,
        says: |b| !b.subjects.is_empty(),
        show: |b| (!b.subjects.is_empty()).then(|| b.subjects.join(", ")),
        pair: None,
        federated: Federated::Fill(|d, s| d.subjects = s.subjects.clone()),
    },
    Rule {
        col: "series",
        merge: Merge::Coalesce,
        says: |b| b.series.is_some(),
        show: |b| b.series.clone(),
        pair: Some("series_index"),
        federated: Federated::Fill(|d, s| d.series = s.series.clone()),
    },
    Rule {
        // Fractional: novellas are 0.5.
        col: "series_index",
        merge: Merge::Coalesce,
        says: |b| b.series_index.is_some(),
        show: |b| b.series_index.map(|n| format!("#{}", series_index_text(n))),
        pair: Some("series"),
        // Assigned unconditionally so a stale index clears, even when the provider has none; only a present
        // value is claimed.
        federated: Federated::WithPair(|d, s| d.series_index = s.series_index),
    },
];

pub(crate) fn merge_columns() -> impl Iterator<Item = &'static str> {
    MERGE_RULES.iter().map(|r| r.col)
}

/// `None` both for an unknown column and for a record silent about it.
pub(crate) fn field_value(book: &Book, col: &str) -> Option<String> {
    MERGE_RULES
        .iter()
        .find(|r| r.col == col)
        .and_then(|r| (r.show)(book))
}

/// Merges one provider's record into a partial one and returns the columns it supplied; writes
/// nothing.
pub(crate) fn merge_provider_record(
    dst: &mut Book,
    src: &Book,
    provider: Source,
) -> Vec<&'static str> {
    let mut claimed: Vec<&'static str> = Vec::new();
    for rule in MERGE_RULES.iter() {
        let (take, outranks) = match rule.federated {
            Federated::Local | Federated::WithPair(_) => continue,
            Federated::Fill(take) => (take, false),
            Federated::Prefer(wins, take) => (take, wins == provider),
        };
        if !(rule.says)(src) || (!outranks && (rule.says)(dst)) {
            continue;
        }
        take(dst, src);
        claimed.push(rule.col);

        // Assigned unconditionally (clears a stale index from another series), claimed only where a value
        // exists.
        if let Some(pair) = MERGE_RULES.iter().find(|r| Some(r.col) == rule.pair)
            && let Federated::WithPair(take_pair) = pair.federated
        {
            take_pair(dst, src);
            if (pair.says)(src) {
                claimed.push(pair.col);
            }
        }
    }
    claimed
}

pub(crate) fn field_pair(col: &str) -> Option<&'static str> {
    MERGE_RULES
        .iter()
        .find(|r| r.col == col)
        .and_then(|r| r.pair)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Winner {
    /// `upsert_book`, `enrich_book`: stored values survive only where the record is silent.
    Incoming,
    /// `merge_books`: `dst` is the kept row and `src` only fills its gaps.
    Stored,
}

/// SET clause for `MERGE_RULES`; `src` gives each column's incoming expression, and the caller
/// appends
/// `last_modified`. `guard_user` stops user-owned fields being overwritten; `merge_books` turns it
/// off.
fn merge_set(winner: Winner, guard_user: bool, src: impl Fn(usize, &str) -> String) -> String {
    let mut cover_value: Option<String> = None;
    let mut sets: Vec<String> = MERGE_RULES
        .iter()
        .enumerate()
        .map(|(i, rule)| {
            let col = rule.col;
            let new = src(i, col);
            let stored = format!("books.{col}");
            let (a, b) = match winner {
                Winner::Incoming => (&new, &stored),
                Winner::Stored => (&stored, &new),
            };
            let merged = match rule.merge {
                Merge::NonEmptyText => format!("CASE WHEN {a} != '' THEN {a} ELSE {b} END"),
                Merge::NonEmptyList => format!("CASE WHEN {a} != '[]' THEN {a} ELSE {b} END"),
                Merge::Coalesce => format!("COALESCE({a}, {b})"),
            };
            let value = if guard_user {
                // A paired column is guarded by either half's claim, or one half could be filled from another
                // book's record.
                let owned = match rule.pair {
                    Some(other) => format!(
                        "({} OR {})",
                        field_provenance::user_owns("books.id", col),
                        field_provenance::user_owns("books.id", other)
                    ),
                    None => field_provenance::user_owns("books.id", col),
                };
                format!("CASE WHEN {owned} THEN {stored} ELSE {merged} END")
            } else {
                merged
            };
            if col == "cover_path" {
                cover_value = Some(value.clone());
            }
            format!("{col} = {value}")
        })
        .collect();
    if let Some(cover) = cover_value {
        sets.push(invalidate_cover_metrics(&cover));
    }
    sets.join(",\n            ")
}

/// Cover columns measured from `cover_path`; stale whenever it moves, so any statement that moves
/// it resets them.
const COVER_METRICS: [&str; 4] = [
    "cover_width",
    "cover_height",
    "cover_accent",
    "cover_thumb_path",
];

/// Any write that moves `cover_path` clears the measurements of the old file, generated from the
/// same
/// expression that stores the path. `MERGE_RULES` gives it writers besides `set_cover`.
fn invalidate_cover_metrics(cover_value: &str) -> String {
    COVER_METRICS
        .iter()
        .map(|col| format!("{col} = CASE WHEN ({cover_value}) IS books.cover_path THEN books.{col} ELSE NULL END"))
        .collect::<Vec<_>>()
        .join(",\n            ")
}

/// Recomputes `sort_title`/`sort_author` from the stored row, never the incoming record, which may
/// be partial. Every write that can change a title or author list calls it last in its own
/// transaction. A user-owned `sort_title` is kept, and `last_modified` is not touched.
pub(super) async fn refresh_sort_keys(
    conn: &mut sqlx::SqliteConnection,
    book_id: i64,
) -> Result<()> {
    let Some(row) = sqlx::query("SELECT title, authors FROM books WHERE id = ?")
        .bind(book_id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        // Not an error: `merge_books` may have deleted `src` in the same transaction.
        return Ok(());
    };
    let title: String = row.try_get("title")?;
    let authors: String = row.try_get("authors")?;
    let authors: Vec<String> = serde_json::from_str(&authors).unwrap_or_default();

    let author_key = crate::sort::author_key(&authors);
    if field_provenance::user_owns_now(&mut *conn, book_id, "sort_title").await? {
        sqlx::query("UPDATE books SET sort_author = ?1 WHERE id = ?2")
            .bind(author_key)
            .bind(book_id)
            .execute(&mut *conn)
            .await?;
        return Ok(());
    }
    sqlx::query("UPDATE books SET sort_title = ?1, sort_author = ?2 WHERE id = ?3")
        .bind(crate::sort::sort_title(&title))
        .bind(author_key)
        .bind(book_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Everyone except the user is guarded; an unattributed write (`None`) is guarded too.
fn guards_user(source: Option<Source>) -> bool {
    source != Some(Source::User)
}

/// A write is guarded unless every field it claims is the user's own.
fn guards_all(claims: &[(&'static str, Source)]) -> bool {
    claims.iter().all(|(_, s)| guards_user(Some(*s)))
}

/// Stamps each claimed field the record actually says, per source, inside the write's transaction.
async fn stamp_claims(
    conn: &mut sqlx::SqliteConnection,
    book_id: i64,
    book: &Book,
    claims: &[(&'static str, Source)],
    guard_user: bool,
) -> Result<()> {
    let said = fields_said(book);
    for source in Source::ALL {
        let fields: Vec<&'static str> = claims
            .iter()
            .filter(|(field, s)| *s == source && said.contains(field))
            .map(|(field, _)| *field)
            .collect();
        let fields = claimable(&mut *conn, book_id, &fields, guard_user).await?;
        if !fields.is_empty() {
            field_provenance::stamp(&mut *conn, book_id, &fields, source).await?;
        }
    }
    Ok(())
}

/// Columns the record has something to say about; exactly those get a `field_provenance` row.
pub(crate) fn fields_said(book: &Book) -> Vec<&'static str> {
    MERGE_RULES
        .iter()
        .filter(|r| (r.says)(book))
        .map(|r| r.col)
        .collect()
}

/// Fields a guarded write may claim: a paired column held by the other half's user claim is
/// excluded,
/// since the stamp's own check would not refuse it.
async fn claimable(
    conn: &mut sqlx::SqliteConnection,
    book_id: i64,
    fields: &[&'static str],
    guard_user: bool,
) -> Result<Vec<&'static str>> {
    if !guard_user {
        return Ok(fields.to_vec());
    }
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        let held = match MERGE_RULES
            .iter()
            .find(|r| r.col == *field)
            .and_then(|r| r.pair)
        {
            Some(other) => field_provenance::user_owns_now(conn, book_id, other).await?,
            None => false,
        };
        if !held {
            out.push(*field);
        }
    }
    Ok(out)
}

/// Columns `stored` is silent about and `incoming` is not: all a `Winner::Stored` merge takes, so
/// all
/// whose attribution may travel.
fn gap_fields(stored: &Book, incoming: &Book) -> Vec<&'static str> {
    MERGE_RULES
        .iter()
        .filter(|r| !(r.says)(stored) && (r.says)(incoming))
        .map(|r| r.col)
        .collect()
}

/// Positional params after the `MERGE_RULES` columns: `last_modified`, then the row id; derived
/// from the table length.
fn tail_params() -> (String, String) {
    let n = MERGE_RULES.len();
    (format!("?{}", n + 1), format!("?{}", n + 2))
}

/// Binds the `MERGE_RULES` columns in order; must stay in step with `merge_set`.
fn bind_merge_columns<'q>(
    q: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    book: &Book,
) -> Result<sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>> {
    Ok(q.bind(book.title.clone().unwrap_or_default())
        .bind(serde_json::to_string(&book.authors)?)
        .bind(serde_json::to_string(&book.translators)?)
        .bind(book.publisher.clone())
        .bind(book.publish_year)
        .bind(book.first_published)
        .bind(book.language.clone())
        .bind(book.isbn_10.clone())
        .bind(book.isbn_13.clone())
        .bind(book.openlibrary_key.clone())
        .bind(book.googlebooks_id.clone())
        .bind(book.cover_url.clone())
        .bind(book.cover_path.clone())
        .bind(book.page_count)
        .bind(book.description.clone())
        .bind(book.first_sentence.clone())
        .bind(serde_json::to_string(&book.subjects)?)
        .bind(book.series.clone())
        .bind(book.series_index))
}

impl Storage {
    /// Insert-or-merge keyed on isbn_10, else isbn_13, else plain insert; returns the row id.
    /// Ignores `Book`'s progress fields: reading state is written only by `update_progress`.
    /// `source` stamps provenance on every field the record supplies; `None` stamps nothing.
    pub async fn upsert_book(&self, book: &Book, source: Option<Source>) -> Result<i64> {
        let claims: Vec<(&'static str, Source)> = match source {
            Some(source) => fields_said(book).into_iter().map(|f| (f, source)).collect(),
            None => Vec::new(),
        };
        self.upsert_book_attributed(book, &claims).await
    }

    /// `upsert_book` where fields came from different sources, each named; a field with no claim
    /// gets no provenance row.
    pub async fn upsert_book_attributed(
        &self,
        book: &Book,
        claims: &[(&'static str, Source)],
    ) -> Result<i64> {
        // One transaction: a row whose values landed but whose provenance did not can never be repaired.
        let mut tx = self.pool().begin().await?;
        let id = upsert_book_in(&mut tx, book, claims).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// `upsert_book` that also records `first_copy(id)` when no stored book shared one of `book`'s
    /// ISBNs, in one transaction: a created book whose copy failed would stay out of the library,
    /// and a retry would find the book and add nothing.
    pub async fn upsert_book_with_first_copy(
        &self,
        book: &Book,
        first_copy: impl FnOnce(i64) -> NewCopy,
    ) -> Result<i64> {
        let mut tx = self.pool().begin().await?;
        let mut existed = false;
        for isbn in [&book.isbn_13, &book.isbn_10].into_iter().flatten() {
            existed |= sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM books WHERE isbn_10 = ?1 OR isbn_13 = ?1)",
            )
            .bind(isbn)
            .fetch_one(&mut *tx)
            .await?;
        }
        let id = upsert_book_in(&mut tx, book, &[]).await?;
        if !existed {
            copies::insert_copy(&mut tx, &first_copy(id)).await?;
        }
        tx.commit().await?;
        Ok(id)
    }
}

async fn upsert_book_in(
    conn: &mut sqlx::SqliteConnection,
    book: &Book,
    claims: &[(&'static str, Source)],
) -> Result<i64> {
    let guard_user = guards_all(claims);
    let set_clause = format!(
        "{},\n            last_modified = excluded.last_modified",
        merge_set(Winner::Incoming, guard_user, |_, col| format!(
            "excluded.{col}"
        ))
    );

    // Built from `MERGE_RULES` plus `created_at` and `last_modified`, so a column change cannot desync
    // it.
    let holes = std::iter::repeat_n("?", MERGE_RULES.len() + 2)
        .collect::<Vec<_>>()
        .join(", ");
    let insert = format!(
        "INSERT INTO books ({}, created_at, last_modified) VALUES ({holes})",
        merge_columns().collect::<Vec<_>>().join(", ")
    );

    let sql = if book.isbn_10.is_some() {
        format!("{insert} ON CONFLICT(isbn_10) DO UPDATE SET {set_clause} RETURNING id")
    } else if book.isbn_13.is_some() {
        format!("{insert} ON CONFLICT(isbn_13) DO UPDATE SET {set_clause} RETURNING id")
    } else {
        format!("{insert} RETURNING id")
    };

    let now = now_unix();
    let row = bind_merge_columns(sqlx::query(&sql), book)?
        .bind(now)
        .bind(now)
        .fetch_one(&mut *conn)
        .await?;
    let id: i64 = row.try_get("id")?;
    stamp_claims(&mut *conn, id, book, claims, guard_user).await?;
    // After the stamp: a `Source::User` claim on `sort_title` tells the derivation to leave it alone.
    refresh_sort_keys(&mut *conn, id).await?;
    Ok(id)
}

impl Storage {
    /// Merges a partial record into a book already identified by id. Never inserts, unlike
    /// `upsert_book`, whose ISBN-less branch would create a second row.
    /// `Some(Source::User)` claims the fields it writes, so no provider merge overwrites them.
    pub async fn enrich_book(
        &self,
        book_id: i64,
        book: &Book,
        source: Option<Source>,
    ) -> Result<()> {
        let claims: Vec<(&'static str, Source)> = match source {
            Some(source) => fields_said(book).into_iter().map(|f| (f, source)).collect(),
            // `None` is a writer that cannot name a source, so it claims no fields.
            None => Vec::new(),
        };
        self.enrich_book_attributed(book_id, book, &claims).await
    }

    /// `enrich_book` where fields came from different sources, each named: one merged write,
    /// attribution stamped per field.
    pub async fn enrich_book_attributed(
        &self,
        book_id: i64,
        book: &Book,
        claims: &[(&'static str, Source)],
    ) -> Result<()> {
        let guard_user = guards_all(claims);
        let (modified, id) = tail_params();
        let sql = format!(
            "UPDATE books SET {}, last_modified = {modified} WHERE id = {id}",
            merge_set(Winner::Incoming, guard_user, |i, _| format!("?{}", i + 1))
        );
        let mut tx = self.pool().begin().await?;
        bind_merge_columns(sqlx::query(&sql), book)?
            .bind(now_unix())
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        stamp_claims(&mut tx, book_id, book, claims, guard_user).await?;
        refresh_sort_keys(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Merges a fallback record: the stored row wins and this fills only what it is silent about.
    pub async fn fill_book(&self, book_id: i64, book: &Book, source: Option<Source>) -> Result<()> {
        let (modified, id) = tail_params();
        let sql = format!(
            "UPDATE books SET {}, last_modified = {modified} WHERE id = {id}",
            merge_set(Winner::Stored, guards_user(source), |i, _| format!(
                "?{}",
                i + 1
            ))
        );
        let mut tx = self.pool().begin().await?;
        // Read before the write, or the gaps it filled are already closed.
        let select = format!("SELECT {BOOK_COLUMNS} {BOOK_FROM} WHERE books.id = ?");
        let stored = sqlx::query(&select)
            .bind(book_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {book_id}")))?;
        let gaps = gap_fields(&row_to_book(&stored)?, book);

        bind_merge_columns(sqlx::query(&sql), book)?
            .bind(now_unix())
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        if let Some(source) = source {
            let gaps = claimable(&mut tx, book_id, &gaps, guards_user(Some(source))).await?;
            field_provenance::stamp(&mut tx, book_id, &gaps, source).await?;
        }
        refresh_sort_keys(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_book(&self, id: i64) -> Result<Option<Book>> {
        let sql = format!("SELECT {BOOK_COLUMNS} {BOOK_FROM} WHERE books.id = ?");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_book).transpose()
    }

    pub async fn find_book_by_isbn(&self, isbn: &str) -> Result<Option<Book>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM} WHERE books.isbn_10 = ?1 OR books.isbn_13 = ?1"
        );
        let row = sqlx::query(&sql)
            .bind(isbn)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_book).transpose()
    }

    pub async fn find_books_by_title(&self, fragment: &str) -> Result<Vec<Book>> {
        self.find_books_by_title_in(fragment, LibraryScope::All)
            .await
    }

    pub async fn find_books_by_title_in(
        &self,
        fragment: &str,
        scope: LibraryScope,
    ) -> Result<Vec<Book>> {
        let scoped = scope
            .clause("books.id")
            .map(|c| format!(" AND {c}"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM} WHERE books.title LIKE ?{scoped}
             ORDER BY books.last_modified DESC, books.id DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(format!("%{fragment}%"))
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_book).collect()
    }

    pub async fn list_books(&self, query: &BookQuery) -> Result<Vec<Book>> {
        let predicate = query.filter.predicate();
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM} {} ORDER BY {} LIMIT ? OFFSET ?",
            predicate.sql,
            order_by(query.sort)
        );
        let q = bind_all(sqlx::query(&sql), &predicate.binds)
            .bind(query.limit)
            .bind(query.offset.max(0));
        let rows = q.fetch_all(self.pool()).await?;
        rows.iter().map(row_to_book).collect()
    }

    pub async fn count_books(&self, filter: &BookFilter) -> Result<i64> {
        let predicate = filter.predicate();
        let sql = format!("SELECT COUNT(*) {BOOK_FROM} {}", predicate.sql);
        let row = bind_all(sqlx::query(&sql), &predicate.binds)
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get::<i64, _>(0)?)
    }

    /// One `BookSummary` per id, in the order given and zeros included; cost does not grow per row.
    pub async fn book_summaries(&self, book_ids: &[i64]) -> Result<Vec<BookSummary>> {
        let mut totals: HashMap<i64, BookSummary> = HashMap::new();
        for id in book_ids {
            totals.entry(*id).or_insert(BookSummary {
                book_id: *id,
                ..Default::default()
            });
        }
        // Chunked to stay under SQLite's parameter ceiling (999 on older builds).
        let ids: Vec<i64> = totals.keys().copied().collect();
        for chunk in ids.chunks(500) {
            let holes = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            for (table, pick) in [
                ("highlights", 0usize),
                ("notes", 1usize),
                ("book_files", 2usize),
            ] {
                let sql = format!(
                    "SELECT book_id, COUNT(*) AS n FROM {table} \
                     WHERE book_id IN ({holes}) GROUP BY book_id"
                );
                let mut q = sqlx::query(&sql);
                for id in chunk {
                    q = q.bind(*id);
                }
                for row in q.fetch_all(self.pool()).await? {
                    let id: i64 = row.try_get("book_id")?;
                    let n: i64 = row.try_get("n")?;
                    if let Some(s) = totals.get_mut(&id) {
                        match pick {
                            0 => s.highlights = n,
                            1 => s.notes = n,
                            _ => s.files = n,
                        }
                    }
                }
            }
        }
        Ok(book_ids.iter().map(|id| totals[id]).collect())
    }

    /// Folds `src` into `dst` in one transaction: moves highlights, notes, flashcards, readings, device
    /// links and files, fills only `dst`'s empty fields, then deletes `src`. A moved highlight that
    /// collides with one on `dst` is dropped after its anchors and cards are repointed.
    /// Repeating it returns a report with `src_existed == false`; a missing `dst` is an error.
    #[expect(clippy::too_many_lines)]
    pub async fn merge_books(&self, src: i64, dst: i64) -> Result<MergeReport> {
        let mut report = MergeReport::default();
        if src == dst {
            // Not an error: merging a book into itself is already true, and proceeding would delete it.
            report.src_existed = self.get_book(src).await?.is_some();
            return Ok(report);
        }

        // Nothing below may call back through `self`: an in-memory Storage has one connection and would
        // deadlock.
        let mut tx = self.pool().begin().await?;

        // Read before the fill, which changes which fields `dst` is silent about.
        let sql = format!("SELECT {BOOK_COLUMNS} {BOOK_FROM} WHERE books.id = ?");
        let dst_row = sqlx::query(&sql).bind(dst).fetch_optional(&mut *tx).await?;
        let Some(dst_row) = dst_row else {
            return Err(EngineError::NotFound(format!("book id {dst}")));
        };
        let dst_book = row_to_book(&dst_row)?;

        let Some(src_row) = sqlx::query(&sql).bind(src).fetch_optional(&mut *tx).await? else {
            // Already merged or never existed: an empty report makes merging twice equal merging once.
            return Ok(report);
        };
        let src_book = row_to_book(&src_row)?;
        report.src_existed = true;

        let rows =
            sqlx::query(
                "SELECT id, text, pos0, ko_datetime, identity_hash, origin FROM highlights WHERE book_id = ?",
            )
                .bind(src)
                .fetch_all(&mut *tx)
                .await?;
        for r in &rows {
            let id: i64 = r.try_get("id")?;
            let text: String = r.try_get("text")?;
            let pos0: Option<String> = r.try_get("pos0")?;
            let ko_datetime: Option<String> = r.try_get("ko_datetime")?;
            // An imported row's hash omits `book_id` and is not KOReader's formula, so it moves unchanged.
            let origin: Option<String> = r.try_get("origin")?;
            let hash = match origin.as_deref() {
                Some(o) if o != "koreader" && o != "user" => r.try_get("identity_hash")?,
                _ => identity_hash_of(dst, ko_datetime.as_deref(), pos0.as_deref(), &text),
            };

            let survivor: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM highlights WHERE book_id = ? AND identity_hash = ?",
            )
            .bind(dst)
            .bind(&hash)
            .fetch_optional(&mut *tx)
            .await?;
            match survivor {
                Some(keep) => {
                    // Move what hangs off the copy before dropping it: `flashcards.highlight_id` cascades and
                    // `notes.highlight_id` nulls, so the other order loses note anchors.
                    sqlx::query("UPDATE notes SET highlight_id = ? WHERE highlight_id = ?")
                        .bind(keep)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("UPDATE flashcards SET highlight_id = ? WHERE highlight_id = ?")
                        .bind(keep)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    sqlx::query("DELETE FROM highlights WHERE id = ?")
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                    report.highlights_dropped += 1;
                }
                None => {
                    sqlx::query(
                        "UPDATE highlights SET book_id = ?, identity_hash = ? WHERE id = ?",
                    )
                    .bind(dst)
                    .bind(&hash)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                    report.highlights_moved += 1;
                }
            }
        }

        report.notes_moved = sqlx::query("UPDATE notes SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        report.flashcards_dropped = sqlx::query(
            "DELETE FROM flashcards WHERE book_id = ?1
             AND word IN (SELECT word FROM flashcards WHERE book_id = ?2)",
        )
        .bind(src)
        .bind(dst)
        .execute(&mut *tx)
        .await?
        .rows_affected() as usize;
        report.flashcards_moved = sqlx::query("UPDATE flashcards SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        // Two open readings would violate `idx_readings_one_open`: close the older as `abandoned` first,
        // before the
        // move and before `src`'s delete cascades its readings away.
        sqlx::query(
            r#"UPDATE readings SET finished_at = last_modified, status = 'abandoned',
                                   put_down_at = last_modified
               WHERE id = (
                   SELECT id FROM readings
                    WHERE book_id IN (?1, ?2) AND finished_at IS NULL
                    ORDER BY COALESCE(started_at, created_at) ASC, id ASC
                    LIMIT 1)
                 AND (SELECT count(*) FROM readings
                       WHERE book_id IN (?1, ?2) AND finished_at IS NULL) > 1"#,
        )
        .bind(src)
        .bind(dst)
        .execute(&mut *tx)
        .await?;
        report.readings_moved = sqlx::query("UPDATE readings SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        report.device_links_moved =
            sqlx::query("UPDATE device_books SET book_id = ? WHERE book_id = ?")
                .bind(dst)
                .bind(src)
                .execute(&mut *tx)
                .await?
                .rows_affected() as usize;

        copies::repoint_copies(&mut tx, src, dst).await?;

        // `book_files` cascades on `books`: left alone, its rows would be deleted with `src` while the
        // bytes stay on disk.
        // Plain UPDATE: the sha256 is the primary key, so `src` and `dst` cannot both hold one file.
        report.files_moved = sqlx::query("UPDATE book_files SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;

        // `external_ids` and `book_tags` cascade on `books`; losing `external_ids` would make the next
        // Goodreads
        // import recreate the duplicate. `UPDATE OR IGNORE` for tags: both books on one shelf is ordinary.
        sqlx::query("UPDATE external_ids SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE OR IGNORE book_tags SET book_id = ? WHERE book_id = ?")
            .bind(dst)
            .bind(src)
            .execute(&mut *tx)
            .await?;

        // List entries cascade too: repoint them, keeping the earlier position.
        super::lists::repoint_entries(&mut tx, src, dst).await?;

        // Only the gaps' claims travel: the fill is `dst`-wins, so `src`'s claim on a field `dst` has would
        // mis-attribute it.
        field_provenance::move_claims(&mut tx, src, dst, &gap_fields(&dst_book, &src_book)).await?;

        // `src` goes before `dst` is updated: isbn_10 and isbn_13 are UNIQUE, so giving `dst` an ISBN `src`
        // still holds would fail.
        sqlx::query("DELETE FROM books WHERE id = ?")
            .bind(src)
            .execute(&mut *tx)
            .await?;

        // Covers are content-addressed, so another book may share `src`'s file; only report a path nothing
        // else references.
        if dst_book.cover_path.is_some()
            && let Some(src_cover) = &src_book.cover_path
        {
            let others: i64 = sqlx::query_scalar("SELECT count(*) FROM books WHERE cover_path = ?")
                .bind(src_cover)
                .fetch_one(&mut *tx)
                .await?;
            if others == 0 {
                report.orphaned_cover = Some(src_cover.clone());
            }
        }

        // `dst` wins: `src` fills only what `dst` lacks (the inverse of `upsert_book`). The user guard is
        // off; both books are ours.
        let (modified, id_param) = tail_params();
        let fill = format!(
            "UPDATE books SET {}, last_modified = {modified} WHERE id = {id_param}",
            merge_set(Winner::Stored, false, |i, _| format!("?{}", i + 1))
        );
        bind_merge_columns(sqlx::query(&fill), &src_book)?
            .bind(now_unix())
            .bind(dst)
            .execute(&mut *tx)
            .await?;

        // Cover metrics move with the path, and only when `dst` had none and takes `src`'s; they describe
        // that file.
        if dst_book.cover_path.is_none() && src_book.cover_path.is_some() {
            sqlx::query(
                "UPDATE books SET cover_width = ?1, cover_height = ?2, cover_accent = ?3,
                        cover_thumb_path = ?4 WHERE id = ?5",
            )
            .bind(src_book.cover_width)
            .bind(src_book.cover_height)
            .bind(src_book.cover_accent)
            .bind(src_book.cover_thumb_path.clone())
            .bind(dst)
            .execute(&mut *tx)
            .await?;
        }

        // Filing keys are re-derived from the merged row: `dst` may have taken `src`'s title or authors.
        refresh_sort_keys(&mut tx, dst).await?;

        tx.commit().await?;
        Ok(report)
    }

    /// Backdates `created_at`, for an importer that has just created the book; the caller must check
    /// the book is new.
    pub async fn set_book_created_at(&self, book_id: i64, created_at: i64) -> Result<()> {
        sqlx::query("UPDATE books SET created_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(book_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Records a cover file and its measurements in one statement, by straight assignment; the only
    /// writer of `cover_width`, `cover_height`, `cover_accent` and `cover_thumb_path`.
    /// `source` claims `cover_path` only; the measurements have no origin to name.
    pub async fn set_cover(
        &self,
        book_id: i64,
        cover: &crate::images::CoverFile,
        source: Option<Source>,
    ) -> Result<()> {
        let metrics = cover.metrics;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE books SET cover_path = ?1, cover_thumb_path = ?2, cover_width = ?3,
                    cover_height = ?4, cover_accent = ?5, last_modified = ?6
             WHERE id = ?7",
        )
        .bind(cover.path.display().to_string())
        .bind(cover.thumb_path.as_ref().map(|p| p.display().to_string()))
        .bind(metrics.map(|m| m.width))
        .bind(metrics.map(|m| m.height))
        .bind(metrics.map(|m| m.accent))
        .bind(now_unix())
        .bind(book_id)
        .execute(&mut *tx)
        .await?;
        if let Some(source) = source {
            field_provenance::stamp(&mut tx, book_id, &["cover_path"], source).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn cover_paths(&self) -> Result<Vec<(i64, Option<String>, Option<String>)>> {
        let rows = sqlx::query(
            "SELECT id, cover_path, cover_thumb_path FROM books
             WHERE cover_path IS NOT NULL OR cover_thumb_path IS NOT NULL",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("id")?,
                    r.try_get("cover_path")?,
                    r.try_get("cover_thumb_path")?,
                ))
            })
            .collect()
    }

    /// Re-points cover paths only; `last_modified`, provenance and measurements are untouched.
    pub async fn reroot_cover(
        &self,
        book_id: i64,
        cover_path: Option<&str>,
        cover_thumb_path: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE books SET cover_path = ?1, cover_thumb_path = ?2 WHERE id = ?3")
            .bind(cover_path)
            .bind(cover_thumb_path)
            .bind(book_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Books with a cover on disk and unmeasured; `cover_width IS NULL` means not measured, never no
    /// cover.
    pub async fn unmeasured_covers(&self) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query(
            "SELECT id, cover_path FROM books
              WHERE cover_path IS NOT NULL AND cover_width IS NULL
              ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("cover_path")?)))
            .collect()
    }

    /// Books whose filing keys were never computed (`sort_author IS NULL`); `sort_title` rides along,
    /// as one
    /// statement writes both.
    pub async fn stale_sort_keys(&self) -> Result<Vec<i64>> {
        Ok(
            sqlx::query_scalar("SELECT id FROM books WHERE sort_author IS NULL ORDER BY id")
                .fetch_all(self.pool())
                .await?,
        )
    }

    /// Recomputes both filing keys for one book; the only writer of `sort_title` and `sort_author`.
    pub async fn refresh_sort_keys_of(&self, book_id: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        refresh_sort_keys(&mut tx, book_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Recomputes filing keys for books that lack them; returns rows written. Idempotent.
    pub async fn rebuild_sort_keys(&self) -> Result<usize> {
        let stale = self.stale_sort_keys().await?;
        for id in &stale {
            self.refresh_sort_keys_of(*id).await?;
        }
        Ok(stale.len())
    }

    /// Deletes a book; returns its cover path only when no other book uses it, since covers are shared
    /// by content hash.
    pub async fn delete_book(&self, id: i64) -> Result<Option<String>> {
        let mut tx = self.pool().begin().await?;
        // The cascade removes list entries but cannot close the gap: note the lists first, renumber after.
        let on_lists = super::lists::lists_of_book(&mut tx, id).await?;
        let cover: Option<Option<String>> =
            sqlx::query_scalar("DELETE FROM books WHERE id = ? RETURNING cover_path")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(cover) = cover else {
            return Err(EngineError::NotFound(format!("book id {id}")));
        };
        super::lists::compact_lists(&mut tx, &on_lists).await?;
        let mut orphaned = None;
        if let Some(path) = cover {
            let others: i64 = sqlx::query_scalar("SELECT count(*) FROM books WHERE cover_path = ?")
                .bind(&path)
                .fetch_one(&mut *tx)
                .await?;
            if others == 0 {
                orphaned = Some(path);
            }
        }
        tx.commit().await?;
        Ok(orphaned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Book {
        Book {
            title: Some("Middlemarch".into()),
            authors: vec!["George Eliot".into()],
            isbn_13: Some("9781900000178".into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn upsert_merges_without_clobbering() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut b = sample();
        b.description = Some("A sweeping saga.".into());
        let id1 = s.upsert_book(&b, None).await.unwrap();

        let mut b2 = sample();
        b2.page_count = Some(490);
        let id2 = s.upsert_book(&b2, None).await.unwrap();
        assert_eq!(id1, id2);

        let got = s.get_book(id1).await.unwrap().unwrap();
        assert_eq!(got.description.as_deref(), Some("A sweeping saga."));
        assert_eq!(got.page_count, Some(490));
        assert_eq!(got.authors, vec!["George Eliot".to_string()]);
    }

    #[tokio::test]
    async fn enrich_merges_a_partial_record_exactly_as_the_upsert_does() {
        let mut full = sample();
        full.description = Some("A sweeping saga.".into());
        full.publisher = Some("Blackwood".into());
        full.page_count = Some(490);

        let partial = Book {
            title: None,
            isbn_13: full.isbn_13.clone(),
            publish_year: Some(2017),
            language: Some("en".into()),
            ..Default::default()
        };

        let (by_upsert, prov_upsert) = {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s
                .upsert_book(&full, Some(Source::GoogleBooks))
                .await
                .unwrap();
            assert_eq!(
                s.upsert_book(&partial, Some(Source::OpenLibrary))
                    .await
                    .unwrap(),
                id
            );
            (
                s.get_book(id).await.unwrap().unwrap(),
                sources(&s, id).await,
            )
        };
        let (by_enrich, prov_enrich) = {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s
                .upsert_book(&full, Some(Source::GoogleBooks))
                .await
                .unwrap();
            s.enrich_book(id, &partial, Some(Source::OpenLibrary))
                .await
                .unwrap();
            (
                s.get_book(id).await.unwrap().unwrap(),
                sources(&s, id).await,
            )
        };

        for (name, a, b) in [
            ("title", by_upsert.title.clone(), by_enrich.title.clone()),
            (
                "publisher",
                by_upsert.publisher.clone(),
                by_enrich.publisher.clone(),
            ),
            (
                "description",
                by_upsert.description.clone(),
                by_enrich.description.clone(),
            ),
            (
                "language",
                by_upsert.language.clone(),
                by_enrich.language.clone(),
            ),
        ] {
            assert_eq!(a, b, "{name} merged differently");
        }
        assert_eq!(by_upsert.authors, by_enrich.authors);
        assert_eq!(by_upsert.page_count, by_enrich.page_count);
        assert_eq!(by_upsert.publish_year, by_enrich.publish_year);
        // Guards against both blanking everything, which would pass a same-vs-same assertion.
        assert_eq!(by_enrich.title.as_deref(), Some("Middlemarch"));
        assert_eq!(by_enrich.authors, vec!["George Eliot".to_string()]);
        assert_eq!(by_enrich.page_count, Some(490));
        assert_eq!(by_enrich.publish_year, Some(2017));

        assert_eq!(prov_upsert, prov_enrich, "provenance stamped differently");
        assert!(
            prov_enrich.contains(&("publish_year".into(), "openlibrary".into())),
            "the field the partial record supplied is claimed by it: {prov_enrich:?}"
        );
        assert!(
            prov_enrich.contains(&("description".into(), "googlebooks".into())),
            "a field the partial record is silent about keeps its claim: {prov_enrich:?}"
        );
    }

    #[tokio::test]
    async fn enrich_updates_an_isbn_less_book_where_upsert_would_duplicate_it() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Untitled Draft".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        s.enrich_book(
            id,
            &Book {
                publisher: Some("Self".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            s.list_books(&BookQuery::new(10, BookSort::Title))
                .await
                .unwrap()
                .len(),
            1
        );
        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.publisher.as_deref(), Some("Self"));
        assert_eq!(got.title.as_deref(), Some("Untitled Draft"));

        s.upsert_book(
            &Book {
                id: Some(id),
                publisher: Some("Self".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            s.list_books(&BookQuery::new(10, BookSort::Title))
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn upsert_branches_on_isbn10() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let b = Book {
            title: Some("X".into()),
            isbn_10: Some("0306406152".into()),
            ..Default::default()
        };
        let id1 = s.upsert_book(&b, None).await.unwrap();
        let id2 = s.upsert_book(&b, None).await.unwrap();
        assert_eq!(id1, id2);
        let found = s.find_book_by_isbn("0306406152").await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn progress_and_finish_flow() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        let b = s.update_progress(id, Some(100), None).await.unwrap();
        assert_eq!(b.current_page, Some(100));
        assert!(!b.finished);
        assert!(b.date_started.is_some());

        let b = s.update_progress(id, None, Some(true)).await.unwrap();
        assert!(b.finished);
        assert!(b.date_finished.is_some());
        assert_eq!(b.current_page, Some(100));
    }

    #[tokio::test]
    async fn a_provider_upsert_never_touches_reading_state() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        s.update_progress(id, Some(100), Some(true)).await.unwrap();
        let before = s.list_readings(id).await.unwrap();

        let refreshed = Book {
            page_count: Some(490),
            current_page: Some(1),
            finished: false,
            date_started: Some(1),
            date_finished: Some(2),
            ..sample()
        };
        assert_eq!(s.upsert_book(&refreshed, None).await.unwrap(), id);

        let b = s.get_book(id).await.unwrap().unwrap();
        assert!(b.finished, "a provider upsert must not un-finish a book");
        assert_eq!(b.current_page, Some(100));
        assert_eq!(b.page_count, Some(490), "metadata still lands");
        assert_eq!(s.list_readings(id).await.unwrap(), before);

        let fresh = s
            .upsert_book(
                &Book {
                    title: Some("Never opened".into()),
                    current_page: Some(42),
                    finished: true,
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        assert!(s.list_readings(fresh).await.unwrap().is_empty());
        let b = s.get_book(fresh).await.unwrap().unwrap();
        assert_eq!(b.current_page, None);
        assert!(!b.finished);
    }

    use crate::storage::{LinkedBy, NewHighlight, Reading};

    fn hl(text: &str, datetime: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[1]/text().{}", text.len())),
            pos1: None,
            ko_datetime: Some(datetime.into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    async fn book(s: &Storage, title: &str) -> i64 {
        s.upsert_book(
            &Book {
                title: Some(title.into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap()
    }

    async fn count(s: &Storage, sql: &str, id: i64) -> i64 {
        sqlx::query_scalar(sql)
            .bind(id)
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn merge_moves_everything_and_drops_the_identity_collision() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = book(&s, "Middlemarch (dupe)").await;
        let dst = book(&s, "Middlemarch").await;

        // Same annotation on both sides: it differs only because `book_id` feeds the hash.
        let shared = hl(
            "We are all of us born in moral stupidity.",
            "2026-01-05 21:14:08",
        );
        let only_on_src = hl("middlemarch", "2026-01-06 08:02:11");
        s.insert_highlight(dst, &shared).await.unwrap();
        let src_shared = s.insert_highlight(src, &shared).await.unwrap().unwrap();
        s.insert_highlight(src, &only_on_src).await.unwrap();

        s.insert_flashcard(src, Some(src_shared), "middlemarch", None)
            .await
            .unwrap();
        s.link_device_book("abc", src, LinkedBy::Auto)
            .await
            .unwrap();

        let r = s.merge_books(src, dst).await.unwrap();
        assert!(r.src_existed);
        assert_eq!(r.highlights_moved, 1);
        assert_eq!(r.highlights_dropped, 1, "the collision must not duplicate");
        assert_eq!(r.flashcards_moved, 1);
        assert_eq!(r.device_links_moved, 1);

        assert!(s.get_book(src).await.unwrap().is_none(), "src is gone");
        let texts: Vec<String> = s
            .list_highlights(dst)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.text)
            .collect();
        assert_eq!(texts.len(), 2, "two distinct annotations, got {texts:?}");
        assert_eq!(
            s.find_book_by_partial_md5("abc").await.unwrap().unwrap().id,
            Some(dst)
        );
        assert_eq!(
            count(&s, "SELECT count(*) FROM flashcards WHERE book_id = ?", dst).await,
            1
        );
    }

    #[tokio::test]
    async fn a_dropped_duplicate_hands_its_dependents_to_the_survivor() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = book(&s, "Middlemarch (dupe)").await;
        let dst = book(&s, "Middlemarch").await;

        let shared = hl("a shared passage", "2026-01-05 21:14:08");
        let keep = s.insert_highlight(dst, &shared).await.unwrap().unwrap();
        let doomed = s.insert_highlight(src, &shared).await.unwrap().unwrap();
        s.insert_flashcard(src, Some(doomed), "shared", None)
            .await
            .unwrap();

        s.merge_books(src, dst).await.unwrap();

        let anchored: Option<i64> =
            sqlx::query_scalar("SELECT highlight_id FROM flashcards WHERE word = ?")
                .bind("shared")
                .fetch_one(s.pool())
                .await
                .unwrap();
        assert_eq!(
            anchored,
            Some(keep),
            "the flashcard must follow the survivor"
        );
    }

    #[tokio::test]
    async fn a_word_both_books_captured_is_dropped_not_a_constraint_error() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = book(&s, "Middlemarch (dupe)").await;
        let dst = book(&s, "Middlemarch").await;
        s.insert_flashcard(src, None, "middlemarch", None)
            .await
            .unwrap();
        s.insert_flashcard(dst, None, "middlemarch", None)
            .await
            .unwrap();
        s.insert_flashcard(src, None, "ardent", None).await.unwrap();

        let r = s.merge_books(src, dst).await.unwrap();
        assert_eq!(r.flashcards_dropped, 1);
        assert_eq!(r.flashcards_moved, 1);
        assert_eq!(
            count(&s, "SELECT count(*) FROM flashcards WHERE book_id = ?", dst).await,
            2
        );
    }

    #[tokio::test]
    async fn dst_wins_and_src_fills_the_gaps() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    isbn_13: Some("9781900000178".into()),
                    description: Some("from the duplicate".into()),
                    page_count: Some(490),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        s.update_progress(src, None, Some(true)).await.unwrap();
        let dst = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch (the keeper)".into()),
                    description: Some("the one to keep".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        s.merge_books(src, dst).await.unwrap();

        let got = s.get_book(dst).await.unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("Middlemarch (the keeper)"));
        assert_eq!(got.description.as_deref(), Some("the one to keep"));
        assert_eq!(got.isbn_13.as_deref(), Some("9781900000178"), "gap filled");
        assert_eq!(got.page_count, Some(490));
        assert!(got.finished, "src's reading survives the fold");
    }

    #[tokio::test]
    async fn a_merge_moves_a_claim_only_where_it_moved_the_value() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch (dupe)".into()),
                    description: Some("from the duplicate".into()),
                    page_count: Some(490),
                    publisher: Some("from the duplicate".into()),
                    ..Default::default()
                },
                Some(Source::Goodreads),
            )
            .await
            .unwrap();
        let dst = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    description: Some("the one to keep".into()),
                    ..Default::default()
                },
                Some(Source::Calibre),
            )
            .await
            .unwrap();
        sqlx::query("UPDATE books SET publisher = 'unattributed history' WHERE id = ?")
            .bind(dst)
            .execute(s.pool())
            .await
            .unwrap();

        s.merge_books(src, dst).await.unwrap();

        let got = s.get_book(dst).await.unwrap().unwrap();
        assert_eq!(got.description.as_deref(), Some("the one to keep"));
        assert_eq!(got.page_count, Some(490), "the gap is filled");
        assert_eq!(got.publisher.as_deref(), Some("unattributed history"));

        assert_eq!(
            s.field_source(dst, "description").await.unwrap().as_deref(),
            Some("calibre"),
            "dst kept its value, so it keeps its claim"
        );
        assert_eq!(
            s.field_source(dst, "page_count").await.unwrap().as_deref(),
            Some("goodreads"),
            "src's value arrived, so src's claim arrived with it"
        );
        assert_eq!(
            s.field_source(dst, "publisher").await.unwrap(),
            None,
            "an unattributed value must not inherit the claim of a discarded one"
        );
    }

    #[tokio::test]
    async fn an_attributed_merge_stamps_each_field_with_its_own_source() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();

        s.enrich_book_attributed(
            id,
            &Book {
                page_count: Some(490),
                description: Some("a sweeping saga".into()),
                publisher: Some("Blackwood".into()),
                ..Default::default()
            },
            &[
                ("page_count", Source::OpenLibrary),
                ("description", Source::GoogleBooks),
                ("language", Source::OpenLibrary),
            ],
        )
        .await
        .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.page_count, Some(490));
        assert_eq!(got.description.as_deref(), Some("a sweeping saga"));
        assert_eq!(got.publisher.as_deref(), Some("Blackwood"));

        assert_eq!(
            s.field_source(id, "page_count").await.unwrap().as_deref(),
            Some("openlibrary")
        );
        assert_eq!(
            s.field_source(id, "description").await.unwrap().as_deref(),
            Some("googlebooks")
        );
        assert_eq!(
            s.field_source(id, "language").await.unwrap(),
            None,
            "a claim on a field the record is silent about must not be recorded"
        );
        assert_eq!(
            s.field_source(id, "publisher").await.unwrap(),
            None,
            "a field written with no claim stays unattributed"
        );
    }

    #[tokio::test]
    async fn an_attributed_merge_still_cannot_take_a_user_field() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        s.enrich_book(
            id,
            &Book {
                page_count: Some(496),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        s.enrich_book_attributed(
            id,
            &Book {
                page_count: Some(490),
                description: Some("a sweeping saga".into()),
                ..Default::default()
            },
            &[
                ("page_count", Source::OpenLibrary),
                ("description", Source::OpenLibrary),
            ],
        )
        .await
        .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.page_count, Some(496));
        assert_eq!(got.description.as_deref(), Some("a sweeping saga"));
        assert_eq!(
            s.field_source(id, "page_count").await.unwrap().as_deref(),
            Some("user")
        );
    }

    #[tokio::test]
    async fn merging_moves_readings_and_leaves_exactly_one_open() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = book(&s, "Middlemarch (dupe)").await;
        let dst = book(&s, "Middlemarch").await;

        s.open_reading(src, Some(1_000), "manual").await.unwrap();
        s.finish_reading(src).await.unwrap();
        s.open_reading(src, Some(2_000), "manual").await.unwrap();
        s.open_reading(dst, Some(3_000), "manual").await.unwrap();

        let r = s.merge_books(src, dst).await.unwrap();
        assert_eq!(r.readings_moved, 2);

        let readings = s.list_readings(dst).await.unwrap();
        assert_eq!(readings.len(), 3, "nothing is deleted");
        let open: Vec<&Reading> = readings
            .iter()
            .filter(|r| r.finished_at.is_none())
            .collect();
        assert_eq!(open.len(), 1, "the index invariant survives the merge");
        assert_eq!(open[0].started_at, Some(3_000), "the newer one stays open");

        let closed = readings
            .iter()
            .find(|r| r.started_at == Some(2_000))
            .expect("src's open reading moved");
        assert_eq!(closed.status, crate::storage::STATUS_ABANDONED);
        assert!(closed.finished_at.is_some());
    }

    #[tokio::test]
    async fn merging_one_open_reading_leaves_it_open() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = book(&s, "Middlemarch (dupe)").await;
        let dst = book(&s, "Middlemarch").await;
        s.open_reading(src, Some(2_000), "manual").await.unwrap();

        s.merge_books(src, dst).await.unwrap();
        let readings = s.list_readings(dst).await.unwrap();
        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].finished_at, None);
        assert_eq!(readings[0].status, crate::storage::STATUS_READING);
    }

    #[tokio::test]
    async fn a_cover_is_adopted_or_reported_as_orphaned() {
        for (dst_cover, expect_orphan) in [(None, None), (Some("dst.jpg"), Some("src.jpg"))] {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let src = s
                .upsert_book(
                    &Book {
                        title: Some("a".into()),
                        cover_path: Some("src.jpg".into()),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();
            let dst = s
                .upsert_book(
                    &Book {
                        title: Some("b".into()),
                        cover_path: dst_cover.map(str::to_string),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap();

            let r = s.merge_books(src, dst).await.unwrap();
            assert_eq!(r.orphaned_cover.as_deref(), expect_orphan);
            assert_eq!(
                s.get_book(dst)
                    .await
                    .unwrap()
                    .unwrap()
                    .cover_path
                    .as_deref(),
                Some(dst_cover.unwrap_or("src.jpg"))
            );
        }
    }

    #[tokio::test]
    async fn the_degenerate_cases_are_distinguished() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = book(&s, "Middlemarch").await;

        assert!(matches!(
            s.merge_books(id, 9999).await,
            Err(EngineError::NotFound(_))
        ));

        let gone = s.merge_books(9999, id).await.unwrap();
        assert!(!gone.src_existed);

        let itself = s.merge_books(id, id).await.unwrap();
        assert!(itself.src_existed);
        assert!(
            s.get_book(id).await.unwrap().is_some(),
            "must not self-delete"
        );
    }

    fn cover_at(path: &str) -> crate::images::CoverFile {
        crate::images::CoverFile {
            path: path.into(),
            thumb_path: Some(crate::images::thumb_path_of(std::path::Path::new(path))),
            metrics: Some(crate::images::CoverMetrics {
                width: 600,
                height: 900,
                accent: 0x00a0_2020,
            }),
        }
    }

    #[test]
    fn the_cover_metrics_sit_outside_the_merge_table_and_the_path_does_not() {
        let ruled: Vec<&str> = MERGE_RULES.iter().map(|r| r.col).collect();
        for col in COVER_METRICS {
            assert!(
                !ruled.contains(&col),
                "{col} became a MERGE_RULES column — read migration 0014 before \
                 deleting this assertion, including why Rule::pair is not the fix"
            );
        }
        assert!(
            ruled.contains(&"cover_path"),
            "cover_path left the merge table, and invalidate_cover_metrics is \
             generated from its value expression"
        );
        for winner in [Winner::Incoming, Winner::Stored] {
            for guard in [false, true] {
                let sql = merge_set(winner, guard, |i, _| format!("?{}", i + 1));
                for col in COVER_METRICS {
                    assert!(sql.contains(&format!("{col} = CASE WHEN")), "{col}: {sql}");
                }
            }
        }
    }

    #[tokio::test]
    async fn a_provider_write_never_touches_the_cover_metrics() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        s.set_cover(id, &cover_at("/img/aaa.jpg"), None)
            .await
            .unwrap();

        // `cover_path` is cleared on purpose: a record that moves it invalidates the metrics, tested
        // separately.
        let loud = Book {
            cover_path: None,
            ..says_everything(9)
        };
        s.enrich_book(id, &loud, Some(Source::GoogleBooks))
            .await
            .unwrap();
        s.fill_book(id, &loud, Some(Source::OpenLibrary))
            .await
            .unwrap();
        s.enrich_book(
            id,
            &Book {
                cover_width: Some(1),
                cover_height: Some(1),
                cover_accent: Some(1),
                cover_thumb_path: Some("/img/lie.jpg".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.cover_thumb_path.as_deref(), Some("/img/aaa.thumb.jpg"));
        assert_eq!((got.cover_width, got.cover_height), (Some(600), Some(900)));
        assert_eq!(got.cover_accent, Some(0x00a0_2020));
        assert_eq!(got.cover_path.as_deref(), Some("/img/aaa.jpg"));
    }

    #[tokio::test]
    async fn moving_the_cover_path_throws_away_what_was_measured_from_the_old_one() {
        for user in [false, true] {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s.upsert_book(&sample(), None).await.unwrap();
            s.set_cover(id, &cover_at("/img/aaa.jpg"), None)
                .await
                .unwrap();

            let source = user.then_some(Source::User).or(Some(Source::Calibre));
            s.enrich_book(
                id,
                &Book {
                    cover_path: Some("/img/bbb.jpg".into()),
                    ..Default::default()
                },
                source,
            )
            .await
            .unwrap();

            let got = s.get_book(id).await.unwrap().unwrap();
            assert_eq!(got.cover_path.as_deref(), Some("/img/bbb.jpg"));
            assert_eq!(
                (
                    got.cover_width,
                    got.cover_height,
                    got.cover_accent,
                    got.cover_thumb_path
                ),
                (None, None, None, None),
                "user={user}: the row kept the old jacket's dimensions"
            );
            s.set_cover(id, &cover_at("/img/bbb.jpg"), None)
                .await
                .unwrap();
            s.enrich_book(
                id,
                &Book {
                    publisher: Some("Blackwood".into()),
                    ..Default::default()
                },
                source,
            )
            .await
            .unwrap();
            assert_eq!(
                s.get_book(id).await.unwrap().unwrap().cover_width,
                Some(600),
                "user={user}: a write that did not touch the cover cleared it"
            );
        }
    }

    #[tokio::test]
    async fn refetching_a_cover_repoints_the_row() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        s.set_cover(id, &cover_at("/img/old.jpg"), None)
            .await
            .unwrap();
        s.set_cover(id, &cover_at("/img/new.jpg"), Some(Source::OpenLibrary))
            .await
            .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.cover_path.as_deref(), Some("/img/new.jpg"));
        assert_eq!(got.cover_thumb_path.as_deref(), Some("/img/new.thumb.jpg"));
        assert_eq!(
            s.field_source(id, "cover_path").await.unwrap().as_deref(),
            Some("openlibrary")
        );
    }

    #[tokio::test]
    async fn a_shared_cover_survives_deleting_one_of_its_books() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mk = |t: &str| Book {
            title: Some(t.into()),
            ..Default::default()
        };
        let a = s.upsert_book(&mk("a"), None).await.unwrap();
        let b = s.upsert_book(&mk("b"), None).await.unwrap();
        let shared = cover_at("/img/shared.jpg");
        s.set_cover(a, &shared, None).await.unwrap();
        s.set_cover(b, &shared, None).await.unwrap();

        assert_eq!(
            s.delete_book(a).await.unwrap(),
            None,
            "the surviving book is still using this file"
        );
        assert_eq!(
            s.delete_book(b).await.unwrap(),
            Some("/img/shared.jpg".to_string()),
            "the last holder is gone; now it is an orphan"
        );
    }

    #[tokio::test]
    async fn a_cover_and_its_measurements_change_hands_together() {
        let mk = |t: &str| Book {
            title: Some(t.into()),
            ..Default::default()
        };

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = s.upsert_book(&mk("src"), None).await.unwrap();
        let dst = s.upsert_book(&mk("dst"), None).await.unwrap();
        s.set_cover(src, &cover_at("/img/src.jpg"), None)
            .await
            .unwrap();
        let report = s.merge_books(src, dst).await.unwrap();
        let got = s.get_book(dst).await.unwrap().unwrap();
        assert_eq!(got.cover_path.as_deref(), Some("/img/src.jpg"));
        assert_eq!(got.cover_width, Some(600));
        assert_eq!(got.cover_thumb_path.as_deref(), Some("/img/src.thumb.jpg"));
        assert_eq!(report.orphaned_cover, None, "the file is in use by dst");

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = s.upsert_book(&mk("src"), None).await.unwrap();
        let dst = s.upsert_book(&mk("dst"), None).await.unwrap();
        s.set_cover(src, &cover_at("/img/src.jpg"), None)
            .await
            .unwrap();
        s.enrich_book(
            dst,
            &Book {
                cover_path: Some("/img/dst.jpg".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        let report = s.merge_books(src, dst).await.unwrap();
        let got = s.get_book(dst).await.unwrap().unwrap();
        assert_eq!(got.cover_path.as_deref(), Some("/img/dst.jpg"));
        assert_eq!(
            (got.cover_width, got.cover_height, got.cover_accent),
            (None, None, None),
            "dst kept its own jacket and took src's dimensions for it"
        );
        assert_eq!(got.cover_thumb_path, None);
        assert_eq!(report.orphaned_cover.as_deref(), Some("/img/src.jpg"));

        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let src = s.upsert_book(&mk("src"), None).await.unwrap();
        let dst = s.upsert_book(&mk("dst"), None).await.unwrap();
        let shared = cover_at("/img/same.jpg");
        s.set_cover(src, &shared, None).await.unwrap();
        s.set_cover(dst, &shared, None).await.unwrap();
        let report = s.merge_books(src, dst).await.unwrap();
        assert_eq!(
            report.orphaned_cover, None,
            "unlinking this would take the surviving book's cover with it"
        );
        assert_eq!(
            s.get_book(dst).await.unwrap().unwrap().cover_width,
            Some(600)
        );
    }

    #[tokio::test]
    async fn the_backfill_list_is_covers_on_disk_without_measurements() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mk = |t: &str| Book {
            title: Some(t.into()),
            ..Default::default()
        };
        let none = s.upsert_book(&mk("no cover at all"), None).await.unwrap();
        let old = s.upsert_book(&mk("pre-0014"), None).await.unwrap();
        let fresh = s.upsert_book(&mk("measured"), None).await.unwrap();
        s.enrich_book(
            old,
            &Book {
                cover_path: Some("/img/old.jpg".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
        s.set_cover(fresh, &cover_at("/img/fresh.jpg"), None)
            .await
            .unwrap();

        assert_eq!(
            s.unmeasured_covers().await.unwrap(),
            vec![(old, "/img/old.jpg".to_string())]
        );
        assert!(
            !s.unmeasured_covers()
                .await
                .unwrap()
                .iter()
                .any(|(id, _)| { *id == none || *id == fresh })
        );
    }

    use crate::book::ReadingState;
    use crate::storage::query::StatusFilter;

    /// Five books with colliding sort keys on purpose: two share a title (case-insensitive), two a
    /// year,
    /// all written in the same second.
    async fn shelf(s: &Storage) -> Vec<i64> {
        let mut ids = Vec::new();
        for (title, author, year, lang) in [
            ("Dune", "Frank Herbert", Some(1965), "en"),
            ("dune", "Frank Herbert", Some(1965), "en"),
            ("Kindred", "Octavia E. Butler", Some(1979), "en"),
            ("Solaris", "Stanisław Lem", Some(1961), "pl"),
            ("Ubik", "Philip K. Dick", None, "en"),
        ] {
            ids.push(
                s.upsert_book(
                    &Book {
                        title: Some(title.into()),
                        authors: vec![author.into()],
                        publish_year: year,
                        language: Some(lang.into()),
                        page_count: Some(300),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap(),
            );
        }
        ids
    }

    fn all(sort: BookSort) -> BookQuery {
        BookQuery {
            sort,
            ..Default::default()
        }
    }

    const EVERY_SORT: [BookSort; 5] = [
        BookSort::LastModified,
        BookSort::Title,
        BookSort::Progress,
        BookSort::Year,
        BookSort::Author,
    ];

    #[tokio::test]
    async fn a_page_and_its_successor_partition_the_list() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let ids = shelf(&s).await;
        for sort in EVERY_SORT {
            let whole: Vec<Option<i64>> = s
                .list_books(&all(sort))
                .await
                .unwrap()
                .iter()
                .map(|b| b.id)
                .collect();
            assert_eq!(whole.len(), ids.len(), "{sort:?}");
            for page in 1..=ids.len() as i64 + 1 {
                let mut walked = Vec::new();
                let mut offset = 0;
                loop {
                    let got = s
                        .list_books(&BookQuery::new(page, sort).at_offset(offset))
                        .await
                        .unwrap();
                    if got.is_empty() {
                        break;
                    }
                    assert!(got.len() as i64 <= page, "{sort:?} overran its limit");
                    walked.extend(got.iter().map(|b| b.id));
                    offset += page;
                }
                assert_eq!(walked, whole, "{sort:?} at page size {page}");
            }
        }
    }

    /// Reads the keys via SQL: `sort_author` is deliberately not a field on `Book`.
    async fn keys(s: &Storage, id: i64) -> (Option<String>, Option<String>) {
        let row = sqlx::query("SELECT sort_title, sort_author FROM books WHERE id = ?")
            .bind(id)
            .fetch_one(s.pool())
            .await
            .unwrap();
        (
            row.try_get("sort_title").unwrap(),
            row.try_get("sort_author").unwrap(),
        )
    }

    fn titled(title: &str, authors: &[&str]) -> Book {
        Book {
            title: Some(title.to_string()),
            authors: authors.iter().map(|a| a.to_string()).collect(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_written_book_files_under_both_keys_immediately() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(&titled("The Overstory", &["Richard Powers"]), None)
            .await
            .unwrap();
        let (title, author) = keys(&s, id).await;
        assert_eq!(
            title.as_deref(),
            Some("Overstory"),
            "the article is dropped"
        );
        assert_eq!(
            author,
            Some(crate::sort::author_key(&["Richard Powers".into()]))
        );
    }

    #[tokio::test]
    async fn every_writer_that_moves_a_title_moves_the_key() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut seed = titled("A Wizard of Earthsea", &["Ursula K. Le Guin"]);
        seed.isbn_13 = Some("9780553383041".into());
        let id = s.upsert_book(&seed, None).await.unwrap();
        assert_eq!(keys(&s, id).await.0.as_deref(), Some("Wizard of Earthsea"));
        let mut again = seed.clone();
        again.title = Some("The Tombs of Atuan".into());
        assert_eq!(s.upsert_book(&again, None).await.unwrap(), id);
        assert_eq!(keys(&s, id).await.0.as_deref(), Some("Tombs of Atuan"));

        s.enrich_book(id, &titled("An Ordinary Title", &[]), None)
            .await
            .unwrap();
        assert_eq!(keys(&s, id).await.0.as_deref(), Some("Ordinary Title"));

        let before = keys(&s, id).await;
        s.fill_book(id, &titled("Something Else", &["Nobody At All"]), None)
            .await
            .unwrap();
        assert_eq!(
            keys(&s, id).await,
            before,
            "a fill that filled nothing must not move a key"
        );

        let src = s
            .upsert_book(
                &titled("The Left Hand of Darkness", &["Ursula K. Le Guin"]),
                None,
            )
            .await
            .unwrap();
        let dst = s.upsert_book(&Book::default(), None).await.unwrap();
        s.merge_books(src, dst).await.unwrap();
        assert_eq!(
            keys(&s, dst).await.0.as_deref(),
            Some("Left Hand of Darkness"),
            "dst took src's title and must take its filing name with it"
        );
    }

    #[tokio::test]
    async fn an_authorless_book_has_a_key_and_files_last() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let lonely = s.upsert_book(&titled("Beowulf", &[]), None).await.unwrap();
        let known = s
            .upsert_book(&titled("Zuleika Dobson", &["Max Beerbohm"]), None)
            .await
            .unwrap();
        let (_, lonely_key) = keys(&s, lonely).await;
        assert!(lonely_key.is_some(), "absence must not be spelled as NULL");
        assert!(lonely_key > keys(&s, known).await.1);

        assert!(
            s.stale_sort_keys().await.unwrap().is_empty(),
            "a book written through the engine is never in the back-fill's list"
        );

        let order: Vec<Option<i64>> = s
            .list_books(&BookQuery::new(-1, BookSort::Author))
            .await
            .unwrap()
            .iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(order, vec![Some(known), Some(lonely)]);
    }

    #[tokio::test]
    async fn the_back_fill_files_a_row_no_writer_touched_and_then_does_nothing() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO books (title, authors, created_at, last_modified)
             VALUES ('The Dispossessed', '[\"Ursula K. Le Guin\"]', 0, 0) RETURNING id",
        )
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(keys(&s, id).await, (None, None));
        assert_eq!(s.stale_sort_keys().await.unwrap(), vec![id]);

        assert_eq!(s.rebuild_sort_keys().await.unwrap(), 1);
        let filed = keys(&s, id).await;
        assert_eq!(filed.0.as_deref(), Some("Dispossessed"));

        assert_eq!(s.rebuild_sort_keys().await.unwrap(), 0);
        assert_eq!(keys(&s, id).await, filed);
        let fresh = s
            .upsert_book(&titled("The Dispossessed", &["Ursula K. Le Guin"]), None)
            .await
            .unwrap();
        assert_eq!(keys(&s, fresh).await, filed);
    }

    #[tokio::test]
    async fn a_row_the_writer_never_reached_still_files_under_its_title() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for title in ["Middlemarch", "Zuleika Dobson"] {
            sqlx::query("INSERT INTO books (title, created_at, last_modified) VALUES (?, 0, 0)")
                .bind(title)
                .execute(s.pool())
                .await
                .unwrap();
        }
        let titles: Vec<String> = s
            .list_books(&BookQuery::new(-1, BookSort::Title))
            .await
            .unwrap()
            .iter()
            .map(|b| b.display_title().to_string())
            .collect();
        assert_eq!(titles, vec!["Middlemarch", "Zuleika Dobson"]);
    }

    /// The outer query's own plan lines (`parent` 0): `BOOK_FROM`'s correlated subquery has its own
    /// temp b-tree
    /// by design, which would pollute a whole-tree assertion.
    async fn outer_plan(s: &Storage, sql: &str) -> Vec<String> {
        sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
            .fetch_all(s.pool())
            .await
            .expect("query plan")
            .iter()
            .filter(|r| r.try_get::<i64, _>("parent").unwrap_or(-1) == 0)
            .map(|r| r.try_get::<String, _>("detail").unwrap_or_default())
            .collect()
    }

    fn list_sql(sort: BookSort) -> String {
        format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM}  ORDER BY {} LIMIT 20 OFFSET 40",
            super::order_by(sort)
        )
    }

    #[tokio::test]
    async fn the_sort_key_indexes_are_the_plan_the_planner_picks() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for (sort, index) in [
            (BookSort::LastModified, "idx_books_last_modified"),
            (BookSort::Title, "idx_books_sort_title"),
            (BookSort::Year, "idx_books_publish_year"),
            (BookSort::Author, "idx_books_sort_author"),
        ] {
            let plan = outer_plan(&s, &list_sql(sort)).await.join("; ");
            assert!(
                plan.contains(index),
                "{sort:?} must scan {index}, got: {plan}"
            );
            assert!(
                !plan.contains("USE TEMP B-TREE FOR ORDER BY"),
                "{sort:?} reached {index} and sorted anyway, which buys nothing: {plan}"
            );
        }
    }

    #[tokio::test]
    async fn the_one_sort_that_cannot_be_indexed_still_says_so() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let plan = outer_plan(&s, &list_sql(BookSort::Progress))
            .await
            .join("; ");
        assert!(
            plan.contains("USE TEMP B-TREE FOR ORDER BY"),
            "a computed ratio has no index to reach; if this passes, the plan \
             filter above is wrong rather than the sort being fast: {plan}"
        );
    }

    #[test]
    fn order_by_is_a_total_order() {
        for sort in EVERY_SORT {
            let clause = super::order_by(sort);
            assert!(
                clause.trim_end().ends_with("books.id ASC")
                    || clause.trim_end().ends_with("books.id DESC"),
                "{sort:?} orders by `{clause}`, which ties — and a tie under \
                 LIMIT/OFFSET is a book on two pages and another on none"
            );
        }
    }

    #[tokio::test]
    async fn the_count_agrees_with_the_page_for_every_filter() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let ids = shelf(&s).await;
        s.update_progress(ids[0], Some(100), None).await.unwrap();
        s.update_progress(ids[1], None, Some(true)).await.unwrap();
        s.add_book_tags(ids[2], "goodreads", &[("sf".to_string(), "SF".to_string())])
            .await
            .unwrap();

        for filter in [
            BookFilter::default(),
            BookFilter {
                status: Some(StatusFilter::NoReading),
                ..Default::default()
            },
            BookFilter {
                status: Some(StatusFilter::Is(ReadingState::Reading)),
                ..Default::default()
            },
            BookFilter {
                status: Some(StatusFilter::Is(ReadingState::Finished)),
                ..Default::default()
            },
            BookFilter {
                title: Some("dun".into()),
                ..Default::default()
            },
            BookFilter {
                title: Some("nothing here".into()),
                ..Default::default()
            },
            BookFilter {
                author: Some("herbert".into()),
                ..Default::default()
            },
            BookFilter {
                year: Some(1965),
                ..Default::default()
            },
            BookFilter {
                language: Some("PL".into()),
                ..Default::default()
            },
            BookFilter {
                tag: Some("SF".into()),
                ..Default::default()
            },
            BookFilter {
                has_cover: Some(false),
                ..Default::default()
            },
            BookFilter {
                author: Some("herbert".into()),
                year: Some(1965),
                language: Some("en".into()),
                ..Default::default()
            },
            BookFilter {
                title: Some("dun".into()),
                author: Some("herbert".into()),
                year: Some(1965),
                ..Default::default()
            },
        ] {
            let count = s.count_books(&filter).await.unwrap();
            for sort in EVERY_SORT {
                let rows = s
                    .list_books(&all(sort).with_filter(filter.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    count,
                    rows.len() as i64,
                    "{filter:?} counted {count} and listed {} under {sort:?}",
                    rows.len()
                );
            }
        }
    }

    #[tokio::test]
    async fn a_title_filter_pages_and_counts_like_every_other_predicate() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        shelf(&s).await;

        let dunes = BookFilter {
            title: Some("DUNE".into()),
            ..Default::default()
        };
        assert_eq!(s.count_books(&dunes).await.unwrap(), 2);

        let query = BookQuery::new(1, BookSort::Title).with_filter(dunes.clone());
        let first = s.list_books(&query).await.unwrap();
        let second = s.list_books(&query.clone().at_offset(1)).await.unwrap();
        let third = s.list_books(&query.at_offset(2)).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(third.is_empty(), "there is no third page of two books");
        assert_ne!(first[0].id, second[0].id, "the pages partition the answer");

        let none = BookFilter {
            title: Some("thermodynamics".into()),
            ..Default::default()
        };
        assert_eq!(s.count_books(&none).await.unwrap(), 0);
        assert!(
            s.list_books(&BookQuery::default().with_filter(none))
                .await
                .unwrap()
                .is_empty()
        );

        assert_eq!(
            s.count_books(&BookFilter {
                title: Some("%".into()),
                ..Default::default()
            })
            .await
            .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn a_status_filter_has_four_cases_and_one_of_them_is_absence() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let ids = shelf(&s).await;
        s.update_progress(ids[0], Some(100), None).await.unwrap();
        s.update_progress(ids[1], None, Some(true)).await.unwrap();
        s.open_reading(ids[2], None, "local").await.unwrap();
        s.abandon_reading(ids[2]).await.unwrap();

        let of = |st: StatusFilter| BookFilter {
            status: Some(st),
            ..Default::default()
        };
        let mut counted = 0;
        for (case, want) in [
            (StatusFilter::Is(ReadingState::Reading), vec![ids[0]]),
            (StatusFilter::Is(ReadingState::Finished), vec![ids[1]]),
            (StatusFilter::Is(ReadingState::Abandoned), vec![ids[2]]),
            (StatusFilter::NoReading, vec![ids[3], ids[4]]),
        ] {
            let mut got: Vec<i64> = s
                .list_books(&all(BookSort::Title).with_filter(of(case.clone())))
                .await
                .unwrap()
                .iter()
                .filter_map(|b| b.id)
                .collect();
            got.sort();
            let mut want = want;
            want.sort();
            assert_eq!(got, want, "{case:?}");
            counted += s.count_books(&of(case)).await.unwrap();
        }
        assert_eq!(counted, ids.len() as i64);
    }

    #[tokio::test]
    async fn a_limit_selects_along_the_sort_key() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        shelf(&s).await;
        let titles: Vec<String> = s
            .list_books(&BookQuery::new(2, BookSort::Title))
            .await
            .unwrap()
            .iter()
            .filter_map(|b| b.title.clone())
            .collect();
        assert_eq!(titles, vec!["Dune".to_string(), "dune".to_string()]);
        let authors: Vec<String> = s
            .list_books(&BookQuery::new(2, BookSort::Author))
            .await
            .unwrap()
            .iter()
            .map(|b| b.authors[0].clone())
            .collect();
        assert_eq!(
            authors,
            vec![
                "Octavia E. Butler".to_string(),
                "Philip K. Dick".to_string()
            ],
            "Butler and Dick by last name, not whatever the recency order was"
        );
    }

    #[tokio::test]
    async fn the_author_arm_filters_and_pages_like_the_others() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        shelf(&s).await;
        let q = all(BookSort::Author).with_filter(BookFilter {
            language: Some("en".into()),
            ..Default::default()
        });
        let names: Vec<String> = s
            .list_books(&q)
            .await
            .unwrap()
            .iter()
            .map(|b| b.authors[0].clone())
            .collect();
        assert_eq!(
            names,
            vec![
                "Octavia E. Butler".to_string(),
                "Philip K. Dick".to_string(),
                "Frank Herbert".to_string(),
                "Frank Herbert".to_string(),
            ]
        );
        let second = s
            .list_books(&BookQuery::new(1, BookSort::Author).at_offset(1))
            .await
            .unwrap();
        assert_eq!(second[0].authors[0], "Philip K. Dick");
    }

    #[tokio::test]
    async fn a_negative_limit_is_the_whole_library_in_every_arm() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let ids = shelf(&s).await;
        for sort in EVERY_SORT {
            assert_eq!(
                s.list_books(&BookQuery::new(-1, sort)).await.unwrap().len(),
                ids.len(),
                "{sort:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_summary_counts_what_is_behind_a_book() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let ids = shelf(&s).await;
        for text in ["one", "two", "three"] {
            s.insert_highlight(
                ids[1],
                &crate::NewHighlight {
                    text: text.into(),
                    chapter: None,
                    page: None,
                    pos0: None,
                    pos1: None,
                    ko_datetime: None,
                    ko_datetime_updated: None,
                    color: None,
                    note: None,
                    source: "manual".into(),
                },
            )
            .await
            .unwrap();
        }
        s.insert_note(
            crate::storage::NewNoteMeta {
                book_id: Some(ids[1]),
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "vault/a.md",
                title: "a thought",
                kind: "note",
            },
            "a body",
            &[],
        )
        .await
        .unwrap();

        let got = s.book_summaries(&[ids[0], ids[1]]).await.unwrap();
        assert_eq!(got[0].book_id, ids[0], "the order asked for is kept");
        assert_eq!(
            (got[0].highlights, got[0].notes, got[0].files),
            (0, 0, 0),
            "nothing behind a book is an answer, not a missing row"
        );
        assert!(!got[0].has());
        assert_eq!((got[1].highlights, got[1].notes, got[1].files), (3, 1, 0));
        assert!(got[1].has());
        assert!(
            s.book_summaries(&[]).await.unwrap().is_empty(),
            "no ids is no rows, not every row"
        );
    }

    use super::tests_support::{PROBES, columns, probes_cover_the_merge_table, says_everything};

    async fn sources(s: &Storage, id: i64) -> Vec<(String, String)> {
        s.field_provenance(id)
            .await
            .unwrap()
            .into_iter()
            .map(|f| (f.field, f.source))
            .collect()
    }

    #[test]
    fn show_and_says_agree_on_every_column() {
        probes_cover_the_merge_table();
        for (col, set) in PROBES {
            let mut book = Book::default();
            assert!(!MERGE_RULES.iter().any(|r| r.col == col && (r.says)(&book)));
            assert_eq!(field_value(&book, col), None, "{col} printed from nothing");

            set(&mut book, 7);
            let rule = MERGE_RULES
                .iter()
                .find(|r| r.col == col)
                .expect("probes cover the table");
            assert!(
                (rule.says)(&book),
                "{col}: says nothing about a value it has"
            );
            assert!(
                field_value(&book, col).is_some(),
                "{col}: claimed but unprintable"
            );
        }
    }

    #[test]
    fn an_unknown_column_prints_nothing() {
        assert_eq!(field_value(&says_everything(1), "ko_percent"), None);
        assert_eq!(
            merge_columns().count(),
            MERGE_RULES.len(),
            "merge_columns must be the whole table"
        );
    }

    #[tokio::test]
    async fn a_record_claims_exactly_the_fields_it_supplies() {
        probes_cover_the_merge_table();
        for (col, set) in PROBES {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s
                .upsert_book(&says_everything(1), Some(Source::GoogleBooks))
                .await
                .unwrap();
            let before = columns(&s, id).await;

            let mut partial = Book::default();
            set(&mut partial, 2);
            s.enrich_book(id, &partial, Some(Source::OpenLibrary))
                .await
                .unwrap();

            let after = columns(&s, id).await;
            for ((name, was), (_, now)) in before.iter().zip(after.iter()) {
                if name == col {
                    assert_ne!(
                        was, now,
                        "{col}: the record said something and nothing moved"
                    );
                } else {
                    assert_eq!(was, now, "a {col} record moved {name}");
                }
            }
            for (name, _) in PROBES {
                let want = if name == col {
                    "openlibrary"
                } else {
                    "googlebooks"
                };
                assert_eq!(
                    s.field_source(id, name).await.unwrap().as_deref(),
                    Some(want),
                    "a {col} record claimed {name}"
                );
            }
        }
    }

    #[test]
    fn every_provider_column_survives_a_federated_merge() {
        probes_cover_the_merge_table();
        for (col, set) in PROBES {
            let rule = MERGE_RULES.iter().find(|r| r.col == col).unwrap();
            let mut src = Book::default();
            set(&mut src, 2);
            let mut dst = Book::default();
            let claimed = merge_provider_record(&mut dst, &src, Source::OpenLibrary);

            match rule.federated {
                Federated::Local => assert!(
                    claimed.is_empty() && !(rule.says)(&dst),
                    "{col} is Local and merged anyway: {claimed:?}"
                ),
                Federated::WithPair(_) => assert!(
                    claimed.is_empty(),
                    "{col} moves with its pair and was taken on its own: {claimed:?}"
                ),
                Federated::Fill(_) | Federated::Prefer(..) => {
                    assert!(
                        (rule.says)(&dst),
                        "{col} was supplied and did not reach the merged record"
                    );
                    assert!(
                        claimed.contains(&col),
                        "{col} moved without being claimed, which is how a \
                         federated field loses its origin: {claimed:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn only_our_own_columns_sit_out_the_federated_merge() {
        let local: Vec<&str> = MERGE_RULES
            .iter()
            .filter(|r| matches!(r.federated, Federated::Local))
            .map(|r| r.col)
            .collect();
        assert_eq!(local, ["cover_path"]);
    }

    #[test]
    fn a_series_index_travels_with_the_name_or_not_at_all() {
        let mut dst = Book::default();
        let orphan = Book {
            series_index: Some(7.0),
            ..Default::default()
        };
        assert!(merge_provider_record(&mut dst, &orphan, Source::GoogleBooks).is_empty());
        assert_eq!(dst.series_index, None);

        let both = Book {
            series: Some("Dune".into()),
            series_index: Some(2.0),
            ..Default::default()
        };
        let claimed = merge_provider_record(&mut dst, &both, Source::GoogleBooks);
        assert_eq!(claimed, ["series", "series_index"]);
        assert_eq!(dst.series_index, Some(2.0));

        let other = Book {
            series: Some("Other".into()),
            series_index: Some(9.0),
            ..Default::default()
        };
        assert!(merge_provider_record(&mut dst, &other, Source::OpenLibrary).is_empty());
        assert_eq!(dst.series.as_deref(), Some("Dune"));
        assert_eq!(dst.series_index, Some(2.0));

        let mut fresh = Book::default();
        let nameless = Book {
            series: Some("Dune".into()),
            ..Default::default()
        };
        let claimed = merge_provider_record(&mut fresh, &nameless, Source::OpenLibrary);
        assert_eq!(claimed, ["series"]);
        assert_eq!(fresh.series_index, None);
    }

    #[tokio::test]
    async fn a_user_owned_field_survives_every_provider_merge() {
        probes_cover_the_merge_table();
        for (col, set) in PROBES {
            let s = Storage::connect("sqlite::memory:").await.unwrap();
            let id = s
                .upsert_book(&says_everything(1), Some(Source::GoogleBooks))
                .await
                .unwrap();

            let mut mine = Book::default();
            set(&mut mine, 2);
            s.enrich_book(id, &mine, Some(Source::User)).await.unwrap();
            let corrected = columns(&s, id).await;

            for source in Source::ALL.into_iter().filter(|s| *s != Source::User) {
                let mut theirs = Book::default();
                set(&mut theirs, 3);
                s.enrich_book(id, &theirs, Some(source)).await.unwrap();
                s.enrich_book(id, &theirs, None).await.unwrap();
            }

            assert_eq!(columns(&s, id).await, corrected, "{col} was overwritten");
            assert_eq!(
                s.field_source(id, col).await.unwrap().as_deref(),
                Some("user"),
                "{col} lost the user's claim"
            );
        }
    }

    #[test]
    fn pairs_are_symmetric() {
        for rule in MERGE_RULES.iter() {
            let Some(other) = rule.pair else { continue };
            let back = MERGE_RULES
                .iter()
                .find(|r| r.col == other)
                .unwrap_or_else(|| panic!("{}: pair names no column", rule.col));
            assert_eq!(
                back.pair,
                Some(rule.col),
                "{} pairs with {other}, which does not pair back",
                rule.col
            );
        }
    }

    #[tokio::test]
    async fn owning_half_a_pair_holds_the_other_half() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();

        s.enrich_book(
            id,
            &Book {
                series: Some("Dune".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();
        s.enrich_book(
            id,
            &Book {
                isbn_13: Some("9780441013593".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        s.enrich_book(
            id,
            &Book {
                series: Some("Dune Chronicles".into()),
                series_index: Some(2.0),
                isbn_10: Some("0441013597".into()),
                ..Default::default()
            },
            Some(Source::OpenLibrary),
        )
        .await
        .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.series.as_deref(), Some("Dune"));
        assert_eq!(
            got.series_index, None,
            "an index from a series the user did not name is not this book's"
        );
        assert_eq!(got.isbn_13.as_deref(), Some("9780441013593"));
        assert_eq!(
            got.isbn_10, None,
            "the other edition's isbn_10 landed beside the user's isbn_13"
        );
        assert_eq!(s.field_source(id, "series_index").await.unwrap(), None);
        assert_eq!(s.field_source(id, "isbn_10").await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_unowned_pair_still_fills() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s.upsert_book(&sample(), None).await.unwrap();
        s.enrich_book(
            id,
            &Book {
                series: Some("Dune".into()),
                series_index: Some(0.5),
                ..Default::default()
            },
            Some(Source::OpenLibrary),
        )
        .await
        .unwrap();
        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.series.as_deref(), Some("Dune"));
        assert_eq!(got.series_index, Some(0.5));
    }

    #[tokio::test]
    async fn subjects_are_replaced_whole_and_an_empty_set_says_nothing() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut b = sample();
        b.subjects = vec!["Fiction / Literary".into(), "Historical".into()];
        let id = s.upsert_book(&b, Some(Source::GoogleBooks)).await.unwrap();
        assert_eq!(
            s.get_book(id).await.unwrap().unwrap().subjects,
            vec!["Fiction / Literary", "Historical"]
        );

        s.enrich_book(id, &Book::default(), Some(Source::OpenLibrary))
            .await
            .unwrap();
        assert_eq!(
            s.get_book(id).await.unwrap().unwrap().subjects.len(),
            2,
            "an empty set must read as silence, not as 'no subjects'"
        );

        s.enrich_book(
            id,
            &Book {
                subjects: vec!["Provincial England".into()],
                ..Default::default()
            },
            Some(Source::OpenLibrary),
        )
        .await
        .unwrap();
        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.subjects, vec!["Provincial England"]);
        assert_eq!(
            s.field_source(id, "subjects").await.unwrap().as_deref(),
            Some("openlibrary")
        );
    }

    #[tokio::test]
    async fn the_upsert_honours_the_user_guard_too() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut b = sample();
        b.description = Some("what the provider said".into());
        let id = s.upsert_book(&b, Some(Source::GoogleBooks)).await.unwrap();

        s.enrich_book(
            id,
            &Book {
                description: Some("what I actually think".into()),
                ..Default::default()
            },
            Some(Source::User),
        )
        .await
        .unwrap();

        let mut refreshed = sample();
        refreshed.description = Some("the provider, again".into());
        refreshed.publisher = Some("Blackwood".into());
        assert_eq!(
            s.upsert_book(&refreshed, Some(Source::OpenLibrary))
                .await
                .unwrap(),
            id
        );

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.description.as_deref(), Some("what I actually think"));
        assert_eq!(
            got.publisher.as_deref(),
            Some("Blackwood"),
            "one held field must not hold the rest of the record"
        );
        assert_eq!(
            s.field_source(id, "description").await.unwrap().as_deref(),
            Some("user")
        );
        assert_eq!(
            s.field_source(id, "publisher").await.unwrap().as_deref(),
            Some("openlibrary")
        );
    }

    #[tokio::test]
    async fn a_fallback_record_claims_only_the_gaps_it_filled() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    description: Some("from the provider".into()),
                    ..Default::default()
                },
                Some(Source::GoogleBooks),
            )
            .await
            .unwrap();

        s.fill_book(
            id,
            &Book {
                title: Some("MIDDLEMARCH (epub metadata)".into()),
                description: Some("from the file".into()),
                language: Some("en".into()),
                ..Default::default()
            },
            Some(Source::Epub),
        )
        .await
        .unwrap();

        let got = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(got.title.as_deref(), Some("Middlemarch"), "stored wins");
        assert_eq!(got.description.as_deref(), Some("from the provider"));
        assert_eq!(got.language.as_deref(), Some("en"), "the gap is filled");

        let mut got = sources(&s, id).await;
        got.sort();
        assert_eq!(
            got,
            vec![
                ("description".to_string(), "googlebooks".to_string()),
                ("language".to_string(), "epub".to_string()),
                ("title".to_string(), "googlebooks".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn delete_returns_cover_path() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut b = sample();
        b.cover_path = Some("database/images/x.jpg".into());
        let id = s.upsert_book(&b, None).await.unwrap();
        let cover = s.delete_book(id).await.unwrap();
        assert_eq!(cover.as_deref(), Some("database/images/x.jpg"));
        assert!(s.get_book(id).await.unwrap().is_none());
        assert!(matches!(
            s.delete_book(id).await,
            Err(EngineError::NotFound(_))
        ));
    }
}

#[cfg(test)]
mod props {
    use super::tests_support::*;
    use super::{Book, BookFilter, BookQuery, Storage};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn merging_twice_equals_merging_once(
            src_texts in proptest::collection::vec("[a-c]{1,3}", 0..6),
            dst_texts in proptest::collection::vec("[a-c]{1,3}", 0..6),
            src_words in proptest::collection::vec("[x-z]{1,2}", 0..4),
            dst_words in proptest::collection::vec("[x-z]{1,2}", 0..4),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let s = fixture(&src_texts, &dst_texts, &src_words, &dst_words).await;
                let (src, dst) = (s.src, s.dst);

                let first = s.storage.merge_books(src, dst).await.unwrap();
                prop_assert!(first.src_existed);
                let after_one = snapshot(&s.storage, dst).await;

                let second = s.storage.merge_books(src, dst).await.unwrap();
                prop_assert!(!second.src_existed, "src is gone; there is nothing left to do");
                prop_assert_eq!(second, super::MergeReport::default());
                prop_assert_eq!(after_one, snapshot(&s.storage, dst).await);
                Ok(())
            })?;
        }

        #[test]
        fn paging_partitions_the_library(
            titles in proptest::collection::vec("[a-c]", 1..7),
            years in proptest::collection::vec(proptest::option::of(1960i64..1963), 1..7),
            page in 1usize..5,
            sort in 0usize..5,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let sort = [
                    super::BookSort::LastModified,
                    super::BookSort::Title,
                    super::BookSort::Progress,
                    super::BookSort::Year,
                    super::BookSort::Author,
                ][sort];
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                for (i, title) in titles.iter().enumerate() {
                    s.upsert_book(
                        &Book {
                            title: Some(title.clone()),
                            authors: vec![title.clone()],
                            publish_year: *years.get(i).unwrap_or(&None),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .unwrap();
                }

                let whole: Vec<Option<i64>> = s
                    .list_books(&BookQuery { sort, ..Default::default() })
                    .await
                    .unwrap()
                    .iter()
                    .map(|b| b.id)
                    .collect();

                let mut walked = Vec::new();
                let mut offset = 0i64;
                loop {
                    let got = s
                        .list_books(&BookQuery::new(page as i64, sort).at_offset(offset))
                        .await
                        .unwrap();
                    if got.is_empty() {
                        break;
                    }
                    walked.extend(got.iter().map(|b| b.id));
                    offset += page as i64;
                }
                prop_assert_eq!(&walked, &whole, "{:?} at page size {}", sort, page);
                prop_assert_eq!(
                    s.count_books(&BookFilter::default()).await.unwrap(),
                    walked.len() as i64
                );
                Ok(())
            })?;
        }
    }
}

#[cfg(test)]
mod tests_support {
    use super::*;
    use crate::storage::NewHighlight;

    pub struct Fixture {
        pub storage: Storage,
        pub src: i64,
        pub dst: i64,
    }

    /// One writer per `MERGE_RULES` column; tests assert its names equal the table's, so a new rule
    /// fails loudly.
    #[allow(clippy::type_complexity)]
    pub const PROBES: [(&str, fn(&mut Book, u32)); 19] = [
        ("title", |b, n| b.title = Some(format!("title-{n}"))),
        ("authors", |b, n| b.authors = vec![format!("author-{n}")]),
        ("translators", |b, n| {
            b.translators = vec![format!("translator-{n}")]
        }),
        ("publisher", |b, n| b.publisher = Some(format!("pub-{n}"))),
        ("publish_year", |b, n| {
            b.publish_year = Some(1900 + i64::from(n))
        }),
        ("first_published", |b, n| {
            b.first_published = Some(1800 + i64::from(n))
        }),
        ("language", |b, n| b.language = Some(format!("l{n}"))),
        ("isbn_10", |b, n| b.isbn_10 = Some(format!("isbn10-{n}"))),
        ("isbn_13", |b, n| b.isbn_13 = Some(format!("isbn13-{n}"))),
        ("openlibrary_key", |b, n| {
            b.openlibrary_key = Some(format!("/works/OL{n}W"))
        }),
        ("googlebooks_id", |b, n| {
            b.googlebooks_id = Some(format!("gb-{n}"))
        }),
        ("cover_url", |b, n| {
            b.cover_url = Some(format!("https://example.invalid/{n}.jpg"))
        }),
        ("cover_path", |b, n| {
            b.cover_path = Some(format!("database/images/{n}.jpg"))
        }),
        ("page_count", |b, n| b.page_count = Some(100 + i64::from(n))),
        ("description", |b, n| {
            b.description = Some(format!("description-{n}"))
        }),
        ("first_sentence", |b, n| {
            b.first_sentence = Some(format!("first-{n}"))
        }),
        ("subjects", |b, n| b.subjects = vec![format!("subject-{n}")]),
        ("series", |b, n| b.series = Some(format!("series-{n}"))),
        ("series_index", |b, n| {
            b.series_index = Some(f64::from(n) + 0.5)
        }),
    ];

    pub fn says_everything(n: u32) -> Book {
        let mut b = Book::default();
        for (_, set) in PROBES {
            set(&mut b, n);
        }
        b
    }

    pub async fn columns(storage: &Storage, id: i64) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        for (col, _) in PROBES {
            let v: Option<String> = sqlx::query_scalar(&format!(
                "SELECT CAST({col} AS TEXT) FROM books WHERE id = ?"
            ))
            .bind(id)
            .fetch_one(storage.pool())
            .await
            .unwrap();
            out.push((col.to_string(), v));
        }
        out
    }

    pub fn probes_cover_the_merge_table() {
        let probed: Vec<&str> = PROBES.iter().map(|(c, _)| *c).collect();
        let ruled: Vec<&str> = super::MERGE_RULES.iter().map(|r| r.col).collect();
        assert_eq!(
            probed, ruled,
            "PROBES must name every MERGE_RULES column, in order: a column with \
             no probe is a column no provenance test covers"
        );
    }

    /// Highlights are keyed on text: a text in both lists is an identity collision, in one a plain
    /// move.
    /// Flashcard words work the same way against `UNIQUE(book_id, word)`.
    pub async fn fixture(
        src_texts: &[String],
        dst_texts: &[String],
        src_words: &[String],
        dst_words: &[String],
    ) -> Fixture {
        let storage = Storage::connect("sqlite::memory:").await.unwrap();
        let mk = |title: &str| Book {
            title: Some(title.to_string()),
            ..Default::default()
        };
        let src = storage.upsert_book(&mk("src"), None).await.unwrap();
        let dst = storage.upsert_book(&mk("dst"), None).await.unwrap();

        for (book_id, texts) in [(src, src_texts), (dst, dst_texts)] {
            for t in texts {
                let h = NewHighlight {
                    text: t.clone(),
                    chapter: None,
                    page: Some(1),
                    pos0: Some(format!("/body/p[1]/text().{t}")),
                    pos1: None,
                    ko_datetime: Some("2026-01-01 00:00:00".into()),
                    ko_datetime_updated: None,
                    color: None,
                    note: None,
                    source: "koreader".into(),
                };
                storage.insert_highlight(book_id, &h).await.unwrap();
            }
        }
        for (book_id, words) in [(src, src_words), (dst, dst_words)] {
            for w in words {
                storage.insert_flashcard(book_id, None, w, None).await.ok();
            }
        }
        Fixture { storage, src, dst }
    }

    pub async fn snapshot(storage: &Storage, dst: i64) -> (Vec<String>, Vec<String>, i64) {
        let mut texts: Vec<String> = storage
            .list_highlights(dst)
            .await
            .unwrap()
            .into_iter()
            .map(|h| h.text)
            .collect();
        texts.sort();
        let mut words: Vec<String> = storage
            .list_flashcards_for_book(dst)
            .await
            .unwrap()
            .into_iter()
            .map(|f| f.word)
            .collect();
        words.sort();
        let books: i64 = sqlx::query_scalar("SELECT count(*) FROM books")
            .fetch_one(storage.pool())
            .await
            .unwrap();
        (texts, words, books)
    }
}
