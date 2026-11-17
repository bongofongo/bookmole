//! Whole-library reads of a narrow shape, from which history and library figures are derived.
//! Nothing here reads a mark's text; passages and notes cross as ids, books and instants.

use std::collections::HashMap;

use sqlx::Row;

use super::ratings::row_to_scale;
use super::readings::{
    ALL_READING_WINDOWS, reading_age_key, reading_columns_as, row_to_reading_prefixed,
};
use super::{Rating, ReadCount, Reading, Storage};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct ReadFacts {
    pub reading: Reading,
    pub count: ReadCount,
    pub rating: Option<Rating>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayFact {
    pub book_id: i64,
    /// `YYYY-MM-DD`, UTC.
    pub day: String,
    pub minutes: Option<i64>,
    pub pages: Option<i64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkStamp {
    pub id: i64,
    /// Always `Some` for a passage; a note whose book was deleted has none.
    pub book_id: Option<i64>,
    /// As stored; `None` for an ordinary note, set for a reflection or review.
    pub reading_id: Option<i64>,
    /// Unix seconds: a passage's device stamp read as UTC, else when stored; a note's `created_at`.
    pub at: i64,
    /// `highlights.source` for a passage; `vault` for a note.
    pub source: String,
    /// `passage`, or the note's kind (`note`, `reflection`, `review`, …).
    pub kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadWindow {
    pub reading_id: i64,
    pub book_id: i64,
    pub start: i64,
    pub end: i64,
}

impl Storage {
    /// The review join cannot duplicate a row: `idx_one_review` allows one review per reading.
    pub(crate) async fn history_reads(&self) -> Result<Vec<ReadFacts>> {
        let reading = reading_columns_as("readings", "r_");
        let mine = reading_age_key("readings");
        let sibling = reading_age_key("sib");
        let sql = format!(
            "SELECT {reading},
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
                    count: ReadCount::new(r.try_get("read_number")?, r.try_get("of_reads")?),
                    rating: value
                        .map(|value| row_to_scale(r).map(|scale| Rating { scale, value }))
                        .transpose()?,
                })
            })
            .collect()
    }

    /// Inclusive `from..=to` as `YYYY-MM-DD`; either end may be open.
    pub(crate) async fn history_events(
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
        rows.iter()
            .map(|r| {
                Ok(DayFact {
                    book_id: r.try_get("book_id")?,
                    day: r.try_get("day")?,
                    minutes: r.try_get("minutes")?,
                    pages: r.try_get("pages")?,
                    source: r.try_get("source")?,
                })
            })
            .collect()
    }

    /// A `ko_datetime` that cannot be read falls back to when the row was stored.
    pub(crate) async fn passage_stamps(&self) -> Result<Vec<MarkStamp>> {
        let rows = sqlx::query(
            "SELECT id, book_id, reading_id, source,
                    COALESCE(CAST(strftime('%s', ko_datetime) AS INTEGER), created_at) AS at
               FROM highlights ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MarkStamp {
                    id: r.try_get("id")?,
                    book_id: Some(r.try_get("book_id")?),
                    reading_id: r.try_get("reading_id")?,
                    at: r.try_get("at")?,
                    source: r.try_get("source")?,
                    kind: "passage".into(),
                })
            })
            .collect()
    }

    /// Dated by `created_at`, not by when its latest part was written.
    pub(crate) async fn note_stamps_for_history(&self) -> Result<Vec<MarkStamp>> {
        let rows =
            sqlx::query("SELECT id, book_id, reading_id, kind, created_at FROM notes ORDER BY id")
                .fetch_all(self.pool())
                .await?;
        rows.iter()
            .map(|r| {
                Ok(MarkStamp {
                    id: r.try_get("id")?,
                    book_id: r.try_get("book_id")?,
                    reading_id: r.try_get("reading_id")?,
                    at: r.try_get("created_at")?,
                    source: "vault".into(),
                    kind: r.try_get("kind")?,
                })
            })
            .collect()
    }

    /// Ids that name no reading are absent from the map.
    pub(crate) async fn read_counts(&self, reading_ids: &[i64]) -> Result<HashMap<i64, ReadCount>> {
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
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("id")?,
                    ReadCount::new(r.try_get("read_number")?, r.try_get("of_reads")?),
                ))
            })
            .collect()
    }

    pub(crate) async fn read_windows(&self) -> Result<Vec<ReadWindow>> {
        let rows = sqlx::query(ALL_READING_WINDOWS)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(ReadWindow {
                    reading_id: r.try_get("reading_id")?,
                    book_id: r.try_get("book_id")?,
                    start: r.try_get("win_start")?,
                    end: r.try_get("win_end")?,
                })
            })
            .collect()
    }
}
