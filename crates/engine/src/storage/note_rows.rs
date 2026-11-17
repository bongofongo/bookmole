//! The note list and the link graph: the rows a notes view lists, the threads
//! built around a hub note, and the graph of links between notes.
//!
//! Every query here reads **the index**, never the vault: bodies come from
//! `notes_fts` (an ordinary fts5 table, readable by rowid — see
//! [`Storage::indexed_body`]) and edges from `note_links`. A list is therefore
//! one or two statements, not one file read per row, and the shapes built from
//! these rows (excerpts, headings, threads, the graph) are derived in Rust by
//! the engine facade, where they can be tested without a database.
//!
//! **Sets cross as one JSON bind.** A list of ids or kinds is bound as a single
//! JSON array and opened with `json_each`, so a caller handing the engine every
//! note in the vault cannot meet `SQLITE_MAX_VARIABLE_NUMBER` (999 on builds
//! before 3.32) — the problem `citation_batches` solves by chunking, solved here
//! without a second statement, since these lists are ordered by one `ORDER BY`
//! that a chunked read would have to re-merge.

use std::collections::HashMap;

use sqlx::Row;

use super::notes::{NOTE_COLUMNS, RESOLVE_TITLE, qualified, row_to_note};
use super::{NoteRecord, NoteScope, Storage};
use crate::error::Result;

/// Which notes a list is about, and how many. The engine's half of
/// `ListNoteRows`.
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

/// One note as the list query reads it, before the engine derives anything.
#[derive(Debug, Clone)]
pub struct NoteRowFacts {
    pub note: NoteRecord,
    pub parts: i64,
    pub latest_part_at: i64,
    /// The indexed body — `""` for a note whose index row is missing, which is
    /// a note the reconcile has not reached and not an error.
    pub body: String,
    pub links_out: i64,
    pub links_in: i64,
}

/// One edge as `note_links` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkEdge {
    pub from: i64,
    /// `None` is an unwritten target — a forward reference.
    pub to: Option<i64>,
    pub target_title: String,
}

/// The three facts about a note the graph and the threads are built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteStamp {
    pub id: i64,
    pub latest_part_at: i64,
    pub book_id: Option<i64>,
}

/// `latest_part_at` as every reader here spells it. The column is back-filled
/// and stamped by every writer (migration `0023`), so the `COALESCE` is a
/// belt for a row nothing can produce rather than a case — and it is **not** in
/// any `WHERE` or `ORDER BY`, where it would cost the index.
const LATEST: &str = "COALESCE(n.latest_part_at, n.created_at)";

/// Distinct targets a note's body names: a resolved edge counts once per
/// note, a dangling one once per `NOCASE` spelling (SQLite's `lower` is ASCII,
/// exactly `NOCASE`'s fold). A self-link names nothing.
const LINKS_OUT: &str = "(SELECT count(DISTINCT CASE WHEN l.to_note IS NOT NULL
                                     THEN 'n' || l.to_note
                                     ELSE 't' || lower(l.target_title) END)
            FROM note_links l
           WHERE l.from_note = n.id AND (l.to_note IS NULL OR l.to_note <> n.id))";

/// Distinct notes linking here, a self-link excluded.
const LINKS_IN: &str = "(SELECT count(DISTINCT l.from_note) FROM note_links l
           WHERE l.to_note = n.id AND l.from_note <> n.id)";

