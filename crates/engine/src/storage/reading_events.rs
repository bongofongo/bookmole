//! Source-agnostic activity log: every source writes `(book, day, source) -> minutes?, pages?,
//! confidence`. Nothing invents a day, and aggregates report `None`, not zero, when no event
//! carried the value.

use sqlx::{Row, Sqlite, Transaction};

use super::readings::READING_WINDOWS;
use super::{Storage, now_unix};
use crate::error::{EngineError, Result};

pub const SOURCE_KOREADER: &str = "koreader";
pub const SOURCE_VAULT: &str = "vault";
/// A page typed here. Not the reading's own `source`: one word per claimant, so device and hand
/// claims stay separate rows.
pub const SOURCE_LOCAL: &str = "local";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    Measured,
    Inferred,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Measured => "measured",
            Confidence::Inferred => "inferred",
        }
    }

    /// Unrecognised tokens read as `Inferred`: claiming a measurement nobody made is the worse error.
    fn from_db(s: &str) -> Confidence {
        match s {
            "measured" => Confidence::Measured,
            _ => Confidence::Inferred,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingEvent {
    pub book_id: i64,
    pub reading_id: Option<i64>,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    /// `None` is not known, never zero.
    pub minutes: Option<i64>,
    /// `None` is not known, never zero.
    pub pages: Option<i64>,
    pub source: String,
    pub confidence: Confidence,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewReadingEvent {
    pub book_id: i64,
    pub reading_id: Option<i64>,
    pub day: String,
    pub minutes: Option<i64>,
    pub pages: Option<i64>,
    pub source: String,
    pub confidence: Confidence,
}

/// `updated` counts rows that actually changed: the upsert's `WHERE` skips unchanged rows so
/// idempotency is observable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FillStats {
    pub inserted: u64,
    pub updated: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefillReport {
    pub highlights: FillStats,
    pub notes: FillStats,
    pub readings: FillStats,
}

impl RefillReport {
    pub fn inserted(&self) -> u64 {
        self.highlights.inserted + self.notes.inserted + self.readings.inserted
    }

    pub fn updated(&self) -> u64 {
        self.highlights.updated + self.notes.updated + self.readings.updated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayRange {
    from: String,
    to: String,
    /// Inclusive lower bound, unix seconds.
    start: i64,
    /// Exclusive upper bound, unix seconds: midnight UTC closing `to`.
    end: i64,
}

/// UTC, matching `date(x, 'unixepoch')` and `reading_events.day`.
fn midnight_utc(d: time::Date) -> i64 {
    time::PrimitiveDateTime::new(d, time::Time::MIDNIGHT)
        .assume_utc()
        .unix_timestamp()
}

impl DayRange {
    /// Inclusive; refuses a non-day and an inverted span, which would select nothing and report a
    /// confident zero.
    pub fn new(from: &str, to: &str) -> Result<DayRange> {
        let mut days = Vec::with_capacity(2);
        for d in [from, to] {
            match parse_day(d) {
                Some(day) => days.push(day),
                None => {
                    return Err(EngineError::InvalidInput(format!(
                        "{d:?} is not a YYYY-MM-DD day"
                    )));
                }
            }
        }
        if from > to {
            return Err(EngineError::InvalidInput(format!(
                "the range {from}..{to} ends before it starts"
            )));
        }
        Ok(DayRange {
            from: from.to_string(),
            to: to.to_string(),
            start: midnight_utc(days[0]),
            // A unix day is exactly 86 400 seconds (no leap seconds), so this is the next midnight.
            end: midnight_utc(days[1]) + 86_400,
        })
    }

    pub fn from(&self) -> &str {
        &self.from
    }

    pub fn to(&self) -> &str {
        &self.to
    }

    /// `start` inclusive, `end` exclusive, so callers compare the bare column (`col >= ? AND col <
    /// ?`) and an index on it stays usable.
    pub fn unix_bounds(&self) -> (i64, i64) {
        (self.start, self.end)
    }
}

/// `None` means no data, never zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySummary {
    pub range: DayRange,
    pub books_finished: i64,
    pub activity_days: i64,
    pub notes_created: i64,
    /// Attributed to the note: `note_links` has no timestamp and `set_note_links` rewrites the whole
    /// set on each save.
    pub links_created: i64,
    /// `None` when nothing in the period measured minutes, not zero.
    pub minutes: Option<i64>,
    /// `None` when nothing in the period measured pages.
    pub pages: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayActivity {
    pub day: String,
    pub books: i64,
    pub minutes: Option<i64>,
    pub pages: Option<i64>,
}

/// Not `DayActivity` summed: distinct `books` do not add across days. Only months with an event
/// appear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthActivity {
    pub month: String,
    /// Distinct books with an event anywhere in the month; not derivable from the days.
    pub books: i64,
    pub activity_days: i64,
    /// `None` when nothing in the month measured minutes; never `Some(0)`.
    pub minutes: Option<i64>,
    pub pages: Option<i64>,
}

pub fn day_of_unix(ts: i64) -> Result<String> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    Ok(time::OffsetDateTime::from_unix_timestamp(ts)
        .map_err(|e| EngineError::InvalidInput(format!("timestamp {ts} is out of range: {e}")))?
        .format(fmt)?)
}

/// `None` for a stamp that is not one. Taken from the text because the device writes zone-less
/// local time that is read as UTC.
pub fn day_of_ko_datetime(s: &str) -> Option<String> {
    let s = s.trim();
    let day = s.get(..10)?;
    is_day(day).then(|| day.to_string())
}

fn is_day(s: &str) -> bool {
    parse_day(s).is_some()
}

fn parse_day(s: &str) -> Option<time::Date> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(s, fmt).ok()
}

/// No-clobber merge shared by the fillers: a NULL means no opinion and must not erase another's
/// value; `confidence` ratchets to `measured`, so filler order does not matter.
const EVENT_MERGE: &str = "
    reading_id = COALESCE(excluded.reading_id, reading_events.reading_id),
    minutes    = COALESCE(excluded.minutes,    reading_events.minutes),
    pages      = COALESCE(excluded.pages,      reading_events.pages),
    confidence = CASE WHEN 'measured' IN (excluded.confidence, reading_events.confidence)
                      THEN 'measured' ELSE reading_events.confidence END";

/// The `WHERE` on `DO UPDATE`, so `rows_affected` means changed; `IS NOT` because `<>` is not
/// null-safe.
const EVENT_DIFFERS: &str = "
       reading_events.reading_id IS NOT COALESCE(excluded.reading_id, reading_events.reading_id)
    OR reading_events.minutes    IS NOT COALESCE(excluded.minutes,    reading_events.minutes)
    OR reading_events.pages      IS NOT COALESCE(excluded.pages,      reading_events.pages)
    OR (excluded.confidence = 'measured' AND reading_events.confidence <> 'measured')";

const EVENT_COLUMNS: &str =
    "book_id, reading_id, day, minutes, pages, source, confidence, created_at";

fn row_to_event(r: &sqlx::sqlite::SqliteRow) -> Result<ReadingEvent> {
    let confidence: String = r.try_get("confidence")?;
    Ok(ReadingEvent {
        book_id: r.try_get("book_id")?,
        reading_id: r.try_get("reading_id")?,
        day: r.try_get("day")?,
        minutes: r.try_get("minutes")?,
        pages: r.try_get("pages")?,
        source: r.try_get("source")?,
        confidence: Confidence::from_db(&confidence),
        created_at: r.try_get("created_at")?,
    })
}

/// Counts rows because `rows_affected` on an upsert cannot split inserts from changes.
async fn count(tx: &mut Transaction<'_, Sqlite>) -> Result<i64> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reading_events")
            .fetch_one(&mut **tx)
            .await?,
    )
}

async fn fill(tx: &mut Transaction<'_, Sqlite>, sql: &str, now: i64) -> Result<FillStats> {
    let before = count(tx).await?;
    let touched = sqlx::query(sql).bind(now).execute(&mut **tx).await?;
    let after = count(tx).await?;
    let inserted = (after - before).max(0) as u64;
    Ok(FillStats {
        inserted,
        updated: touched.rows_affected().saturating_sub(inserted),
    })
}

impl Storage {
    /// One transaction, since the fillers overlap. Idempotent: a second call with nothing new reports
    /// zero inserted and updated.
    #[tracing::instrument(skip(self))]
    pub(crate) async fn refill_reading_events(&self) -> Result<RefillReport> {
        let now = now_unix();
        let mut tx = self.pool().begin().await?;
        let report = RefillReport {
            highlights: fill(&mut tx, &highlight_fill_sql(), now).await?,
            notes: fill(&mut tx, &note_fill_sql(), now).await?,
            readings: fill(&mut tx, &reading_fill_sql(), now).await?,
        };
        tx.commit().await?;
        tracing::debug!(
            inserted = report.inserted(),
            updated = report.updated(),
            "reading events refilled"
        );
        Ok(report)
    }

    pub async fn fill_events_from_highlights(&self) -> Result<FillStats> {
        let mut tx = self.pool().begin().await?;
        let stats = fill(&mut tx, &highlight_fill_sql(), now_unix()).await?;
        tx.commit().await?;
        Ok(stats)
    }

    pub async fn fill_events_from_notes(&self) -> Result<FillStats> {
        let mut tx = self.pool().begin().await?;
        let stats = fill(&mut tx, &note_fill_sql(), now_unix()).await?;
        tx.commit().await?;
        Ok(stats)
    }

    /// `source` is the reading's own (for example `goodreads`), not a fixed filler word.
    pub async fn fill_events_from_readings(&self) -> Result<FillStats> {
        let mut tx = self.pool().begin().await?;
        let stats = fill(&mut tx, &reading_fill_sql(), now_unix()).await?;
        tx.commit().await?;
        Ok(stats)
    }

    /// Same merge as the fillers, so a hand-written and a derived event for one day combine.
    pub async fn record_reading_events(&self, events: &[NewReadingEvent]) -> Result<FillStats> {
        let now = now_unix();
        let mut tx = self.pool().begin().await?;
        let mut stats = FillStats::default();
        for e in events {
            if !is_day(&e.day) {
                return Err(EngineError::InvalidInput(format!(
                    "{:?} is not a YYYY-MM-DD day",
                    e.day
                )));
            }
            let before = count(&mut tx).await?;
            let touched = sqlx::query(&format!(
                "INSERT INTO reading_events ({EVENT_COLUMNS})
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (book_id, day, source) DO UPDATE SET {EVENT_MERGE}
                 WHERE {EVENT_DIFFERS}"
            ))
            .bind(e.book_id)
            .bind(e.reading_id)
            .bind(&e.day)
            .bind(e.minutes)
            .bind(e.pages)
            .bind(&e.source)
            .bind(e.confidence.as_str())
            .bind(now)
            .execute(&mut *tx)
            .await?;
            let after = count(&mut tx).await?;
            let inserted = (after - before).max(0) as u64;
            stats.inserted += inserted;
            stats.updated += touched.rows_affected().saturating_sub(inserted);
        }
        tx.commit().await?;
        Ok(stats)
    }

    /// Called by `update_progress` only. The first typed page records `pages: None` (a page is not a
    /// count); a lower page adds nothing; later pages add to the day's total; confidence is
    /// `Measured`.
    pub(crate) async fn record_typed_page(
        &self,
        book_id: i64,
        reading_id: i64,
        previous_page: Option<i64>,
        page: i64,
        at: i64,
    ) -> Result<FillStats> {
        let day = day_of_unix(at)?;

        let delta = previous_page.map(|p| page - p).filter(|d| *d > 0);
        let pages = match delta {
            Some(d) => {
                let so_far: Option<i64> = sqlx::query_scalar(
                    "SELECT pages FROM reading_events
                      WHERE book_id = ? AND day = ? AND source = ?",
                )
                .bind(book_id)
                .bind(&day)
                .bind(SOURCE_LOCAL)
                .fetch_optional(self.pool())
                .await?
                .flatten();
                Some(so_far.unwrap_or(0) + d)
            }
            None => None,
        };

        self.record_reading_events(&[NewReadingEvent {
            book_id,
            reading_id: Some(reading_id),
            day,
            // `minutes: None`: a typed page says when and how far, not how long, and `Some(0)` would be a
            // claim.
            minutes: None,
            pages,
            source: SOURCE_LOCAL.to_string(),
            confidence: Confidence::Measured,
        }])
        .await
    }

    /// Uses the same `READING_WINDOWS` as `attribute_highlights`; overlap, not containment, and two
    /// overlapping windows give `None` since the day's minutes cannot be split.
    pub(crate) async fn reading_for_day(&self, book_id: i64, day: &str) -> Result<Option<i64>> {
        if !is_day(day) {
            return Err(EngineError::InvalidInput(format!(
                "{day:?} is not a YYYY-MM-DD day"
            )));
        }
        Ok(sqlx::query_scalar(&format!(
            "WITH windows AS ({READING_WINDOWS}),
                  span AS (SELECT CAST(strftime('%s', ?2 || ' 00:00:00') AS INTEGER) AS d0)
             SELECT CASE WHEN count(*) = 1 THEN min(w.reading_id) END
               FROM windows w, span
              WHERE w.win_start <= span.d0 + 86399 AND w.win_end >= span.d0"
        ))
        .bind(book_id)
        .bind(day)
        .fetch_one(self.pool())
        .await?)
    }

    pub async fn reading_events(&self, book_id: i64) -> Result<Vec<ReadingEvent>> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM reading_events WHERE book_id = ?
             ORDER BY day ASC, source ASC"
        ))
        .bind(book_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(row_to_event).collect()
    }

    /// Counts over instant columns compare the bare column via `unix_bounds` so indexes serve; `day`
    /// is a zero-padded ISO string, so `BETWEEN` on it is the range query. `None` sums mean no data.
    pub(crate) async fn activity_summary(&self, range: &DayRange) -> Result<ActivitySummary> {
        let (from, to) = (range.from(), range.to());
        let (start, end) = range.unix_bounds();

        let books_finished: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM readings
              WHERE finished_at >= ? AND finished_at < ?",
        )
        .bind(start)
        .bind(end)
        .fetch_one(self.pool())
        .await?;

        let activity_days: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT day) FROM reading_events WHERE day BETWEEN ? AND ?",
        )
        .bind(from)
        .bind(to)
        .fetch_one(self.pool())
        .await?;

        let notes_created: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notes WHERE created_at >= ? AND created_at < ?",
        )
        .bind(start)
        .bind(end)
        .fetch_one(self.pool())
        .await?;

        let links_created: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM note_links l JOIN notes n ON n.id = l.from_note
              WHERE n.created_at >= ? AND n.created_at < ?",
        )
        .bind(start)
        .bind(end)
        .fetch_one(self.pool())
        .await?;

        let totals = sqlx::query(
            "SELECT sum(minutes) AS minutes, sum(pages) AS pages
               FROM reading_events WHERE day BETWEEN ? AND ?",
        )
        .bind(from)
        .bind(to)
        .fetch_one(self.pool())
        .await?;

        Ok(ActivitySummary {
            range: range.clone(),
            books_finished,
            activity_days,
            notes_created,
            links_created,
            minutes: totals.try_get("minutes")?,
            pages: totals.try_get("pages")?,
        })
    }

    pub(crate) async fn activity_by_day(&self, range: &DayRange) -> Result<Vec<DayActivity>> {
        let rows = sqlx::query(
            "SELECT day,
                    count(DISTINCT book_id) AS books,
                    sum(minutes)            AS minutes,
                    sum(pages)              AS pages
               FROM reading_events
              WHERE day BETWEEN ? AND ?
              GROUP BY day
              ORDER BY day ASC",
        )
        .bind(range.from())
        .bind(range.to())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(DayActivity {
                    day: r.try_get("day")?,
                    books: r.try_get("books")?,
                    minutes: r.try_get("minutes")?,
                    pages: r.try_get("pages")?,
                })
            })
            .collect()
    }

    /// Grouped by `substr(day, 1, 7)`; a month at the range edge covers only its days inside the
    /// range. Not derivable from `activity_by_day`: `None` minutes and distinct `books` do not sum.
    pub(crate) async fn activity_by_month(&self, range: &DayRange) -> Result<Vec<MonthActivity>> {
        let rows = sqlx::query(
            "SELECT substr(day, 1, 7)    AS month,
                    count(DISTINCT book_id) AS books,
                    count(DISTINCT day)     AS activity_days,
                    sum(minutes)            AS minutes,
                    sum(pages)              AS pages
               FROM reading_events
              WHERE day BETWEEN ? AND ?
              GROUP BY month
              ORDER BY month ASC",
        )
        .bind(range.from())
        .bind(range.to())
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MonthActivity {
                    month: r.try_get("month")?,
                    books: r.try_get("books")?,
                    activity_days: r.try_get("activity_days")?,
                    minutes: r.try_get("minutes")?,
                    pages: r.try_get("pages")?,
                })
            })
            .collect()
    }
}

