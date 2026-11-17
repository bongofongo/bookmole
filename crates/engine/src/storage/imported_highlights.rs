//! The one write path for every non-KOReader highlight importer (Preview,
//! Apple Books, the Kindle's clippings, the Kindle app's notebook).
//!
//! What every source wrote (the passage, its note, its colour) is that
//! source's own and is refreshed toward it; `annotation` is bookmole's and
//! no import touches it. Positions stay in the source's own unit (`pos_kind`),
//! never converted. The same passage from two apps stays two rows, one per
//! origin, and deletion follows each source's own behaviour (the importer
//! decides whether its source forgets).
//!
//! ## Identity
//!
//! An importer names a highlight by a **key**: the source's own `origin_id`
//! when it has one, else its `identity` string. A stored row is the same
//! highlight when `(book_id, origin, origin_id)` matches, or (no `origin_id`)
//! when its `identity_hash` is one of the key's hashes.
//!
//! The hash is `sha256("rb-imported\0" origin "\0" kind "\0" key)`, with `kind`
//! either `id` (the key is `origin_id`) or `identity`. It differs from
//! KOReader's `sha256(book_id|datetime|pos0|text)` in its input domain (a fixed
//! prefix and NUL separators, which KOReader's `|`-joined decimal-led string
//! cannot produce), and the origin is inside it, so two apps never collide. It
//! leaves `book_id` **out** on purpose: `merge_books` re-hashes KOReader rows
//! against the surviving book, and it keeps an imported row's hash as it is.
//!
//! ### Scope: where a key is unique
//!
//! Some keys are unique wherever they are seen (Apple Books' annotation uuid, a
//! Kindle clipping's title + location + text); others only inside one file (a
//! PDF annotation's `/NM`, or its page and rect). A book can have two PDF
//! copies (two `path` places: a copy is identified by its place), and the same
//! `/NM` or the same rect in
//! both is **two** highlights. So the scope is a property of the importer's
//! key, carried on [`ImportedHighlight::identity_scope`], not a blanket rule:
//!
//! * [`IdentityScope::Global`]: the copy plays no part. Lookup is by key alone,
//!   the hash is the one above, and a row is refreshed whichever copy (or none)
//!   the import names. This is the behaviour every importer had before scopes.
//! * [`IdentityScope::Copy`] with `copy_id = Some(c)`: lookup is by key among
//!   the rows of copy `c` **and rows with no copy** (preferring `c`'s; a
//!   no-copy row found this way is claimed by stamping `c` on it). A row of
//!   another copy is never found, refreshed or removed. With `copy_id = None`
//!   there is nothing to scope by, and it behaves as `Global`.
//!
//! `UNIQUE(book_id, identity_hash)` means two copies holding one key need two
//! hashes. A copy-scoped key therefore has **slots**: slot 1 is the hash above,
//! slot `n > 1` puts `@n` after `kind` (`"id@2"`, which no slot-1 pre-image can
//! produce, since `kind` is followed by NUL). Lookup tries all
//! [`MAX_SCOPE_SLOTS`] slots within the scope; an insert takes the first slot
//! no other row holds.
//!
//! **Why slots and not the copy id inside the hash** (the obvious design):
//!
//! 1. A book with one copy, which is nearly every book, keeps exactly the hash
//!    it had before scopes, so rows already written match without a rekey.
//! 2. `merge_copies` repoints `highlights.copy_id` and leaves hashes alone. A
//!    hash that baked in the absorbed copy's id would never be produced again,
//!    so the next import of the absorbed file would miss every identity-keyed
//!    row and insert them all a second time. A slot hash is found by its key
//!    within the surviving copy, so nothing is duplicated.
//! 3. `copies.id` is a plain `INTEGER PRIMARY KEY`, so a deleted copy's id can
//!    be handed to the next copy made; a hash naming that id would then belong
//!    to a stranger's rows.
//!
//! Known limits, each pinned by a test in `tests/imported_highlights.rs`:
//!
//! * After a merge, one copy can hold the same key twice (once from each file).
//!   Lookup then prefers the row whose text, note and `pos0` already equal the
//!   import (so re-importing either file is all `Unchanged`), then the oldest.
//!   Merging said the two are one copy; the importer cannot tell its files apart.
//! * A row stored with no copy is claimed by the first copy-scoped import of
//!   its key, whichever file that was.
//! * At most [`MAX_SCOPE_SLOTS`] rows of one book may hold one copy-scoped key;
//!   past that the upsert is `InvalidInput` (Preview counts it `unparsed`).
//!
//! ## Ownership
//!
//! What the source wrote — text, `text_unavailable`, its note, colour, chapter,
//! page, position and creation stamp — is refreshed toward the source on every
//! import, and `last_seen_origin_note` follows. `annotation`, `reading_id`,
//! `created_at` and the links that hang off a row are bookmole's and are
//! never written here. `copy_id` (and a missing `origin_id`) is stamped only
//! while NULL, and stamping is not a change the importer counts.
//!
//! The FTS triggers keep the search index right by themselves.
//!
//! Highlight text and notes are the user's private reading; nothing here logs
//! them.

