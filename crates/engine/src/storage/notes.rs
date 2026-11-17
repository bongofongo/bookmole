use std::collections::HashMap;

use sqlx::Row;
use time::OffsetDateTime;

use super::highlights::{HIGHLIGHT_COLUMNS, row_to_highlight};
use super::{Highlight, Storage, now_unix};
use crate::error::Result;

/// Metadata for a new note row (body is passed separately — it lives on
/// disk and only enters the DB as the FTS cache).
#[derive(Debug, Clone, Copy)]
pub struct NewNoteMeta<'a> {
    pub book_id: Option<i64>,
    /// The reading this note belongs to. A reflection or a review always has
    /// one; an ordinary note may float free.
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
    /// When the body was last kept — stamped by every insert and every body
    /// re-index (`reindex`), so it moves with what the reader wrote rather
    /// than with a metadata edit.
    pub last_modified: Option<OffsetDateTime>,
}

pub(super) const NOTE_COLUMNS: &str = "id, book_id, reading_id, highlight_id, page, location, \
     file_path, title, kind, created_at, last_modified";

/// Which notes a list is about.
///
/// An enum rather than the two `Option<i64>`s the API carries, for the reason
/// `SearchHit` states one module over: *a shape that can represent neither or
/// both is a shape somebody eventually reaches for `unwrap` on.* Here the extra
/// state is worse than useless — a reading belongs to exactly one book, so
/// "book 3 **and** reading 9" is either redundant or a contradiction that
/// returns nothing and looks like an empty vault. The three cases are the three
/// questions, and the fourth is not a question.
///
/// It also closes a plain hazard. `list_notes` would otherwise take three
/// adjacent `Option<i64>` — book, reading, limit — where a transposition
/// compiles, and book ids and reading ids overlap from `1` in every library, so
/// the wrong list would look like a right one.
///
/// [`StatusFilter`](super::StatusFilter) is the precedent: absence is a case of
/// the question, not a variant of the thing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoteScope {
    /// Every note in the vault — including the unanchored ones.
    #[default]
    All,
    /// Every note of one book, across all of its readings, plus the ones
    /// anchored to the book and to no read of it.
    Book(i64),
    /// The notes of one reading: a reading's card is per reading, not per
    /// book.
    Reading(i64),
}

impl NoteScope {
    /// The scope a caller holding an optional book id means — `rb note list`
    /// and `rb book` both have one.
    pub fn of_book(book_id: Option<i64>) -> Self {
        match book_id {
            Some(id) => Self::Book(id),
            None => Self::All,
        }
    }

    /// The `WHERE` this scope is, or the empty string. Never interpolates the
    /// id — it travels as a bind, beside the clause.
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

/// One outgoing edge, read from the linking note's side.
///
/// `to` is `None` for a **dangling** target: a `[[wikilink]]` naming a note that
/// has not been written yet. That is an ordinary zettelkasten forward reference,
/// not an error, which is why the raw text is carried beside the resolution
/// rather than the row being dropped.
#[derive(Debug, Clone)]
pub struct OutgoingLink {
    /// The wikilink target exactly as the body wrote it.
    pub target_title: String,
    /// The note it resolves to, when one exists.
    pub to: Option<NoteRecord>,
}

/// Which passages one note cites, as handles.
///
/// The batch answer, and deliberately **not** a `Vec<Highlight>` per note: see
/// [`Storage::citations_for_notes`] for why the text does not travel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteCitations {
    pub note_id: i64,
    /// In the order a book reads, the same order [`Storage::citations_for`]
    /// returns its rows in.
    pub highlight_ids: Vec<i64>,
}

