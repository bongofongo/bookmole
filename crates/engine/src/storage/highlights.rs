use sha2::{Digest, Sha256};
use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::{EngineError, Result};
use crate::highlight_origin::{HighlightOrigin, PosKind};

#[derive(Debug, Clone)]
pub struct NewHighlight {
    pub text: String,
    pub chapter: Option<String>,
    pub page: Option<i64>,
    pub pos0: Option<String>,
    pub pos1: Option<String>,
    pub ko_datetime: Option<String>,
    /// Parsed but not persisted. Must never enter `identity_hash`: it changes on every edit.
    pub ko_datetime_updated: Option<String>,
    pub color: Option<String>,
    pub note: Option<String>,
    pub source: String,
}

impl NewHighlight {
    /// Hashes the book id, `ko_datetime`, `pos0` and text only; device-rewritable fields are excluded.
    pub fn identity_hash(&self, book_id: i64) -> String {
        identity_hash_of(
            book_id,
            self.ko_datetime.as_deref(),
            self.pos0.as_deref(),
            &self.text,
        )
    }
}

/// The single copy of the formula, so `merge_books` can recompute a moved highlight's hash.
pub(crate) fn identity_hash_of(
    book_id: i64,
    ko_datetime: Option<&str>,
    pos0: Option<&str>,
    text: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(book_id.to_string());
    hasher.update("|");
    hasher.update(ko_datetime.unwrap_or(""));
    hasher.update("|");
    hasher.update(pos0.unwrap_or(""));
    hasher.update("|");
    hasher.update(text);
    format!("{:x}", hasher.finalize())
}

/// One piece of code hashes both the sidecar's and the library's device-owned state.
/// `annotation` is excluded so the reader's own note never looks like a device change.
#[derive(Default)]
pub(crate) struct DeviceDigest {
    parts: Vec<String>,
}

pub(crate) struct DeviceEntry<'a> {
    pub ko_datetime: Option<&'a str>,
    pub pos0: Option<&'a str>,
    pub text: &'a str,
    pub origin_note: Option<&'a str>,
    pub color: Option<&'a str>,
    pub chapter: Option<&'a str>,
    pub page: Option<i64>,
}

impl NewHighlight {
    pub(crate) fn device_entry(&self) -> DeviceEntry<'_> {
        DeviceEntry {
            ko_datetime: self.ko_datetime.as_deref(),
            pos0: self.pos0.as_deref(),
            text: &self.text,
            origin_note: self.note.as_deref(),
            color: self.color.as_deref(),
            chapter: self.chapter.as_deref(),
            page: self.page,
        }
    }
}

impl DeviceDigest {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add(&mut self, e: DeviceEntry<'_>) {
        // `\u{1f}` cannot occur in any field, so no value can impersonate a boundary.
        self.parts.push(format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            e.ko_datetime.unwrap_or(""),
            e.pos0.unwrap_or(""),
            e.text,
            e.origin_note.unwrap_or(""),
            e.color.unwrap_or(""),
            e.chapter.unwrap_or(""),
            e.page.map(|p| p.to_string()).unwrap_or_default(),
        ));
    }

    pub(crate) fn finish(mut self) -> String {
        self.parts.sort();
        let mut hasher = Sha256::new();
        for p in &self.parts {
            hasher.update(p);
            hasher.update("\u{1e}");
        }
        format!("{:x}", hasher.finalize())
    }
}

#[derive(Debug, Clone)]
pub struct Highlight {
    pub id: i64,
    pub book_id: i64,
    pub text: String,
    pub chapter: Option<String>,
    pub page: Option<i64>,
    pub origin_note: Option<String>,
    pub annotation: Option<String>,
    pub ko_datetime: Option<String>,
    /// `None` is ordinary: a highlight between readings, or not yet attributed. Derived on import.
    pub reading_id: Option<i64>,
    /// Legacy provenance word; new code reads `origin`. A device refresh must leave it alone.
    pub source: String,
    pub origin: HighlightOrigin,
    /// The copy it was made in; `None` is history with no (unambiguous) copy.
    pub copy_id: Option<i64>,
    /// The source's own stable id (Apple Books); `None` for KOReader, which uses `identity_hash`.
    pub origin_id: Option<String>,
    /// The unit of the stored position, never converted.
    pub pos_kind: Option<PosKind>,
    pub text_unavailable: bool,
    /// Unix seconds when first stored; a refresh never moves it.
    pub created_at: i64,
}