fn json_ids(ids: &[i64]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

impl Storage {
    /// The rows a note list draws, newest part first — the one query behind
    /// `ListNoteRows`, `ThreadsFor`'s hubs, `LinksFromNotes`' sources and the
    /// graph's nodes.
    ///
    /// **Ordered by `latest_part_at DESC, id DESC`**: a continued note moves up
    /// when a part is added and never when it is edited, and `id` closes the
    /// second-resolution tie (every paged list needs a total order). Every
    /// narrowing is a predicate before the `LIMIT` — a limit that cuts first and a
    /// filter that runs after it answers *nothing* whenever the newest rows are
    /// somebody else's.
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
        // The row value compares like the `ORDER BY` reads, reversed: strictly
        // after `(at, id)` in `latest_part_at DESC, id DESC` is strictly less.
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
        Ok(rows
            .iter()
            .map(|r| NoteRowFacts {
                note: row_to_note(r),
                parts: r.get("parts"),
                latest_part_at: r.get("latest"),
                body: r.get::<Option<String>, _>("body").unwrap_or_default(),
                links_out: r.get("links_out"),
                links_in: r.get("links_in"),
            })
            .collect())
    }

    /// Every calendar month some note's latest part falls in, newest first, as
    /// `(year, month)`. **UTC**, like every other day-grained answer the engine
    /// gives (`reading_events.day`, `activity_by_month`): the engine has no
    /// zone, and a frontend phrasing *October* for a reader at UTC−8 accepts
    /// that a note written on the evening of 31 October files under November.
    pub async fn note_periods(&self) -> Result<Vec<(i32, u8)>> {
        self.note_periods_in(0).await
    }

    /// [`Storage::note_periods`] in a fixed zone `offset_seconds` east of UTC:
    /// each instant is shifted before it is read as a calendar month, so a
    /// note written at 23:30 on 31 October at UTC−8 files under October.
    /// The offset is the caller's to validate; `0` is UTC.
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
        Ok(rows
            .iter()
            .map(|r| (r.get::<i64, _>("y") as i32, r.get::<i64, _>("m") as u8))
            .collect())
    }

    /// Every edge in the vault, in body order within each source
    /// (`note_links.rowid` — `set_note_links` rewrites a note's whole set, so
    /// insertion order is the order the body wrote them).
    pub async fn all_link_edges(&self) -> Result<Vec<LinkEdge>> {
        let rows = sqlx::query(
            "SELECT from_note, to_note, target_title FROM note_links ORDER BY from_note, rowid",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| LinkEdge {
                from: r.get("from_note"),
                to: r.get("to_note"),
                target_title: r.get("target_title"),
            })
            .collect())
    }

    /// Every note reduced to what the graph needs: its id, its recency and its
    /// book. Three integers a row, so the whole vault is one cheap read.
    pub async fn note_stamps(&self) -> Result<Vec<NoteStamp>> {
        let sql =
            format!("SELECT n.id AS id, {LATEST} AS latest, n.book_id AS book_id FROM notes n");
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        Ok(rows
            .iter()
            .map(|r| NoteStamp {
                id: r.get("id"),
                latest_part_at: r.get("latest"),
                book_id: r.get("book_id"),
            })
            .collect())
    }

    /// The wikilinks these notes wrote, each with the note it resolves to —
    /// grouped by source, body order within one (`LinksFromNotes`). The
    /// engine orders the sources; this only keeps each one's links in order.
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
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get("from_id"),
                    r.get("target_title"),
                    // The join missed for a forward reference; see `outgoing_links`.
                    r.get::<Option<i64>, _>("id").map(|_| row_to_note(r)),
                )
            })
            .collect())
    }

    /// The note a `[[title]]` resolves to right now, by the same clause
    /// `write_links` resolves with ([`RESOLVE_TITLE`]).
    pub async fn resolve_title(&self, title: &str) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE {RESOLVE_TITLE}");
        Ok(sqlx::query(&sql)
            .bind(title.trim())
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(row_to_note))
    }

    /// The **dangling** edges naming `title` — the links a note with that title
    /// would resolve the moment it was written (`write_links` back-resolves
    /// only `to_note IS NULL`, so these and no others). Newest-kept source
    /// first, `incoming_links`' order.
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
        Ok(rows
            .iter()
            .map(|r| (r.get("target_title"), row_to_note(r)))
            .collect())
    }

    /// Notes whose **body** holds `phrase` as an fts5 phrase, in the index's
    /// own rank, with the indexed body and a snippet — the candidates for
    /// `UnlinkedMentions`. The engine then keeps only the ones
    /// [`crate::notes::find_mention`] agrees are unlinked mentions, which is
    /// why the body comes back: tokenisation ignores punctuation and `[[…]]`,
    /// and an offer `LinkMention` could not honour is a dead end.
    ///
    /// The phrase is the note's title — the reader's own words, so it is never
    /// logged above `trace!`.
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
        Ok(rows
            .iter()
            .map(|r| (row_to_note(r), r.get("body"), r.get("snip")))
            .collect())
    }

    /// The notes linked to this one in either direction, resolved edges only.
    pub async fn linked_note_ids(&self, note_id: i64) -> Result<Vec<i64>> {
        let rows = sqlx::query(
            "SELECT from_note AS id FROM note_links WHERE to_note = ?1
             UNION SELECT to_note FROM note_links WHERE from_note = ?1 AND to_note IS NOT NULL",
        )
        .bind(note_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(|r| r.get("id")).collect())
    }

    /// `latest_part_at` for these notes, keyed by id.
    pub async fn latest_parts(&self, ids: &[i64]) -> Result<HashMap<i64, i64>> {
        let sql = format!(
            "SELECT n.id AS id, {LATEST} AS latest FROM notes n
             WHERE n.id IN (SELECT value FROM json_each(?))"
        );
        let rows = sqlx::query(&sql)
            .bind(json_ids(ids))
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("latest")))
            .collect())
    }
}
