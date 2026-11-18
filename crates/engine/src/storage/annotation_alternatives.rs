//! A highlight's other annotations: the reader's texts it holds besides the one it shows. A fold
//! of two highlights puts them here (`fold_highlight`); the reader promotes or removes them.

use sqlx::{Row, SqliteConnection};

use super::{Storage, now_unix};
use crate::error::{EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationAlternative {
    pub highlight_id: i64,
    pub text: String,
    /// Unix seconds when it stopped being the shown annotation, or arrived by a fold.
    pub set_aside_at: i64,
}

const COLUMNS: &str = "highlight_id, text, set_aside_at";

/// Oldest first within a highlight; the text breaks ties so the order is total.
const ORDER: &str = "highlight_id, set_aside_at, text";

fn row_to_alternative(r: &sqlx::sqlite::SqliteRow) -> Result<AnnotationAlternative> {
    Ok(AnnotationAlternative {
        highlight_id: r.try_get("highlight_id")?,
        text: r.try_get("text")?,
        set_aside_at: r.try_get("set_aside_at")?,
    })
}

async fn highlight_exists(conn: &mut SqliteConnection, id: i64) -> Result<()> {
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM highlights WHERE id = ?")
        .bind(id)
        .fetch_optional(conn)
        .await?;
    found
        .map(|_| ())
        .ok_or_else(|| EngineError::NotFound(format!("highlight {id}")))
}

impl Storage {
    pub async fn annotation_alternatives(
        &self,
        highlight_id: i64,
    ) -> Result<Vec<AnnotationAlternative>> {
        let mut conn = self.pool().acquire().await?;
        highlight_exists(&mut conn, highlight_id).await?;
        let sql = format!(
            "SELECT {COLUMNS} FROM annotation_alternatives WHERE highlight_id = ? ORDER BY {ORDER}"
        );
        let rows = sqlx::query(&sql)
            .bind(highlight_id)
            .fetch_all(&mut *conn)
            .await?;
        rows.iter().map(row_to_alternative).collect()
    }

    /// Every alternative of every highlight of the book, in one statement, so a screen of a book's
    /// passages never asks once per highlight. A highlight with none is simply absent.
    pub async fn book_annotation_alternatives(
        &self,
        book_id: i64,
    ) -> Result<Vec<AnnotationAlternative>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM annotation_alternatives
              WHERE highlight_id IN (SELECT id FROM highlights WHERE book_id = ?)
              ORDER BY {ORDER}"
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_alternative).collect()
    }

    /// The alternative becomes the annotation, and the annotation it replaces (if any) becomes an
    /// alternative, in one transaction. Nothing the reader wrote is dropped.
    pub async fn promote_annotation_alternative(
        &self,
        highlight_id: i64,
        text: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        highlight_exists(&mut tx, highlight_id).await?;
        let taken =
            sqlx::query("DELETE FROM annotation_alternatives WHERE highlight_id = ? AND text = ?")
                .bind(highlight_id)
                .bind(text)
                .execute(&mut *tx)
                .await?;
        if taken.rows_affected() == 0 {
            return Err(EngineError::NotFound(format!(
                "that alternative annotation of highlight {highlight_id}"
            )));
        }
        let shown: Option<String> =
            sqlx::query_scalar("SELECT annotation FROM highlights WHERE id = ?")
                .bind(highlight_id)
                .fetch_one(&mut *tx)
                .await?;
        if let Some(shown) = shown.filter(|s| !s.trim().is_empty() && s.trim() != text.trim()) {
            sqlx::query(
                "INSERT OR IGNORE INTO annotation_alternatives (highlight_id, text, set_aside_at)
                 VALUES (?, ?, ?)",
            )
            .bind(highlight_id)
            .bind(shown)
            .bind(now_unix())
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE highlights SET annotation = ? WHERE id = ?")
            .bind(text)
            .bind(highlight_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removing one already gone is not an error; the highlight must exist.
    pub async fn remove_annotation_alternative(&self, highlight_id: i64, text: &str) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        highlight_exists(&mut tx, highlight_id).await?;
        sqlx::query("DELETE FROM annotation_alternatives WHERE highlight_id = ? AND text = ?")
            .bind(highlight_id)
            .bind(text)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
