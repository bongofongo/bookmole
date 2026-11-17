//! Readings: one row per reading of a book; `Book` carries read-only projections of the current
//! one.

use std::collections::HashMap;

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::books::{BOOK_COLUMNS, BOOK_FROM, bind_all, book_current_join, row_to_book};
use super::query::{Predicate, ReadingFilter, ReadingQuery};
use super::{Highlight, Storage, now_unix};
use crate::book::Book;
use crate::error::{EngineError, Result};
use crate::koreader::KoStatus;
use crate::progress::Progress;

/// Numbers a highlight's read as a person counts them; relies on `list_readings` order (oldest
/// first).
#[derive(Debug, Clone, Copy)]
pub struct ReadNumbering<'a> {
    readings: &'a [Reading],
}

impl<'a> ReadNumbering<'a> {
    /// `readings` must be in `Storage::list_readings` order (oldest first); numbers count in that
    /// order.
    pub fn new(readings: &'a [Reading]) -> ReadNumbering<'a> {
        ReadNumbering { readings }
    }

    /// 1-based read number, or `None` when unattributed, not in the list, or the book was read once.
    pub fn number_of(&self, reading_id: Option<i64>) -> Option<usize> {
        let rid = reading_id?;
        let pos = self.readings.iter().position(|r| r.id == rid)?;
        ReadCount::new(pos as i64 + 1, self.readings.len() as i64)
            .ordinal()
            .map(|n| n as usize)
    }

    pub fn is_meaningful(&self) -> bool {
        ReadCount::new(1, self.readings.len() as i64).tells_reads_apart()
    }
}

/// Which read this is and how many there are, counted in SQL over every reading, not just the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCount {
    /// 1-based, oldest first (the `list_readings` order).
    pub number: i64,
    /// Total readings of the book, not just those on this page.
    pub of: i64,
}

impl ReadCount {
    pub fn new(number: i64, of: i64) -> ReadCount {
        ReadCount { number, of }
    }

    pub fn tells_reads_apart(&self) -> bool {
        self.of > 1
    }

    /// The number to show; `None` for a book read once.
    pub fn ordinal(&self) -> Option<i64> {
        self.tells_reads_apart().then_some(self.number)
    }
}

/// Our own status vocabulary; distinct from `ko_status`, which mirrors the device and is never
/// written by us.
pub const STATUS_READING: &str = "reading";
pub const STATUS_FINISHED: &str = "finished";
pub const STATUS_ABANDONED: &str = "abandoned";

#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub id: i64,
    pub book_id: i64,
    pub started_at: Option<i64>,
    /// `NULL` means open; `idx_readings_one_open` allows at most one open reading per book.
    pub finished_at: Option<i64>,
    pub status: String,
    /// Who wrote this row: `manual` | `koreader` | `goodreads` | `migrated`.
    /// A string, not an enum: it grows with each importer and nothing branches on it.
    pub source: String,
    pub current_page: Option<i64>,
    /// Device-owned mirror, refreshed by straight assignment, never `COALESCE`, so a cleared rating
    /// clears.
    pub ko_status: Option<String>,
    pub ko_percent: Option<f64>,
    pub ko_rating: Option<i64>,
    pub created_at: i64,
    pub last_modified: i64,
}

pub(super) const READING_COLUMNS: &str = "id, book_id, started_at, finished_at, status, source, current_page, \
     ko_status, ko_percent, ko_rating, created_at, last_modified";

/// Prefix for the reading's columns in a joined row; six names collide with `BOOK_COLUMNS`.
const JOINED_READING_PREFIX: &str = "r_";

/// Derived from `READING_COLUMNS` so the rename list cannot drift from the column list.
pub(super) fn reading_columns_as(alias: &str, prefix: &str) -> String {
    READING_COLUMNS
        .split(',')
        .map(|c| {
            let c = c.trim();
            format!("{alias}.{c} AS {prefix}{c}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn row_to_reading(row: &SqliteRow) -> Result<Reading> {
    row_to_reading_prefixed(row, "")
}

pub(super) fn row_to_reading_prefixed(row: &SqliteRow, prefix: &str) -> Result<Reading> {
    let col = |name: &str| format!("{prefix}{name}");
    Ok(Reading {
        id: row.try_get(col("id").as_str())?,
        book_id: row.try_get(col("book_id").as_str())?,
        started_at: row.try_get(col("started_at").as_str())?,
        finished_at: row.try_get(col("finished_at").as_str())?,
        status: row.try_get(col("status").as_str())?,
        source: row.try_get(col("source").as_str())?,
        current_page: row.try_get(col("current_page").as_str())?,
        ko_status: row.try_get(col("ko_status").as_str())?,
        ko_percent: row.try_get(col("ko_percent").as_str())?,
        ko_rating: row.try_get(col("ko_rating").as_str())?,
        created_at: row.try_get(col("created_at").as_str())?,
        last_modified: row.try_get(col("last_modified").as_str())?,
    })
}

/// Null-safe "sidecar differs" test: `IS NOT`, not `!=`, so a rating cleared on the device counts
/// as a change.
const DEVICE_STATE_DIFFER: &str =
    "(ko_status IS NOT ?2 OR ko_percent IS NOT ?3 OR ko_rating IS NOT ?4)";

/// A book's readings as unix-second windows (`?1` is the book id, used twice).
/// One macro so the missing-`started_at` derivation stays a single text.
macro_rules! reading_windows_select {
    () => {
        "
    SELECT r.id AS reading_id,
           r.book_id AS book_id,
           COALESCE(
               r.started_at,
               (SELECT MAX(p.finished_at) + 1 FROM readings p
                 WHERE p.book_id = r.book_id
                   AND p.id <> r.id
                   AND p.finished_at IS NOT NULL
                   AND p.finished_at <= COALESCE(r.finished_at, 8640000000000)),
               -8640000000000
           ) AS win_start,
           COALESCE(r.finished_at, 8640000000000) AS win_end
      FROM readings r"
    };
}
pub(super) const READING_WINDOWS: &str =
    concat!(reading_windows_select!(), "\n     WHERE r.book_id = ?1");
pub(super) const ALL_READING_WINDOWS: &str = reading_windows_select!();

/// Reading age key; `list_readings`, read ordinals and the library sort must all count in this one
/// order.
pub(super) fn reading_age_key(alias: &str) -> String {
    format!("{}, {alias}.id", reading_began(alias))
}

/// Leading term of the age key and the expression `idx_readings_started` is on; a comma key cannot
/// take `DESC`.
pub(super) fn reading_began(alias: &str) -> String {
    format!("COALESCE({alias}.started_at, {alias}.created_at)")
}

/// Three indexed arms; no title sort, since a `books` column cannot use a `readings` index.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReadingSort {
    #[default]
    Finished,
    Started,
    LastModified,
}

/// Always ends in `readings.id`, in the sort's direction, so the order is total and pages never
/// overlap.
fn reading_order_by(sort: ReadingSort) -> &'static str {
    match sort {
        ReadingSort::Finished => "readings.finished_at DESC, readings.id DESC",
        // Spelled to match `idx_readings_started`; the comma key plus ` DESC` would reverse only `id`.
        ReadingSort::Started => {
            "COALESCE(readings.started_at, readings.created_at) DESC, readings.id DESC"
        }
        ReadingSort::LastModified => "readings.last_modified DESC, readings.id DESC",
    }
}

fn reading_rows_sql(sort: ReadingSort, predicate: &Predicate) -> String {
    let reading = reading_columns_as("readings", JOINED_READING_PREFIX);
    let mine = reading_age_key("readings");
    let sibling = reading_age_key("sib");
    let join = book_current_join!();
    let where_sql = &predicate.sql;
    let order = reading_order_by(sort);
    format!(
        "SELECT {BOOK_COLUMNS}, {reading},
                (SELECT count(*) FROM readings sib
                  WHERE sib.book_id = readings.book_id) AS of_reads,
                (SELECT count(*) FROM readings sib
                  WHERE sib.book_id = readings.book_id
                    AND ({sibling}) <= ({mine})) AS read_number,
                readings.card_highlight_id AS passage_id
           FROM readings JOIN books ON books.id = readings.book_id {join}
          {where_sql} ORDER BY {order} LIMIT ? OFFSET ?"
    )
}

/// Keep `FROM readings` without the `books` join: alone it is served by a covering index, the join
/// forces a `books` scan.
/// `strftime` stays in the projection, never the `WHERE`, or the index is skipped.
fn reading_years_sql(predicate: &Predicate) -> String {
    let where_sql = &predicate.sql;
    format!(
        "SELECT CAST(strftime('%Y', readings.finished_at, 'unixepoch') AS INTEGER) AS year
           FROM readings {where_sql}
          GROUP BY year ORDER BY year DESC"
    )
}

/// Years in which matching readings ended, and whether any is still open. Deliberately no per-year
/// counts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadingYears {
    /// Newest first; only years in which a matching reading closed.
    pub years: Vec<i32>,
    /// Whether any matching reading is open; open readings are in no year.
    pub open: bool,
}