/// Shared with `citations_for`, so every query builds a highlight from one projection.
pub(super) const HIGHLIGHT_COLUMNS: &str = "id, book_id, text, chapter, page, origin_note, annotation, ko_datetime, reading_id, \
     source, origin, copy_id, origin_id, pos_kind, text_unavailable, created_at";

pub(super) fn row_to_highlight(r: &sqlx::sqlite::SqliteRow) -> Result<Highlight> {
    let source: String = r.try_get("source")?;
    Ok(Highlight {
        id: r.try_get("id")?,
        book_id: r.try_get("book_id")?,
        text: r.try_get("text")?,
        chapter: r.try_get("chapter")?,
        page: r.try_get("page")?,
        origin_note: r.try_get("origin_note")?,
        annotation: r.try_get("annotation")?,
        ko_datetime: r.try_get("ko_datetime")?,
        reading_id: r.try_get("reading_id")?,
        origin: r
            .try_get::<Option<String>, _>("origin")?
            .and_then(|o| o.parse().ok())
            .unwrap_or_else(|| HighlightOrigin::from_source(&source)),
        source,
        copy_id: r.try_get("copy_id")?,
        origin_id: r.try_get("origin_id")?,
        pos_kind: r
            .try_get::<Option<String>, _>("pos_kind")?
            .and_then(|k| k.parse().ok()),
        text_unavailable: r.try_get::<i64, _>("text_unavailable")? != 0,
        created_at: r.try_get("created_at")?,
    })
}

/// Device-owned columns, shared by the UPDATE and the dry-run SELECT so they agree.
/// `IS NOT`, not `!=`: NULL-safe, so a note deleted on the device is noticed.
const DEVICE_FIELDS_DIFFER: &str =
    "(origin_note IS NOT ?3 OR color IS NOT ?4 OR chapter IS NOT ?5 OR page IS NOT ?6)";

impl Storage {
    /// Some(id) only when newly inserted; the conflict clause stays `DO NOTHING` so counts stay exact.
    pub async fn insert_highlight(&self, book_id: i64, h: &NewHighlight) -> Result<Option<i64>> {
        self.insert_highlight_in(book_id, h, None).await
    }