/// `date()` yields NULL for unreadable stamps, so `WHERE day IS NOT NULL` stops a source
/// inventing a day; a day spanning two readings gets a NULL reading id.
fn upsert_wrapper(select: &str) -> String {
    format!(
        "INSERT INTO reading_events ({EVENT_COLUMNS})
         {select}
         ON CONFLICT (book_id, day, source) DO UPDATE SET {EVENT_MERGE}
         WHERE {EVENT_DIFFERS}"
    )
}

fn highlight_fill_sql() -> String {
    upsert_wrapper(
        "SELECT book_id,
                CASE WHEN count(DISTINCT reading_id) = 1 THEN min(reading_id) END,
                day, NULL, NULL, 'koreader', 'inferred', ?
           FROM (SELECT book_id, reading_id, date(ko_datetime) AS day
                   FROM highlights
                  WHERE ko_datetime IS NOT NULL)
          WHERE day IS NOT NULL
          GROUP BY book_id, day",
    )
}

fn note_fill_sql() -> String {
    // `notes.book_id` is nullable (a deleted book's note) and an event needs a book.
    upsert_wrapper(
        "SELECT book_id,
                CASE WHEN count(DISTINCT reading_id) = 1 THEN min(reading_id) END,
                day, NULL, NULL, 'vault', 'measured', ?
           FROM (SELECT book_id, reading_id, date(created_at, 'unixepoch') AS day
                   FROM notes
                  WHERE book_id IS NOT NULL)
          WHERE day IS NOT NULL
          GROUP BY book_id, day",
    )
}

