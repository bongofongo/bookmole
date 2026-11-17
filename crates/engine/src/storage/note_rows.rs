//! The note list and link graph, read from the index (`notes_fts`, `note_links`), never the vault.
//! Id and kind sets bind as one JSON array opened with `json_each`, avoiding the variable cap.

use std::collections::HashMap;

use sqlx::Row;

use super::notes::{NOTE_COLUMNS, RESOLVE_TITLE, qualified, row_to_note};
use super::{NoteRecord, NoteScope, Storage};
use crate::error::Result;

#[derive(Debug, Clone, Default)]
pub struct NoteRowFilter {
    /// `None` is every kind; `Some(vec![])` is none.
    pub kinds: Option<Vec<String>>,
    pub scope: NoteScope,
    /// Exactly these notes — still in the engine's order, never the caller's.
    pub ids: Option<Vec<i64>>,
    /// Unix seconds, exclusive: rows whose latest part is before this.
    pub until: Option<i64>,
    /// `(latest_part_at, id)`: rows strictly after this one in the list's
    /// order — the keyset cursor `ListNoteRowsAfter` pages with.
    pub after: Option<(i64, i64)>,
    /// SQLite's reading: negative is no limit, `list_notes`' contract.
    pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct NoteRowFacts {
    pub note: NoteRecord,
    pub parts: i64,
    pub latest_part_at: i64,
    /// `""` for a note whose index row is missing (not yet reconciled); not an error.
    pub body: String,
    pub links_out: i64,
    pub links_in: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEdge {
    pub from: i64,
    /// `None` is an unwritten target — a forward reference.
    pub to: Option<i64>,
    pub target_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteStamp {
    pub id: i64,
    pub latest_part_at: i64,
    pub book_id: Option<i64>,
}

/// Keep out of any `WHERE` or `ORDER BY`: the `COALESCE` would defeat the index.
const LATEST: &str = "COALESCE(n.latest_part_at, n.created_at)";

/// Distinct targets: a resolved edge counts once per note, a dangling one once per `NOCASE`
/// spelling; self-links are excluded.
const LINKS_OUT: &str = "(SELECT count(DISTINCT CASE WHEN l.to_note IS NOT NULL
                                     THEN 'n' || l.to_note
                                     ELSE 't' || lower(l.target_title) END)
            FROM note_links l
           WHERE l.from_note = n.id AND (l.to_note IS NULL OR l.to_note <> n.id))";

const LINKS_IN: &str = "(SELECT count(DISTINCT l.from_note) FROM note_links l
           WHERE l.to_note = n.id AND l.from_note <> n.id)";

