use std::collections::HashMap;

use sqlx::Row;
use time::OffsetDateTime;

use super::highlights::{HIGHLIGHT_COLUMNS, row_to_highlight};
use super::{Highlight, Storage, now_unix};
use crate::error::{EngineError, Result};

#[derive(Debug, Clone, Copy)]
pub struct NewNoteMeta<'a> {
    pub book_id: Option<i64>,
    pub reading_id: Option<i64>,
    pub highlight_id: Option<i64>,
    pub page: Option<i64>,
    pub location: Option<&'a str>,
    pub file_path: &'a str,
    pub title: &'a str,
    pub kind: &'a str,
}

#[derive(Debug, Clone)]
pub struct NoteRecord {
    pub id: i64,
    pub book_id: Option<i64>,
    pub reading_id: Option<i64>,
    pub highlight_id: Option<i64>,
    pub page: Option<i64>,
    pub location: Option<String>,
    pub file_path: String,
    pub title: String,
    pub kind: String,
    pub created_at: Option<OffsetDateTime>,
    /// Moves with the body (insert, reindex), not with metadata edits.
    pub last_modified: Option<OffsetDateTime>,
}

pub(super) const NOTE_COLUMNS: &str = "id, book_id, reading_id, highlight_id, page, location, \
     file_path, title, kind, created_at, last_modified";

/// Which notes a list is about. An enum so a book id and a reading id, which overlap, cannot be
/// swapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoteScope {
    #[default]
    All,
    Book(i64),
    Reading(i64),
}

impl NoteScope {
    pub fn of_book(book_id: Option<i64>) -> Self {
        match book_id {
            Some(id) => Self::Book(id),
            None => Self::All,
        }
    }

    fn predicate(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Book(_) => "WHERE book_id = ?",
            Self::Reading(_) => "WHERE reading_id = ?",
        }
    }

    fn bind(self) -> Option<i64> {
        match self {
            Self::All => None,
            Self::Book(id) | Self::Reading(id) => Some(id),
        }
    }
}

/// `to` is `None` for a dangling `[[wikilink]]` whose note is not written yet; the row is kept,
/// not dropped.
#[derive(Debug, Clone)]
pub struct OutgoingLink {
    pub target_title: String,
    pub to: Option<NoteRecord>,
}

/// Highlight ids only, not text, so a reader's passages do not travel once per citing note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCitations {
    pub note_id: i64,
    pub highlight_ids: Vec<i64>,
}

/// Shared by both citation queries so they agree on order; `h.id` breaks ties because `page` and
/// `ko_datetime` are nullable.
const CITATION_ORDER: &str = "h.page ASC, h.ko_datetime ASC, h.id ASC";

/// Note ids bound per statement; stays under the 999-variable limit of SQLite builds before 3.32.
const CITATION_CHUNK: usize = 500;

/// The bundled SQLite allows far more, so a test cannot detect exceeding the old ceiling by
/// querying.
const OLD_PARAMETER_CEILING: usize = 999;

const _: () = assert!(CITATION_CHUNK <= OLD_PARAMETER_CEILING);

/// Pure function of the ids so tests can read the split; the bundled SQLite hides a missing
/// chunk.
fn citation_batches(ids: &[i64]) -> Vec<(String, &[i64])> {
    ids.chunks(CITATION_CHUNK)
        .map(|chunk| {
            let holes = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT c.note_id, h.id AS highlight_id
                 FROM citations c JOIN highlights h ON h.id = c.highlight_id
                 WHERE c.note_id IN ({holes}) ORDER BY {CITATION_ORDER}"
            );
            (sql, chunk)
        })
        .collect()
}