fn reading_fill_sql() -> String {
    // `UNION ALL` so a read opened and closed on one day collapses to one event; grouped by `source`
    // since it varies per row.
    upsert_wrapper(
        "SELECT book_id,
                CASE WHEN count(DISTINCT id) = 1 THEN min(id) END,
                day, NULL, NULL, source, 'measured', ?
           FROM (SELECT book_id, id, source, date(started_at,  'unixepoch') AS day
                   FROM readings WHERE started_at IS NOT NULL
                  UNION ALL
                 SELECT book_id, id, source, date(finished_at, 'unixepoch') AS day
                   FROM readings WHERE finished_at IS NOT NULL)
          WHERE day IS NOT NULL
          GROUP BY book_id, day, source",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;
    use crate::storage::{NewHighlight, NewNoteMeta};

    const DAY: i64 = 86_400;
    const JAN5: i64 = 1_767_571_200;

    async fn seeded() -> (Storage, i64) {
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
        (s, id)
    }

    fn hl(text: &str, when: Option<&str>) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[1]/text().{text}")),
            pos1: None,
            ko_datetime: when.map(str::to_string),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    async fn note_on(s: &Storage, book: i64, title: &str, at: i64) -> i64 {
        let path = format!("vault/{title}.md");
        let id = s
            .insert_note(
                NewNoteMeta {
                    book_id: Some(book),
                    highlight_id: None,
                    reading_id: None,
                    file_path: &path,
                    title,
                    kind: "note",
                    page: None,
                    location: None,
                },
                "",
                &[],
            )
            .await
            .unwrap();
        sqlx::query("UPDATE notes SET created_at = ? WHERE id = ?")
            .bind(at)
            .bind(id)
            .execute(s.pool())
            .await
            .unwrap();
        id
    }

    async fn days(s: &Storage, book: i64) -> Vec<(String, String)> {
        s.reading_events(book)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.day, e.source))
            .collect()
    }

    async fn snapshot(s: &Storage) -> Vec<ReadingEvent> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM reading_events ORDER BY book_id, day, source"
        ))
        .fetch_all(s.pool())
        .await
        .unwrap();
        rows.iter().map(|r| row_to_event(r).unwrap()).collect()
    }

    #[test]
    fn a_day_is_read_off_a_ko_stamp_and_never_guessed() {
        assert_eq!(
            day_of_ko_datetime("2026-01-05 21:14:08"),
            Some("2026-01-05".into())
        );
        assert_eq!(
            day_of_ko_datetime("  2026-01-05 21:14:08 "),
            Some("2026-01-05".into())
        );
        assert_eq!(day_of_ko_datetime("not a date"), None);
        assert_eq!(day_of_ko_datetime(""), None);
        assert_eq!(day_of_ko_datetime("2026-13-45 00:00:00"), None);
        assert_eq!(day_of_unix(JAN5).unwrap(), "2026-01-05");
        assert_eq!(
            day_of_ko_datetime("2026-01-05 21:14:08").unwrap(),
            day_of_unix(super::super::ko_datetime_to_unix("2026-01-05 21:14:08").unwrap()).unwrap()
        );
    }

    #[test]
    fn a_range_refuses_junk_and_refuses_to_run_backwards() {
        assert!(DayRange::new("2026-01-01", "2026-01-31").is_ok());
        assert!(DayRange::new("2026-01-01", "2026-01-01").is_ok());
        assert!(DayRange::new("2026-1-1", "2026-01-31").is_err());
        assert!(DayRange::new("2026-02-30", "2026-03-31").is_err());
        assert!(
            DayRange::new("2026-01-31", "2026-01-01").is_err(),
            "a backwards range selects nothing, and every aggregate would then \
             report a confident zero"
        );
    }

    #[tokio::test]
    async fn the_bare_bounds_select_exactly_what_the_date_expression_did() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for (from, to) in [
            ("2026-01-01", "2026-01-31"),
            ("2026-01-05", "2026-01-05"),
            ("2025-12-31", "2026-01-01"),
            ("1969-12-30", "1970-01-02"), // before the epoch, where the sign flips
            ("2024-02-28", "2024-02-29"), // a leap day
        ] {
            let range = DayRange::new(from, to).unwrap();
            let (start, end) = range.unix_bounds();
            // Every second around both boundaries: an off-by-one is one second wide.
            for probe in [
                start - 1,
                start,
                start + 1,
                (start + end) / 2,
                end - 1,
                end,
                end + 1,
            ] {
                let by_expression: bool =
                    sqlx::query_scalar("SELECT date(?1, 'unixepoch') BETWEEN ?2 AND ?3")
                        .bind(probe)
                        .bind(from)
                        .bind(to)
                        .fetch_one(s.pool())
                        .await
                        .unwrap();
                assert_eq!(
                    by_expression,
                    probe >= start && probe < end,
                    "{from}..{to} disagrees about unix second {probe}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_highlight_filler_makes_one_inferred_day_per_book_day() {
        let (s, book) = seeded().await;
        for h in [
            hl("a", Some("2026-01-05 08:00:00")),
            hl("b", Some("2026-01-05 21:14:08")),
            hl("c", Some("2026-01-07 09:00:00")),
        ] {
            s.insert_highlight(book, &h).await.unwrap();
        }
        let stats = s.fill_events_from_highlights().await.unwrap();
        assert_eq!(stats.inserted, 2, "two days, not three highlights");
        assert_eq!(stats.updated, 0);

        let events = s.reading_events(book).await.unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].day, "2026-01-05");
        assert_eq!(events[0].source, SOURCE_KOREADER);
        assert_eq!(events[0].confidence, Confidence::Inferred);
        assert_eq!(events[0].minutes, None, "a highlight measures no minutes");
        assert_eq!(events[0].pages, None);
    }

    #[tokio::test]
    async fn a_highlight_without_a_readable_day_produces_no_event() {
        let (s, book) = seeded().await;
        for h in [
            hl("no stamp", None),
            hl("junk", Some("sometime last winter")),
            hl("impossible", Some("2026-13-45 00:00:00")),
            hl("real", Some("2026-01-05 08:00:00")),
        ] {
            s.insert_highlight(book, &h).await.unwrap();
        }
        s.fill_events_from_highlights().await.unwrap();
        assert_eq!(
            days(&s, book).await,
            vec![("2026-01-05".into(), "koreader".into())],
            "three unusable stamps invent no days between them"
        );
    }

    #[tokio::test]
    async fn the_vault_filler_is_measured_and_needs_no_importer() {
        let (s, book) = seeded().await;
        note_on(&s, book, "first", JAN5 + 3600).await;
        note_on(&s, book, "second", JAN5 + 7200).await;
        note_on(&s, book, "later", JAN5 + 3 * DAY).await;

        let stats = s.fill_events_from_notes().await.unwrap();
        assert_eq!(stats.inserted, 2, "two days, three notes");
        let events = s.reading_events(book).await.unwrap();
        assert_eq!(events[0].day, "2026-01-05");
        assert_eq!(events[0].source, SOURCE_VAULT);
        assert_eq!(
            events[0].confidence,
            Confidence::Measured,
            "our own timestamp on our own file"
        );
    }

    #[tokio::test]
    async fn the_reading_filler_carries_the_readings_own_source() {
        let (s, book) = seeded().await;
        s.record_reading(
            book,
            Some(JAN5),
            Some(JAN5 + 5 * DAY),
            "finished",
            "goodreads",
        )
        .await
        .unwrap();

        s.fill_events_from_readings().await.unwrap();
        let events = s.reading_events(book).await.unwrap();
        assert_eq!(events.len(), 2, "an opening day and a closing day");
        assert_eq!(events[0].day, "2026-01-05");
        assert_eq!(events[1].day, "2026-01-10");
        assert!(
            events.iter().all(|e| e.source == "goodreads"),
            "a Goodreads CSV's dates are Goodreads', not this filler's"
        );
        assert!(events.iter().all(|e| e.confidence == Confidence::Measured));
        assert!(
            events.iter().all(|e| e.reading_id.is_some()),
            "one reading owns both endpoints, so both are attributable"
        );
    }

    #[tokio::test]
    async fn a_read_opened_and_closed_in_one_day_is_one_event() {
        let (s, book) = seeded().await;
        s.record_reading(book, Some(JAN5), Some(JAN5 + 3600), "finished", "manual")
            .await
            .unwrap();
        s.fill_events_from_readings().await.unwrap();
        assert_eq!(
            days(&s, book).await,
            vec![("2026-01-05".into(), "manual".into())]
        );
    }

    #[tokio::test]
    async fn a_day_straddling_two_reads_is_left_unattributed() {
        let (s, book) = seeded().await;
        // Two reads that both close on the same day is the shape; the second's
        // start is what puts two ids on one day.
        s.record_reading(
            book,
            Some(JAN5 - 10 * DAY),
            Some(JAN5),
            "finished",
            "manual",
        )
        .await
        .unwrap();
        s.record_reading(book, Some(JAN5), Some(JAN5 + DAY), "finished", "manual")
            .await
            .unwrap();
        s.fill_events_from_readings().await.unwrap();

        let on_jan5 = s
            .reading_events(book)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.day == "2026-01-05")
            .unwrap();
        assert_eq!(
            on_jan5.reading_id, None,
            "picking one of two reads is wrong half the time, and looks right"
        );
    }

    #[tokio::test]
    async fn a_second_refill_changes_nothing() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("a", Some("2026-01-05 08:00:00")))
            .await
            .unwrap();
        note_on(&s, book, "n", JAN5 + 3600).await;
        s.record_reading(book, Some(JAN5), None, "reading", "koreader")
            .await
            .unwrap();

        let first = s.refill_reading_events().await.unwrap();
        assert!(first.inserted() > 0);
        let before = snapshot(&s).await;

        let second = s.refill_reading_events().await.unwrap();
        assert_eq!(second.inserted(), 0, "a refill duplicates nothing");
        assert_eq!(
            second.updated(),
            0,
            "and rewrites nothing either — otherwise idempotency is unobservable"
        );
        assert_eq!(snapshot(&s).await, before);
    }

    #[tokio::test]
    async fn one_koreader_day_holds_both_the_highlight_and_the_endpoint() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("a", Some("2026-01-05 08:00:00")))
            .await
            .unwrap();
        s.record_reading(book, Some(JAN5), None, "reading", "koreader")
            .await
            .unwrap();

        s.fill_events_from_highlights().await.unwrap();
        let inferred = s.reading_events(book).await.unwrap();
        assert_eq!(inferred.len(), 1);
        assert_eq!(inferred[0].confidence, Confidence::Inferred);

        let stats = s.fill_events_from_readings().await.unwrap();
        assert_eq!(stats.inserted, 0, "same book, same day, same source");
        assert_eq!(stats.updated, 1);
        let merged = s.reading_events(book).await.unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(
            merged[0].confidence,
            Confidence::Measured,
            "confidence ratchets up when a measured filler reaches the same day"
        );
        assert!(merged[0].reading_id.is_some());
    }

    #[tokio::test]
    async fn a_recorded_event_merges_with_a_derived_one_rather_than_fighting_it() {
        let (s, book) = seeded().await;
        s.insert_highlight(book, &hl("a", Some("2026-01-05 08:00:00")))
            .await
            .unwrap();
        s.fill_events_from_highlights().await.unwrap();

        let stats = s
            .record_reading_events(&[NewReadingEvent {
                book_id: book,
                reading_id: None,
                day: "2026-01-05".into(),
                minutes: Some(42),
                pages: Some(19),
                source: SOURCE_KOREADER.into(),
                confidence: Confidence::Measured,
            }])
            .await
            .unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.updated, 1);

        let e = &s.reading_events(book).await.unwrap()[0];
        assert_eq!(e.minutes, Some(42));
        assert_eq!(e.pages, Some(19));
        assert_eq!(e.confidence, Confidence::Measured);

        s.fill_events_from_highlights().await.unwrap();
        assert_eq!(s.reading_events(book).await.unwrap()[0].minutes, Some(42));
    }

    #[tokio::test]
    async fn deleting_a_reading_keeps_the_days_it_explained() {
        let (s, book) = seeded().await;
        let r = s
            .record_reading(book, Some(JAN5), Some(JAN5 + DAY), "finished", "manual")
            .await
            .unwrap();
        s.fill_events_from_readings().await.unwrap();
        sqlx::query("DELETE FROM readings WHERE id = ?")
            .bind(r)
            .execute(s.pool())
            .await
            .unwrap();

        let events = s.reading_events(book).await.unwrap();
        assert_eq!(events.len(), 2, "the days survive the read");
        assert!(events.iter().all(|e| e.reading_id.is_none()));
    }

    #[tokio::test]
    async fn deleting_a_book_takes_its_events() {
        let (s, book) = seeded().await;
        note_on(&s, book, "n", JAN5).await;
        s.refill_reading_events().await.unwrap();
        assert!(!s.reading_events(book).await.unwrap().is_empty());
        s.delete_book(book).await.unwrap();
        assert!(s.reading_events(book).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_recorded_event_refuses_a_day_that_is_not_one() {
        let (s, book) = seeded().await;
        let bad = NewReadingEvent {
            book_id: book,
            reading_id: None,
            day: "yesterday".into(),
            minutes: Some(10),
            pages: None,
            source: "local".into(),
            confidence: Confidence::Measured,
        };
        assert!(matches!(
            s.record_reading_events(&[bad]).await,
            Err(EngineError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn a_period_with_no_device_data_has_no_minutes_rather_than_zero() {
        let (s, book) = seeded().await;
        note_on(&s, book, "n", JAN5 + 3600).await;
        s.refill_reading_events().await.unwrap();

        let jan = DayRange::new("2026-01-01", "2026-01-31").unwrap();
        let sum = s.activity_summary(&jan).await.unwrap();
        assert_eq!(sum.activity_days, 1, "the vault knows you were here");
        assert_eq!(sum.notes_created, 1);
        assert_eq!(
            sum.minutes, None,
            "nothing measured minutes, and zero would be a claim"
        );
        assert_eq!(sum.pages, None);

        let feb = DayRange::new("2026-02-01", "2026-02-28").unwrap();
        let sum = s.activity_summary(&feb).await.unwrap();
        assert_eq!(sum.activity_days, 0);
        assert_eq!(sum.minutes, None);
    }

    #[tokio::test]
    async fn a_measured_zero_is_not_an_absent_one() {
        let (s, book) = seeded().await;
        s.record_reading_events(&[NewReadingEvent {
            book_id: book,
            reading_id: None,
            day: "2026-01-05".into(),
            // A device that says you opened the book and turned no pages is
            // saying something; it is not saying nothing.
            minutes: Some(3),
            pages: Some(0),
            source: SOURCE_KOREADER.into(),
            confidence: Confidence::Measured,
        }])
        .await
        .unwrap();

        let jan = DayRange::new("2026-01-01", "2026-01-31").unwrap();
        let sum = s.activity_summary(&jan).await.unwrap();
        assert_eq!(sum.pages, Some(0));
        assert_ne!(sum.pages, None);
        assert_eq!(sum.minutes, Some(3));
    }

    #[tokio::test]
    async fn the_named_aggregates_count_what_the_period_holds() {
        let (s, book) = seeded().await;
        s.record_reading(book, Some(JAN5), Some(JAN5 + 5 * DAY), "finished", "manual")
            .await
            .unwrap();
        let a = note_on(&s, book, "alpha", JAN5 + 3600).await;
        note_on(&s, book, "beta", JAN5 + 2 * DAY).await;
        s.set_note_links(a, "alpha", &["beta".into(), "gamma".into()])
            .await
            .unwrap();
        s.refill_reading_events().await.unwrap();

        let jan = DayRange::new("2026-01-01", "2026-01-31").unwrap();
        let sum = s.activity_summary(&jan).await.unwrap();
        assert_eq!(sum.books_finished, 1);
        assert_eq!(sum.notes_created, 2);
        assert_eq!(sum.links_created, 2, "a resolved edge and a dangling one");
        assert_eq!(
            sum.activity_days, 3,
            "the two endpoints and the day of the second note"
        );

        let by_day = s.activity_by_day(&jan).await.unwrap();
        assert_eq!(
            by_day.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(),
            vec!["2026-01-05", "2026-01-07", "2026-01-10"]
        );
        assert!(by_day.iter().all(|d| d.books == 1 && d.minutes.is_none()));

        let dec = DayRange::new("2025-12-01", "2025-12-31").unwrap();
        let sum = s.activity_summary(&dec).await.unwrap();
        assert_eq!(
            (sum.books_finished, sum.activity_days, sum.notes_created),
            (0, 0, 0)
        );
        assert_eq!(sum.minutes, None);
    }

    #[tokio::test]
    async fn the_day_index_is_the_plan_the_planner_picks() {
        let (s, book) = seeded().await;
        for n in 0..40 {
            s.record_reading_events(&[NewReadingEvent {
                book_id: book,
                reading_id: None,
                day: format!("2026-01-{:02}", (n % 28) + 1),
                minutes: Some(n),
                pages: None,
                source: format!("s{n}"),
                confidence: Confidence::Measured,
            }])
            .await
            .unwrap();
        }
        sqlx::query("ANALYZE").execute(s.pool()).await.unwrap();

        let plan = sqlx::query(
            "EXPLAIN QUERY PLAN
             SELECT count(DISTINCT day) FROM reading_events WHERE day BETWEEN ? AND ?",
        )
        .bind("2026-01-01")
        .bind("2026-01-07")
        .fetch_all(s.pool())
        .await
        .unwrap()
        .iter()
        .map(|r| r.try_get::<String, _>("detail").unwrap())
        .collect::<Vec<_>>()
        .join("; ");
        assert!(
            plan.contains("idx_reading_events_day"),
            "the period aggregate must not scan the log: {plan}"
        );
    }

    async fn event(s: &Storage, book: i64, day: &str, minutes: Option<i64>) {
        s.record_reading_events(&[NewReadingEvent {
            book_id: book,
            reading_id: None,
            day: day.into(),
            minutes,
            pages: None,
            source: SOURCE_KOREADER.into(),
            confidence: Confidence::Measured,
        }])
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_months_minutes_sum_only_what_was_measured() {
        let (s, book) = seeded().await;
        event(&s, book, "2026-03-02", None).await;
        event(&s, book, "2026-03-05", Some(40)).await;
        event(&s, book, "2026-03-09", None).await;
        event(&s, book, "2026-03-21", Some(20)).await;
        event(&s, book, "2026-04-03", None).await;
        event(&s, book, "2026-04-04", None).await;

        let year = DayRange::new("2026-01-01", "2026-12-31").unwrap();
        let months = s.activity_by_month(&year).await.unwrap();

        let march = months.iter().find(|m| m.month == "2026-03").unwrap();
        assert_eq!(
            march.minutes,
            Some(60),
            "the unmeasured days must contribute nothing, not zero"
        );
        assert_eq!(march.activity_days, 4, "all four days still happened");

        let april = months.iter().find(|m| m.month == "2026-04").unwrap();
        assert_eq!(
            april.minutes, None,
            "a month with no device data has no minutes; it does not have zero"
        );
        assert_ne!(april.minutes, Some(0));
        assert_eq!(april.activity_days, 2, "and the days are still known");
        assert_eq!(april.pages, None);
    }

    #[tokio::test]
    async fn a_months_books_are_distinct_over_the_month_and_do_not_sum_from_days() {
        let (s, a) = seeded().await;
        let b = s
            .upsert_book(
                &Book {
                    title: Some("Piranesi".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();

        for day in ["2026-05-01", "2026-05-02", "2026-05-03"] {
            event(&s, a, day, None).await;
        }
        event(&s, b, "2026-05-02", None).await;

        let may = DayRange::new("2026-05-01", "2026-05-31").unwrap();
        let by_day = s.activity_by_day(&may).await.unwrap();
        let summed: i64 = by_day.iter().map(|d| d.books).sum();
        assert_eq!(summed, 4, "three days of one book and one day of two");

        let months = s.activity_by_month(&may).await.unwrap();
        assert_eq!(months.len(), 1);
        assert_eq!(
            months[0].books, 2,
            "two books were open that month, however many days each was"
        );
        assert_ne!(
            months[0].books, summed,
            "summing the days is the arithmetic this method exists to refuse"
        );
        assert_eq!(months[0].activity_days, 3);
    }

    #[tokio::test]
    async fn only_months_carrying_an_event_appear() {
        let (s, book) = seeded().await;
        event(&s, book, "2026-03-11", Some(5)).await;
        event(&s, book, "2026-05-02", Some(7)).await;

        let year = DayRange::new("2026-01-01", "2026-12-31").unwrap();
        let months = s.activity_by_month(&year).await.unwrap();
        assert_eq!(
            months.iter().map(|m| m.month.as_str()).collect::<Vec<_>>(),
            vec!["2026-03", "2026-05"],
            "April is not a row of zeroes, and neither are the other nine"
        );

        let before = DayRange::new("2025-01-01", "2025-12-31").unwrap();
        assert!(s.activity_by_month(&before).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_month_at_the_edge_covers_only_the_days_inside_the_range() {
        let (s, book) = seeded().await;
        for day in ["2026-01-05", "2026-01-25", "2026-02-05", "2026-02-20"] {
            event(&s, book, day, Some(10)).await;
        }

        let straddling = DayRange::new("2026-01-20", "2026-02-10").unwrap();
        let months = s.activity_by_month(&straddling).await.unwrap();
        assert_eq!(
            months.iter().map(|m| m.month.as_str()).collect::<Vec<_>>(),
            vec!["2026-01", "2026-02"]
        );
        assert_eq!(months[0].activity_days, 1, "only the 25th is in range");
        assert_eq!(months[0].minutes, Some(10));
        assert_eq!(months[1].activity_days, 1, "only the 5th is in range");
        assert_eq!(months[1].minutes, Some(10));

        let sum = s.activity_summary(&straddling).await.unwrap();
        assert_eq!(sum.activity_days, 2);
        assert_eq!(sum.minutes, Some(20));
    }

    #[tokio::test]
    async fn an_inverted_range_never_reaches_the_month_aggregate() {
        assert!(DayRange::new("2026-12-31", "2026-01-01").is_err());
    }

    async fn local_rows(s: &Storage, book: i64) -> Vec<(String, Option<i64>, Option<i64>, bool)> {
        s.reading_events(book)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.source == SOURCE_LOCAL)
            .map(|e| {
                (
                    e.day,
                    e.minutes,
                    e.pages,
                    e.confidence == Confidence::Measured,
                )
            })
            .collect()
    }

    async fn open_read(s: &Storage, book: i64) -> i64 {
        s.open_reading(book, Some(JAN5), "manual").await.unwrap()
    }

    #[tokio::test]
    async fn the_first_typed_page_files_the_day_and_claims_no_pages() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, None, 42, JAN5).await.unwrap();

        assert_eq!(
            local_rows(&s, b).await,
            vec![("2026-01-05".to_string(), None, None, true)],
            "\"you are on page 42\" is not \"you read 42 pages today\""
        );
    }

    #[tokio::test]
    async fn a_second_page_claims_the_difference_and_nothing_more() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, None, 42, JAN5).await.unwrap();
        s.record_typed_page(b, r, Some(42), 90, JAN5).await.unwrap();

        assert_eq!(local_rows(&s, b).await[0].2, Some(48));
    }

    #[tokio::test]
    async fn two_updates_in_one_day_accumulate_rather_than_replace() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, Some(0), 10, JAN5).await.unwrap();
        s.record_typed_page(b, r, Some(10), 30, JAN5).await.unwrap();

        assert_eq!(local_rows(&s, b).await[0].2, Some(30), "10 + 20, not 20");
    }

    #[tokio::test]
    async fn correcting_a_page_downwards_is_not_negative_pages() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, Some(0), 200, JAN5).await.unwrap();
        s.record_typed_page(b, r, Some(200), 190, JAN5)
            .await
            .unwrap();

        assert_eq!(local_rows(&s, b).await[0].2, Some(200));
    }

    #[tokio::test]
    async fn retyping_the_same_page_touches_no_row() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, Some(0), 55, JAN5).await.unwrap();

        let again = s.record_typed_page(b, r, Some(55), 55, JAN5).await.unwrap();
        assert_eq!(
            again,
            FillStats::default(),
            "an unchanged day must report neither an insert nor an update"
        );
    }

    #[tokio::test]
    async fn two_days_of_typing_are_two_rows() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, Some(0), 20, JAN5).await.unwrap();
        s.record_typed_page(b, r, Some(20), 55, JAN5 + DAY)
            .await
            .unwrap();

        let rows = local_rows(&s, b).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].2, Some(20));
        assert_eq!(rows[1].2, Some(35));
    }

    #[tokio::test]
    async fn a_typed_page_measures_pages_and_never_minutes() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, Some(1), 2, JAN5).await.unwrap();

        let (_, minutes, _, measured) = local_rows(&s, b).await[0].clone();
        assert_eq!(minutes, None);
        assert!(measured);
    }

    #[tokio::test]
    async fn the_event_names_the_reading_it_was_written_to() {
        let (s, b) = seeded().await;
        let r = open_read(&s, b).await;
        s.record_typed_page(b, r, None, 7, JAN5).await.unwrap();

        let e = s.reading_events(b).await.unwrap();
        assert_eq!(e[0].reading_id, Some(r));
    }

    #[tokio::test]
    async fn update_progress_files_the_day_by_itself() {
        let (s, b) = seeded().await;
        s.update_progress(b, Some(30), None).await.unwrap();

        let rows = local_rows(&s, b).await;
        assert_eq!(rows.len(), 1, "a typed page files exactly one local day");
        assert_eq!(rows[0].1, None);
        assert!(rows[0].3, "the user typed it, so it is measured");
    }

    #[tokio::test]
    async fn toggling_finished_alone_files_nothing_local() {
        let (s, b) = seeded().await;
        s.update_progress(b, None, Some(true)).await.unwrap();
        assert!(
            local_rows(&s, b).await.is_empty(),
            "closing a read is `fill_events_from_readings`' business, not this one"
        );
    }

    #[tokio::test]
    async fn a_typed_page_and_the_reads_own_endpoint_share_a_day_without_colliding() {
        let (s, b) = seeded().await;
        s.update_progress(b, Some(12), None).await.unwrap();
        s.refill_reading_events().await.unwrap();

        let mut sources: Vec<String> = s
            .reading_events(b)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.source)
            .collect();
        sources.sort();
        assert_eq!(sources, vec!["local".to_string(), "manual".to_string()]);
    }

    #[tokio::test]
    async fn refilling_does_not_erase_a_typed_days_pages() {
        let (s, b) = seeded().await;
        s.update_progress(b, Some(10), None).await.unwrap();
        s.update_progress(b, Some(40), None).await.unwrap();
        let before = local_rows(&s, b).await;
        assert_eq!(before[0].2, Some(30));

        s.refill_reading_events().await.unwrap();
        s.refill_reading_events().await.unwrap();
        assert_eq!(local_rows(&s, b).await, before);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use crate::storage::{NewHighlight, NewNoteMeta};
    use proptest::prelude::*;

    const JAN5_UTC: i64 = 1_767_571_200;

    #[derive(Debug, Clone)]
    enum Evidence {
        Highlight { book: usize, day: u8 },
        Note { book: usize, day: u8 },
        Reading { book: usize, start: u8, len: u8 },
        Measured { book: usize, day: u8, minutes: u8 },
    }

    fn evidence() -> impl Strategy<Value = Evidence> {
        prop_oneof![
            (0usize..3, 0u8..20).prop_map(|(book, day)| Evidence::Highlight { book, day }),
            (0usize..3, 0u8..20).prop_map(|(book, day)| Evidence::Note { book, day }),
            (0usize..3, 0u8..20, 0u8..5).prop_map(|(book, start, len)| Evidence::Reading {
                book,
                start,
                len
            }),
            (0usize..3, 0u8..20, 0u8..90).prop_map(|(book, day, minutes)| Evidence::Measured {
                book,
                day,
                minutes
            }),
        ]
    }

    const DAY: i64 = 86_400;
    const BASE: i64 = 1_767_225_600;

    async fn seed(s: &Storage, books: &[i64], ev: &[Evidence], n: &mut usize) {
        for e in ev {
            *n += 1;
            match *e {
                Evidence::Highlight { book, day } => {
                    let stamp = day_of_unix(BASE + i64::from(day) * DAY).unwrap();
                    s.insert_highlight(
                        books[book],
                        &NewHighlight {
                            text: format!("h{n}"),
                            chapter: None,
                            page: Some(1),
                            pos0: Some(format!("/p[{n}]")),
                            pos1: None,
                            ko_datetime: Some(format!("{stamp} 12:00:00")),
                            ko_datetime_updated: None,
                            color: None,
                            note: None,
                            source: "koreader".into(),
                        },
                    )
                    .await
                    .unwrap();
                }
                Evidence::Note { book, day } => {
                    let path = format!("vault/n{n}.md");
                    let title = format!("n{n}");
                    let id = s
                        .insert_note(
                            NewNoteMeta {
                                book_id: Some(books[book]),
                                highlight_id: None,
                                reading_id: None,
                                file_path: &path,
                                title: &title,
                                kind: "note",
                                page: None,
                                location: None,
                            },
                            "",
                            &[],
                        )
                        .await
                        .unwrap();
                    sqlx::query("UPDATE notes SET created_at = ? WHERE id = ?")
                        .bind(BASE + i64::from(day) * DAY + 3600)
                        .bind(id)
                        .execute(s.pool())
                        .await
                        .unwrap();
                }
                Evidence::Reading { book, start, len } => {
                    let from = BASE + i64::from(start) * DAY;
                    s.record_reading(
                        books[book],
                        Some(from),
                        Some(from + i64::from(len) * DAY),
                        "finished",
                        "manual",
                    )
                    .await
                    .unwrap();
                }
                Evidence::Measured { book, day, minutes } => {
                    s.record_reading_events(&[NewReadingEvent {
                        book_id: books[book],
                        reading_id: None,
                        day: day_of_unix(BASE + i64::from(day) * DAY).unwrap(),
                        minutes: Some(i64::from(minutes)),
                        pages: None,
                        source: SOURCE_KOREADER.into(),
                        confidence: Confidence::Measured,
                    }])
                    .await
                    .unwrap();
                }
            }
        }
    }

    async fn library(ev: &[Evidence]) -> Storage {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let mut books = Vec::new();
        for n in 0..3 {
            books.push(
                s.upsert_book(
                    &Book {
                        title: Some(format!("book {n}")),
                        ..Default::default()
                    },
                    None,
                )
                .await
                .unwrap(),
            );
        }
        let mut n = 0;
        seed(&s, &books, ev, &mut n).await;
        s
    }

    async fn snapshot(s: &Storage) -> Vec<ReadingEvent> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLUMNS} FROM reading_events ORDER BY book_id, day, source"
        ))
        .fetch_all(s.pool())
        .await
        .unwrap();
        rows.iter().map(|r| row_to_event(r).unwrap()).collect()
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    proptest! {
        #[test]
        fn a_days_pages_are_the_forward_moves_of_that_day(
            pages in proptest::collection::vec(0i64..500, 1..12),
        ) {
            rt().block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let b = s.upsert_book(&Book { title: Some("P".into()), ..Default::default() }, None)
                    .await.unwrap();
                let r = s.open_reading(b, Some(JAN5_UTC), "manual").await.unwrap();

                let mut expected = 0i64;
                let mut prev: Option<i64> = None;
                for p in &pages {
                    s.record_typed_page(b, r, prev, *p, JAN5_UTC).await.unwrap();
                    if let Some(q) = prev {
                        expected += (p - q).max(0);
                    }
                    prev = Some(*p);
                }

                let row = s.reading_events(b).await.unwrap();
                let local: Vec<_> = row.iter().filter(|e| e.source == SOURCE_LOCAL).collect();
                prop_assert_eq!(local.len(), 1, "one book, one day, one local row");
                if expected == 0 {
                    prop_assert_eq!(local[0].pages, None);
                } else {
                    prop_assert_eq!(local[0].pages, Some(expected));
                }
                prop_assert_eq!(local[0].minutes, None);
                prop_assert_eq!(local[0].confidence, Confidence::Measured);
                Ok(())
            })?;
        }

        #[test]
        fn refilling_is_idempotent_and_order_independent(
            ev in proptest::collection::vec(evidence(), 0..14),
        ) {
            rt().block_on(async {
                let a = library(&ev).await;
                a.refill_reading_events().await.unwrap();
                let once = snapshot(&a).await;

                let again = a.refill_reading_events().await.unwrap();
                prop_assert_eq!(again.inserted(), 0, "a refill duplicated a row");
                prop_assert_eq!(again.updated(), 0, "a refill rewrote an unchanged row");
                prop_assert_eq!(snapshot(&a).await, once.clone());

                let b = library(&ev).await;
                for _ in 0..2 {
                    b.fill_events_from_readings().await.unwrap();
                    b.fill_events_from_notes().await.unwrap();
                    b.fill_events_from_highlights().await.unwrap();
                }
                let reversed = snapshot(&b).await;

                // `created_at` is a wall clock, so compare only what the fillers decide.
                prop_assert_eq!(reversed.len(), once.len());
                for (x, y) in reversed.iter().zip(once.iter()) {
                    prop_assert_eq!(&x.day, &y.day);
                    prop_assert_eq!(&x.source, &y.source);
                    prop_assert_eq!(x.book_id, y.book_id);
                    prop_assert_eq!(x.reading_id, y.reading_id);
                    prop_assert_eq!(x.minutes, y.minutes);
                    prop_assert_eq!(x.pages, y.pages);
                    prop_assert_eq!(x.confidence, y.confidence);
                }
                Ok(())
            })?;
        }

        #[test]
        fn minutes_are_absent_exactly_when_nothing_measured_them(
            ev in proptest::collection::vec(evidence(), 0..14),
            (from, to) in (0u8..20, 0u8..20),
        ) {
            let (from, to) = (from.min(to), from.max(to));
            rt().block_on(async {
                let s = library(&ev).await;
                s.refill_reading_events().await.unwrap();

                let range = DayRange::new(
                    &day_of_unix(BASE + i64::from(from) * DAY).unwrap(),
                    &day_of_unix(BASE + i64::from(to) * DAY).unwrap(),
                ).unwrap();

                let events: Vec<ReadingEvent> = snapshot(&s).await
                    .into_iter()
                    .filter(|e| e.day.as_str() >= range.from() && e.day.as_str() <= range.to())
                    .collect();
                let measured: Vec<i64> = events.iter().filter_map(|e| e.minutes).collect();

                let sum = s.activity_summary(&range).await.unwrap();
                if measured.is_empty() {
                    prop_assert_eq!(sum.minutes, None, "zero is a claim this period cannot make");
                } else {
                    prop_assert_eq!(sum.minutes, Some(measured.iter().sum::<i64>()));
                }
                prop_assert_eq!(sum.pages, None, "no filler here supplies pages");

                let mut days: Vec<&str> = events.iter().map(|e| e.day.as_str()).collect();
                days.sort_unstable();
                days.dedup();
                prop_assert_eq!(sum.activity_days, days.len() as i64);
                prop_assert_eq!(
                    s.activity_by_day(&range).await.unwrap().len(),
                    days.len()
                );
                Ok(())
            })?;
        }

        #[test]
        fn months_regroup_the_days_and_books_is_the_field_that_does_not(
            ev in proptest::collection::vec(evidence(), 0..14),
            (from, to) in (0u8..20, 0u8..20),
        ) {
            let (from, to) = (from.min(to), from.max(to));
            rt().block_on(async {
                let s = library(&ev).await;
                s.refill_reading_events().await.unwrap();

                let range = DayRange::new(
                    &day_of_unix(BASE + i64::from(from) * DAY).unwrap(),
                    &day_of_unix(BASE + i64::from(to) * DAY).unwrap(),
                ).unwrap();

                let events: Vec<ReadingEvent> = snapshot(&s).await
                    .into_iter()
                    .filter(|e| e.day.as_str() >= range.from() && e.day.as_str() <= range.to())
                    .collect();

                let by_day = s.activity_by_day(&range).await.unwrap();
                let months = s.activity_by_month(&range).await.unwrap();

                let mut want: Vec<String> =
                    by_day.iter().map(|d| d.day[..7].to_string()).collect();
                want.dedup();
                prop_assert_eq!(
                    months.iter().map(|m| m.month.clone()).collect::<Vec<_>>(),
                    want
                );

                for m in &months {
                    let days: Vec<&DayActivity> =
                        by_day.iter().filter(|d| d.day.starts_with(&m.month)).collect();

                    prop_assert_eq!(m.activity_days, days.len() as i64);
                    let measured: Vec<i64> = days.iter().filter_map(|d| d.minutes).collect();
                    if measured.is_empty() {
                        prop_assert_eq!(
                            m.minutes, None,
                            "a month whose every day was unmeasured cannot claim a number"
                        );
                    } else {
                        prop_assert_eq!(m.minutes, Some(measured.iter().sum::<i64>()));
                    }

                    let mut books: Vec<i64> = events.iter()
                        .filter(|e| e.day.starts_with(&m.month))
                        .map(|e| e.book_id)
                        .collect();
                    books.sort_unstable();
                    books.dedup();
                    prop_assert_eq!(m.books, books.len() as i64);

                    let most = days.iter().map(|d| d.books).max().unwrap_or(0);
                    let summed: i64 = days.iter().map(|d| d.books).sum();
                    prop_assert!(
                        m.books >= most && m.books <= summed,
                        "a month holds at least its busiest day's books and at most their sum"
                    );
                }
                Ok(())
            })?;
        }
    }
}
