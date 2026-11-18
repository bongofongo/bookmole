use sha2::{Digest, Sha256};
use sqlx::sqlite::SqlitePool;
use sqlx::{Row, SqliteConnection};

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
    /// Hashes `ko_datetime`, `pos0` and text only. Not the book, so the same highlight hashes alike
    /// in every library and across a merge; not device-rewritable fields.
    pub fn identity_hash(&self) -> String {
        identity_hash_of(
            self.ko_datetime.as_deref(),
            self.pos0.as_deref(),
            &self.text,
        )
    }
}

/// Begins every hash `identity_hash_of` makes. A KOReader or user highlight stored without it
/// holds the earlier formula, which also hashed the book id; the storage rewrites those on open.
pub(crate) const IDENTITY_PREFIX: &str = "v2:";

/// The single copy of the formula for KOReader and user highlights. Imported highlights have their
/// own (`imported_identity_hash`), which never carries the prefix, so the two cannot meet.
pub(crate) fn identity_hash_of(
    ko_datetime: Option<&str>,
    pos0: Option<&str>,
    text: &str,
) -> String {
    identity_hash_of_bytes(
        ko_datetime.unwrap_or("").as_bytes(),
        pos0.unwrap_or("").as_bytes(),
        text.as_bytes(),
    )
}

/// Bytes, so a stored value that is not valid UTF-8 still hashes instead of failing to decode.
fn identity_hash_of_bytes(ko_datetime: &[u8], pos0: &[u8], text: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"rb-local\0");
    // Each field carries its length, so no value can move bytes across a boundary.
    for field in [ko_datetime, pos0, text] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    format!("{IDENTITY_PREFIX}{:x}", hasher.finalize())
}

/// KOReader and user highlights still on the earlier formula; `?1` is `IDENTITY_PREFIX`. A NULL
/// origin is a row `insert_highlight` wrote for a source naming no origin, on that formula too.
const ON_OLD_FORMULA: &str = "(origin IS NULL OR origin IN ('koreader', 'user'))
     AND substr(identity_hash, 1, length(?1)) <> ?1";

/// Rows rehashed per transaction when the storage opens.
pub(super) const REHASH_BATCH: i64 = 500;

/// Rewrites every highlight still on the earlier formula; returns how many rows it touched. Each
/// batch is one transaction, so an interrupted pass leaves each row whole and the next open
/// finishes it. Two rows of one book that now hash alike are one highlight, folded as
/// `merge_books` folds a duplicate. The older row (lower id) stays, whichever already holds the new
/// hash: a holder with a higher id was written while an earlier rehash failed, and keeping it would
/// replace the original's id, `created_at`, copy and reading.
pub(super) async fn rehash_old_identities(pool: &SqlitePool, batch: i64) -> Result<usize> {
    // Read first: once every row is converted, opening a library takes no write lock for this.
    let pending: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM highlights WHERE {ON_OLD_FORMULA})"
    ))
    .bind(IDENTITY_PREFIX)
    .fetch_one(pool)
    .await?;
    if !pending {
        return Ok(0);
    }
    let mut touched = 0;
    // A cursor, so each batch starts where the last ended instead of scanning from the first row.
    let mut after = i64::MIN;
    loop {
        // Immediate, so two processes opening one library at once wait for each other instead of
        // both reading a batch and then failing to upgrade to a write.
        let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
        let sql = format!(
            "SELECT id, book_id, CAST(ko_datetime AS BLOB) AS ko_datetime,
                    CAST(pos0 AS BLOB) AS pos0, CAST(text AS BLOB) AS text
               FROM highlights WHERE {ON_OLD_FORMULA} AND id > ?3 ORDER BY id LIMIT ?2"
        );
        let rows = sqlx::query(&sql)
            .bind(IDENTITY_PREFIX)
            .bind(batch)
            .bind(after)
            .fetch_all(&mut *tx)
            .await?;
        if rows.is_empty() {
            return Ok(touched);
        }
        for r in &rows {
            let id: i64 = r.try_get("id")?;
            after = id;
            let book_id: i64 = r.try_get("book_id")?;
            let hash = identity_hash_of_bytes(
                &r.try_get::<Option<Vec<u8>>, _>("ko_datetime")?
                    .unwrap_or_default(),
                &r.try_get::<Option<Vec<u8>>, _>("pos0")?.unwrap_or_default(),
                &r.try_get::<Option<Vec<u8>>, _>("text")?.unwrap_or_default(),
            );
            let holder: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM highlights WHERE book_id = ? AND identity_hash = ?",
            )
            .bind(book_id)
            .bind(&hash)
            .fetch_optional(&mut *tx)
            .await?;
            match holder {
                Some(older) if older < id => fold_highlight(&mut tx, id, older).await?,
                newer => {
                    if let Some(newer) = newer {
                        fold_highlight(&mut tx, newer, id).await?;
                    }
                    sqlx::query("UPDATE highlights SET identity_hash = ? WHERE id = ?")
                        .bind(&hash)
                        .bind(id)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            touched += 1;
        }
        tx.commit().await?;
    }
}