pub(super) fn qualified(columns: &str, alias: &str) -> String {
    columns
        .split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn row_to_note(r: &sqlx::sqlite::SqliteRow) -> Result<NoteRecord> {
    Ok(NoteRecord {
        id: r.try_get("id")?,
        book_id: r.try_get("book_id")?,
        reading_id: r.try_get("reading_id")?,
        highlight_id: r.try_get("highlight_id")?,
        page: r.try_get("page")?,
        location: r.try_get("location")?,
        file_path: r.try_get("file_path")?,
        title: r.try_get("title")?,
        kind: r.try_get("kind")?,
        created_at: OffsetDateTime::from_unix_timestamp(r.try_get::<i64, _>("created_at")?).ok(),
        last_modified: OffsetDateTime::from_unix_timestamp(r.try_get::<i64, _>("last_modified")?)
            .ok(),
    })
}

/// Shared by `write_links` and `resolve_title` so previews and saves resolve a link alike; `ORDER
/// BY id` fixes ties.
pub(super) const RESOLVE_TITLE: &str = "title = ? COLLATE NOCASE ORDER BY id LIMIT 1";

/// Shared by insert and reindex so both resolve links alike; also back-resolves older dangling
/// links to this title.
async fn write_links(
    tx: &mut sqlx::SqliteConnection,
    note_id: i64,
    title: &str,
    links: &[String],
) -> Result<()> {
    for target in links {
        let sql = format!("SELECT id FROM notes WHERE {RESOLVE_TITLE}");
        let to_note: Option<i64> = sqlx::query_scalar(&sql)
            .bind(target)
            .fetch_optional(&mut *tx)
            .await?;
        sqlx::query(
            r#"INSERT INTO note_links (from_note, to_note, target_title) VALUES (?, ?, ?)
               ON CONFLICT(from_note, target_title) DO UPDATE SET to_note = excluded.to_note"#,
        )
        .bind(note_id)
        .bind(to_note)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "UPDATE note_links SET to_note = ? WHERE to_note IS NULL AND target_title = ? COLLATE NOCASE",
    )
    .bind(note_id)
    .bind(title)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn reindex(
    tx: &mut sqlx::SqliteConnection,
    note_id: i64,
    title: &str,
    body: &str,
) -> Result<()> {
    sqlx::query("DELETE FROM notes_fts WHERE rowid = ?")
        .bind(note_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO notes_fts (rowid, title, body) VALUES (?, ?, ?)")
        .bind(note_id)
        .bind(title)
        .bind(body)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE notes SET last_modified = ? WHERE id = ?")
        .bind(now_unix())
        .bind(note_id)
        .execute(&mut *tx)
        .await?;
    put_body(tx, note_id, body).await?;
    stamp_parts(tx, note_id, body).await
}

/// Brings the index of a note in line with its stored row and body, as another device left them.
/// A note whose body has not arrived is indexed by title. Writes nothing that syncs, and leaves
/// `last_modified` alone because the vault file has not been read.
pub(super) async fn reindex_stored(tx: &mut sqlx::SqliteConnection, note_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM notes_fts WHERE rowid = ?")
        .bind(note_id)
        .execute(&mut *tx)
        .await?;
    let Some(title) = sqlx::query_scalar::<_, String>("SELECT title FROM notes WHERE id = ?")
        .bind(note_id)
        .fetch_optional(&mut *tx)
        .await?
    else {
        return Ok(());
    };
    let body: String = sqlx::query_scalar("SELECT body FROM note_bodies WHERE note_id = ?")
        .bind(note_id)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or_default();
    sqlx::query("INSERT INTO notes_fts (rowid, title, body) VALUES (?, ?, ?)")
        .bind(note_id)
        .bind(&title)
        .bind(&body)
        .execute(&mut *tx)
        .await?;
    stamp_parts(tx, note_id, &body).await?;
    sqlx::query("DELETE FROM note_links WHERE from_note = ?")
        .bind(note_id)
        .execute(&mut *tx)
        .await?;
    write_links(tx, note_id, &title, &crate::notes::extract_wikilinks(&body)).await
}

/// Whether another note has the same path but for ASCII case and a path that sorts first by
/// its bytes: on a case-insensitive file system the two are one file, and every device gives it
/// to the same note. Unicode case and normalisation variants are not caught.
pub(crate) async fn has_case_twin(
    conn: &mut sqlx::SqliteConnection,
    note_id: i64,
    file_path: &str,
) -> Result<bool> {
    let twin: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM notes
          WHERE id != ?1 AND lower(file_path) = lower(?2) AND file_path < ?2 LIMIT 1",
    )
    .bind(note_id)
    .bind(file_path)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(twin.is_some())
}

/// Takes a note out of the index: what [`reindex_stored`] does for a note that is gone, for one
/// whose stored file path is refused. It stays listed, unsearchable and unlinked.
pub(super) async fn unindex_stored(tx: &mut sqlx::SqliteConnection, note_id: i64) -> Result<()> {
    for sql in [
        "DELETE FROM notes_fts WHERE rowid = ?",
        "DELETE FROM note_links WHERE from_note = ?",
    ] {
        sqlx::query(sql).bind(note_id).execute(&mut *tx).await?;
    }
    Ok(())
}

/// An unchanged body must not write the row at all, because a write to `note_bodies` is taken to
/// mean the body changed. Trailing whitespace is no change, as in `reindex_from_body`. The promise
/// holds for AFTER triggers only: a BEFORE INSERT trigger fires ahead of the conflict check.
async fn put_body(tx: &mut sqlx::SqliteConnection, note_id: i64, body: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO note_bodies (note_id, body) VALUES (?1, ?2)
         ON CONFLICT(note_id) DO UPDATE SET body = excluded.body
         WHERE rtrim(note_bodies.body, char(9, 10, 13, 32))
               IS NOT rtrim(excluded.body, char(9, 10, 13, 32))",
    )
    .bind(note_id)
    .bind(body)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Called by every writer of `notes_fts` and nothing else, so the parts columns always track the
/// indexed body.
async fn stamp_parts(tx: &mut sqlx::SqliteConnection, note_id: i64, body: &str) -> Result<()> {
    let (parts, latest_marker) = crate::notes::part_stamps(body);
    sqlx::query(
        "UPDATE notes SET parts = ?1,
                latest_part_at = CASE WHEN ?2 IS NULL THEN created_at
                                      ELSE max(created_at, ?2) END
         WHERE id = ?3",
    )
    .bind(parts)
    .bind(latest_marker)
    .bind(note_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct NoteFile {
    pub id: i64,
    pub file_path: String,
    pub title: String,
    /// Unix seconds of the last index write.
    pub last_modified: i64,
}

impl Storage {
    pub async fn insert_note(
        &self,
        meta: NewNoteMeta<'_>,
        body: &str,
        links: &[String],
    ) -> Result<i64> {
        let NewNoteMeta {
            book_id,
            reading_id,
            highlight_id,
            page,
            location,
            file_path,
            title,
            kind,
        } = meta;
        if !crate::notes::note_path_is_plain(file_path) {
            return Err(EngineError::InvalidInput(
                "a note's file is not a markdown file inside the vault".into(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let now = now_unix();
        let note_id: i64 = sqlx::query_scalar(
            r#"INSERT INTO notes (book_id, reading_id, highlight_id, page, location, file_path, title, kind, created_at, last_modified)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id"#,
        )
        .bind(book_id)
        .bind(reading_id)
        .bind(highlight_id)
        .bind(page)
        .bind(location)
        .bind(file_path)
        .bind(title)
        .bind(kind)
        .bind(now)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query("INSERT INTO notes_fts (rowid, title, body) VALUES (?, ?, ?)")
            .bind(note_id)
            .bind(title)
            .bind(body)
            .execute(&mut *tx)
            .await?;

        put_body(&mut tx, note_id, body).await?;
        write_links(&mut tx, note_id, title, links).await?;
        stamp_parts(&mut tx, note_id, body).await?;

        tx.commit().await?;
        Ok(note_id)
    }

    pub async fn set_note_links(&self, note_id: i64, title: &str, links: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        // Replaced, not merged: a link deleted from the body must leave the graph.
        sqlx::query("DELETE FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        write_links(&mut tx, note_id, title, links).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn refresh_note_body(&self, note_id: i64, title: &str, body: &str) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        reindex(&mut tx, note_id, title, body).await?;
        tx.commit().await?;
        Ok(())
    }

    /// One transaction so a dropped future (the watcher is raced in `select!`) rolls back instead of
    /// leaving body and edges disagreeing.
    pub async fn reindex_note(
        &self,
        note_id: i64,
        title: &str,
        body: &str,
        links: &[String],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        reindex(&mut tx, note_id, title, body).await?;
        sqlx::query("DELETE FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        write_links(&mut tx, note_id, title, links).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The body as last indexed, so a refresh can skip writing when the file is unchanged.
    pub async fn indexed_body(&self, note_id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT body FROM note_bodies WHERE note_id = ?")
                .bind(note_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// A file under the vault that no row claims is not a note; files bookmole did not write are not
    /// adopted.
    /// Whether another note has this note's path but for ASCII case, and the file is that note's.
    pub(crate) async fn note_is_case_twin(&self, note_id: i64, file_path: &str) -> Result<bool> {
        let mut conn = self.pool().acquire().await?;
        has_case_twin(&mut conn, note_id, file_path).await
    }

    pub async fn note_by_path(&self, file_path: &str) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE file_path = ?");
        sqlx::query(&sql)
            .bind(file_path)
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(row_to_note)
            .transpose()
    }

    /// Not a field of `NoteRecord`: `last_modified` is index bookkeeping and must not reach
    /// `NoteDto`.
    pub async fn note_files(&self) -> Result<Vec<NoteFile>> {
        let rows = sqlx::query("SELECT id, file_path, title, last_modified FROM notes")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(NoteFile {
                    id: r.try_get("id")?,
                    file_path: r.try_get("file_path")?,
                    title: r.try_get("title")?,
                    last_modified: r.try_get("last_modified")?,
                })
            })
            .collect()
    }

    /// The FTS table is virtual, so it is cleared explicitly; a foreign key cannot cascade into it.
    pub async fn delete_note(&self, note_id: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM notes_fts WHERE rowid = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM notes WHERE id = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Newest first. `limit` cuts after the scope is applied; `None` returns every note, for callers
    /// that walk the whole graph.
    pub async fn list_notes(
        &self,
        scope: NoteScope,
        limit: Option<i64>,
    ) -> Result<Vec<NoteRecord>> {
        // `id` breaks ties between notes created in the same second, so a limit cuts deterministically.
        let order = "ORDER BY created_at DESC, id DESC";
        // A negative limit means no limit in SQLite, so one statement serves both cases.
        let limit = limit.unwrap_or(-1);
        let sql = format!(
            "SELECT {NOTE_COLUMNS} FROM notes {} {order} LIMIT ?",
            scope.predicate()
        );
        let mut q = sqlx::query(&sql);
        if let Some(id) = scope.bind() {
            q = q.bind(id);
        }
        let rows = q.bind(limit).fetch_all(self.pool()).await?;
        rows.iter().map(row_to_note).collect()
    }

    pub async fn note_for_reading(
        &self,
        reading_id: i64,
        kind: &str,
    ) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE reading_id = ? AND kind = ?");
        let row = sqlx::query(&sql)
            .bind(reading_id)
            .bind(kind)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_note).transpose()
    }

    pub async fn get_note(&self, note_id: i64) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE id = ?");
        let row = sqlx::query(&sql)
            .bind(note_id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_note).transpose()
    }

    /// Cites by reference, never by copying text, so device refreshes cannot stale it. Idempotent.
    pub async fn add_citation(&self, note_id: i64, highlight_id: i64) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO citations (note_id, highlight_id, created_at) VALUES (?, ?, ?)
             ON CONFLICT (note_id, highlight_id) DO NOTHING",
        )
        .bind(note_id)
        .bind(highlight_id)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn remove_citation(&self, note_id: i64, highlight_id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM citations WHERE note_id = ? AND highlight_id = ?")
            .bind(note_id)
            .bind(highlight_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn citations_for(&self, note_id: i64) -> Result<Vec<Highlight>> {
        let columns = qualified(HIGHLIGHT_COLUMNS, "h");
        let sql = format!(
            "SELECT {columns} FROM citations c JOIN highlights h ON h.id = c.highlight_id
             WHERE c.note_id = ? ORDER BY {CITATION_ORDER}"
        );
        let rows = sqlx::query(&sql)
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_highlight).collect()
    }

    /// Ids only, one entry per requested id in the order given, empties included (unknown note ids
    /// too).
    pub async fn citations_for_notes(&self, note_ids: &[i64]) -> Result<Vec<NoteCitations>> {
        let mut cited: HashMap<i64, Vec<i64>> = HashMap::new();
        for id in note_ids {
            cited.entry(*id).or_default();
        }
        let ids: Vec<i64> = cited.keys().copied().collect();
        for (sql, chunk) in citation_batches(&ids) {
            let mut q = sqlx::query(&sql);
            for id in chunk {
                q = q.bind(*id);
            }
            for row in q.fetch_all(self.pool()).await? {
                let note_id: i64 = row.try_get("note_id")?;
                let highlight_id: i64 = row.try_get("highlight_id")?;
                if let Some(v) = cited.get_mut(&note_id) {
                    v.push(highlight_id);
                }
            }
        }
        Ok(note_ids
            .iter()
            .map(|id| NoteCitations {
                note_id: *id,
                highlight_ids: cited[id].clone(),
            })
            .collect())
    }

    pub async fn note_links(&self, note_id: i64) -> Result<Vec<(String, Option<i64>)>> {
        let rows = sqlx::query("SELECT target_title, to_note FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("target_title")?, r.try_get("to_note")?)))
            .collect()
    }

    /// Ordered by `rowid`, which is body order because `set_note_links` rewrites the whole set.
    pub async fn outgoing_links(&self, note_id: i64) -> Result<Vec<OutgoingLink>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "SELECT l.target_title AS target_title, {columns}
             FROM note_links l LEFT JOIN notes n ON n.id = l.to_note
             WHERE l.from_note = ? ORDER BY l.rowid"
        );
        let rows = sqlx::query(&sql)
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(OutgoingLink {
                    target_title: r.try_get("target_title")?,
                    // A missed join leaves every `n.` column NULL; read `id` as an `Option` first so a
                    // dangling target is not a decode error.
                    to: r
                        .try_get::<Option<i64>, _>("id")?
                        .map(|_| row_to_note(r))
                        .transpose()?,
                })
            })
            .collect()
    }

    /// Resolved edges only, deliberately: a dangling edge reads as dangling from both ends so the two
    /// directions agree.
    pub async fn backlinks(&self, note_id: i64) -> Result<Vec<NoteRecord>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "SELECT {columns} FROM note_links l JOIN notes n ON n.id = l.from_note
             WHERE l.to_note = ? ORDER BY n.created_at DESC, n.id DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_note).collect()
    }

    /// Like `backlinks` plus `target_title`; excludes self-links, ordered by the source's last write,
    /// `created_at` if never modified.
    pub async fn incoming_links(&self, note_id: i64) -> Result<Vec<(String, NoteRecord)>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "SELECT l.target_title AS target_title, {columns}
             FROM note_links l JOIN notes n ON n.id = l.from_note
             WHERE l.to_note = ?1 AND l.from_note <> ?1
             ORDER BY COALESCE(n.last_modified, n.created_at) DESC, n.id DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("target_title")?, row_to_note(r)?)))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResurfaceReason {
    OpenBook,
    Notebook,
}

#[derive(Debug, Clone)]
pub struct Resurfaced {
    pub note: NoteRecord,
    pub reason: ResurfaceReason,
}

#[derive(Debug, Clone)]
pub struct ResurfacedPassage {
    pub highlight: Highlight,
    pub reason: ResurfaceReason,
}

/// A note kept within this many days is not offered back: it is still fresh in the reader's head.
pub const RESURFACE_REST_DAYS: i64 = 7;

/// Parses a caller's local `YYYY-MM-DD`; a malformed date is `InvalidInput`, never a fall back to
/// UTC's today.
pub fn parse_local_date(text: &str) -> Result<time::Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(text, format).map_err(|_| {
        crate::error::EngineError::InvalidInput(format!("a date must be YYYY-MM-DD, got {text:?}"))
    })
}

