//! The reads a reading history and a library table are derived from.
//!
//! Every query here is a **whole-library read of a narrow shape** — a reading
//! with its count and rating, a day of the activity log, a mark's instant —
//! and the figures a frontend draws are derived from them in Rust by
//! [`crate::history`] and [`crate::library`], where the arithmetic is tested as
//! properties without a database. That is `note_rows.rs`' arrangement, one
//! subject over: a library is hundreds of books and a few thousand marks, and
//! one cheap read of each beats a statement per figure that would have to agree
//! with its neighbours about what *in the span* means.
//!
//! **Nothing here reads a mark's text.** Passages and notes cross as ids,
//! books and instants only.

use std::collections::HashMap;

use sqlx::Row;

use super::ratings::row_to_scale;
use super::readings::{
    ALL_READING_WINDOWS, reading_age_key, reading_columns_as, row_to_reading_prefixed,
};
use super::{Rating, ReadCount, Reading, Storage};
use crate::error::Result;

/// One reading, with what History says about it beside the row.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadFacts {
    pub reading: Reading,
    /// `readings.put_down_at` (migration `0025`), **only while the status is
    /// `abandoned`** — a stale stamp on a reading picked up again is not read.
    pub put_down_at: Option<i64>,
    /// Which read of its book, over every sibling — `list_reading_rows`' count.
    pub count: ReadCount,
    /// The rating on this read's review, raw and with its scale.
    pub rating: Option<Rating>,
}

/// One row of the activity log, as History sums it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayFact {
    pub book_id: i64,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub minutes: Option<i64>,
    pub pages: Option<i64>,
    pub source: String,
}

/// A passage or a note, reduced to when, where and whose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkStamp {
    pub id: i64,
    /// Always `Some` for a passage; a note whose book was deleted has none.
    pub book_id: Option<i64>,
    /// As stored: `highlights.reading_id` (attributed by window at import) or
    /// `notes.reading_id` (set for a reflection or a review, `None` for an
    /// ordinary note — see [`crate::history::attribute_note`]).
    pub reading_id: Option<i64>,
    /// Unix seconds. A passage's is its device stamp (`ko_datetime`, read as
    /// UTC like every other reader of it) and else when it was stored; a
    /// note's is `created_at`.
    pub at: i64,
    /// `highlights.source` for a passage (`koreader`, `user`); `vault` for a
    /// note.
    pub source: String,
    /// `passage`, or the note's kind (`note`, `reflection`, `review`, …).
    pub kind: String,
}

/// One reading's window, as `attribute_highlights` draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWindow {
    pub reading_id: i64,
    pub book_id: i64,
    pub start: i64,
    pub end: i64,
}