use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use super::highlights::{HIGHLIGHT_COLUMNS, row_to_highlight};
use super::{Storage, now_unix};
use crate::error::{EngineError, Result};
use crate::highlight_origin::{
    HighlightOrigin, IdentityScope, ImportedHighlight, ImportedOutcome, PosKind,
};
use crate::storage::Highlight;

/// How many rows of one book may hold the same copy-scoped key: one per copy
/// that has it, plus what merges leave behind. Sixteen PDF copies of one book
/// annotated at the same spot is far past anything real, and every scoped
/// lookup hashes this many slots.
pub const MAX_SCOPE_SLOTS: u32 = 16;

/// The `identity_hash` of an imported highlight, slot 1 (the only slot a
/// [`IdentityScope::Global`] key has). See the module doc.
pub fn imported_identity_hash(h: &ImportedHighlight) -> String {
    slot_hash(h, 1)
}

fn slot_hash(h: &ImportedHighlight, slot: u32) -> String {
    let (kind, key) = match &h.origin_id {
        Some(id) => ("id", id.as_str()),
        None => ("identity", h.identity.as_str()),
    };
    let mut hasher = Sha256::new();
    hasher.update(b"rb-imported\0");
    hasher.update(h.origin.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(kind.as_bytes());
    if slot > 1 {
        hasher.update(format!("@{slot}").as_bytes());
    }
    hasher.update(b"\0");
    hasher.update(key.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Which stored rows an import may find.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Any row of the book and origin.
    Any,
    /// Rows of this copy, or rows with no copy.
    Copy(i64),
}

fn scope_of(h: &ImportedHighlight) -> Scope {
    match (h.identity_scope, h.copy_id) {
        (IdentityScope::Copy, Some(c)) => Scope::Copy(c),
        _ => Scope::Any,
    }
}

/// The hashes a key may be stored under, slot order.
fn key_hashes(h: &ImportedHighlight, scope: Scope) -> Vec<String> {
    match scope {
        Scope::Any => vec![slot_hash(h, 1)],
        Scope::Copy(_) => (1..=MAX_SCOPE_SLOTS).map(|n| slot_hash(h, n)).collect(),
    }
}

/// Something of bookmole's own hangs off the row: the reader's annotation,
/// a note anchored to it, a flashcard, a citation, a reading's card passage.
/// Deleting such a row would cascade or null that away.
const KEPT_SQL: &str = "(COALESCE(h.annotation, '') <> ''
        OR EXISTS (SELECT 1 FROM notes n WHERE n.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM flashcards f WHERE f.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM citations c WHERE c.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM readings r WHERE r.card_highlight_id = h.id))";

/// A source-owned field of the row differs from the import. Parameters: `?1`
/// the row id, then `?2` text, `?3` text_unavailable, `?4` origin_note, `?5`
/// colour, `?6` chapter, `?7` page, `?8` pos_kind, `?9` pos0, `?10` pos1,
/// `?11` the stamp. Shared by the refresh and its dry-run preview.
const SOURCE_FIELDS_DIFFER: &str = "(text IS NOT ?2 OR text_unavailable IS NOT ?3
        OR origin_note IS NOT ?4 OR last_seen_origin_note IS NOT ?4 OR color IS NOT ?5
        OR chapter IS NOT ?6 OR page IS NOT ?7 OR pos_kind IS NOT ?8 OR pos0 IS NOT ?9
        OR pos1 IS NOT ?10 OR ko_datetime IS NOT ?11)";

fn check_origin(origin: HighlightOrigin) -> Result<()> {
    match origin {
        HighlightOrigin::Koreader | HighlightOrigin::User => Err(EngineError::InvalidInput(
            format!("{origin} highlights are not written through the imported path"),
        )),
        _ => Ok(()),
    }
}