/// SplitMix64 finaliser, not `DefaultHasher`, whose algorithm may change and would reshuffle
/// every reader's pick.
fn day_mix(day: i64) -> u64 {
    let mut z = (day as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Storage {
    /// Qualifies if last kept before `today - RESURFACE_REST_DAYS`; notes tied to a book being read
    /// (open and `reading`) win; the pick is `day_mix(julian day) % len` over `id` order, stable all day.
    pub async fn resurface_note(&self, today: time::Date) -> Result<Option<Resurfaced>> {
        let rest_start = today
            .checked_sub(time::Duration::days(RESURFACE_REST_DAYS))
            .unwrap_or(today)
            .midnight()
            .assume_utc()
            .unix_timestamp();
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "WITH open_books AS (
                 SELECT book_id FROM readings WHERE finished_at IS NULL AND status = 'reading'
             ),
             near AS (
                 SELECT id FROM notes WHERE book_id IN (SELECT book_id FROM open_books)
             )
             SELECT {columns},
                    (n.id IN (SELECT id FROM near)
                     OR EXISTS (SELECT 1 FROM note_links l
                                WHERE (l.from_note = n.id AND l.to_note IN (SELECT id FROM near))
                                   OR (l.to_note = n.id AND l.from_note IN (SELECT id FROM near)))
                    ) AS connected
             FROM notes n
             WHERE COALESCE(n.last_modified, n.created_at) < ?
             ORDER BY n.id"
        );
        let rows = sqlx::query(&sql)
            .bind(rest_start)
            .fetch_all(self.pool())
            .await?;
        let mut near = Vec::new();
        let mut rest = Vec::new();
        for row in &rows {
            let note = row_to_note(row)?;
            if row.try_get::<bool, _>("connected")? {
                near.push(note);
            } else {
                rest.push(note);
            }
        }
        let (pool, reason) = if near.is_empty() {
            (rest, ResurfaceReason::Notebook)
        } else {
            (near, ResurfaceReason::OpenBook)
        };
        if pool.is_empty() {
            return Ok(None);
        }
        let i = (day_mix(today.to_julian_day() as i64) % pool.len() as u64) as usize;
        Ok(pool
            .into_iter()
            .nth(i)
            .map(|note| Resurfaced { note, reason }))
    }

    /// `resurface_note`'s rule for highlights. Uses `created_at`, which every highlight has and which
    /// errs toward silence.
    pub async fn resurface_passage(&self, today: time::Date) -> Result<Option<ResurfacedPassage>> {
        let rest_start = today
            .checked_sub(time::Duration::days(RESURFACE_REST_DAYS))
            .unwrap_or(today)
            .midnight()
            .assume_utc()
            .unix_timestamp();
        let columns = qualified(HIGHLIGHT_COLUMNS, "h");
        let sql = format!(
            "SELECT {columns},
                    (h.book_id IN (SELECT book_id FROM readings
                                  WHERE finished_at IS NULL AND status = 'reading'))
                        AS connected
             FROM highlights h
             WHERE h.created_at < ? AND trim(h.text) <> ''
             ORDER BY h.id"
        );
        let rows = sqlx::query(&sql)
            .bind(rest_start)
            .fetch_all(self.pool())
            .await?;
        let mut near = Vec::new();
        let mut rest = Vec::new();
        for row in &rows {
            let highlight = row_to_highlight(row)?;
            if row.try_get::<bool, _>("connected")? {
                near.push(highlight);
            } else {
                rest.push(highlight);
            }
        }
        let (pool, reason) = if near.is_empty() {
            (rest, ResurfaceReason::Notebook)
        } else {
            (near, ResurfaceReason::OpenBook)
        };
        if pool.is_empty() {
            return Ok(None);
        }
        let i = (day_mix(today.to_julian_day() as i64) % pool.len() as u64) as usize;
        Ok(pool
            .into_iter()
            .nth(i)
            .map(|highlight| ResurfacedPassage { highlight, reason }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the generated SQL because the bundled SQLite's higher parameter limit hides a missing
    /// split.
    #[test]
    fn no_batch_statement_exceeds_the_parameter_ceiling() {
        for n in [0usize, 1, CITATION_CHUNK - 1, CITATION_CHUNK, 1_203] {
            let ids: Vec<i64> = (0..n as i64).collect();
            let batches = citation_batches(&ids);

            let mut seen: Vec<i64> = Vec::new();
            for (sql, chunk) in &batches {
                let holes = sql.matches('?').count();
                assert_eq!(holes, chunk.len(), "a hole per bind, and no more");
                assert!(
                    holes <= OLD_PARAMETER_CEILING,
                    "{holes} parameters in one statement, for {n} ids"
                );
                assert!(sql.contains(CITATION_ORDER), "one order, not two");
                seen.extend_from_slice(chunk);
            }
            assert_eq!(seen, ids, "the batches partition the ids, in order");
            assert_eq!(batches.is_empty(), n == 0, "nothing asked, nothing issued");
        }
    }

    #[tokio::test]
    async fn a_body_save_moves_last_modified_and_the_row_carries_it() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: "unsorted/kept.md",
                    title: "Kept",
                    kind: "note",
                },
                "First words.",
                &[],
            )
            .await
            .unwrap();
        let fresh = s.get_note(id).await.unwrap().unwrap();
        assert!(fresh.last_modified.is_some(), "stamped on insert");
        assert_eq!(
            fresh.last_modified, fresh.created_at,
            "one write, one moment"
        );

        sqlx::query("UPDATE notes SET last_modified = 1000 WHERE id = ?")
            .bind(id)
            .execute(s.pool())
            .await
            .unwrap();
        let old = s
            .get_note(id)
            .await
            .unwrap()
            .unwrap()
            .last_modified
            .unwrap();
        assert_eq!(old.unix_timestamp(), 1000, "the column is what is read");

        s.refresh_note_body(id, "Kept", "Second words.")
            .await
            .unwrap();
        let after = s.get_note(id).await.unwrap().unwrap();
        assert!(after.last_modified.unwrap() > old, "a body save moves it");
        assert_eq!(after.created_at, fresh.created_at, "and nothing else");
    }

    #[tokio::test]
    async fn incoming_links_lead_with_the_last_kept_source_and_skip_self() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let meta = |file_path, title| NewNoteMeta {
            book_id: None,
            reading_id: None,
            highlight_id: None,
            page: None,
            location: None,
            file_path,
            title,
            kind: "note",
        };
        let t = s
            .insert_note(meta("unsorted/t.md", "Target"), "x", &["Target".into()])
            .await
            .unwrap();
        let older = s
            .insert_note(meta("unsorted/o.md", "Older"), "x", &["Target".into()])
            .await
            .unwrap();
        let newer = s
            .insert_note(meta("unsorted/n.md", "Newer"), "x", &["target".into()])
            .await
            .unwrap();
        // `older` was created first but kept last.
        for (id, at) in [(older, 3000), (newer, 2000)] {
            sqlx::query("UPDATE notes SET last_modified = ? WHERE id = ?")
                .bind(at)
                .bind(id)
                .execute(s.pool())
                .await
                .unwrap();
        }
        let got: Vec<_> = s
            .incoming_links(t)
            .await
            .unwrap()
            .into_iter()
            .map(|(title, n)| (title, n.id))
            .collect();
        assert_eq!(
            got,
            vec![("Target".to_string(), older), ("target".to_string(), newer)],
            "the target text as each source wrote it, newest-kept first, no self"
        );
    }

    #[tokio::test]
    async fn fts_roundtrip_and_link_resolution() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();

        let n1 = s
            .insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: "unsorted/a.md",
                    title: "First thought",
                    kind: "note",
                },
                "Reminds me of [[Vocation]] as a concept.",
                &["Vocation".to_string()],
            )
            .await
            .unwrap();
        let links = s.note_links(n1).await.unwrap();
        assert_eq!(links, vec![("Vocation".to_string(), None)]);

        let n2 = s
            .insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: "unsorted/vocation.md",
                    title: "Vocation",
                    kind: "note",
                },
                "A calling, and what it costs.",
                &[],
            )
            .await
            .unwrap();
        let links = s.note_links(n1).await.unwrap();
        assert_eq!(links, vec![("Vocation".to_string(), Some(n2))]);

        let search = |s: Storage, q: &'static str| async move {
            s.search_marks(q, Some(super::super::SearchSource::Note), None, 10)
                .await
                .unwrap()
        };
        let hits = search(s.clone(), "calling").await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].as_note().unwrap().id, n2);
        assert!(hits[0].snippet().contains(">>calling<<"));

        s.refresh_note_body(n2, "Vocation", "Now about resilience.")
            .await
            .unwrap();
        assert!(search(s.clone(), "calling").await.is_empty());
        assert_eq!(search(s.clone(), "resilience").await.len(), 1);
    }

    #[tokio::test]
    async fn a_note_limit_selects_the_newest_and_its_absence_is_everything() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        for (i, title) in ["one", "two", "three", "four"].into_iter().enumerate() {
            s.insert_note(
                NewNoteMeta {
                    book_id: None,
                    reading_id: None,
                    highlight_id: None,
                    page: None,
                    location: None,
                    file_path: &format!("unsorted/{i}.md"),
                    title,
                    kind: "note",
                },
                "body",
                &[],
            )
            .await
            .unwrap();
        }

        let all = s.list_notes(NoteScope::All, None).await.unwrap();
        assert_eq!(all.len(), 4, "no limit is every note");
        let page = s.list_notes(NoteScope::All, Some(2)).await.unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(
            page.iter().map(|n| n.id).collect::<Vec<_>>(),
            all.iter().take(2).map(|n| n.id).collect::<Vec<_>>(),
            "a limited list is the head of the unlimited one — the newest, not \
             two arbitrary notes"
        );
        assert!(
            s.list_notes(NoteScope::All, Some(0))
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// Seed one book with two readings and `per` notes each; the second reading's notes are the
    /// newest.
    async fn a_book_read_twice(per: usize) -> (Storage, i64, i64, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let book = s
            .upsert_book(
                &crate::book::Book {
                    title: Some("Middlemarch".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        // `idx_readings_one_open` allows one open reading per book, so the first closes before the second
        // opens.
        let first = s
            .open_reading(book, Some(1_700_000_000), "manual")
            .await
            .unwrap();
        s.close_reading_at(first, 1_700_100_000, "complete")
            .await
            .unwrap();
        let second = s
            .open_reading(book, Some(1_700_200_000), "manual")
            .await
            .unwrap();

        for (reading, tag) in [(first, "first"), (second, "second")] {
            for i in 0..per {
                let path = format!("middlemarch/{tag}-{i}.md");
                let title = format!("{tag} read, note {i}");
                s.insert_note(
                    NewNoteMeta {
                        book_id: Some(book),
                        reading_id: Some(reading),
                        highlight_id: None,
                        page: None,
                        location: None,
                        file_path: &path,
                        title: &title,
                        kind: "note",
                    },
                    "body",
                    &[],
                )
                .await
                .unwrap();
            }
        }
        (s, book, first, second)
    }

    #[tokio::test]
    async fn a_reading_scope_finds_notes_a_filter_above_the_seam_would_have_lost() {
        const LIMIT: i64 = 5;
        let (s, book, first, second) = a_book_read_twice(8).await;

        let page = s
            .list_notes(NoteScope::Book(book), Some(LIMIT))
            .await
            .unwrap();
        assert_eq!(page.len(), LIMIT as usize);
        assert!(
            page.iter().all(|n| n.reading_id == Some(second)),
            "the fixture must fill the book's page with the second read, or \
             this proves nothing"
        );
        assert!(
            !page.iter().any(|n| n.reading_id == Some(first)),
            "filtering this page to the first read is the empty list the scope \
             prevents"
        );

        let scoped = s
            .list_notes(NoteScope::Reading(first), Some(LIMIT))
            .await
            .unwrap();
        assert_eq!(
            scoped.len(),
            LIMIT as usize,
            "a full page of the read asked for"
        );
        assert!(scoped.iter().all(|n| n.reading_id == Some(first)));
    }

    #[tokio::test]
    async fn a_book_holds_its_reads_and_the_notes_that_name_no_read() {
        let (s, book, first, second) = a_book_read_twice(2).await;
        s.insert_note(
            NewNoteMeta {
                book_id: Some(book),
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "middlemarch/loose.md",
                title: "before either read",
                kind: "note",
            },
            "body",
            &[],
        )
        .await
        .unwrap();
        s.insert_note(
            NewNoteMeta {
                book_id: None,
                reading_id: None,
                highlight_id: None,
                page: None,
                location: None,
                file_path: "unsorted/free.md",
                title: "about no book at all",
                kind: "note",
            },
            "body",
            &[],
        )
        .await
        .unwrap();

        assert_eq!(s.list_notes(NoteScope::All, None).await.unwrap().len(), 6);
        assert_eq!(
            s.list_notes(NoteScope::Book(book), None)
                .await
                .unwrap()
                .len(),
            5,
            "both reads plus the book-anchored one, and never the free-floating \
             note"
        );
        for r in [first, second] {
            let read = s.list_notes(NoteScope::Reading(r), None).await.unwrap();
            assert_eq!(read.len(), 2);
            assert!(read.iter().all(|n| n.reading_id == Some(r)));
        }
    }

    #[tokio::test]
    async fn a_scope_restricts_the_list_and_never_reorders_it() {
        let (s, book, first, second) = a_book_read_twice(4).await;
        let all = s.list_notes(NoteScope::All, None).await.unwrap();
        for (scope, keep) in [
            (NoteScope::Book(book), None),
            (NoteScope::Reading(first), Some(first)),
            (NoteScope::Reading(second), Some(second)),
        ] {
            let scoped: Vec<i64> = s
                .list_notes(scope, None)
                .await
                .unwrap()
                .iter()
                .map(|n| n.id)
                .collect();
            let expected: Vec<i64> = all
                .iter()
                .filter(|n| match keep {
                    Some(r) => n.reading_id == Some(r),
                    None => n.book_id == Some(book),
                })
                .map(|n| n.id)
                .collect();
            assert!(!scoped.is_empty());
            assert_eq!(scoped, expected, "{scope:?}");
        }
    }
}