/// The order a book reads, written once and used by **both** citation queries.
///
/// [`Storage::citations_for`] and [`Storage::citations_for_notes`] answer the
/// same question at two grains, and a client that prefers the batch on a page
/// and the single call in a pane must not see two orders — so the clause is
/// shared for `BookFilter::predicate`'s reason, which is that two hand-written
/// clauses that agree today disagree the first time one of them changes.
///
/// **`h.id` is a tie-break and not part of the reading order.** `page` and
/// `ko_datetime` are both nullable — a legacy sidecar supplies neither — so two
/// cited passages of one note can tie on the whole key, and SQL leaves a tie
/// unordered. The two queries are different plans over different row sets, so
/// nothing makes them break such a tie the same way, and the agreement between
/// them would then be true only by luck. `list_books` learned this on
/// `books.id`, where the behavioural test cannot see the tie-break's absence;
/// the difference here is that the two statements are ordered by *one
/// string*, so the guarantee is structural rather than asserted.
const CITATION_ORDER: &str = "h.page ASC, h.ko_datetime ASC, h.id ASC";

/// How many note ids one statement of [`Storage::citations_for_notes`] binds.
///
/// `book_summaries`' number, for `book_summaries`' reason: a caller may hand
/// the batch every note in the vault, and **`SQLITE_MAX_VARIABLE_NUMBER` is 999
/// on builds before 3.32**. Chunked rather than documented-as-a-limit, because
/// a limit stated in a doc comment is a limit somebody exceeds.
const CITATION_CHUNK: usize = 500;

/// The ceiling the chunk exists to stay under. Not the ceiling *this* build
/// enforces — sqlx bundles a modern SQLite whose default is 32 766, which is
/// exactly why the chunk cannot be checked by asking the database.
const OLD_PARAMETER_CEILING: usize = 999;

// The relationship between the two, pinned where raising the chunk is written
// rather than where it is discovered. `no_batch_statement_exceeds_the_parameter_ceiling`
// reads the generated SQL; this reads the number.
const _: () = assert!(CITATION_CHUNK <= OLD_PARAMETER_CEILING);

/// The statements [`Storage::citations_for_notes`] will issue, as a pure
/// function of the ids — so the split can be **read** rather than trusted.
///
/// This shape is not tidiness. The behavioural test *cannot see the chunk's
/// absence*: measured, not assumed — remove the split and 1 203 ids still bind
/// and still return the right rows, because the SQLite sqlx bundles allows
/// 32 766 parameters. The failure is reserved for whichever older build a user
/// happens to have, on whichever library happens to be big enough, which is
/// nobody's development machine. `order_by_is_a_total_order` hit the
/// identical wall and drew the identical conclusion: when the
/// behavioural test is green either way, the guard has to read the SQL.
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