impl Storage {
    /// Insert or refresh one imported highlight.
    ///
    /// `Updated` only when a source-owned field really differs from what is
    /// stored, so importing the same source twice reports `Unchanged` the
    /// second time. Never touches `annotation`. Which stored rows count as
    /// "the same highlight" follows [`ImportedHighlight::identity_scope`]
    /// (module doc, "Scope").
    pub async fn upsert_imported_highlight(
        &self,
        book_id: i64,
        h: &ImportedHighlight,
    ) -> Result<ImportedOutcome> {
        check_origin(h.origin)?;
        let scope = scope_of(h);
        let hashes = key_hashes(h, scope);

        let Some(row) = self.find_imported(book_id, h, scope, &hashes).await? else {
            for hash in &hashes {
                if let Some(id) = self.insert_imported(book_id, h, hash).await? {
                    return Ok(ImportedOutcome::Inserted(id));
                }
                // The slot is held. By a row this import may find (an identity
                // row that later gained an id, say), it is this highlight;
                // otherwise it is another copy's, and the next slot is tried.
                if let Some(done) = self
                    .refresh_imported_by_hash(book_id, h, hash, scope)
                    .await?
                {
                    return Ok(done);
                }
            }
            return Err(EngineError::InvalidInput(format!(
                "{MAX_SCOPE_SLOTS} rows of one book already hold this {} highlight's key",
                h.origin
            )));
        };

        let id: i64 = row.get("id");
        self.stamp_imported(id, h).await?;
        self.refresh_imported_row(id, h).await
    }

    /// The stored row this import names, within its scope (module doc).
    async fn find_imported(
        &self,
        book_id: i64,
        h: &ImportedHighlight,
        scope: Scope,
        hashes: &[String],
    ) -> Result<Option<SqliteRow>> {
        let mut sql = String::from("SELECT id FROM highlights WHERE book_id = ? AND origin = ?");
        match &h.origin_id {
            Some(_) => sql.push_str(" AND origin_id = ?"),
            None => {
                let marks = vec!["?"; hashes.len()].join(", ");
                sql.push_str(&format!(" AND identity_hash IN ({marks})"));
            }
        }
        match scope {
            Scope::Any => sql.push_str(" ORDER BY id LIMIT 1"),
            // This copy's rows before unclaimed ones; then (two of this copy's
            // rows share the key only after a merge) the one already equal to
            // the import, so re-importing either merged file is a no-op.
            Scope::Copy(_) => sql.push_str(
                " AND (copy_id = ? OR copy_id IS NULL)
                  ORDER BY copy_id IS NULL,
                           (text IS NOT ? OR origin_note IS NOT ? OR pos0 IS NOT ?), id
                  LIMIT 1",
            ),
        }
        let mut q = sqlx::query(&sql).bind(book_id).bind(h.origin.as_str());
        match &h.origin_id {
            Some(id) => q = q.bind(id),
            None => {
                for hash in hashes {
                    q = q.bind(hash);
                }
            }
        }
        if let Scope::Copy(c) = scope {
            q = q
                .bind(c)
                .bind(&h.text)
                .bind(h.origin_note.as_ref())
                .bind(h.pos0.as_ref());
        }
        Ok(q.fetch_optional(self.pool()).await?)
    }