fn json_ids(ids: &[i64]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

impl Storage {
    /// Ordered by `latest_part_at DESC, id DESC`. Every narrowing is a predicate before the `LIMIT`,
    /// since filtering after a limit can return nothing.
    pub async fn note_row_facts(&self, f: &NoteRowFilter) -> Result<Vec<NoteRowFacts>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let mut clauses: Vec<&str> = Vec::new();
        if f.kinds.is_some() {
            clauses.push("n.kind IN (SELECT value FROM json_each(?))");
        }
        match f.scope {
            NoteScope::All => {}
            NoteScope::Book(_) => clauses.push("n.book_id = ?"),
            NoteScope::Reading(_) => clauses.push("n.reading_id = ?"),
        }
        if f.ids.is_some() {
            clauses.push("n.id IN (SELECT value FROM json_each(?))");
        }
        if f.until.is_some() {
            clauses.push("n.latest_part_at < ?");
        }
        // Strictly after `(at, id)` in a DESC order is strictly less.
        if f.after.is_some() {
            clauses.push("(n.latest_part_at, n.id) < (?, ?)");
        }
        let predicate = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {columns}, n.parts AS parts, {LATEST} AS latest,
                    f.body AS body, {LINKS_OUT} AS links_out, {LINKS_IN} AS links_in
             FROM notes n LEFT JOIN notes_fts f ON f.rowid = n.id
             {predicate}
             ORDER BY n.latest_part_at DESC, n.id DESC
             LIMIT ?"
        );
        let mut q = sqlx::query(&sql);
        if let Some(kinds) = &f.kinds {
            q = q.bind(serde_json::to_string(kinds).unwrap_or_else(|_| "[]".into()));
        }
        match f.scope {
            NoteScope::All => {}
            NoteScope::Book(id) | NoteScope::Reading(id) => q = q.bind(id),
        }
        if let Some(ids) = &f.ids {
            q = q.bind(json_ids(ids));
        }
        if let Some(until) = f.until {
            q = q.bind(until);
        }
        if let Some((at, id)) = f.after {
            q = q.bind(at).bind(id);
        }
        let rows = q.bind(f.limit).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|r| {
                Ok(NoteRowFacts {
                    note: row_to_note(r)?,
                    parts: r.try_get("parts")?,
                    latest_part_at: r.try_get("latest")?,
                    body: r.try_get::<Option<String>, _>("body")?.unwrap_or_default(),
                    links_out: r.try_get("links_out")?,
                    links_in: r.try_get("links_in")?,
                })
            })
            .collect()
    }

    /// `(year, month)` in UTC, newest first; the engine has no zone.
    pub async fn note_periods(&self) -> Result<Vec<(i32, u8)>> {
        self.note_periods_in(0).await
    }

    /// In a fixed zone `offset_seconds` east of UTC; the caller validates the offset.
    pub async fn note_periods_in(&self, offset_seconds: i64) -> Result<Vec<(i32, u8)>> {
        let rows = sqlx::query(
            "SELECT DISTINCT CAST(strftime('%Y', latest_part_at + ?1, 'unixepoch') AS INTEGER) AS y,
                             CAST(strftime('%m', latest_part_at + ?1, 'unixepoch') AS INTEGER) AS m
             FROM notes WHERE latest_part_at IS NOT NULL
             ORDER BY y DESC, m DESC",
        )
        .bind(offset_seconds)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get::<i64, _>("y")? as i32,
                    r.try_get::<i64, _>("m")? as u8,
                ))
            })
            .collect()
    }

    /// In body order within each source (`note_links.rowid`).
    pub async fn all_link_edges(&self) -> Result<Vec<LinkEdge>> {
        let rows = sqlx::query(
            "SELECT from_note, to_note, target_title FROM note_links ORDER BY from_note, rowid",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(LinkEdge {
                    from: r.try_get("from_note")?,
                    to: r.try_get("to_note")?,
                    target_title: r.try_get("target_title")?,
                })
            })
            .collect()
    }

    pub async fn note_stamps(&self) -> Result<Vec<NoteStamp>> {
        let sql =
            format!("SELECT n.id AS id, {LATEST} AS latest, n.book_id AS book_id FROM notes n");
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        rows.iter()
            .map(|r| {
                Ok(NoteStamp {
                    id: r.try_get("id")?,
                    latest_part_at: r.try_get("latest")?,
                    book_id: r.try_get("book_id")?,
                })
            })
            .collect()
    }

    /// Grouped by source, body order within one; the engine orders the sources.
    pub async fn links_written(
        &self,
        note_ids: &[i64],
    ) -> Result<Vec<(i64, String, Option<NoteRecord>)>> {
        let columns = qualified(NOTE_COLUMNS, "t");
        let sql = format!(
            "SELECT l.from_note AS from_id, l.target_title AS target_title, {columns}
             FROM note_links l LEFT JOIN notes t ON t.id = l.to_note
             WHERE l.from_note IN (SELECT value FROM json_each(?))
             ORDER BY l.from_note, l.rowid"
        );
        let rows = sqlx::query(&sql)
            .bind(json_ids(note_ids))
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("from_id")?,
                    r.try_get("target_title")?,
                    r.try_get::<Option<i64>, _>("id")?
                        .map(|_| row_to_note(r))
                        .transpose()?,
                ))
            })
            .collect()
    }

    pub async fn resolve_title(&self, title: &str) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE {RESOLVE_TITLE}");
        sqlx::query(&sql)
            .bind(title.trim())
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(row_to_note)
            .transpose()
    }

    /// Only dangling edges (`to_note IS NULL`): exactly the ones a note with this title would resolve.
    pub async fn dangling_links_to(&self, title: &str) -> Result<Vec<(String, NoteRecord)>> {
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "SELECT l.target_title AS target_title, {columns}
             FROM note_links l JOIN notes n ON n.id = l.from_note
             WHERE l.to_note IS NULL AND l.target_title = ? COLLATE NOCASE
             ORDER BY COALESCE(n.last_modified, n.created_at) DESC, n.id DESC"
        );
        let rows = sqlx::query(&sql)
            .bind(title.trim())
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("target_title")?, row_to_note(r)?)))
            .collect()
    }

    /// The body comes back because fts tokenisation ignores punctuation and `[[…]]`; the engine then
    /// filters with `find_mention`. The phrase is the reader's words: never logged above `trace!`.
    pub async fn mention_candidates(
        &self,
        phrase: &str,
    ) -> Result<Vec<(NoteRecord, String, String)>> {
        let quoted = format!("body : \"{}\"", phrase.trim().replace('"', "\"\""));
        tracing::trace!(phrase = %quoted, "looking for unlinked mentions");
        let columns = qualified(NOTE_COLUMNS, "n");
        let sql = format!(
            "SELECT {columns}, notes_fts.body AS body,
                    snippet(notes_fts, -1, '>>', '<<', '…', 12) AS snip
             FROM notes_fts JOIN notes n ON n.id = notes_fts.rowid
             WHERE notes_fts MATCH ? ORDER BY rank"
        );
        let rows = sqlx::query(&sql)
            .bind(quoted)
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((row_to_note(r)?, r.try_get("body")?, r.try_get("snip")?)))
            .collect()
    }

    pub async fn linked_note_ids(&self, note_id: i64) -> Result<Vec<i64>> {
        let rows = sqlx::query(
            "SELECT from_note AS id FROM note_links WHERE to_note = ?1
             UNION SELECT to_note FROM note_links WHERE from_note = ?1 AND to_note IS NOT NULL",
        )
        .bind(note_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
    }

    pub async fn latest_parts(&self, ids: &[i64]) -> Result<HashMap<i64, i64>> {
        let sql = format!(
            "SELECT n.id AS id, {LATEST} AS latest FROM notes n
             WHERE n.id IN (SELECT value FROM json_each(?))"
        );
        let rows = sqlx::query(&sql)
            .bind(json_ids(ids))
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("latest")?)))
            .collect()
    }
}