/// A reading, its book and what one round trip needs to draw a page of readings; not a card layout.
#[derive(Debug, Clone)]
pub struct ReadingRow {
    pub book: Book,
    pub reading: Reading,
    /// This reading's progress, not the book's; filled here because only this query holds the length.
    pub progress: Progress,
    /// Counted over every reading of the book, never over the page.
    pub count: ReadCount,
    pub passage: Option<Highlight>,
}

/// KOReader's zoneless `datetime` read as UTC, matching the SQLite `strftime('%s', ...)` that
/// attribution compares against.
pub fn ko_datetime_to_unix(s: &str) -> Option<i64> {
    let fmt = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    time::PrimitiveDateTime::parse(s.trim(), fmt)
        .ok()
        .map(|dt| dt.assume_utc().unix_timestamp())
}

impl Storage {
    pub(crate) async fn active_reading(&self, book_id: i64) -> Result<Option<Reading>> {
        let sql = format!(
            "SELECT {READING_COLUMNS} FROM readings WHERE book_id = ? AND finished_at IS NULL"
        );
        let row = sqlx::query(&sql)
            .bind(book_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_reading).transpose()
    }

    pub(crate) async fn get_reading(&self, id: i64) -> Result<Option<Reading>> {
        let sql = format!("SELECT {READING_COLUMNS} FROM readings WHERE id = ?");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_reading).transpose()
    }

    /// Oldest first; read numbering everywhere counts in this order (see `reading_age_key`).
    pub async fn list_readings(&self, book_id: i64) -> Result<Vec<Reading>> {
        let age = reading_age_key("readings");
        let sql = format!(
            "SELECT {READING_COLUMNS} FROM readings WHERE book_id = ?
             ORDER BY {age} ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_reading).collect()
    }

    /// Books with an open reading (abandoned included), newest of book/reading `last_modified` first.
    /// Keep `cur.id IS NOT NULL`: the LEFT JOIN yields all-NULL `cur` rows for books with no reading.
    pub(crate) async fn list_open_readings(&self, limit: i64) -> Result<Vec<(Book, Reading)>> {
        let reading = reading_columns_as("cur", JOINED_READING_PREFIX);
        let sql = format!(
            "SELECT {BOOK_COLUMNS}, {reading} {BOOK_FROM}
             WHERE cur.id IS NOT NULL AND cur.finished_at IS NULL
             ORDER BY MAX(books.last_modified, cur.last_modified) DESC, books.id DESC
             LIMIT ?"
        );
        let rows = sqlx::query(&sql).bind(limit).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row_to_book(row)?,
                    row_to_reading_prefixed(row, JOINED_READING_PREFIX)?,
                ))
            })
            .collect()
    }

    /// One page of the library's readings, driven from `readings` so the sort reaches its indexes.
    /// The read number is a correlated subquery, not a window function, so a filtered page still counts
    /// every sibling.
    pub(crate) async fn list_reading_rows(&self, query: &ReadingQuery) -> Result<Vec<ReadingRow>> {
        let predicate = query.filter.predicate();
        let sql = reading_rows_sql(query.sort, &predicate);
        let q = bind_all(sqlx::query(&sql), &predicate.binds)
            .bind(query.limit)
            .bind(query.offset.max(0));
        let rows = q.fetch_all(self.pool()).await?;

        let passage_ids: Vec<i64> = rows
            .iter()
            .filter_map(|r| r.try_get::<Option<i64>, _>("passage_id").ok().flatten())
            .collect();
        let mut passages: HashMap<i64, Highlight> = self
            .highlights_by_ids(&passage_ids)
            .await?
            .into_iter()
            .map(|h| (h.id, h))
            .collect();

        rows.iter()
            .map(|row| {
                let book = row_to_book(row)?;
                let reading = row_to_reading_prefixed(row, JOINED_READING_PREFIX)?;
                let progress = Progress::of_reading(&reading, book.page_count);
                let passage = row
                    .try_get::<Option<i64>, _>("passage_id")?
                    .and_then(|id| passages.remove(&id));
                Ok(ReadingRow {
                    book,
                    reading,
                    progress,
                    count: ReadCount::new(row.try_get("read_number")?, row.try_get("of_reads")?),
                    passage,
                })
            })
            .collect()
    }

    /// Count under the same predicate as the page (`ReadingFilter::predicate`); a separate call, not a
    /// field on the rows.
    pub(crate) async fn count_readings(&self, filter: &ReadingFilter) -> Result<i64> {
        let predicate = filter.predicate();
        let where_sql = &predicate.sql;
        let sql = format!(
            "SELECT COUNT(*) FROM readings JOIN books ON books.id = readings.book_id {where_sql}"
        );
        let row = bind_all(sqlx::query(&sql), &predicate.binds)
            .fetch_one(self.pool())
            .await?;
        Ok(row.try_get::<i64, _>(0)?)
    }

    /// Years matching readings ended in, plus whether any is open; shares `ReadingFilter::predicate`
    /// with the page.
    /// With `finished_in` set `open` is false; with `open: Some(true)` `years` is empty.
    pub(crate) async fn reading_years(&self, filter: &ReadingFilter) -> Result<ReadingYears> {
        let predicate = filter.predicate();
        let sql = reading_years_sql(&predicate);
        let rows = bind_all(sqlx::query(&sql), &predicate.binds)
            .fetch_all(self.pool())
            .await?;

        let mut years = Vec::with_capacity(rows.len());
        let mut open = false;
        for row in &rows {
            match row.try_get::<Option<i64>, _>("year")? {
                Some(y) => years.push(y as i32),
                None => open = true,
            }
        }
        Ok(ReadingYears { years, open })
    }

    /// Open a reading, returning its id; a second open reading maps to `EngineError::InvalidInput`.
    pub async fn open_reading(
        &self,
        book_id: i64,
        started_at: Option<i64>,
        source: &str,
    ) -> Result<i64> {
        let now = now_unix();
        let row = sqlx::query(
            "INSERT INTO readings (book_id, started_at, status, source, created_at, last_modified)
             VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(book_id)
        .bind(started_at)
        .bind(STATUS_READING)
        .bind(source)
        .bind(now)
        .bind(now)
        .fetch_one(self.pool())
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => EngineError::InvalidInput(
                format!("book {book_id} already has an open reading; finish it first"),
            ),
            _ => EngineError::from(e),
        })?;
        Ok(row.try_get("id")?)
    }

    /// Like `open_reading` but with explicit dates and status, for imports; a second open reading is
    /// `InvalidInput`.
    pub async fn record_reading(
        &self,
        book_id: i64,
        started_at: Option<i64>,
        finished_at: Option<i64>,
        status: &str,
        source: &str,
    ) -> Result<i64> {
        let now = now_unix();
        let row = sqlx::query(
            "INSERT INTO readings (book_id, started_at, finished_at, status, source,
                                   created_at, last_modified)
             VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(book_id)
        .bind(started_at)
        .bind(finished_at)
        .bind(status)
        .bind(source)
        .bind(now)
        .bind(now)
        .fetch_one(self.pool())
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => EngineError::InvalidInput(
                format!("book {book_id} already has an open reading; finish it first"),
            ),
            _ => EngineError::from(e),
        })?;
        Ok(row.try_get("id")?)
    }

    /// Lets an importer count only the readings it recorded itself.
    pub(crate) async fn readings_from_source(
        &self,
        book_id: i64,
        source: &str,
    ) -> Result<Vec<Reading>> {
        let age = reading_age_key("readings");
        let sql = format!(
            "SELECT {READING_COLUMNS} FROM readings WHERE book_id = ? AND source = ?
             ORDER BY {age} ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .bind(source)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_reading).collect()
    }

    pub(crate) async fn close_reading_at(
        &self,
        reading_id: i64,
        finished_at: i64,
        status: &str,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE readings SET finished_at = ?2, status = ?3, last_modified = ?4
             WHERE id = ?1 AND (finished_at IS NOT ?2 OR status IS NOT ?3)",
        )
        .bind(reading_id)
        .bind(finished_at)
        .bind(status)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn finish_reading(&self, book_id: i64) -> Result<bool> {
        let now = now_unix();
        let done = sqlx::query(
            "UPDATE readings SET finished_at = ?2, status = ?3, last_modified = ?2
             WHERE book_id = ?1 AND finished_at IS NULL",
        )
        .bind(book_id)
        .bind(now)
        .bind(STATUS_FINISHED)
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Marks the open reading abandoned without closing it, so resuming is not a reread.
    /// Stamps `put_down_at` only on the transition.
    pub async fn abandon_reading(&self, book_id: i64) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE readings SET status = ?2, last_modified = ?3, put_down_at = ?3
             WHERE book_id = ?1 AND finished_at IS NULL AND status IS NOT ?2",
        )
        .bind(book_id)
        .bind(STATUS_ABANDONED)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// The current reading's id, opening one only if the book has none at all, not none open:
    /// a `complete` sidecar closes its reading and the idempotent import would otherwise open one per
    /// run.
    pub(crate) async fn ensure_reading(
        &self,
        book_id: i64,
        started_at: Option<i64>,
        source: &str,
    ) -> Result<i64> {
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM readings WHERE book_id = ?
             ORDER BY (finished_at IS NULL) DESC, COALESCE(started_at, created_at) DESC, id DESC
             LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(self.pool())
        .await?;
        match current {
            Some(id) => Ok(id),
            None => self.open_reading(book_id, started_at, source).await,
        }
    }

    /// Close the open reading (if any), then open a new one; the unique index allows only that order.
    pub async fn reread(&self, book_id: i64) -> Result<i64> {
        self.finish_reading(book_id).await?;
        self.open_reading(book_id, Some(now_unix()), "manual").await
    }

    /// `id` is the book id. Opens a reading unless clearing `finished`, which reopens the latest
    /// instead.
    /// Also files a typed page in the activity log, here so every frontend does.
    pub async fn update_progress(
        &self,
        id: i64,
        page: Option<i64>,
        finished: Option<bool>,
    ) -> Result<Book> {
        if self.get_book(id).await?.is_none() {
            return Err(EngineError::NotFound(format!("book id {id}")));
        }
        let now = now_unix();

        let reading_id = match self.active_reading(id).await? {
            Some(r) => r.id,
            None if finished == Some(false) => {
                let latest: Option<i64> = sqlx::query_scalar(
                    "SELECT id FROM readings WHERE book_id = ?
                     ORDER BY COALESCE(finished_at, started_at, created_at) DESC, id DESC LIMIT 1",
                )
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
                match latest {
                    Some(rid) => rid,
                    None => self.open_reading(id, Some(now), "manual").await?,
                }
            }
            None => self.open_reading(id, Some(now), "manual").await?,
        };

        // Read before the write: the activity log records the delta from the previous page.
        let previous_page: Option<i64> =
            sqlx::query_scalar("SELECT current_page FROM readings WHERE id = ?")
                .bind(reading_id)
                .fetch_optional(self.pool())
                .await?
                .flatten();

        // `started_at` stays unfilled: an open reading with no start is an import's "unknown",
        // and stamping now would strand earlier highlights outside its window.
        sqlx::query(
            r#"UPDATE readings SET
                   current_page  = COALESCE(?2, current_page),
                   status        = CASE WHEN ?4 = 1 THEN 'finished'
                                        WHEN ?4 = 0 THEN 'reading'
                                        ELSE status END,
                   finished_at   = CASE WHEN ?4 = 1 THEN COALESCE(finished_at, ?3)
                                        WHEN ?4 = 0 THEN NULL
                                        ELSE finished_at END,
                   last_modified = ?3
               WHERE id = ?1"#,
        )
        .bind(reading_id)
        .bind(page)
        .bind(now)
        .bind(finished)
        .execute(self.pool())
        .await?;

        sqlx::query("UPDATE books SET last_modified = ?2 WHERE id = ?1")
            .bind(id)
            .bind(now)
            .execute(self.pool())
            .await?;

        if let Some(page) = page {
            self.record_typed_page(id, reading_id, previous_page, page, now)
                .await?;
        }

        self.get_book(id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("book id {id}")))
    }

    /// Mirror device status/percent/rating onto the current reading; returns whether anything changed.
    /// Straight assignment, not `COALESCE`: an absent rating means the user cleared it. Opens no
    /// reading.
    pub(crate) async fn set_device_state(
        &self,
        book_id: i64,
        status: Option<&KoStatus>,
        percent: Option<f64>,
        rating: Option<i64>,
    ) -> Result<bool> {
        let sql = format!(
            "UPDATE readings SET ko_status = ?2, ko_percent = ?3, ko_rating = ?4,
                                 last_modified = ?5
             WHERE id = (SELECT id FROM readings WHERE book_id = ?1
                         ORDER BY (finished_at IS NULL) DESC,
                                  COALESCE(started_at, created_at) DESC, id DESC LIMIT 1)
               AND {DEVICE_STATE_DIFFER}"
        );
        let done = sqlx::query(&sql)
            .bind(book_id)
            .bind(status.map(|s| s.to_string()))
            .bind(percent)
            .bind(rating)
            .bind(now_unix())
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn device_state_differs(
        &self,
        book_id: i64,
        status: Option<&KoStatus>,
        percent: Option<f64>,
        rating: Option<i64>,
    ) -> Result<bool> {
        let sql = format!(
            "SELECT count(*) FROM readings
             WHERE id = (SELECT id FROM readings WHERE book_id = ?1
                         ORDER BY (finished_at IS NULL) DESC,
                                  COALESCE(started_at, created_at) DESC, id DESC LIMIT 1)
               AND {DEVICE_STATE_DIFFER}"
        );
        let n: i64 = sqlx::query_scalar(&sql)
            .bind(book_id)
            .bind(status.map(|s| s.to_string()))
            .bind(percent)
            .bind(rating)
            .fetch_one(self.pool())
            .await?;
        Ok(n > 0)
    }

    /// Assigns `highlights.reading_id` by matching `ko_datetime` into reading windows, from scratch.
    /// `NULL` is correct when no window holds it; a missing `started_at` begins where the previous
    /// reading ended (+1, exclusive), never -infinity.
    pub async fn attribute_highlights(&self, book_id: i64) -> Result<usize> {
        sqlx::query(&format!(
            "WITH windows AS ({READING_WINDOWS})
               UPDATE highlights SET reading_id = (
                   SELECT w.reading_id FROM windows w
                    WHERE CAST(strftime('%s', highlights.ko_datetime) AS INTEGER)
                          BETWEEN w.win_start AND w.win_end
                    ORDER BY w.win_start DESC, w.reading_id DESC
                    LIMIT 1)
               WHERE book_id = ?1"
        ))
        .bind(book_id)
        .execute(self.pool())
        .await?;

        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM highlights WHERE book_id = ? AND reading_id IS NOT NULL",
        )
        .bind(book_id)
        .fetch_one(self.pool())
        .await?;
        Ok(n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seeded() -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    page_count: Some(490),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, id)
    }

    async fn add(s: &Storage, title: &str) -> i64 {
        s.upsert_book(
            &Book {
                title: Some(title.into()),
                page_count: Some(300),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap()
    }

    /// Hand-sets the two stamps; unix seconds tie within a test, so ordering tests must set them
    /// explicitly.
    async fn touched_at(s: &Storage, book_id: i64, book: Option<i64>, reading: Option<i64>) {
        if let Some(t) = book {
            sqlx::query("UPDATE books SET last_modified = ? WHERE id = ?")
                .bind(t)
                .bind(book_id)
                .execute(s.pool())
                .await
                .unwrap();
        }
        if let Some(t) = reading {
            sqlx::query("UPDATE readings SET last_modified = ? WHERE book_id = ?")
                .bind(t)
                .bind(book_id)
                .execute(s.pool())
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn open_readings_are_the_ones_still_open() {
        let (s, middlemarch) = seeded().await;
        let unread = add(&s, "Kokoro").await;

        assert!(
            s.list_open_readings(10).await.unwrap().is_empty(),
            "a library nobody has opened is not a currently-reading list"
        );

        s.update_progress(middlemarch, Some(120), None)
            .await
            .unwrap();
        let open = s.list_open_readings(10).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].0.id, Some(middlemarch));
        assert_eq!(open[0].1.current_page, Some(120));
        assert!(
            !open.iter().any(|(b, _)| b.id == Some(unread)),
            "a book with no reading has nothing open"
        );

        s.update_progress(middlemarch, None, Some(true))
            .await
            .unwrap();
        assert!(
            s.list_open_readings(10).await.unwrap().is_empty(),
            "finishing it takes it off the list"
        );

        let second = s.reread(middlemarch).await.unwrap();
        let open = s.list_open_readings(10).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].1.id, second);
        assert_eq!(open[0].1.current_page, None);
    }

    #[tokio::test]
    async fn a_typed_page_does_not_invent_a_start_date() {
        let (s, id) = seeded().await;
        let rid = s.open_reading(id, None, "goodreads").await.unwrap();
        s.update_progress(id, Some(21), None).await.unwrap();
        let r = s.get_reading(rid).await.unwrap().unwrap();
        assert_eq!(r.current_page, Some(21));
        assert_eq!(r.started_at, None);

        let other = s
            .upsert_book(
                &Book {
                    title: Some("Dracula".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        s.update_progress(other, Some(5), None).await.unwrap();
        let opened = s.active_reading(other).await.unwrap().unwrap();
        assert!(opened.started_at.is_some());
    }

    #[tokio::test]
    async fn the_projections_agree_with_the_reading_beside_them() {
        let (s, id) = seeded().await;
        // A finished reading first, so "current" and "open" are different rows.
        s.update_progress(id, Some(490), Some(true)).await.unwrap();
        s.reread(id).await.unwrap();
        s.update_progress(id, Some(40), None).await.unwrap();

        let open = s.list_open_readings(10).await.unwrap();
        assert_eq!(open.len(), 1);
        let (book, reading) = &open[0];

        assert_eq!(book.current_page, reading.current_page);
        assert_eq!(book.date_started, reading.started_at);
        assert_eq!(book.date_finished, reading.finished_at);
        assert_eq!(book.finished, reading.status == STATUS_FINISHED);
        let elsewhere = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(book.id, elsewhere.id);
        assert_eq!(book.current_page, elsewhere.current_page);
        assert_eq!(book.finished, elsewhere.finished);
        assert_eq!(book.date_started, elsewhere.date_started);
        assert_eq!(book.date_finished, elsewhere.date_finished);
    }

    #[tokio::test]
    async fn ordering_is_most_recently_touched() {
        let (s, first) = seeded().await;
        let second = add(&s, "Kokoro").await;
        let third = add(&s, "Snow Country").await;
        for id in [first, second, third] {
            s.update_progress(id, Some(10), None).await.unwrap();
        }

        touched_at(&s, first, Some(300), Some(300)).await;
        touched_at(&s, second, Some(100), Some(100)).await;
        touched_at(&s, third, Some(200), Some(200)).await;
        let order: Vec<i64> = s
            .list_open_readings(10)
            .await
            .unwrap()
            .iter()
            .map(|(b, _)| b.id.unwrap())
            .collect();
        assert_eq!(order, vec![first, third, second]);

        touched_at(&s, second, None, Some(900)).await;
        let order: Vec<i64> = s
            .list_open_readings(10)
            .await
            .unwrap()
            .iter()
            .map(|(b, _)| b.id.unwrap())
            .collect();
        assert_eq!(order, vec![second, first, third]);
    }

    #[tokio::test]
    async fn an_abandoned_reading_is_still_listed_and_says_so() {
        let (s, id) = seeded().await;
        s.update_progress(id, Some(60), None).await.unwrap();
        assert!(s.abandon_reading(id).await.unwrap());

        let open = s.list_open_readings(10).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].1.status, STATUS_ABANDONED);
    }

    #[tokio::test]
    async fn the_projection_tells_abandoned_from_reading_from_never_opened() {
        let (s, reading) = seeded().await;
        let abandoned = add(&s, "Kokoro").await;
        let untouched = add(&s, "Season of Migration to the North").await;

        s.update_progress(reading, Some(60), None).await.unwrap();
        s.update_progress(abandoned, Some(20), None).await.unwrap();
        assert!(s.abandon_reading(abandoned).await.unwrap());

        let mut got = Vec::new();
        for id in [reading, abandoned, untouched] {
            let b = s
                .get_book(id)
                .await
                .unwrap()
                .expect("seeded book is stored");
            assert!(!b.finished);
            got.push(b.reading_status);
        }
        // `None`, not an invented "unread" status: the book has no reading row.
        assert_eq!(
            got,
            vec![
                Some(STATUS_READING.to_string()),
                Some(STATUS_ABANDONED.to_string()),
                None
            ]
        );
    }

    #[tokio::test]
    async fn the_limit_is_honoured() {
        let (s, first) = seeded().await;
        let second = add(&s, "Kokoro").await;
        for id in [first, second] {
            s.update_progress(id, Some(10), None).await.unwrap();
        }
        assert_eq!(s.list_open_readings(1).await.unwrap().len(), 1);
        assert_eq!(s.list_open_readings(0).await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn the_index_refuses_a_second_open_reading() {
        let (s, id) = seeded().await;
        s.open_reading(id, Some(1), "manual").await.unwrap();
        assert!(matches!(
            s.open_reading(id, Some(2), "manual").await,
            Err(EngineError::InvalidInput(_))
        ));
        assert!(s.finish_reading(id).await.unwrap());
        assert!(s.open_reading(id, Some(2), "manual").await.is_ok());
        assert_eq!(s.list_readings(id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn finishing_nothing_reports_nothing() {
        let (s, id) = seeded().await;
        assert!(!s.finish_reading(id).await.unwrap());
    }

    #[tokio::test]
    async fn a_reread_gets_its_own_reading_and_the_book_follows_it() {
        let (s, id) = seeded().await;
        s.update_progress(id, Some(200), None).await.unwrap();
        let b = s.update_progress(id, None, Some(true)).await.unwrap();
        assert!(b.finished);
        assert_eq!(b.current_page, Some(200));
        let first_finished = b.date_finished.expect("stamped");

        s.reread(id).await.unwrap();
        let b = s.update_progress(id, Some(30), None).await.unwrap();
        assert_eq!(b.current_page, Some(30), "the book follows the new reading");
        assert!(!b.finished);

        let readings = s.list_readings(id).await.unwrap();
        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].current_page, Some(200));
        assert_eq!(readings[0].finished_at, Some(first_finished));
        assert_eq!(readings[1].current_page, Some(30));
        assert_eq!(readings[1].finished_at, None);
    }

    #[tokio::test]
    async fn unfinishing_reopens_rather_than_starting_over() {
        let (s, id) = seeded().await;
        s.update_progress(id, Some(120), Some(true)).await.unwrap();
        let b = s.update_progress(id, None, Some(false)).await.unwrap();
        assert!(!b.finished);
        assert_eq!(b.current_page, Some(120), "progress survives the toggle");
        assert_eq!(b.date_finished, None);
        assert_eq!(s.list_readings(id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_book_with_no_reading_projects_as_blank() {
        let (s, id) = seeded().await;
        let b = s.get_book(id).await.unwrap().unwrap();
        assert_eq!(b.current_page, None);
        assert!(!b.finished);
        assert_eq!(b.date_started, None);
        assert_eq!(b.date_finished, None);
        assert!(s.list_readings(id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn device_state_reports_only_a_real_change() {
        let (s, id) = seeded().await;
        s.open_reading(id, Some(1), "koreader").await.unwrap();

        let complete = KoStatus::Complete;
        assert!(
            s.device_state_differs(id, Some(&complete), Some(0.5), Some(4))
                .await
                .unwrap()
        );
        assert!(
            s.set_device_state(id, Some(&complete), Some(0.5), Some(4))
                .await
                .unwrap()
        );
        assert!(
            !s.set_device_state(id, Some(&complete), Some(0.5), Some(4))
                .await
                .unwrap(),
            "an identical sidecar is not an update"
        );
        assert!(
            !s.device_state_differs(id, Some(&complete), Some(0.5), Some(4))
                .await
                .unwrap(),
            "the preview must agree with the write"
        );

        let r = &s.list_readings(id).await.unwrap()[0];
        assert_eq!(r.ko_status.as_deref(), Some("complete"));
        assert_eq!(r.ko_percent, Some(0.5));
        assert_eq!(r.ko_rating, Some(4));

        assert!(s.set_device_state(id, None, None, None).await.unwrap());
        let r = &s.list_readings(id).await.unwrap()[0];
        assert_eq!(r.ko_rating, None);
        assert_eq!(r.ko_status, None);
    }

    #[tokio::test]
    async fn device_state_with_no_reading_writes_nothing() {
        let (s, id) = seeded().await;
        assert!(
            !s.set_device_state(id, Some(&KoStatus::Reading), Some(0.1), None)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn ko_datetime_parses_as_utc_and_rejects_junk() {
        assert_eq!(ko_datetime_to_unix("1970-01-01 00:00:01"), Some(1));
        assert_eq!(ko_datetime_to_unix("2026-01-05 21:14:08"), Some(1767647648));
        assert_eq!(ko_datetime_to_unix("not a date"), None);
        assert_eq!(ko_datetime_to_unix(""), None);
    }

    use crate::storage::NewHighlight;

    fn hl(text: &str, when: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[1]/text().{text}")),
            pos1: None,
            ko_datetime: Some(when.into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    async fn reading_of(s: &Storage, book_id: i64, text: &str) -> Option<i64> {
        sqlx::query_scalar("SELECT reading_id FROM highlights WHERE book_id = ? AND text = ?")
            .bind(book_id)
            .bind(text)
            .fetch_one(s.pool())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn attribution_splits_by_window_and_leaves_the_rest_null() {
        let (s, id) = seeded().await;
        let first = s
            .open_reading(id, ko_datetime_to_unix("2024-01-01 00:00:00"), "manual")
            .await
            .unwrap();
        s.finish_reading(id).await.unwrap();
        sqlx::query("UPDATE readings SET finished_at = ? WHERE id = ?")
            .bind(ko_datetime_to_unix("2024-02-01 00:00:00"))
            .bind(first)
            .execute(s.pool())
            .await
            .unwrap();
        let second = s
            .open_reading(id, ko_datetime_to_unix("2026-01-01 00:00:00"), "manual")
            .await
            .unwrap();

        for h in [
            hl("in the first", "2024-01-15 09:00:00"),
            hl("in the second", "2026-03-02 09:00:00"),
            hl("in neither", "2025-06-01 09:00:00"),
        ] {
            s.insert_highlight(id, &h).await.unwrap();
        }

        assert_eq!(s.attribute_highlights(id).await.unwrap(), 2);
        assert_eq!(reading_of(&s, id, "in the first").await, Some(first));
        assert_eq!(reading_of(&s, id, "in the second").await, Some(second));
        assert_eq!(reading_of(&s, id, "in neither").await, None);
    }

    #[tokio::test]
    async fn attribution_is_recomputed_not_accumulated() {
        let (s, id) = seeded().await;
        let r = s
            .open_reading(id, ko_datetime_to_unix("2024-01-01 00:00:00"), "manual")
            .await
            .unwrap();
        s.insert_highlight(id, &hl("inside", "2024-01-15 09:00:00"))
            .await
            .unwrap();
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 1);

        sqlx::query("UPDATE readings SET started_at = ? WHERE id = ?")
            .bind(ko_datetime_to_unix("2025-01-01 00:00:00"))
            .bind(r)
            .execute(s.pool())
            .await
            .unwrap();
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 0);
        assert_eq!(reading_of(&s, id, "inside").await, None);
    }

    #[tokio::test]
    async fn unstarted_readings_do_not_swallow_the_earlier_ones_highlights() {
        let (s, id) = seeded().await;
        let first = s
            .record_reading(
                id,
                None,
                ko_datetime_to_unix("2020-02-01 00:00:00"),
                STATUS_FINISHED,
                "goodreads",
            )
            .await
            .unwrap();
        let second = s
            .record_reading(
                id,
                None,
                ko_datetime_to_unix("2023-02-01 00:00:00"),
                STATUS_FINISHED,
                "goodreads",
            )
            .await
            .unwrap();
        let third = s
            .record_reading(
                id,
                None,
                ko_datetime_to_unix("2026-02-01 00:00:00"),
                STATUS_FINISHED,
                "goodreads",
            )
            .await
            .unwrap();

        for h in [
            hl("read once", "2020-01-15 09:00:00"),
            hl("read twice", "2023-01-15 09:00:00"),
            hl("read thrice", "2026-01-15 09:00:00"),
        ] {
            s.insert_highlight(id, &h).await.unwrap();
        }
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 3);

        assert_eq!(reading_of(&s, id, "read once").await, Some(first));
        assert_eq!(reading_of(&s, id, "read twice").await, Some(second));
        assert_eq!(reading_of(&s, id, "read thrice").await, Some(third));
    }

    #[tokio::test]
    async fn an_unstarted_reread_leaves_the_first_reads_highlights_alone() {
        let (s, id) = seeded().await;
        let first = s
            .record_reading(
                id,
                ko_datetime_to_unix("2020-01-01 00:00:00"),
                ko_datetime_to_unix("2020-02-01 00:00:00"),
                STATUS_FINISHED,
                "manual",
            )
            .await
            .unwrap();
        s.insert_highlight(id, &hl("first read", "2020-01-15 09:00:00"))
            .await
            .unwrap();
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 1);
        assert_eq!(reading_of(&s, id, "first read").await, Some(first));

        let second = s.open_reading(id, None, "koreader").await.unwrap();
        s.insert_highlight(id, &hl("second read", "2026-03-01 09:00:00"))
            .await
            .unwrap();
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 2);

        assert_eq!(
            reading_of(&s, id, "first read").await,
            Some(first),
            "opening a reread must not move the first read's highlights onto it"
        );
        assert_eq!(reading_of(&s, id, "second read").await, Some(second));
    }

    #[tokio::test]
    async fn the_derived_start_is_exclusive_and_an_explicit_one_is_not() {
        let (s, id) = seeded().await;
        let closed_at = ko_datetime_to_unix("2020-02-01 00:00:00");
        let first = s
            .record_reading(
                id,
                ko_datetime_to_unix("2020-01-01 00:00:00"),
                closed_at,
                STATUS_FINISHED,
                "manual",
            )
            .await
            .unwrap();
        s.open_reading(id, None, "manual").await.unwrap();
        s.insert_highlight(id, &hl("on the boundary", "2020-02-01 00:00:00"))
            .await
            .unwrap();
        s.attribute_highlights(id).await.unwrap();
        assert_eq!(
            reading_of(&s, id, "on the boundary").await,
            Some(first),
            "a reading owns the instant it finished; the derived start is the second after"
        );

        let (s, id) = seeded().await;
        s.record_reading(
            id,
            ko_datetime_to_unix("2020-01-01 00:00:00"),
            closed_at,
            STATUS_FINISHED,
            "manual",
        )
        .await
        .unwrap();
        let second = s.open_reading(id, closed_at, "manual").await.unwrap();
        s.insert_highlight(id, &hl("on the boundary", "2020-02-01 00:00:00"))
            .await
            .unwrap();
        s.attribute_highlights(id).await.unwrap();
        assert_eq!(reading_of(&s, id, "on the boundary").await, Some(second));
    }

    use crate::book::ReadingState;
    use crate::storage::DayRange;

    const EVERY_SORT: [ReadingSort; 3] = [
        ReadingSort::Finished,
        ReadingSort::Started,
        ReadingSort::LastModified,
    ];

    /// Only the outer query's plan lines (`parent` 0); the subqueries carry their own ORDER BY.
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

    fn plan_of(sort: ReadingSort, filter: ReadingFilter) -> String {
        reading_rows_sql(sort, &filter.predicate())
    }

    #[tokio::test]
    async fn the_reading_sort_indexes_are_the_plan_the_planner_picks() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for (sort, index) in [
            (ReadingSort::Finished, "idx_readings_finished_at"),
            (ReadingSort::Started, "idx_readings_started"),
            (ReadingSort::LastModified, "idx_readings_last_modified"),
        ] {
            let plan = outer_plan(&s, &plan_of(sort, ReadingFilter::default()))
                .await
                .join("; ");
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
    async fn a_year_filter_over_an_expression_loses_the_index() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let honest = plan_of(
            ReadingSort::LastModified,
            ReadingFilter {
                finished_in: Some(DayRange::new("2025-01-01", "2025-12-31").unwrap()),
                ..Default::default()
            },
        );
        let plan = outer_plan(&s, &honest).await.join("; ");
        assert!(
            plan.contains("idx_readings_finished_at"),
            "the bare-column year filter must reach the index: {plan}"
        );

        let naive = honest.replace(
            "readings.finished_at >= ? AND readings.finished_at < ?",
            "date(readings.finished_at, 'unixepoch') BETWEEN ? AND ?",
        );
        assert_ne!(naive, honest, "the substitution must have bitten");
        let plan = outer_plan(&s, &naive).await.join("; ");
        assert!(
            !plan.contains("idx_readings_finished_at"),
            "an index that serves a function of its own column would make \
             the bare-column rewrite unnecessary; it does not: {plan}"
        );
    }

    #[test]
    fn the_reading_order_is_a_total_order() {
        for sort in EVERY_SORT {
            let clause = reading_order_by(sort);
            assert!(
                clause.trim_end().ends_with("readings.id ASC")
                    || clause.trim_end().ends_with("readings.id DESC"),
                "{sort:?} orders by `{clause}`, which ties — and a tie under \
                 LIMIT/OFFSET is a reading on two pages and another on none"
            );
        }
    }

    #[test]
    fn the_started_sort_orders_by_the_key_list_readings_counts_in() {
        let began = reading_began("readings");
        let clause = reading_order_by(ReadingSort::Started);
        assert!(
            clause.starts_with(&format!("{began} DESC")),
            "`{clause}` does not lead with `{began}`"
        );
        assert!(
            reading_age_key("readings").starts_with(&began),
            "the ordinal counts in a different expression from the sort"
        );
    }

    #[test]
    fn the_year_filter_compares_the_bare_column() {
        let p = ReadingFilter {
            finished_in: Some(DayRange::new("2025-01-01", "2025-12-31").unwrap()),
            ..Default::default()
        }
        .predicate();
        assert!(
            !p.sql.contains("date(") && !p.sql.contains("strftime"),
            "a function over the column is an index SQLite declines: {}",
            p.sql
        );
        assert_eq!(
            p.binds,
            vec![
                super::super::query::Bind::Int(1735689600),
                super::super::query::Bind::Int(1767225600),
            ]
        );
    }

    #[test]
    fn an_empty_reading_filter_writes_no_clause() {
        let p = ReadingFilter::default().predicate();
        assert!(p.sql.is_empty());
        assert!(p.binds.is_empty());
        assert!(ReadingFilter::default().is_empty());
        let q = ReadingQuery::default();
        assert!(q.limit < 0, "a negative limit is SQLite's own `no limit`");
        assert_eq!(q.offset, 0);
        assert_eq!(q.sort, ReadingSort::Finished);
    }

    async fn wall(s: &Storage) -> (i64, i64, i64) {
        let a = add(s, "Piranesi").await;
        let b = add(s, "Kokoro").await;
        let c = add(s, "Snow Country").await;
        s.record_reading(
            a,
            day("2024-01-01"),
            day("2024-02-01"),
            STATUS_FINISHED,
            "manual",
        )
        .await
        .unwrap();
        s.record_reading(
            a,
            day("2025-03-01"),
            day("2025-04-01"),
            STATUS_FINISHED,
            "manual",
        )
        .await
        .unwrap();
        s.record_reading(
            b,
            day("2025-05-01"),
            day("2025-06-01"),
            STATUS_FINISHED,
            "manual",
        )
        .await
        .unwrap();
        s.record_reading(
            c,
            day("2024-07-01"),
            day("2024-08-01"),
            STATUS_FINISHED,
            "goodreads",
        )
        .await
        .unwrap();
        s.open_reading(c, day("2026-01-01"), "manual")
            .await
            .unwrap();
        (a, b, c)
    }

    fn day(d: &str) -> Option<i64> {
        ko_datetime_to_unix(&format!("{d} 00:00:00"))
    }

    fn year(y: &str) -> DayRange {
        DayRange::new(&format!("{y}-01-01"), &format!("{y}-12-31")).unwrap()
    }

    #[tokio::test]
    async fn the_reading_count_agrees_with_the_page_for_every_filter() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (a, _, _) = wall(&s).await;

        for filter in [
            ReadingFilter::default(),
            ReadingFilter {
                book_id: Some(a),
                ..Default::default()
            },
            ReadingFilter {
                open: Some(true),
                ..Default::default()
            },
            ReadingFilter {
                open: Some(false),
                ..Default::default()
            },
            ReadingFilter {
                status: Some(ReadingState::Finished),
                ..Default::default()
            },
            ReadingFilter {
                status: Some(ReadingState::Reading),
                ..Default::default()
            },
            ReadingFilter {
                finished_in: Some(year("2024")),
                ..Default::default()
            },
            ReadingFilter {
                finished_in: Some(year("2025")),
                ..Default::default()
            },
            ReadingFilter {
                finished_in: Some(year("2025")),
                book_id: Some(a),
                ..Default::default()
            },
            ReadingFilter {
                finished_in: Some(year("2030")),
                ..Default::default()
            },
        ] {
            let count = s.count_readings(&filter).await.unwrap();
            for sort in EVERY_SORT {
                let rows = s
                    .list_reading_rows(&ReadingQuery::new(-1, sort).with_filter(filter.clone()))
                    .await
                    .unwrap();
                assert_eq!(
                    rows.len() as i64,
                    count,
                    "{filter:?} counted {count} and listed {} under {sort:?}",
                    rows.len()
                );
            }
        }
    }

    #[tokio::test]
    async fn a_page_and_its_successor_partition_the_filtered_list() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        wall(&s).await;
        let filter = ReadingFilter {
            open: Some(false),
            ..Default::default()
        };
        let count = s.count_readings(&filter).await.unwrap();
        assert_eq!(count, 4, "four closed readings on this shelf");

        for sort in EVERY_SORT {
            let query = ReadingQuery::new(2, sort).with_filter(filter.clone());
            let mut walked: Vec<i64> = Vec::new();
            let mut offset = 0;
            loop {
                let page = s
                    .list_reading_rows(&query.clone().at_offset(offset))
                    .await
                    .unwrap();
                if page.is_empty() {
                    break;
                }
                walked.extend(page.iter().map(|r| r.reading.id));
                offset += 2;
            }
            let whole: Vec<i64> = s
                .list_reading_rows(&ReadingQuery::new(-1, sort).with_filter(filter.clone()))
                .await
                .unwrap()
                .iter()
                .map(|r| r.reading.id)
                .collect();
            assert_eq!(walked, whole, "the pages do not reassemble under {sort:?}");
            assert_eq!(walked.len() as i64, count);
        }
    }

    #[tokio::test]
    async fn the_read_number_agrees_with_the_gutter() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (a, b, _) = wall(&s).await;

        for book in [a, b] {
            let readings = s.list_readings(book).await.unwrap();
            let numbering = ReadNumbering::new(&readings);
            let rows = s
                .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                    book_id: Some(book),
                    ..Default::default()
                }))
                .await
                .unwrap();
            assert_eq!(rows.len(), readings.len());
            for row in &rows {
                assert_eq!(
                    row.count.of as usize,
                    readings.len(),
                    "of_reads counts the book's readings"
                );
                assert_eq!(
                    row.count.ordinal().map(|n| n as usize),
                    numbering.number_of(Some(row.reading.id)),
                    "the wall and the gutter number reading {} differently",
                    row.reading.id
                );
                assert_eq!(row.count.tells_reads_apart(), numbering.is_meaningful());
            }
        }
        let twice = s
            .list_reading_rows(&ReadingQuery::new(-1, ReadingSort::Started).with_filter(
                ReadingFilter {
                    book_id: Some(a),
                    ..Default::default()
                },
            ))
            .await
            .unwrap();
        assert_eq!(
            twice.iter().map(|r| r.count.number).collect::<Vec<_>>(),
            vec![2, 1]
        );
        let once = s
            .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                book_id: Some(b),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert_eq!(once[0].count.ordinal(), None, "a lone read has no number");
    }

    #[tokio::test]
    async fn the_read_number_survives_a_filter_that_hides_the_first_read() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (a, _, _) = wall(&s).await;
        let rows = s
            .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                book_id: Some(a),
                finished_in: Some(year("2025")),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "only the 2025 read is in this year");
        assert_eq!(rows[0].count.number, 2, "it is still the second read");
        assert_eq!(rows[0].count.of, 2);
        assert_eq!(rows[0].count.ordinal(), Some(2));
    }

    #[tokio::test]
    async fn the_wall_and_the_single_card_show_the_chosen_passage() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (a, _, _) = wall(&s).await;
        for (text, when) in [
            ("short", "2024-01-15 09:00:00"),
            (
                "the long one that a rule would have shown, dragged across a whole paragraph",
                "2024-01-16 09:00:00",
            ),
            ("the one the reader chose", "2025-03-15 09:00:00"),
        ] {
            s.insert_highlight(a, &hl(text, when)).await.unwrap();
        }
        s.attribute_highlights(a).await.unwrap();

        let rows = s
            .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                book_id: Some(a),
                ..Default::default()
            }))
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter().all(|r| r.passage.is_none()),
            "a passage nobody chose is not the card's"
        );

        let second = rows
            .iter()
            .find(|r| r.count.number == 2)
            .unwrap()
            .reading
            .id;
        let chosen = s
            .highlights_for_reading(second)
            .await
            .unwrap()
            .into_iter()
            .find(|h| h.text == "the one the reader chose")
            .unwrap();
        s.set_card_passage(second, Some(chosen.id)).await.unwrap();

        let rows = s
            .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                book_id: Some(a),
                ..Default::default()
            }))
            .await
            .unwrap();
        for row in &rows {
            let alone = s.card_passage(row.reading.id).await.unwrap();
            assert_eq!(
                row.passage.as_ref().map(|h| h.id),
                alone.as_ref().map(|h| h.id),
                "reading {} shows one passage on the wall and another on its card",
                row.reading.id
            );
        }
        let second_row = rows.iter().find(|r| r.reading.id == second).unwrap();
        assert_eq!(
            second_row.passage.as_ref().map(|h| h.id),
            Some(chosen.id),
            "the wall shows what the reader chose"
        );
        assert!(
            rows.iter()
                .find(|r| r.count.number == 1)
                .unwrap()
                .passage
                .is_none(),
            "a choice is the reading's, never the book's"
        );
    }

    #[tokio::test]
    async fn the_row_progresses_by_its_own_reading() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (_, _, c) = wall(&s).await;
        let readings = s.list_readings(c).await.unwrap();
        sqlx::query("UPDATE readings SET current_page = ? WHERE id = ?")
            .bind(300i64)
            .bind(readings[0].id)
            .execute(s.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE readings SET current_page = ? WHERE id = ?")
            .bind(30i64)
            .bind(readings[1].id)
            .execute(s.pool())
            .await
            .unwrap();

        let rows = s
            .list_reading_rows(&ReadingQuery::default().with_filter(ReadingFilter {
                book_id: Some(c),
                ..Default::default()
            }))
            .await
            .unwrap();
        for row in &rows {
            let own = Progress::of_reading(&row.reading, row.book.page_count);
            assert_eq!(row.progress, own);
        }
        assert_ne!(
            rows[0].progress, rows[1].progress,
            "two reads of one book must be able to report different progress"
        );
    }

    #[tokio::test]
    async fn a_highlight_without_a_datetime_stays_unattributed() {
        let (s, id) = seeded().await;
        s.open_reading(id, Some(0), "manual").await.unwrap();
        let mut h = hl("undated", "");
        h.ko_datetime = None;
        s.insert_highlight(id, &h).await.unwrap();
        assert_eq!(s.attribute_highlights(id).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn the_years_are_the_years_reads_ended_in_newest_first() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        wall(&s).await;
        let y = s.reading_years(&ReadingFilter::default()).await.unwrap();
        assert_eq!(y.years, vec![2025, 2024], "newest first, one entry a year");
        assert!(
            y.open,
            "an open read is in no year, and a picker that did not say so \
             would offer years that do not add up to the wall"
        );
    }

    #[tokio::test]
    async fn every_year_offered_has_rows_and_together_they_are_the_whole_wall() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let (a, _, _) = wall(&s).await;

        for filter in [
            ReadingFilter::default(),
            ReadingFilter {
                book_id: Some(a),
                ..Default::default()
            },
            ReadingFilter {
                status: Some(ReadingState::Finished),
                ..Default::default()
            },
        ] {
            let years = s.reading_years(&filter).await.unwrap();
            let total = s.count_readings(&filter).await.unwrap();

            let mut counted = 0;
            for y in &years.years {
                let scoped = ReadingFilter {
                    finished_in: Some(year(&y.to_string())),
                    ..filter.clone()
                };
                let n = s.count_readings(&scoped).await.unwrap();
                assert!(n > 0, "{y} was offered and holds nothing under {filter:?}");
                counted += n;
            }
            if years.open {
                counted += s
                    .count_readings(&ReadingFilter {
                        open: Some(true),
                        ..filter.clone()
                    })
                    .await
                    .unwrap();
            }
            assert_eq!(
                counted, total,
                "the picker must partition the wall under {filter:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_year_filter_answers_no_open_and_an_open_filter_answers_no_years() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        wall(&s).await;

        let in_2025 = s
            .reading_years(&ReadingFilter {
                finished_in: Some(year("2025")),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(in_2025.years, vec![2025]);
        assert!(!in_2025.open);

        let open = s
            .reading_years(&ReadingFilter {
                open: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(open.years.is_empty());
        assert!(open.open);
    }

    #[tokio::test]
    async fn a_library_with_no_readings_offers_no_years() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        add(&s, "Unread").await;
        assert_eq!(
            s.reading_years(&ReadingFilter::default()).await.unwrap(),
            ReadingYears::default()
        );
    }

    #[tokio::test]
    async fn the_year_boundary_is_the_instant_the_year_filter_binds() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let b = add(&s, "New Year").await;
        s.record_reading(b, None, Some(1_767_225_599), STATUS_FINISHED, "manual")
            .await
            .unwrap();
        let c = add(&s, "New Year, Later").await;
        s.record_reading(c, None, Some(1_767_225_600), STATUS_FINISHED, "manual")
            .await
            .unwrap();

        let y = s.reading_years(&ReadingFilter::default()).await.unwrap();
        assert_eq!(y.years, vec![2026, 2025]);
        for (year_of, expected) in [("2025", 1), ("2026", 1)] {
            let n = s
                .count_readings(&ReadingFilter {
                    finished_in: Some(year(year_of)),
                    ..Default::default()
                })
                .await
                .unwrap();
            assert_eq!(n, expected, "{year_of} must hold what the picker offered");
        }
    }

    #[tokio::test]
    async fn the_year_list_is_covered_by_the_finished_index() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let sql = reading_years_sql(&ReadingFilter::default().predicate());
        let plan = outer_plan(&s, &sql).await.join("; ");
        assert!(
            plan.contains("COVERING INDEX idx_readings_finished_at"),
            "the year list must never touch the table: {plan}"
        );

        let joined = sql.replace(
            "FROM readings ",
            "FROM readings JOIN books ON books.id = readings.book_id ",
        );
        assert_ne!(joined, sql, "the substitution must have bitten");
        let plan = outer_plan(&s, &joined).await.join("; ");
        assert!(
            !plan.contains("COVERING INDEX idx_readings_finished_at"),
            "if the join is free, this assertion is measuring nothing: {plan}"
        );
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn a_book_never_has_two_open_readings(
            steps in proptest::collection::vec(0u8..3, 1..12),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let id = s.upsert_book(&Book { title: Some("t".into()), ..Default::default() }, None)
                    .await.unwrap();

                let mut page = 0i64;
                for step in &steps {
                    match step {
                        0 => { page += 10; s.update_progress(id, Some(page), None).await.unwrap(); }
                        1 => { s.update_progress(id, None, Some(true)).await.unwrap(); }
                        _ => { s.reread(id).await.unwrap(); page = 0; }
                    }
                    let open: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM readings WHERE book_id = ? AND finished_at IS NULL")
                        .bind(id).fetch_one(s.pool()).await.unwrap();
                    prop_assert!(open <= 1, "two open readings after {steps:?}");
                }

                let book = s.get_book(id).await.unwrap().unwrap();
                let readings = s.list_readings(id).await.unwrap();
                let current = readings.iter().find(|r| r.finished_at.is_none())
                    .or_else(|| readings.last())
                    .expect("every sequence writes at least one reading");
                prop_assert_eq!(book.current_page, current.current_page);
                prop_assert_eq!(book.finished, current.status == STATUS_FINISHED);
                prop_assert_eq!(book.date_finished, current.finished_at);
                Ok(())
            })?;
        }

        #[test]
        fn the_open_list_is_exactly_the_books_with_an_open_reading(
            steps in proptest::collection::vec((0usize..3, 0u8..3), 1..16),
        ) {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let mut ids = Vec::new();
                for n in 0..3 {
                    ids.push(s.upsert_book(&Book {
                        title: Some(format!("book {n}")), ..Default::default()
                    }, None).await.unwrap());
                }

                for (which, step) in &steps {
                    let id = ids[*which];
                    match step {
                        0 => { s.update_progress(id, Some(10), None).await.unwrap(); }
                        1 => { s.update_progress(id, None, Some(true)).await.unwrap(); }
                        _ => { s.reread(id).await.unwrap(); }
                    }
                }

                let listed = s.list_open_readings(10).await.unwrap();
                for id in &ids {
                    let active = s.active_reading(*id).await.unwrap();
                    let row = listed.iter().find(|(b, _)| b.id == Some(*id));
                    match (active, row) {
                        (Some(a), Some((_, r))) => prop_assert_eq!(a.id, r.id),
                        (None, None) => {}
                        (a, r) => prop_assert!(
                            false,
                            "book {} listed as {:?} but active is {:?} after {:?}",
                            id, r.map(|(_, r)| r.id), a.map(|a| a.id), steps
                        ),
                    }
                }
                Ok(())
            })?;
        }
    }
}