/// Trims what the reader cannot see, for comparing annotations: two that differ only here are one.
const TRIMMED: &str = "char(9, 10, 13, 32)";

/// Folds a duplicate into the highlight that keeps the identity. Everything hanging off `dup` moves
/// first: flashcards and citations would cascade away, note anchors and card passages would null.
/// No annotation is lost: `keep` takes `dup`'s if it has none, and otherwise holds it (and `dup`'s
/// own alternatives) as alternatives. Plain SQL, so text that is not valid UTF-8 still moves.
pub(super) async fn fold_highlight(conn: &mut SqliteConnection, dup: i64, keep: i64) -> Result<()> {
    let blank = |col: &str| format!("COALESCE(trim({col}, {TRIMMED}), '')");
    let (k, d, x, a) = (
        blank("k.annotation"),
        blank("d.annotation"),
        blank("x.text"),
        blank("a.text"),
    );
    let held = |col: &str| {
        format!(
            "NOT EXISTS (SELECT 1 FROM annotation_alternatives a \
              WHERE a.highlight_id = ?1 AND {a} = {col})"
        )
    };
    let statements = [
        "UPDATE notes SET highlight_id = ?1 WHERE highlight_id = ?2".to_owned(),
        "UPDATE flashcards SET highlight_id = ?1 WHERE highlight_id = ?2".to_owned(),
        // A note citing both keeps its one citation of `keep`; the other goes with `dup`.
        "UPDATE OR IGNORE citations SET highlight_id = ?1 WHERE highlight_id = ?2".to_owned(),
        "UPDATE readings SET card_highlight_id = ?1 WHERE card_highlight_id = ?2".to_owned(),
        format!(
            "UPDATE highlights AS k SET annotation = d.annotation FROM highlights AS d \
              WHERE k.id = ?1 AND d.id = ?2 AND {k} = '' AND {d} <> ''"
        ),
        format!(
            "INSERT OR IGNORE INTO annotation_alternatives (highlight_id, text, set_aside_at) \
             SELECT ?1, d.annotation, ?3 FROM highlights d, highlights k \
              WHERE d.id = ?2 AND k.id = ?1 AND {d} <> '' AND {d} <> {k} AND {}",
            held(&d)
        ),
        format!(
            "INSERT OR IGNORE INTO annotation_alternatives (highlight_id, text, set_aside_at) \
             SELECT ?1, x.text, x.set_aside_at FROM annotation_alternatives x, highlights k \
              WHERE x.highlight_id = ?2 AND k.id = ?1 AND {x} <> '' AND {x} <> {k} AND {}",
            held(&x)
        ),
    ];
    let now = now_unix();
    for sql in &statements {
        sqlx::query(sql)
            .bind(keep)
            .bind(dup)
            .bind(now)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("DELETE FROM highlights WHERE id = ?")
        .bind(dup)
        .execute(&mut *conn)
        .await?;
    Ok(())
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

    pub(crate) async fn insert_highlight_in(
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
        .bind(h.identity_hash())
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
        .bind(h.identity_hash())
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
            .bind(h.identity_hash())
            .bind(h.note.as_ref())
            .bind(h.color.as_ref())
            .bind(h.chapter.as_ref())
            .bind(h.page)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn device_fields_differ(
        &self,
        book_id: i64,
        h: &NewHighlight,
    ) -> Result<bool> {
        let sql = format!(
            "SELECT count(*) FROM highlights
             WHERE book_id = ?1 AND identity_hash = ?2 AND {DEVICE_FIELDS_DIFFER}"
        );
        let n: i64 = sqlx::query_scalar(&sql)
            .bind(book_id)
            .bind(h.identity_hash())
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
    pub(crate) async fn device_highlight_digest(&self, book_id: i64) -> Result<String> {
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

    pub(crate) async fn highlight_exists(&self, book_id: i64, h: &NewHighlight) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM highlights WHERE book_id = ? AND identity_hash = ?",
        )
        .bind(book_id)
        .bind(h.identity_hash())
        .fetch_one(self.pool())
        .await?;
        Ok(n > 0)
    }

    /// Returns only the book id, not the row, to keep private text off a yes/no path.
    pub(crate) async fn highlight_book(&self, highlight_id: i64) -> Result<Option<i64>> {
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
    pub(crate) async fn highlights_for_reading(&self, reading_id: i64) -> Result<Vec<Highlight>> {
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
    pub(crate) async fn card_passage(&self, reading_id: i64) -> Result<Option<Highlight>> {
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
    pub(crate) async fn set_card_passage(
        &self,
        reading_id: i64,
        highlight_id: Option<i64>,
    ) -> Result<()> {
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
    pub(crate) async fn card_passage_candidates(&self, reading_id: i64) -> Result<Vec<Highlight>> {
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
            base.identity_hash(),
            edited.identity_hash(),
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
            assert_ne!(base.identity_hash(), changed.identity_hash());
        }
    }

    /// The formula this one replaced, kept here only to build libraries that predate it.
    fn old_hash(book_id: i64, h: &NewHighlight) -> String {
        let mut hasher = Sha256::new();
        hasher.update(book_id.to_string());
        hasher.update("|");
        hasher.update(h.ko_datetime.as_deref().unwrap_or(""));
        hasher.update("|");
        hasher.update(h.pos0.as_deref().unwrap_or(""));
        hasher.update("|");
        hasher.update(&h.text);
        format!("{:x}", hasher.finalize())
    }

    /// The dev library seed (`corpus`'s `devdb.rs`) pins the same value, since it cannot call this.
    #[test]
    fn the_formula_is_pinned() {
        assert_eq!(
            identity_hash_of(Some("2025-01-01 00:00:00"), None, "Middlemarch."),
            "v2:1793de266aa2d27e2a0c2b6542e94b239cd0d2eaf69f5ba5f68453d289785903"
        );
    }

    #[test]
    fn a_nul_cannot_move_a_field_boundary() {
        let a = identity_hash_of(Some("2025-01-01\0x"), Some(""), "t");
        let b = identity_hash_of(Some("2025-01-01"), Some("x\0"), "t");
        assert_ne!(a, b);
        let c = identity_hash_of(None, Some("p\0q"), "");
        let d = identity_hash_of(None, Some("p"), "q");
        assert_ne!(c, d);
    }

    #[test]
    fn the_hash_is_marked_and_never_the_old_formula() {
        let h = hl("a phrase worth keeping");
        assert!(h.identity_hash().starts_with(IDENTITY_PREFIX));
        assert_ne!(h.identity_hash(), old_hash(1, &h));
    }

    async fn raw_row(s: &Storage, book_id: i64, h: &NewHighlight, origin: &str, hash: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO highlights (book_id, text, pos0, ko_datetime, source, origin,
                                     identity_hash, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 0) RETURNING id",
        )
        .bind(book_id)
        .bind(&h.text)
        .bind(h.pos0.as_ref())
        .bind(h.ko_datetime.as_ref())
        .bind(origin)
        .bind(origin)
        .bind(hash)
        .fetch_one(s.pool())
        .await
        .unwrap()
    }

    async fn hashes(s: &Storage) -> Vec<(i64, String)> {
        sqlx::query_as("SELECT id, identity_hash FROM highlights ORDER BY id")
            .fetch_all(s.pool())
            .await
            .unwrap()
    }

    async fn scalar(s: &Storage, sql: &str) -> Option<i64> {
        sqlx::query_scalar(sql).fetch_one(s.pool()).await.unwrap()
    }

    #[tokio::test]
    async fn opening_moves_old_hashes_to_the_new_formula_once() {
        let (s, book) = seeded().await;
        let ko = hl("from the reader");
        let mine = NewHighlight {
            source: "user".into(),
            ..hl("typed by hand")
        };
        raw_row(&s, book, &ko, "koreader", &old_hash(book, &ko)).await;
        raw_row(&s, book, &mine, "user", &old_hash(book, &mine)).await;
        let imported = raw_row(&s, book, &hl("from a kindle"), "kindle", "an imported hash").await;

        assert_eq!(rehash_old_identities(s.pool(), 1).await.unwrap(), 2);
        let after = hashes(&s).await;
        assert_eq!(after[0].1, ko.identity_hash());
        assert_eq!(after[1].1, mine.identity_hash());
        assert_eq!(
            after[2],
            (imported, "an imported hash".to_string()),
            "an imported hash is another formula and is left alone"
        );

        assert_eq!(rehash_old_identities(s.pool(), 1).await.unwrap(), 0);
        assert_eq!(hashes(&s).await, after, "a second pass changes nothing");
        assert!(
            s.highlight_exists(book, &ko).await.unwrap(),
            "an import finds the rehashed row"
        );
        assert!(s.insert_highlight(book, &ko).await.unwrap().is_none());
    }

    /// A row whose stored hash named another book (moved without being rehashed) and the same
    /// highlight stored correctly are one highlight once the book leaves the formula.
    #[tokio::test]
    async fn a_collision_folds_into_the_earlier_row_with_its_dependents() {
        let (s, book) = seeded().await;
        let h = hl("a phrase worth keeping");
        let keep = raw_row(&s, book, &h, "koreader", &old_hash(book, &h)).await;
        let dup = raw_row(&s, book, &h, "koreader", &old_hash(book + 7, &h)).await;
        s.set_annotation(keep, Some("mine")).await.unwrap();

        let reading = s
            .record_reading(book, Some(1_767_225_600), None, "reading", "manual")
            .await
            .unwrap();
        s.set_card_passage(reading, Some(dup)).await.unwrap();
        s.insert_flashcard(book, Some(dup), "keeping", None)
            .await
            .unwrap();
        for sql in [
            "INSERT INTO notes (id, book_id, highlight_id, file_path, title, kind, created_at,
                                last_modified)
             VALUES (1, NULL, NULL, 'a.md', 'A', 'note', 0, 0),
                    (2, NULL, NULL, 'b.md', 'B', 'note', 0, 0)",
            "UPDATE notes SET highlight_id = ?2 WHERE id = 1",
            "INSERT INTO citations (note_id, highlight_id, created_at) VALUES (1, ?2, 0),
                    (2, ?1, 0), (2, ?2, 0)",
        ] {
            sqlx::query(sql)
                .bind(keep)
                .bind(dup)
                .execute(s.pool())
                .await
                .unwrap();
        }

        assert_eq!(rehash_old_identities(s.pool(), 1).await.unwrap(), 2);
        assert_eq!(hashes(&s).await, vec![(keep, h.identity_hash())]);
        let survivor = s.get_highlight(keep).await.unwrap().unwrap();
        assert_eq!(survivor.annotation.as_deref(), Some("mine"));

        let card = format!("SELECT card_highlight_id FROM readings WHERE id = {reading}");
        assert_eq!(scalar(&s, &card).await, Some(keep));
        let anchors = "SELECT highlight_id FROM flashcards WHERE word = 'keeping'";
        assert_eq!(scalar(&s, anchors).await, Some(keep));
        assert_eq!(
            scalar(&s, "SELECT highlight_id FROM notes WHERE id = 1").await,
            Some(keep)
        );
        let cited: Vec<(i64, i64)> =
            sqlx::query_as("SELECT note_id, highlight_id FROM citations ORDER BY note_id")
                .fetch_all(s.pool())
                .await
                .unwrap();
        assert_eq!(cited, vec![(1, keep), (2, keep)]);

        assert_eq!(rehash_old_identities(s.pool(), 1).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_library_on_the_old_formula_is_rehashed_when_it_opens() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
        let (book, h) = (
            s.upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap(),
            hl("a phrase worth keeping"),
        );
        raw_row(&s, book, &h, "koreader", &old_hash(book, &h)).await;
        s.pool().close().await;

        let s = Storage::connect(&url).await.unwrap();
        assert_eq!(hashes(&s).await[0].1, h.identity_hash());
    }

    #[derive(Clone, Copy, Debug)]
    enum Fold {
        Rehash,
        Merge,
    }

    /// Two rows of one highlight, annotated as given, folded by `how`; returns the survivor's id.
    async fn folded(
        how: Fold,
        keep_note: Option<&str>,
        dup_note: Option<&str>,
        keep_alts: &[&str],
        dup_alts: &[&str],
    ) -> (Storage, i64) {
        let (s, book) = seeded().await;
        let h = hl("a phrase worth keeping");
        let (keep, dup, other_book) = match how {
            Fold::Rehash => {
                let keep = raw_row(&s, book, &h, "koreader", &old_hash(book, &h)).await;
                let dup = raw_row(&s, book, &h, "koreader", &old_hash(book + 7, &h)).await;
                (keep, dup, None)
            }
            Fold::Merge => {
                let other = s
                    .upsert_book(
                        &Book {
                            title: Some("Middlemarch (again)".into()),
                            ..Default::default()
                        },
                        None,
                    )
                    .await
                    .unwrap();
                let keep = s.insert_highlight(book, &h).await.unwrap().unwrap();
                let dup = s.insert_highlight(other, &h).await.unwrap().unwrap();
                (keep, dup, Some(other))
            }
        };
        s.set_annotation(keep, keep_note).await.unwrap();
        s.set_annotation(dup, dup_note).await.unwrap();
        for (id, alts) in [(keep, keep_alts), (dup, dup_alts)] {
            for t in alts {
                sqlx::query(
                    "INSERT INTO annotation_alternatives (highlight_id, text, set_aside_at)
                     VALUES (?, ?, 1)",
                )
                .bind(id)
                .bind(t)
                .execute(s.pool())
                .await
                .unwrap();
            }
        }
        match (how, other_book) {
            (Fold::Rehash, _) => {
                rehash_old_identities(s.pool(), 500).await.unwrap();
            }
            (Fold::Merge, Some(other)) => {
                s.merge_books(other, book).await.unwrap();
            }
            (Fold::Merge, None) => unreachable!(),
        }
        assert!(s.get_highlight(dup).await.unwrap().is_none(), "{how:?}");
        (s, keep)
    }

    async fn annotation_and_alternatives(s: &Storage, id: i64) -> (Option<String>, Vec<String>) {
        let alts = sqlx::query_scalar(
            "SELECT text FROM annotation_alternatives WHERE highlight_id = ? ORDER BY text",
        )
        .bind(id)
        .fetch_all(s.pool())
        .await
        .unwrap();
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM annotation_alternatives WHERE highlight_id <> ?",
        )
        .bind(id)
        .fetch_one(s.pool())
        .await
        .unwrap();
        assert_eq!(left, 0, "nothing is left on the dropped row");
        (s.get_highlight(id).await.unwrap().unwrap().annotation, alts)
    }

    fn owned(v: &[&str]) -> Vec<String> {
        v.iter().map(|t| (*t).to_owned()).collect()
    }

    #[tokio::test]
    async fn a_fold_never_loses_an_annotation() {
        for how in [Fold::Rehash, Fold::Merge] {
            let (s, keep) = folded(how, None, Some("only the duplicate's"), &[], &[]).await;
            assert_eq!(
                annotation_and_alternatives(&s, keep).await,
                (Some("only the duplicate's".into()), vec![]),
                "{how:?}: the survivor had none, so it takes the other's"
            );

            let (s, keep) = folded(
                how,
                Some("the same words"),
                Some("the same words \n"),
                &[],
                &[],
            )
            .await;
            assert_eq!(
                annotation_and_alternatives(&s, keep).await,
                (Some("the same words".into()), vec![]),
                "{how:?}: equal once trimmed is one annotation"
            );

            let (s, keep) = folded(how, Some("mine"), Some("theirs"), &[], &[]).await;
            assert_eq!(
                annotation_and_alternatives(&s, keep).await,
                (Some("mine".into()), owned(&["theirs"])),
                "{how:?}: different texts sit side by side"
            );

            let (s, keep) = folded(
                how,
                Some("mine"),
                None,
                &["held already"],
                &["held already", "mine", "new to the survivor"],
            )
            .await;
            assert_eq!(
                annotation_and_alternatives(&s, keep).await,
                (
                    Some("mine".into()),
                    owned(&["held already", "new to the survivor"])
                ),
                "{how:?}: the other's alternatives move, without repeats"
            );

            let (s, keep) = folded(how, None, Some("theirs"), &[], &["an older thought"]).await;
            assert_eq!(
                annotation_and_alternatives(&s, keep).await,
                (Some("theirs".into()), owned(&["an older thought"])),
                "{how:?}"
            );

            let (s, keep) = folded(how, None, None, &[], &[]).await;
            assert_eq!(annotation_and_alternatives(&s, keep).await, (None, vec![]));
        }
    }

    /// Three rows of one highlight, only the last annotated: the text survives two folds.
    #[tokio::test]
    async fn three_rows_of_one_highlight_keep_the_one_annotation() {
        let (s, book) = seeded().await;
        let h = hl("the collision passage");
        let keep = raw_row(&s, book, &h, "koreader", &old_hash(book, &h)).await;
        raw_row(&s, book, &h, "koreader", &old_hash(book + 1, &h)).await;
        let third = raw_row(&s, book, &h, "user", &old_hash(book + 2, &h)).await;
        s.set_annotation(third, Some("DUP annotation the reader wrote"))
            .await
            .unwrap();

        rehash_old_identities(s.pool(), 1).await.unwrap();
        assert_eq!(hashes(&s).await.len(), 1);
        assert_eq!(
            annotation_and_alternatives(&s, keep).await,
            (Some("DUP annotation the reader wrote".into()), vec![])
        );
    }

    async fn file_library() -> (tempfile::TempDir, String, Storage, i64) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("app.db").display());
        let s = Storage::connect(&url).await.unwrap();
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
        (dir, url, s, book)
    }

    /// Text another tool wrote that is not valid UTF-8 still has bytes, and bytes are hashed.
    #[tokio::test]
    async fn a_row_that_is_not_utf8_is_rehashed_and_the_library_opens() {
        let (_dir, url, s, book) = file_library().await;
        sqlx::query(
            "INSERT INTO highlights (book_id, text, pos0, ko_datetime, source, origin,
                                     identity_hash, created_at)
             VALUES (?, CAST(x'4d6964646c656d61726368ff' AS TEXT), '/p[1]',
                     '2025-01-01 00:00:00', 'koreader', 'koreader', 'deadbeef', 0)",
        )
        .bind(book)
        .execute(s.pool())
        .await
        .unwrap();
        s.pool().close().await;

        let s = Storage::connect(&url).await.expect("the library opens");
        let want = identity_hash_of_bytes(b"2025-01-01 00:00:00", b"/p[1]", b"Middlemarch\xff");
        assert_eq!(hashes(&s).await[0].1, want);
    }

    #[tokio::test]
    async fn a_rehash_that_cannot_finish_does_not_keep_the_library_shut() {
        let (_dir, url, s, book) = file_library().await;
        let h = hl("a phrase worth keeping");
        raw_row(&s, book, &h, "koreader", &old_hash(book, &h)).await;
        sqlx::query(
            "CREATE TRIGGER refuse BEFORE UPDATE OF identity_hash ON highlights
             BEGIN SELECT RAISE(ABORT, 'refused'); END",
        )
        .execute(s.pool())
        .await
        .unwrap();
        s.pool().close().await;

        let s = Storage::connect(&url)
            .await
            .expect("opens although the rehash failed");
        assert_eq!(hashes(&s).await[0].1, old_hash(book, &h), "left for later");
        sqlx::query("DROP TRIGGER refuse")
            .execute(s.pool())
            .await
            .unwrap();
        s.pool().close().await;

        let s = Storage::connect(&url).await.unwrap();
        assert_eq!(
            hashes(&s).await[0].1,
            h.identity_hash(),
            "the next open finished it"
        );
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
