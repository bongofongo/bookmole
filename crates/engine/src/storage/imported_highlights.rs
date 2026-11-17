//! The one write path for non-KOReader highlight importers; `annotation` is never written here.
//! Identity hashes omit `book_id` (merges keep them) and use `@n` slots for copy-scoped keys.

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

/// Max rows of one book holding the same copy-scoped key; every scoped lookup hashes this many.
pub const MAX_SCOPE_SLOTS: u32 = 16;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Any,
    Copy(i64),
}

fn scope_of(h: &ImportedHighlight) -> Scope {
    match (h.identity_scope, h.copy_id) {
        (IdentityScope::Copy, Some(c)) => Scope::Copy(c),
        _ => Scope::Any,
    }
}

fn key_hashes(h: &ImportedHighlight, scope: Scope) -> Vec<String> {
    match scope {
        Scope::Any => vec![slot_hash(h, 1)],
        Scope::Copy(_) => (1..=MAX_SCOPE_SLOTS).map(|n| slot_hash(h, n)).collect(),
    }
}

/// Rows with anything of bookmole's own attached; deleting one would cascade or null it away.
const KEPT_SQL: &str = "(COALESCE(h.annotation, '') <> ''
        OR EXISTS (SELECT 1 FROM notes n WHERE n.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM flashcards f WHERE f.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM citations c WHERE c.highlight_id = h.id)
        OR EXISTS (SELECT 1 FROM readings r WHERE r.card_highlight_id = h.id))";

/// Parameters: ?1 row id, ?2 text, ?3 text_unavailable, ?4 origin_note, ?5 colour, ?6 chapter,
/// ?7 page, ?8 pos_kind, ?9 pos0, ?10 pos1, ?11 stamp. Shared by the refresh and its dry run.
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
    /// `Updated` only when a source-owned field really differs; never touches `annotation`.
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
                // A held slot is this highlight if its row is in scope; otherwise it is another copy's.
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
            // Prefer this copy's rows, then the one already equal to the import, so re-importing a merged
            // file is a no-op.
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

    /// Fill a NULL `copy_id` / `origin_id`; not counted as a change.
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

    /// Dry run of `upsert_imported_highlight` using the same `SOURCE_FIELDS_DIFFER` predicate; `None`
    /// means it would insert. Stamping `copy_id` / `origin_id` is not reported.
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

    async fn refresh_imported_row(
        &self,
        id: i64,
        h: &ImportedHighlight,
    ) -> Result<ImportedOutcome> {
        // `IS NOT`, not `!=`: a note deleted in the source (NULL vs 'x') must count as a change.
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

    /// Deletes only if nothing of bookmole's hangs off it; false when absent, kept, or held by
    /// several rows (copy-scoped sources use `remove_imported_highlight_in_copy`).
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

    /// Only rows of `copy_id`, never another copy's or a copyless row. Same guard.
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

    /// By row id, for sources keyed by `identity`. Same guard: kept, KOReader and user rows survive.
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

    pub async fn imported_highlight_kept(&self, highlight_id: i64) -> Result<bool> {
        let sql = format!("SELECT {KEPT_SQL} FROM highlights h WHERE h.id = ?");
        let kept: Option<bool> = sqlx::query_scalar(&sql)
            .bind(highlight_id)
            .fetch_optional(self.pool())
            .await?;
        Ok(kept.unwrap_or(false))
    }

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