    pub async fn insert_highlight_in(
        &self,
        book_id: i64,
        h: &NewHighlight,
        copy_id: Option<i64>,
    ) -> Result<Option<i64>> {
        // A `source` naming no origin is stored with none rather than guessed to be KOReader's.
        let origin = h.source.parse::<HighlightOrigin>().ok();
        let pos_kind = (origin == Some(HighlightOrigin::Koreader) && h.pos0.is_some())
            .then_some(PosKind::KoreaderXpointer);
        let row = sqlx::query(
            r#"INSERT INTO highlights
                (book_id, text, chapter, page, pos0, pos1, ko_datetime, color, origin_note,
                 last_seen_origin_note, source, identity_hash, created_at,
                 origin, copy_id, pos_kind)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(book_id, identity_hash) DO NOTHING
               RETURNING id"#,
        )
        .bind(book_id)
        .bind(&h.text)
        .bind(h.chapter.as_ref())
        .bind(h.page)
        .bind(h.pos0.as_ref())
        .bind(h.pos1.as_ref())
        .bind(h.ko_datetime.as_ref())
        .bind(h.color.as_ref())
        .bind(h.note.as_ref())
        // Seeded too: a set `origin_note` with NULL `last_seen_origin_note` reads as "device said nothing".
        .bind(h.note.as_ref())
        .bind(&h.source)
        .bind(h.identity_hash(book_id))
        .bind(now_unix())
        .bind(origin.map(|o| o.as_str()))
        .bind(copy_id)
        .bind(pos_kind.map(|k| k.as_str()))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| r.try_get("id")).transpose()?)
    }

    /// Never overwrites a copy already recorded.
    pub async fn stamp_highlight_copy(
        &self,
        book_id: i64,
        h: &NewHighlight,
        copy_id: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE highlights SET copy_id = ?3
             WHERE book_id = ?1 AND identity_hash = ?2 AND copy_id IS NULL",
        )
        .bind(book_id)
        .bind(h.identity_hash(book_id))
        .bind(copy_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Refreshes a highlight's device-owned payload; true if a column changed. Straight assignment,
    /// not COALESCE: a sidecar is complete state, so a missing note means deleted. Updates in place,
    /// since delete-and-reinsert would null note anchors and cascade flashcards away.
    pub async fn refresh_device_fields(&self, book_id: i64, h: &NewHighlight) -> Result<bool> {
        let sql = format!(
            r#"UPDATE highlights SET
                   origin_note           = ?3,
                   color             = ?4,
                   chapter           = ?5,
                   page              = ?6,
                   last_seen_origin_note = ?3
               WHERE book_id = ?1 AND identity_hash = ?2 AND {DEVICE_FIELDS_DIFFER}"#
        );
        let done = sqlx::query(&sql)
            .bind(book_id)
            .bind(h.identity_hash(book_id))
            .bind(h.note.as_ref())
            .bind(h.color.as_ref())
            .bind(h.chapter.as_ref())
            .bind(h.page)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn device_fields_differ(&self, book_id: i64, h: &NewHighlight) -> Result<bool> {
        let sql = format!(
            "SELECT count(*) FROM highlights
             WHERE book_id = ?1 AND identity_hash = ?2 AND {DEVICE_FIELDS_DIFFER}"
        );
        let n: i64 = sqlx::query_scalar(&sql)
            .bind(book_id)
            .bind(h.identity_hash(book_id))
            .bind(h.note.as_ref())
            .bind(h.color.as_ref())
            .bind(h.chapter.as_ref())
            .bind(h.page)
            .fetch_one(self.pool())
            .await?;
        Ok(n > 0)
    }

    pub async fn set_annotation(&self, highlight_id: i64, annotation: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE highlights SET annotation = ? WHERE id = ?")
            .bind(annotation)
            .bind(highlight_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Restricted to `source = 'koreader'` so other highlights cannot make a sidecar look imported.
    pub async fn device_highlight_digest(&self, book_id: i64) -> Result<String> {
        let rows = sqlx::query(
            "SELECT ko_datetime, pos0, text, origin_note, color, chapter, page
             FROM highlights WHERE book_id = ? AND origin = 'koreader'",
        )
        .bind(book_id)
        .fetch_all(self.pool())
        .await?;

        let mut digest = DeviceDigest::new();
        for r in &rows {
            let (ko_datetime, pos0, text, origin_note, color, chapter) = (
                r.try_get::<Option<String>, _>("ko_datetime")?,
                r.try_get::<Option<String>, _>("pos0")?,
                r.try_get::<String, _>("text")?,
                r.try_get::<Option<String>, _>("origin_note")?,
                r.try_get::<Option<String>, _>("color")?,
                r.try_get::<Option<String>, _>("chapter")?,
            );
            digest.add(DeviceEntry {
                ko_datetime: ko_datetime.as_deref(),
                pos0: pos0.as_deref(),
                text: &text,
                origin_note: origin_note.as_deref(),
                color: color.as_deref(),
                chapter: chapter.as_deref(),
                page: r.try_get::<Option<i64>, _>("page")?,
            });
        }
        Ok(digest.finish())
    }

    pub async fn highlight_exists(&self, book_id: i64, h: &NewHighlight) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM highlights WHERE book_id = ? AND identity_hash = ?",
        )
        .bind(book_id)
        .bind(h.identity_hash(book_id))
        .fetch_one(self.pool())
        .await?;
        Ok(n > 0)
    }

    /// Returns only the book id, not the row, to keep private text off a yes/no path.
    pub async fn highlight_book(&self, highlight_id: i64) -> Result<Option<i64>> {
        Ok(
            sqlx::query_scalar("SELECT book_id FROM highlights WHERE id = ?")
                .bind(highlight_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    pub async fn list_highlights(&self, book_id: i64) -> Result<Vec<Highlight>> {
        let sql = format!(
            "SELECT {HIGHLIGHT_COLUMNS} FROM highlights WHERE book_id = ?
             ORDER BY page ASC, ko_datetime ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_highlight).collect()
    }

    /// Cannot ask for unattributed highlights (`reading_id IS NULL`); filter `list_highlights`.
    pub async fn highlights_for_reading(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        let sql = format!(
            "SELECT {HIGHLIGHT_COLUMNS} FROM highlights WHERE reading_id = ?
             ORDER BY page ASC, ko_datetime ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(reading_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_highlight).collect()
    }

    /// The passage the reader chose for the reading's card; `None` means not chosen.
    pub async fn card_passage(&self, reading_id: i64) -> Result<Option<Highlight>> {
        let sql = format!(
            "SELECT {HIGHLIGHT_COLUMNS} FROM highlights
              WHERE id = (SELECT card_highlight_id FROM readings WHERE id = ?)"
        );
        let row = sqlx::query(&sql)
            .bind(reading_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_highlight).transpose()
    }

    /// The highlight must be of the reading's book, and of this reading when attributed at all;
    /// unattributed is accepted because imported highlights start unattributed.
    pub async fn set_card_passage(&self, reading_id: i64, highlight_id: Option<i64>) -> Result<()> {
        let reading = self
            .get_reading(reading_id)
            .await?
            .ok_or_else(|| EngineError::NotFound(format!("reading {reading_id}")))?;
        if let Some(hid) = highlight_id {
            let h = self
                .get_highlight(hid)
                .await?
                .ok_or_else(|| EngineError::NotFound(format!("highlight {hid}")))?;
            if h.book_id != reading.book_id {
                return Err(EngineError::InvalidInput(format!(
                    "highlight {hid} is of book {}, not of reading {reading_id}'s book {}",
                    h.book_id, reading.book_id
                )));
            }
            if h.reading_id.is_some_and(|r| r != reading_id) {
                return Err(EngineError::InvalidInput(format!(
                    "highlight {hid} was marked during another read of this book"
                )));
            }
        }
        sqlx::query("UPDATE readings SET card_highlight_id = ?, last_modified = ? WHERE id = ?")
            .bind(highlight_id)
            .bind(now_unix())
            .bind(reading_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Exactly the set `set_card_passage` accepts, so a chooser never offers a passage it refuses.
    pub async fn card_passage_candidates(&self, reading_id: i64) -> Result<Vec<Highlight>> {
        let sql = format!(
            "SELECT {HIGHLIGHT_COLUMNS} FROM highlights
              WHERE book_id = (SELECT book_id FROM readings WHERE id = ?)
                AND (reading_id IS NULL OR reading_id = ?)
              ORDER BY page ASC, ko_datetime ASC"
        );
        let rows = sqlx::query(&sql)
            .bind(reading_id)
            .bind(reading_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_highlight).collect()
    }

    pub async fn get_highlight(&self, id: i64) -> Result<Option<Highlight>> {
        let sql = format!("SELECT {HIGHLIGHT_COLUMNS} FROM highlights WHERE id = ?");
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_highlight).transpose()
    }

    /// Fetches a page of passages in one statement, avoiding an N+1 across a page of cards.
    pub(super) async fn highlights_by_ids(&self, ids: &[i64]) -> Result<Vec<Highlight>> {
        let mut out = Vec::with_capacity(ids.len());
        // Chunked under SQLite's older 999-parameter ceiling; `limit` may be negative (no limit).
        for chunk in ids.chunks(500) {
            let holes = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!("SELECT {HIGHLIGHT_COLUMNS} FROM highlights WHERE id IN ({holes})");
            let mut q = sqlx::query(&sql);
            for id in chunk {
                q = q.bind(*id);
            }
            for r in q.fetch_all(self.pool()).await? {
                out.push(row_to_highlight(&r)?);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;

    fn hl(text: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: Some("Ch 1".into()),
            page: Some(42),
            pos0: Some("/body/DocFragment[8]/p[3]/text().0".into()),
            pos1: None,
            ko_datetime: Some("2026-01-01 10:00:00".into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    #[test]
    fn identity_survives_an_edit_on_the_device() {
        let base = hl("a phrase worth keeping");

        let edited = NewHighlight {
            ko_datetime_updated: Some("2026-06-01 12:00:00".into()),
            note: Some("a note the user typed later".into()),
            page: Some(43),
            chapter: Some("Ch 1 (renamed)".into()),
            color: Some("gray".into()),
            ..base.clone()
        };

        assert_eq!(
            base.identity_hash(1),
            edited.identity_hash(1),
            "device-owned fields must not take part in identity"
        );
    }

    #[test]
    fn identity_still_tracks_the_fields_it_should() {
        let base = hl("a phrase worth keeping");
        for changed in [
            NewHighlight {
                text: "a different phrase".into(),
                ..base.clone()
            },
            NewHighlight {
                pos0: Some("/body/DocFragment[9]/p[1]/text().0".into()),
                ..base.clone()
            },
            NewHighlight {
                ko_datetime: Some("2026-01-02 10:00:00".into()),
                ..base.clone()
            },
        ] {
            assert_ne!(base.identity_hash(1), changed.identity_hash(1));
        }
        assert_ne!(base.identity_hash(1), base.identity_hash(2));
    }

    #[tokio::test]
    async fn a_row_of_the_wrong_shape_is_an_error_not_a_panic() {
        use crate::error::EngineError;
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let read = |sql: &'static str| {
            let pool = s.pool().clone();
            async move {
                let row = sqlx::query(sql).fetch_one(&pool).await.unwrap();
                row_to_highlight(&row)
            }
        };

        let missing = read("SELECT 1 AS id").await;
        assert!(
            matches!(
                missing,
                Err(EngineError::Db(sqlx::Error::ColumnNotFound(_)))
            ),
            "{missing:?}"
        );
        let mistyped = read("SELECT 'koreader' AS source, 'seven' AS id").await;
        assert!(
            matches!(
                mistyped,
                Err(EngineError::Db(sqlx::Error::ColumnDecode { .. }))
            ),
            "{mistyped:?}"
        );
    }

    #[tokio::test]
    async fn double_insert_is_idempotent() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("T".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let h = hl("a phrase worth keeping");
        assert!(s.insert_highlight(book_id, &h).await.unwrap().is_some());
        assert!(s.insert_highlight(book_id, &h).await.unwrap().is_none());
        assert_eq!(s.list_highlights(book_id).await.unwrap().len(), 1);
        assert!(s.highlight_exists(book_id, &h).await.unwrap());
    }

    async fn seeded() -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book_id = s
            .upsert_book(
                &Book {
                    title: Some("T".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, book_id)
    }

    #[tokio::test]
    async fn a_refresh_reports_only_a_real_change() {
        let (s, book_id) = seeded().await;
        let h = hl("a phrase worth keeping");
        s.insert_highlight(book_id, &h).await.unwrap();

        assert!(
            !s.refresh_device_fields(book_id, &h).await.unwrap(),
            "identical payload is not an update"
        );
        assert!(!s.device_fields_differ(book_id, &h).await.unwrap());

        let edited = NewHighlight {
            note: Some("typed later on the device".into()),
            ..h.clone()
        };
        assert!(s.device_fields_differ(book_id, &edited).await.unwrap());
        assert!(s.refresh_device_fields(book_id, &edited).await.unwrap());
        assert!(
            !s.refresh_device_fields(book_id, &edited).await.unwrap(),
            "the second pass has nothing left to do"
        );

        let stored = &s.list_highlights(book_id).await.unwrap()[0];
        assert_eq!(
            stored.origin_note.as_deref(),
            Some("typed later on the device")
        );
    }

    #[tokio::test]
    async fn every_device_owned_field_refreshes() {
        let base = hl("a phrase worth keeping");
        for edited in [
            NewHighlight {
                note: Some("a note".into()),
                ..base.clone()
            },
            NewHighlight {
                color: Some("cyan".into()),
                ..base.clone()
            },
            NewHighlight {
                chapter: Some("Ch 2".into()),
                ..base.clone()
            },
            NewHighlight {
                page: Some(43),
                ..base.clone()
            },
        ] {
            let (s, book_id) = seeded().await;
            s.insert_highlight(book_id, &base).await.unwrap();
            assert!(
                s.refresh_device_fields(book_id, &edited).await.unwrap(),
                "a changed device field must count as an update"
            );
        }
    }

    #[tokio::test]
    async fn deleting_a_note_on_the_device_deletes_it_here() {
        let (s, book_id) = seeded().await;
        let with_note = NewHighlight {
            note: Some("regretted immediately".into()),
            ..hl("a phrase worth keeping")
        };
        s.insert_highlight(book_id, &with_note).await.unwrap();

        let cleared = NewHighlight {
            note: None,
            ..with_note.clone()
        };
        assert!(s.refresh_device_fields(book_id, &cleared).await.unwrap());
        assert_eq!(
            s.list_highlights(book_id).await.unwrap()[0].origin_note,
            None
        );
    }

    #[tokio::test]
    async fn a_refresh_never_touches_our_annotation() {
        let (s, book_id) = seeded().await;
        let h = hl("a phrase worth keeping");
        let id = s.insert_highlight(book_id, &h).await.unwrap().unwrap();
        s.set_annotation(id, Some("what I actually think"))
            .await
            .unwrap();

        let edited = NewHighlight {
            note: Some("typed later on the device".into()),
            ..h.clone()
        };
        assert!(s.refresh_device_fields(book_id, &edited).await.unwrap());

        let stored = &s.list_highlights(book_id).await.unwrap()[0];
        assert_eq!(stored.id, id, "the row is updated in place, not replaced");
        assert_eq!(stored.annotation.as_deref(), Some("what I actually think"));
    }

    #[tokio::test]
    async fn last_seen_tracks_the_device_from_the_very_first_import() {
        let (s, book_id) = seeded().await;
        let h = NewHighlight {
            note: Some("as captured".into()),
            ..hl("a phrase worth keeping")
        };
        s.insert_highlight(book_id, &h).await.unwrap();

        let last_seen = |s: Storage| async move {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT last_seen_origin_note FROM highlights LIMIT 1",
            )
            .fetch_one(s.pool())
            .await
            .unwrap()
        };
        assert_eq!(last_seen(s.clone()).await.as_deref(), Some("as captured"));

        let edited = NewHighlight {
            note: Some("as edited".into()),
            ..h.clone()
        };
        s.refresh_device_fields(book_id, &edited).await.unwrap();
        assert_eq!(last_seen(s.clone()).await.as_deref(), Some("as edited"));
    }

    #[tokio::test]
    async fn a_refresh_does_not_reach_into_another_book() {
        let (s, first) = seeded().await;
        let second = s
            .upsert_book(
                &Book {
                    title: Some("Another".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let h = hl("a phrase worth keeping");
        s.insert_highlight(first, &h).await.unwrap();
        s.insert_highlight(second, &h).await.unwrap();

        let edited = NewHighlight {
            note: Some("only on the first".into()),
            ..h.clone()
        };
        assert!(s.refresh_device_fields(first, &edited).await.unwrap());
        assert_eq!(
            s.list_highlights(second).await.unwrap()[0].origin_note,
            None
        );
    }

    fn digest_of(hs: &[NewHighlight]) -> String {
        let mut d = DeviceDigest::new();
        for h in hs {
            d.add(h.device_entry());
        }
        d.finish()
    }

    #[test]
    fn the_digest_does_not_depend_on_the_order_it_was_fed() {
        let a = hl("first");
        let mut b = hl("second");
        b.ko_datetime = Some("2026-02-02 10:00:00".into());
        let mut c = hl("third");
        c.page = Some(9);

        let forward = digest_of(&[a.clone(), b.clone(), c.clone()]);
        assert_eq!(digest_of(&[c.clone(), a.clone(), b.clone()]), forward);
        assert_eq!(digest_of(&[b, c, a]), forward);
    }

    #[test]
    fn every_device_owned_field_moves_the_digest() {
        let base = digest_of(&[hl("passage")]);
        let moves = |name: &str, mutate: &dyn Fn(&mut NewHighlight)| {
            let mut h = hl("passage");
            mutate(&mut h);
            assert_ne!(digest_of(&[h]), base, "{name} did not move the digest");
        };
        moves("text", &|h| h.text = "different".into());
        moves("ko_datetime", &|h| {
            h.ko_datetime = Some("2020-01-01 00:00:00".into())
        });
        moves("pos0", &|h| h.pos0 = Some("/body/p[9]/text().0".into()));
        moves("note", &|h| h.note = Some("edited on the device".into()));
        moves("color", &|h| h.color = Some("cyan".into()));
        moves("chapter", &|h| h.chapter = Some("Ch 2".into()));
        moves("page", &|h| h.page = Some(43));
    }

    #[test]
    fn a_value_cannot_impersonate_a_field_boundary() {
        let mut split = hl("a");
        split.chapter = Some("b".into());
        let mut joined = hl("a\u{1f}b");
        joined.chapter = None;
        assert_ne!(digest_of(&[split]), digest_of(&[joined]));
    }

    #[tokio::test]
    async fn our_own_annotation_is_not_part_of_the_device_digest() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        let h = hl("a passage");
        let id = s.insert_highlight(book, &h).await.unwrap().unwrap();

        let before = s.device_highlight_digest(book).await.unwrap();
        assert_eq!(
            before,
            digest_of(std::slice::from_ref(&h)),
            "both sides, one formula"
        );

        s.set_annotation(id, Some("what I thought about it"))
            .await
            .unwrap();
        assert_eq!(
            s.device_highlight_digest(book).await.unwrap(),
            before,
            "ours must not read as a device change"
        );

        let mut mine = hl("something I typed");
        mine.source = "manual".into();
        s.insert_highlight(book, &mine).await.unwrap();
        assert_eq!(s.device_highlight_digest(book).await.unwrap(), before);
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use crate::book::Book;
    use proptest::prelude::*;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    /// The two scripts differ in bytes per character, so a rule that measured bytes fails here.
    fn mark(n: usize, cjk: bool, seq: usize) -> NewHighlight {
        let text = if cjk { "極" } else { "x" }.repeat(n + 1);
        NewHighlight {
            text,
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[{seq}]/text().0")),
            pos1: None,
            ko_datetime: Some("2026-01-05 12:00:00".into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    proptest! {
        #[test]
        fn the_card_passage_is_the_chosen_mark_and_nothing_else(
            marks in proptest::collection::vec((0usize..6, any::<bool>()), 1..9),
            pick in 0usize..8,
        ) {
            rt().block_on(async {
                let s = Storage::connect("sqlite::memory:").await.unwrap();
                let book = s.upsert_book(
                    &Book { title: Some("C".into()), ..Default::default() },
                    None,
                ).await.unwrap();
                let reading = s
                    .record_reading(book, Some(1_767_225_600), None, "reading", "manual")
                    .await
                    .unwrap();

                for (seq, &(n, cjk)) in marks.iter().enumerate() {
                    let id = s.insert_highlight(book, &mark(n, cjk, seq)).await.unwrap()
                        .expect("every anchor here is distinct, so every insert is a row");
                    sqlx::query("UPDATE highlights SET reading_id = ? WHERE id = ?")
                        .bind(reading)
                        .bind(id)
                        .execute(s.pool())
                        .await
                        .unwrap();
                }
                let listed = s.highlights_for_reading(reading).await.unwrap();
                prop_assert_eq!(listed.len(), marks.len());
                let offered = s.card_passage_candidates(reading).await.unwrap();
                prop_assert_eq!(offered.len(), marks.len());

                prop_assert!(s.card_passage(reading).await.unwrap().is_none());

                let want = &listed[pick % listed.len()];
                s.set_card_passage(reading, Some(want.id)).await.unwrap();
                let got = s.card_passage(reading).await.unwrap().unwrap();
                prop_assert_eq!(got.id, want.id);
                prop_assert_eq!(&got.text, &want.text);
                let again = s.card_passage(reading).await.unwrap().unwrap();
                prop_assert_eq!(again.id, got.id);

                s.set_card_passage(reading, None).await.unwrap();
                prop_assert!(s.card_passage(reading).await.unwrap().is_none());
                prop_assert_eq!(s.highlights_for_reading(reading).await.unwrap().len(), marks.len());
                Ok(())
            })?;
        }
    }
}