/// Prefix a canonical column list with a table alias, so a joined query can
/// reuse `NOTE_COLUMNS` / `HIGHLIGHT_COLUMNS` instead of spelling out a second
/// copy that can drift from it.
pub(super) fn qualified(columns: &str, alias: &str) -> String {
    columns
        .split(", ")
        .map(|c| format!("{alias}.{c}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn row_to_note(r: &sqlx::sqlite::SqliteRow) -> NoteRecord {
    NoteRecord {
        id: r.get("id"),
        book_id: r.get("book_id"),
        reading_id: r.get("reading_id"),
        highlight_id: r.get("highlight_id"),
        page: r.get("page"),
        location: r.get("location"),
        file_path: r.get("file_path"),
        title: r.get("title"),
        kind: r.get("kind"),
        created_at: OffsetDateTime::from_unix_timestamp(r.get::<i64, _>("created_at")).ok(),
        last_modified: OffsetDateTime::from_unix_timestamp(r.get::<i64, _>("last_modified")).ok(),
    }
}

/// How a `[[wikilink]]` target finds its note: the title compared `NOCASE`,
/// and the lowest id when two notes share it. Written once and read by
/// [`write_links`] and [`Storage::resolve_title`] (`PreviewLinks`), so a
/// preview resolves a link exactly as saving it would. `ORDER BY id` is the
/// tie-break the bare `LIMIT 1` was already getting from a rowid scan — stated
/// now, because two statements must not break a tie two ways.
pub(super) const RESOLVE_TITLE: &str = "title = ? COLLATE NOCASE ORDER BY id LIMIT 1";

/// Write a note's outgoing edges: each target resolved against existing note
/// titles, kept as text when dangling (zettelkasten forward references), then
/// any older dangling link pointing at *this* note's title back-resolved.
///
/// Shared by the insert and the re-index so the two cannot disagree about what
/// a link means.
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

    // Older notes may already link to this title.
    sqlx::query(
        "UPDATE note_links SET to_note = ? WHERE to_note IS NULL AND target_title = ? COLLATE NOCASE",
    )
    .bind(note_id)
    .bind(title)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Rewrite a note's FTS row and stamp `last_modified`.
///
/// Shared by [`Storage::refresh_note_body`] and [`Storage::reindex_note`] so
/// the two cannot disagree about what re-indexing a body means.
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
    stamp_parts(tx, note_id, body).await
}

/// Recompute migration `0023`'s two columns from the body that was just
/// indexed — `parts`, and `latest_part_at` as the newest of part 1's date
/// (`created_at`) and every marker's.
///
/// Called by **every** writer of `notes_fts` (`insert_note` and `reindex`, so
/// `refresh_note_body`, `reindex_note` and through it `reindex_from_body`), and
/// by nothing else: the columns are an index of the file, so they move exactly
/// when the searchable body moves. An outside edit that deletes a marker
/// un-parts the note on the next reindex, with no second code path to forget.
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

/// A note reduced to what the vault reconcile needs of it.
#[derive(Debug, Clone)]
pub struct NoteFile {
    pub id: i64,
    /// Vault-relative, exactly as `notes.file_path` stores it.
    pub file_path: String,
    pub title: String,
    /// Unix seconds: when this note's *index* was last written.
    pub last_modified: i64,
}

impl Storage {
    /// Insert note metadata + FTS row + wikilink edges in one transaction.
    /// `links` are raw [[wikilink]] target titles; each is resolved against
    /// existing note titles, kept as text when dangling (zettelkasten
    /// forward references). Also back-resolves older dangling links that
    /// pointed at this note's title.
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

        write_links(&mut tx, note_id, title, links).await?;
        stamp_parts(&mut tx, note_id, body).await?;

        tx.commit().await?;
        Ok(note_id)
    }

    /// Re-index a note's outgoing wikilinks after its body was rewritten.
    ///
    /// Without this, a note's edges are whatever its *first* body said for ever
    /// — and a reflection is opened empty and written afterwards, so the hub of
    /// the graph would be the one note with no edges at all.
    pub async fn set_note_links(&self, note_id: i64, title: &str, links: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        // Replaced, not merged: a link the user deleted from the body has to
        // leave the graph too.
        sqlx::query("DELETE FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        write_links(&mut tx, note_id, title, links).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Refresh a note body in the FTS index (delete + insert).
    pub async fn refresh_note_body(&self, note_id: i64, title: &str, body: &str) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        reindex(&mut tx, note_id, title, body).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Bring a note's **whole** index in line with a body — the FTS row, the
    /// wikilink edges and `last_modified` — in one transaction.
    ///
    /// One transaction rather than `refresh_note_body` followed by
    /// `set_note_links`, and that is what makes the vault watcher cancel-safe:
    /// [`crate::VaultWatcher::next`] is dropped mid-await by every `select!`
    /// that races it, and a drop between two transactions would leave a note
    /// whose searchable body is the new file and whose graph edges are the old
    /// one — with nothing on either side looking wrong. Dropped inside *this*
    /// one, the transaction rolls back and the watcher retries.
    pub async fn reindex_note(
        &self,
        note_id: i64,
        title: &str,
        body: &str,
        links: &[String],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        reindex(&mut tx, note_id, title, body).await?;
        // Replaced, not merged — see `set_note_links`.
        sqlx::query("DELETE FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .execute(&mut *tx)
            .await?;
        write_links(&mut tx, note_id, title, links).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The body this note is currently searchable *by*.
    ///
    /// `notes_fts` is an ordinary fts5 table used as a body cache, so its
    /// content is readable by rowid. That is what lets a refresh ask "has the
    /// file actually changed?" before writing anything — which is the whole of
    /// the watcher's answer to seeing its own writes.
    pub async fn indexed_body(&self, note_id: i64) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT body FROM notes_fts WHERE rowid = ?")
                .bind(note_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// The note stored at this **vault-relative** path, if any.
    ///
    /// A markdown file under the vault that no row claims is not an error and
    /// not a note: bookmole does not adopt files it did not write.
    pub async fn note_by_path(&self, file_path: &str) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE file_path = ?");
        Ok(sqlx::query(&sql)
            .bind(file_path)
            .fetch_optional(self.pool())
            .await?
            .as_ref()
            .map(row_to_note))
    }

    /// Every note as the reconcile sweep needs it: where its file is, what to
    /// index it under, and when we last wrote its index.
    ///
    /// Deliberately **not** a field on [`NoteRecord`]. `last_modified` is
    /// bookkeeping about the index rather than a fact about the note, and
    /// putting it on the record would put it on `NoteDto` and therefore on
    /// every frontend's screen vocabulary.
    pub async fn note_files(&self) -> Result<Vec<NoteFile>> {
        let rows = sqlx::query("SELECT id, file_path, title, last_modified FROM notes")
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .iter()
            .map(|r| NoteFile {
                id: r.get("id"),
                file_path: r.get("file_path"),
                title: r.get("title"),
                last_modified: r.get("last_modified"),
            })
            .collect())
    }

    /// Remove a note: its row (cascading `note_links` from it, nulling links
    /// pointing at it) and its FTS entry. The FTS table is virtual, so a
    /// foreign key can't cascade into it — it must be cleared explicitly.
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

    /// Notes, newest first — for one book, for one reading, or for the whole
    /// vault.
    ///
    /// **`limit` selects along `created_at`**, the same contract
    /// [`BookSort`](super::BookSort) states for books:
    /// `list_notes(NoteScope::All, Some(12))` is the twelve most recent notes,
    /// not twelve arbitrary ones.
    ///
    /// `None` is every note, and it stays reachable **deliberately**. This
    /// method once had no limit at all — a full table scan into a `Vec`
    /// for a screen showing twelve rows — and the obvious fix, a default cap, is
    /// wrong for the callers that are not screens: `resolve_note` scans every
    /// title in the vault to answer `rb links "Reflection: Middlemarch"`, and a cap
    /// there would silently stop resolving the notes past it. A truncated
    /// correctness pass is worse than a slow one. So the cap belongs to the
    /// caller that has a viewport, and `None` belongs to the caller that has a
    /// whole graph to walk.
    ///
    /// **The scope is applied here and not above** — `search_marks`' argument
    /// in a second place: `limit` cuts a list that
    /// has already been narrowed, so a caller asking a reading for twelve notes
    /// gets twelve of *that reading's*, where filtering a book's twelve most
    /// recent above the seam yields however many of them happened to belong to
    /// the reading asked about, and nothing at all for an older read.
    pub async fn list_notes(
        &self,
        scope: NoteScope,
        limit: Option<i64>,
    ) -> Result<Vec<NoteRecord>> {
        // `created_at` is a second-resolution timestamp and two notes written in
        // one second are ordinary, so `id` closes the order the way every book
        // sort now does — without it a limit could cut a tie arbitrarily.
        let order = "ORDER BY created_at DESC, id DESC";
        // SQLite reads a negative limit as no limit, which is what makes the two
        // arms one statement rather than four.
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
        Ok(rows.iter().map(row_to_note).collect())
    }

    /// The reflection (or review) of one reading, if it has been opened.
    ///
    /// This is what makes `open_reflection` accrete rather than pile up: the
    /// second call finds the first call's note. The partial unique indexes
    /// `idx_one_reflection` / `idx_one_review` are what make "the" honest.
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
        Ok(row.as_ref().map(row_to_note))
    }

    pub async fn get_note(&self, note_id: i64) -> Result<Option<NoteRecord>> {
        let sql = format!("SELECT {NOTE_COLUMNS} FROM notes WHERE id = ?");
        let row = sqlx::query(&sql)
            .bind(note_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.as_ref().map(row_to_note))
    }

    /// Cite a highlight from a note.
    ///
    /// **By reference, never by copying the text in.** A citation that embedded
    /// the words would go stale the moment a device refresh rewrote them, and
    /// "which highlights did I actually use?" would stop being answerable.
    /// Idempotent: citing twice is the same citation.
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

    /// The highlights a note cites, in the order a book reads.
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
        Ok(rows.iter().map(row_to_highlight).collect())
    }

    /// Which passages each of these notes cites — one call for a whole page of
    /// notes, rather than one [`Storage::citations_for`] per note.
    ///
    /// **Ids, not passages.** The screen this exists for is a book's highlight
    /// list wanting to mark the ones some note already quotes; it is holding
    /// those highlights already, so returning rows would send the reader's
    /// private text back once per citing note to draw a tick. `citations_for`
    /// stays exactly as it is for the pane that shows the passages themselves.
    ///
    /// Returns one entry per requested id **in the order given**, empties
    /// included, so a caller zips it against the list it already has and
    /// "nothing behind this note" is an answer rather than a missing row. A
    /// note id that does not exist is one of those empties: absence and
    /// citing-nothing are the same answer to this question.
    ///
    /// Within each entry the order is [`Storage::citations_for`]'s, from the
    /// same clause — see [`CITATION_ORDER`].
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

    /// Outgoing links of a note: (target_title, resolved note id if any).
    ///
    /// The cheap half of [`Storage::outgoing_links`], kept because
    /// `open_anchored` only ever wants the titles a body wrote and has no use
    /// for the target rows.
    pub async fn note_links(&self, note_id: i64) -> Result<Vec<(String, Option<i64>)>> {
        let rows = sqlx::query("SELECT target_title, to_note FROM note_links WHERE from_note = ?")
            .bind(note_id)
            .fetch_all(self.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("target_title"), r.get("to_note")))
            .collect())
    }

    /// Outgoing links of a note, with the target note itself where the edge
    /// resolves — the half of the graph a body can be read off.
    ///
    /// Ordered by `rowid`, which is insertion order, which is the order the
    /// links appeared in the body: `set_note_links` deletes and rewrites the
    /// whole set, so it never drifts from what the user wrote.
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
        Ok(rows
            .iter()
            .map(|r| OutgoingLink {
                target_title: r.get("target_title"),
                // The join missed, so every other `n.` column is NULL and
                // `row_to_note` would fail decoding them. Reading `id` as an
                // `Option` first is what keeps a dangling target a value rather
                // than an error.
                to: r.get::<Option<i64>, _>("id").map(|_| row_to_note(r)),
            })
            .collect())
    }

    /// The notes that link **to** this one — "what links here".
    ///
    /// A plain `WHERE to_note = ?`, with no dangling-by-title union, and that is
    /// a decision rather than an omission. Two reasons:
    ///
    /// * `write_links` back-resolves every dangling edge naming a title the
    ///   moment a note with that title is written, so an edge left `NULL` is one
    ///   whose target genuinely does not exist. Pinned by
    ///   `a_forward_reference_resolves_when_its_target_is_written`.
    /// * The two directions have to be the same edge set read from opposite
    ///   ends. [`Storage::outgoing_links`] reports an unresolved edge as
    ///   dangling text; a union here would make the same edge read as a real
    ///   backlink from the other side, and the pane would contradict itself.
    ///
    /// The second reason is the one that survives the exception. `notes.title`
    /// is not unique, so an edge that resolved to one of two same-titled notes
    /// dangles again if *that* one is deleted, and nothing re-resolves it toward
    /// the survivor. Both sides then agree it dangles, which is worth more than
    /// one side guessing —
    /// `a_title_shared_by_two_notes_is_where_back_resolution_stops`.
    ///
    /// Newest first, as `list_notes` orders — with `id` breaking the tie, since
    /// `created_at` is only second-resolution.
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
        Ok(rows.iter().map(row_to_note).collect())
    }

    /// The edges **into** this note, each with the target text its source
    /// wrote — [`Storage::backlinks`] plus `target_title`, which is what lets
    /// [`crate::notes::link_paragraph`] find the link in the source's body.
    ///
    /// Same edge set as `backlinks` (resolved edges only, for the reason given
    /// there), minus a self-link, and ordered by when the **source** was last
    /// kept rather than created: the row a reader wants on top is the note they
    /// last wrote in. `created_at` stands in for a row predating
    /// `last_modified`, and `id` breaks the second-resolution tie.
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
        Ok(rows
            .iter()
            .map(|r| (r.get("target_title"), row_to_note(r)))
            .collect())
    }
}