impl Storage {
    /// Every reading in the library, oldest first by the read-number key, with
    /// its put-down instant, its count among its book's reads, and its
    /// review's rating.
    ///
    /// The rating is a `LEFT JOIN` through the review note, which
    /// `idx_one_review` makes at most one per reading — so the join cannot
    /// duplicate a row.
    pub async fn history_reads(&self) -> Result<Vec<ReadFacts>> {
        let reading = reading_columns_as("readings", "r_");
        let mine = reading_age_key("readings");
        let sibling = reading_age_key("sib");
        let sql = format!(
            "SELECT {reading},
                    CASE WHEN readings.status = 'abandoned' THEN readings.put_down_at END
                        AS put_down_at,
                    (SELECT count(*) FROM readings sib
                      WHERE sib.book_id = readings.book_id) AS of_reads,
                    (SELECT count(*) FROM readings sib
                      WHERE sib.book_id = readings.book_id
                        AND ({sibling}) <= ({mine})) AS read_number,
                    s.id AS id, s.name AS name, s.min AS min, s.max AS max, s.step AS step,
                    rr.value AS rating_value
               FROM readings
               LEFT JOIN notes rv ON rv.reading_id = readings.id AND rv.kind = 'review'
               LEFT JOIN review_ratings rr ON rr.note_id = rv.id
               LEFT JOIN rating_scales s ON s.id = rr.scale_id
              ORDER BY {mine}"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|r| {
                let value: Option<f64> = r.try_get("rating_value")?;
                Ok(ReadFacts {
                    reading: row_to_reading_prefixed(r, "r_")?,
                    put_down_at: r.try_get("put_down_at")?,
                    count: ReadCount::new(r.try_get("read_number")?, r.try_get("of_reads")?),
                    rating: value.map(|value| Rating {
                        scale: row_to_scale(r),
                        value,
                    }),
                })
            })
            .collect()
    }

    /// The activity log, oldest day first, inside `from..=to` when given (both
    /// `YYYY-MM-DD`; either end may be open).
    pub async fn history_events(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<DayFact>> {
        let rows = sqlx::query(
            "SELECT book_id, day, minutes, pages, source FROM reading_events
              WHERE (?1 IS NULL OR day >= ?1) AND (?2 IS NULL OR day <= ?2)
              ORDER BY day, book_id, source",
        )
        .bind(from)
        .bind(to)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| DayFact {
                book_id: r.get("book_id"),
                day: r.get("day"),
                minutes: r.get("minutes"),
                pages: r.get("pages"),
                source: r.get("source"),
            })
            .collect())
    }

    /// Every passage as a stamp. `strftime('%s', …)` reads `ko_datetime` the
    /// way `attribute_highlights` does and is NULL for a stamp it cannot read,
    /// which falls back to when we stored the row.
    pub async fn passage_stamps(&self) -> Result<Vec<MarkStamp>> {
        let rows = sqlx::query(
            "SELECT id, book_id, reading_id, source,
                    COALESCE(CAST(strftime('%s', ko_datetime) AS INTEGER), created_at) AS at
               FROM highlights ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| MarkStamp {
                id: r.get("id"),
                book_id: Some(r.get("book_id")),
                reading_id: r.get("reading_id"),
                at: r.get("at"),
                source: r.get("source"),
                kind: "passage".into(),
            })
            .collect())
    }

    /// Every note as a stamp, dated by `created_at` — when it was written,
    /// not when its latest part was.
    pub async fn note_stamps_for_history(&self) -> Result<Vec<MarkStamp>> {
        let rows =
            sqlx::query("SELECT id, book_id, reading_id, kind, created_at FROM notes ORDER BY id")
                .fetch_all(self.pool())
                .await?;
        Ok(rows
            .iter()
            .map(|r| MarkStamp {
                id: r.get("id"),
                book_id: r.get("book_id"),
                reading_id: r.get("reading_id"),
                at: r.get("created_at"),
                source: "vault".into(),
                kind: r.get("kind"),
            })
            .collect())
    }

    /// Which read each of these readings is, over every sibling of its book —
    /// `list_reading_rows`' count, for a set of readings at once. Ids that name
    /// no reading are absent from the map.
    pub async fn read_counts(&self, reading_ids: &[i64]) -> Result<HashMap<i64, ReadCount>> {
        if reading_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mine = reading_age_key("readings");
        let sibling = reading_age_key("sib");
        let sql = format!(
            "SELECT readings.id AS id,
                    (SELECT count(*) FROM readings sib
                      WHERE sib.book_id = readings.book_id) AS of_reads,
                    (SELECT count(*) FROM readings sib
                      WHERE sib.book_id = readings.book_id
                        AND ({sibling}) <= ({mine})) AS read_number
               FROM readings
              WHERE readings.id IN (SELECT value FROM json_each(?))"
        );
        let ids = serde_json::to_string(reading_ids).unwrap_or_else(|_| "[]".into());
        let rows = sqlx::query(&sql).bind(ids).fetch_all(self.pool()).await?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("id"),
                    ReadCount::new(r.get("read_number"), r.get("of_reads")),
                )
            })
            .collect())
    }

    /// Every reading's window, from the one derivation `attribute_highlights`
    /// and `reading_for_day` use.
    pub async fn read_windows(&self) -> Result<Vec<ReadWindow>> {
        let rows = sqlx::query(ALL_READING_WINDOWS)
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .iter()
            .map(|r| ReadWindow {
                reading_id: r.get("reading_id"),
                book_id: r.get("book_id"),
                start: r.get("win_start"),
                end: r.get("win_end"),
            })
            .collect())
    }
}