    /// Insert under `hash`; `None` when another row holds it.
    async fn insert_imported(
        &self,
        book_id: i64,
        h: &ImportedHighlight,
        hash: &str,
    ) -> Result<Option<i64>> {
        let row = sqlx::query(
            r#"INSERT INTO highlights
                (book_id, text, chapter, page, pos0, pos1, ko_datetime, color, origin_note,
                 last_seen_origin_note, source, identity_hash, created_at,
                 origin, copy_id, origin_id, pos_kind, text_unavailable)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
               ON CONFLICT(book_id, identity_hash) DO NOTHING
               RETURNING id"#,
        )
        .bind(book_id)
        .bind(&h.text)
        .bind(h.chapter.as_ref())
        .bind(h.page)
        .bind(h.pos0.as_ref())
        .bind(h.pos1.as_ref())
        .bind(h.made_at.as_ref())
        .bind(h.color.as_ref())
        .bind(h.origin_note.as_ref())
        .bind(h.origin_note.as_ref())
        .bind(h.origin.as_str())
        .bind(hash)
        .bind(now_unix())
        .bind(h.origin.as_str())
        .bind(h.copy_id)
        .bind(h.origin_id.as_ref())
        .bind(h.pos_kind.map(|k: PosKind| k.as_str()))
        .bind(i64::from(h.text_unavailable))
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(|r| r.get("id")))
    }

    /// The insert found `hash` held although no row matched by key: refresh
    /// that row in place **if it is in scope**. `None` when the holder is
    /// another copy's row (or gone), so the caller tries the next slot.
    async fn refresh_imported_by_hash(
        &self,
        book_id: i64,
        h: &ImportedHighlight,
        hash: &str,
        scope: Scope,
    ) -> Result<Option<ImportedOutcome>> {
        let id: Option<i64> = match scope {
            Scope::Any => {
                sqlx::query_scalar(
                    "SELECT id FROM highlights WHERE book_id = ? AND identity_hash = ?",
                )
                .bind(book_id)
                .bind(hash)
                .fetch_optional(self.pool())
                .await?
            }
            Scope::Copy(c) => {
                sqlx::query_scalar(
                    "SELECT id FROM highlights WHERE book_id = ? AND identity_hash = ?
                        AND (copy_id = ? OR copy_id IS NULL)",
                )
                .bind(book_id)
                .bind(hash)
                .bind(c)
                .fetch_optional(self.pool())
                .await?
            }
        };
        let Some(id) = id else { return Ok(None) };
        self.stamp_imported(id, h).await?;
        self.refresh_imported_row(id, h).await.map(Some)
    }

    /// Fill a NULL `copy_id` / `origin_id` from the import. Not counted.
    async fn stamp_imported(&self, id: i64, h: &ImportedHighlight) -> Result<()> {
        if h.copy_id.is_none() && h.origin_id.is_none() {
            return Ok(());
        }
        sqlx::query(
            "UPDATE highlights
                SET copy_id = COALESCE(copy_id, ?2), origin_id = COALESCE(origin_id, ?3)
              WHERE id = ?1 AND ((copy_id IS NULL AND ?2 IS NOT NULL)
                                 OR (origin_id IS NULL AND ?3 IS NOT NULL))",
        )
        .bind(id)
        .bind(h.copy_id)
        .bind(h.origin_id.as_ref())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// What [`Storage::upsert_imported_highlight`] would do with `h`, without
    /// writing anything: `None` when it would insert, else `Updated(id)` or
    /// `Unchanged(id)` for the row it would find. For a dry run.
    ///
    /// The "differs" test is [`SOURCE_FIELDS_DIFFER`], the predicate the real
    /// refresh runs, so a preview cannot disagree with the import it previews.
    /// One approximation, on the side of saying *insert*: a copy-scoped key
    /// whose slot is held only by a row outside the scope is reported as an
    /// insert, which is what the upsert does with it too (it takes the next
    /// slot). `copy_id` / `origin_id` stamping is not reported, as the upsert
    /// does not count it either.
    pub async fn preview_imported_highlight(
        &self,
        book_id: i64,
        h: &ImportedHighlight,
    ) -> Result<Option<ImportedOutcome>> {
        check_origin(h.origin)?;
        let scope = scope_of(h);
        let hashes = key_hashes(h, scope);
        let Some(row) = self.find_imported(book_id, h, scope, &hashes).await? else {
            return Ok(None);
        };
        let id: i64 = row.get("id");
        let sql = format!("SELECT {SOURCE_FIELDS_DIFFER} FROM highlights WHERE id = ?1");
        let differs: bool = sqlx::query_scalar(&sql)
            .bind(id)
            .bind(&h.text)
            .bind(i64::from(h.text_unavailable))
            .bind(h.origin_note.as_ref())
            .bind(h.color.as_ref())
            .bind(h.chapter.as_ref())
            .bind(h.page)
            .bind(h.pos_kind.map(|k| k.as_str()))
            .bind(h.pos0.as_ref())
            .bind(h.pos1.as_ref())
            .bind(h.made_at.as_ref())
            .fetch_one(self.pool())
            .await?;
        Ok(Some(if differs {
            ImportedOutcome::Updated(id)
        } else {
            ImportedOutcome::Unchanged(id)
        }))
    }

    /// Refresh the source-owned fields of row `id` toward `h`.
    async fn refresh_imported_row(
        &self,
        id: i64,
        h: &ImportedHighlight,
    ) -> Result<ImportedOutcome> {
        // One conditional statement, null-safe (`IS NOT`): a note deleted in the
        // source must be removed here, and `!=` would call NULL vs 'x' equal.
        let sql = format!(
            "UPDATE highlights
                SET text = ?2, text_unavailable = ?3, origin_note = ?4, last_seen_origin_note = ?4,
                    color = ?5, chapter = ?6, page = ?7, pos_kind = ?8, pos0 = ?9, pos1 = ?10,
                    ko_datetime = ?11
              WHERE id = ?1 AND {SOURCE_FIELDS_DIFFER}"
        );
        let done = sqlx::query(&sql)
            .bind(id)
            .bind(&h.text)
            .bind(i64::from(h.text_unavailable))
            .bind(h.origin_note.as_ref())
            .bind(h.color.as_ref())
            .bind(h.chapter.as_ref())
            .bind(h.page)
            .bind(h.pos_kind.map(|k| k.as_str()))
            .bind(h.pos0.as_ref())
            .bind(h.pos1.as_ref())
            .bind(h.made_at.as_ref())
            .execute(self.pool())
            .await?;
        Ok(if done.rows_affected() > 0 {
            ImportedOutcome::Updated(id)
        } else {
            ImportedOutcome::Unchanged(id)
        })
    }

    /// Delete the imported highlight an app's own **global** id names — but
    /// **only** if nothing of bookmole's hangs off it (see
    /// [`Storage::imported_highlight_kept`]). False when there is no such row,
    /// when it is kept, *or* when the id is held by more than one row (a
    /// copy-scoped source has it in several copies: such a source removes with
    /// [`Storage::remove_imported_highlight_in_copy`], and this never guesses
    /// which copy was meant). Callers that must tell absent from kept ask
    /// [`Storage::imported_highlight_kept`] first.
    pub async fn remove_imported_highlight(
        &self,
        book_id: i64,
        origin: HighlightOrigin,
        origin_id: &str,
    ) -> Result<bool> {
        check_origin(origin)?;
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM highlights WHERE book_id = ? AND origin = ? AND origin_id = ?
              LIMIT 2",
        )
        .bind(book_id)
        .bind(origin.as_str())
        .bind(origin_id)
        .fetch_all(self.pool())
        .await?;
        match ids.as_slice() {
            [id] => self.remove_imported_highlight_row(*id).await,
            _ => Ok(false),
        }
    }

    /// [`Storage::remove_imported_highlight`] for a copy-scoped source: the
    /// rows of `copy_id` (only; never another copy's, never a row with no
    /// copy) whose `origin_id` is this. Same guard. True when any was deleted.
    pub async fn remove_imported_highlight_in_copy(
        &self,
        book_id: i64,
        origin: HighlightOrigin,
        copy_id: i64,
        origin_id: &str,
    ) -> Result<bool> {
        check_origin(origin)?;
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM highlights
              WHERE book_id = ? AND origin = ? AND copy_id = ? AND origin_id = ?",
        )
        .bind(book_id)
        .bind(origin.as_str())
        .bind(copy_id)
        .bind(origin_id)
        .fetch_all(self.pool())
        .await?;
        let mut any = false;
        for id in ids {
            any |= self.remove_imported_highlight_row(id).await?;
        }
        Ok(any)
    }

    /// [`Storage::remove_imported_highlight`] by row id, for rows the source
    /// identifies by `identity` rather than an id of its own. Same guard: an
    /// imported row that is kept, or a KOReader/user row, is never deleted here.
    /// The row id already names one copy's row, so the caller's selection is
    /// the scope (Preview selects by `copy_id` and origin).
    pub async fn remove_imported_highlight_row(&self, highlight_id: i64) -> Result<bool> {
        let sql = format!(
            "DELETE FROM highlights AS h WHERE h.id = ?
               AND h.origin NOT IN ('koreader', 'user') AND NOT {KEPT_SQL}"
        );
        let done = sqlx::query(&sql)
            .bind(highlight_id)
            .execute(self.pool())
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Whether something of bookmole's own hangs off this highlight: an
    /// `annotation`, a note anchored to it, a flashcard, a citation, a card
    /// passage. Such a row is never removed by an importer.
    pub async fn imported_highlight_kept(&self, highlight_id: i64) -> Result<bool> {
        let sql = format!("SELECT {KEPT_SQL} FROM highlights h WHERE h.id = ?");
        let kept: Option<bool> = sqlx::query_scalar(&sql)
            .bind(highlight_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(kept.unwrap_or(false))
    }

    /// One book's highlights from one importing origin, in id order.
    pub async fn imported_highlights(
        &self,
        book_id: i64,
        origin: HighlightOrigin,
    ) -> Result<Vec<Highlight>> {
        let sql = format!(
            "SELECT {HIGHLIGHT_COLUMNS} FROM highlights WHERE book_id = ? AND origin = ?
             ORDER BY id"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .bind(origin.as_str())
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(row_to_highlight).collect())
    }
}