/// Why [`Storage::resurface_note`] chose the note it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResurfaceReason {
    /// Written against a book with an open reading, or linked to or from a note
    /// that was.
    OpenBook,
    /// Nothing qualifying touches an open book, so the whole notebook was drawn
    /// from.
    Notebook,
}

/// One older note offered back for today.
#[derive(Debug, Clone)]
pub struct Resurfaced {
    pub note: NoteRecord,
    pub reason: ResurfaceReason,
}

/// One older passage offered back for today — [`Storage::resurface_passage`].
///
/// The note's twin, carrying the same [`ResurfaceReason`]: a passage of a book
/// with an open reading is `OpenBook`, anything else is `Notebook`.
#[derive(Debug, Clone)]
pub struct ResurfacedPassage {
    pub highlight: Highlight,
    pub reason: ResurfaceReason,
}

/// How recently kept a note may be and still not come back: a note written
/// this week is still in the reader's head, so returning it is an echo rather
/// than a rediscovery. A chosen value, not a derived one.
pub const RESURFACE_REST_DAYS: i64 = 7;

/// A caller's local date, `YYYY-MM-DD`, as the [`time::Date`] that
/// [`Storage::resurface_note`] takes. Here rather than in each frontend so the
/// seam that carries the date as text does not need `time` of its own; a
/// malformed date is `InvalidInput`, never a silent fall back to UTC's today.
pub fn parse_local_date(text: &str) -> Result<time::Date> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(text, format).map_err(|_| {
        crate::error::EngineError::InvalidInput(format!("a date must be YYYY-MM-DD, got {text:?}"))
    })
}

/// A stable mix of a day number into an index. SplitMix64's finaliser — chosen
/// over `DefaultHasher`, whose algorithm std reserves the right to change,
/// which would move every reader's note of the day under a toolchain bump.
fn day_mix(day: i64) -> u64 {
    let mut z = (day as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Storage {
    /// One older note for `today`, or `None` when nothing qualifies.
    ///
    /// The rule, entire: a note qualifies when it was last kept **before**
    /// the start of `today - RESURFACE_REST_DAYS` (UTC midnight of that local
    /// date — the date is the caller's, the arithmetic is not). Among the
    /// qualifying notes, those **connected to an open reading** win outright:
    /// written against a book with a reading whose `finished_at IS NULL`
    /// (an abandoned reading is open, as `list_open_readings` holds), or
    /// linked to or from such a note. From the winning tier, ordered by `id`,
    /// the pick is `day_mix(julian day) % len` — the same note all day, very
    /// likely a different one tomorrow, and nothing stored to make it so.
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
                 SELECT book_id FROM readings WHERE finished_at IS NULL
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
            let note = row_to_note(row);
            if row.get::<bool, _>("connected") {
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

    /// One older **passage** for `today`, or `None` when nothing qualifies —
    /// [`Self::resurface_note`]'s rule applied to highlights, so the two
    /// offers on one surface cannot follow different ideas of *older* or of
    /// *the same all day*.
    ///
    /// A passage qualifies when we stored it **before** the start of
    /// `today - RESURFACE_REST_DAYS` and its text is not blank. `created_at`
    /// rather than `ko_datetime` deliberately: it is the one stamp every
    /// highlight has, and it can only err toward silence — a device import of
    /// old passages rests for a week before any of them comes back, which
    /// loses an offer and never replays one too soon. Passages of a book with
    /// an open reading (`finished_at IS NULL`, an abandoned one included, as
    /// `list_open_readings` holds) win outright; the pick from the winning
    /// tier, ordered by `id`, is the same `day_mix` the note uses.
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
                    (h.book_id IN (SELECT book_id FROM readings WHERE finished_at IS NULL))
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
            let highlight = row_to_highlight(row);
            if row.get::<bool, _>("connected") {
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

    /// The chunk, asserted structurally — the only way it *can* be asserted.
    ///
    /// A behavioural test at 1 203 ids passes with the split removed, because
    /// the bundled SQLite binds 32 766 parameters; this reads the statements
    /// instead and pins the two things that matter about them: none binds more
    /// than one old build would accept, and together they are a partition of
    /// the ids in order, so no id is asked about twice and none is dropped.
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

    /// `last_modified` is the moment the body was last kept, and it is read
    /// back with the row: stamped on insert, moved by a body save. Backdated
    /// by hand first, because the stamp is second-resolution and a test that
    /// slept past it would be a slow test asserting the clock.
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

    /// `incoming_links` orders by when the **source** was last kept, not by
    /// when it was created or by id — backdated by hand, so the order is the
    /// column's and not the clock's — and drops a self-link.
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

        // First note links to a not-yet-existing title (forward reference).
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

        // Creating "Vocation" back-resolves the dangling link.
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

        // FTS hits body content.
        let search = |s: Storage, q: &'static str| async move {
            s.search_marks(q, Some(super::super::SearchSource::Note), None, 10)
                .await
                .unwrap()
        };
        let hits = search(s.clone(), "calling").await;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].as_note().unwrap().id, n2);
        assert!(hits[0].snippet().contains(">>calling<<"));

        // Refresh replaces body in the index.
        s.refresh_note_body(n2, "Vocation", "Now about resilience.")
            .await
            .unwrap();
        assert!(search(s.clone(), "calling").await.is_empty());
        assert_eq!(search(s.clone(), "resilience").await.len(), 1);
    }

    /// `limit` selects along `created_at`, and its absence is every note.
    ///
    /// Both halves matter: a screen
    /// showing twelve rows no longer reads the table, and `resolve_note` — which
    /// walks every title in the vault — still can.
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

    // ---- the scope, and the truncation it exists to prevent ----------------

    /// Seed one book, two readings of it, and `per` notes per reading. The
    /// second reading's notes are written **last**, so they are the newest and
    /// the ones a limited book-wide list keeps.
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
        // `idx_readings_one_open` permits one open reading per book, so the
        // first has to close before the second exists.
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

    /// **The bug, in the note list's own shape.** A card for one reading that
    /// asks the *book* for twelve notes and filters gets whatever few of the
    /// twelve newest happen to be that reading's — and for an older read, none
    /// at all, which reads as "you wrote nothing about this" rather than as a
    /// list that was cut in the wrong place.
    ///
    /// The precondition is asserted rather than assumed: if the fixture ever
    /// stops putting the second read's notes at the head of the book's list,
    /// this fails loudly instead of passing vacuously.
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

    /// The three scopes are three questions, and a book's is the union of its
    /// reads plus what is anchored to no read of it.
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

    /// A scoped list is the unscoped list restricted, and in the same order —
    /// which, unlike `search_marks`, is true here without qualification: this
    /// list has one order (`created_at DESC, id DESC`) and a `WHERE` cannot
    /// disturb it.
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
