//! A highlight's other annotations: the reader's texts it holds besides the one it shows. A fold
//! of two highlights puts them here (`fold_annotations`); the reader promotes or removes them.
//!
//! Whether an annotation says anything, and whether two are the same, is decided only by
//! `annotation_key`, in Rust: SQL's `trim` knows no Unicode whitespace.

use std::collections::HashSet;

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

/// What an annotation says, or `None` when it says nothing. Text compares by `str::trim`, so a
/// trailing newline or an ideographic space is no difference; Unicode forms are not normalised.
/// Bytes that are not UTF-8 compare whole, so two different ones never pass for one.
pub(crate) fn annotation_key(text: &[u8]) -> Option<Vec<u8>> {
    match std::str::from_utf8(text) {
        Ok(s) => Some(s.trim())
            .filter(|t| !t.is_empty())
            .map(|t| t.as_bytes().to_vec()),
        Err(_) => Some(text.to_vec()),
    }
}

pub(crate) fn is_blank(text: &str) -> bool {
    annotation_key(text.as_bytes()).is_none()
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

/// The key of a highlight's shown annotation. Read as bytes, so text another tool wrote that is not
/// UTF-8 is still compared rather than failing a fold.
async fn shown_key(conn: &mut SqliteConnection, highlight_id: i64) -> Result<Option<Vec<u8>>> {
    let raw: Option<Vec<u8>> = sqlx::query_scalar::<_, Option<Vec<u8>>>(
        "SELECT CAST(annotation AS BLOB) FROM highlights WHERE id = ?",
    )
    .bind(highlight_id)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    Ok(raw.as_deref().and_then(annotation_key))
}

/// `(rowid, key)` of each alternative, oldest first; a blank one has no key.
async fn alternative_keys(
    conn: &mut SqliteConnection,
    highlight_id: i64,
) -> Result<Vec<(i64, Option<Vec<u8>>)>> {
    let rows = sqlx::query(&format!(
        "SELECT rowid, CAST(text AS BLOB) AS text FROM annotation_alternatives
          WHERE highlight_id = ? ORDER BY {ORDER}"
    ))
    .bind(highlight_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter()
        .map(|r| {
            let text: Vec<u8> = r.try_get("text")?;
            Ok((r.try_get("rowid")?, annotation_key(&text)))
        })
        .collect()
}

/// Whether the highlight holds anything the reader wrote: a shown annotation or an alternative.
pub(super) async fn holds_reader_text(
    conn: &mut SqliteConnection,
    highlight_id: i64,
) -> Result<bool> {
    Ok(shown_key(conn, highlight_id).await?.is_some()
        || alternative_keys(conn, highlight_id)
            .await?
            .iter()
            .any(|(_, k)| k.is_some()))
}

/// Carries `dup`'s annotations to `keep` before `dup` is deleted. `keep` shows `dup`'s annotation
/// when its own says nothing; otherwise a different one becomes an alternative. `dup`'s
/// alternatives follow, except one `keep` already shows or holds. The rows are copied in SQL, so the
/// stored text moves byte for byte.
pub(super) async fn fold_annotations(
    conn: &mut SqliteConnection,
    dup: i64,
    keep: i64,
) -> Result<()> {
    let now = now_unix();
    let mut held: HashSet<Vec<u8>> = alternative_keys(conn, keep)
        .await?
        .into_iter()
        .filter_map(|(_, k)| k)
        .collect();
    let mut shown = shown_key(conn, keep).await?;
    if let Some(theirs) = shown_key(conn, dup).await? {
        if shown.is_none() {
            sqlx::query(
                "UPDATE highlights SET annotation = (SELECT annotation FROM highlights WHERE id = ?1)
                  WHERE id = ?2",
            )
            .bind(dup)
            .bind(keep)
            .execute(&mut *conn)
            .await?;
            shown = Some(theirs);
        } else if shown.as_ref() != Some(&theirs) && held.insert(theirs) {
            sqlx::query(
                "INSERT OR IGNORE INTO annotation_alternatives (highlight_id, text, set_aside_at)
                 SELECT ?1, annotation, ?2 FROM highlights WHERE id = ?3",
            )
            .bind(keep)
            .bind(now)
            .bind(dup)
            .execute(&mut *conn)
            .await?;
        }
    }
    for (rowid, key) in alternative_keys(conn, dup).await? {
        let Some(key) = key else { continue };
        if shown.as_ref() == Some(&key) || !held.insert(key) {
            continue;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO annotation_alternatives (highlight_id, text, set_aside_at)
             SELECT ?1, text, set_aside_at FROM annotation_alternatives WHERE rowid = ?2",
        )
        .bind(keep)
        .bind(rowid)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
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

    /// The alternative becomes the annotation, and the annotation it replaces (if it says anything)
    /// becomes an alternative, in one transaction. Nothing the reader wrote is dropped. `text`
    /// matches an alternative by `annotation_key`.
    pub async fn promote_annotation_alternative(
        &self,
        highlight_id: i64,
        text: &str,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        highlight_exists(&mut tx, highlight_id).await?;
        let not_held = || {
            EngineError::NotFound(format!(
                "that alternative annotation of highlight {highlight_id}"
            ))
        };
        let want = annotation_key(text.as_bytes()).ok_or_else(not_held)?;
        let alternatives = alternative_keys(&mut tx, highlight_id).await?;
        let (matching, others): (Vec<_>, Vec<_>) = alternatives
            .into_iter()
            .partition(|(_, k)| k.as_ref() == Some(&want));
        let Some(&(chosen, _)) = matching.first() else {
            return Err(not_held());
        };
        if let Some(shown) = shown_key(&mut tx, highlight_id).await? {
            let held = others.iter().any(|(_, k)| k.as_ref() == Some(&shown));
            if shown != want && !held {
                sqlx::query(
                    "INSERT OR IGNORE INTO annotation_alternatives
                         (highlight_id, text, set_aside_at)
                     SELECT id, annotation, ?1 FROM highlights WHERE id = ?2",
                )
                .bind(now_unix())
                .bind(highlight_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        sqlx::query(
            "UPDATE highlights
                SET annotation = (SELECT text FROM annotation_alternatives WHERE rowid = ?1)
              WHERE id = ?2",
        )
        .bind(chosen)
        .bind(highlight_id)
        .execute(&mut *tx)
        .await?;
        for (rowid, _) in &matching {
            sqlx::query("DELETE FROM annotation_alternatives WHERE rowid = ?")
                .bind(rowid)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Removes every alternative the same as `text` by `annotation_key`; removing one already gone
    /// is not an error. The highlight must exist.
    pub async fn remove_annotation_alternative(&self, highlight_id: i64, text: &str) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        highlight_exists(&mut tx, highlight_id).await?;
        let want = annotation_key(text.as_bytes());
        for (rowid, key) in alternative_keys(&mut tx, highlight_id).await? {
            if want.is_some() && key == want {
                sqlx::query("DELETE FROM annotation_alternatives WHERE rowid = ?")
                    .bind(rowid)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;
    use crate::highlight_origin::{HighlightOrigin, ImportedHighlight};

    #[test]
    fn the_key_is_trim_and_nothing_more() {
        for blank in ["", " ", "\u{3000}", " \t\r\n", "\u{a0}\u{2003}"] {
            assert_eq!(annotation_key(blank.as_bytes()), None, "{blank:?}");
        }
        assert_eq!(
            annotation_key("a thought \n".as_bytes()),
            annotation_key("\u{3000}a thought".as_bytes())
        );
        assert_ne!(
            annotation_key("café".as_bytes()),
            annotation_key("cafe\u{301}".as_bytes())
        );
        assert_ne!(annotation_key(b"x\xff"), annotation_key(b"x\xfe"));
        assert!(annotation_key(b" \xff ").is_some());
    }

    async fn highlight(s: &Storage) -> i64 {
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
        s.upsert_imported_highlight(
            book,
            &ImportedHighlight::new(HighlightOrigin::Kindle, "loc 1", "a passage"),
        )
        .await
        .unwrap()
        .id()
    }

    async fn raw(s: &Storage, sql: &str, id: i64, text: &str) {
        sqlx::query(sql)
            .bind(id)
            .bind(text)
            .execute(s.pool())
            .await
            .unwrap();
    }

    const SHOW: &str = "UPDATE highlights SET annotation = ?2 WHERE id = ?1";
    const SET_ASIDE: &str =
        "INSERT INTO annotation_alternatives (highlight_id, text, set_aside_at) VALUES (?1, ?2, 1)";

    async fn state(s: &Storage, id: i64) -> (Option<String>, Vec<String>) {
        let alts = s.annotation_alternatives(id).await.unwrap();
        let shown = s.get_highlight(id).await.unwrap().unwrap().annotation;
        (shown, alts.into_iter().map(|a| a.text).collect())
    }

    #[tokio::test]
    async fn a_blank_annotation_is_stored_as_none() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = highlight(&s).await;
        for blank in ["\u{3000}", "   ", "\n\t"] {
            s.set_annotation(id, Some("words")).await.unwrap();
            s.set_annotation(id, Some(blank)).await.unwrap();
            assert_eq!(state(&s, id).await.0, None, "{blank:?}");
        }
    }

    #[tokio::test]
    async fn promoting_over_an_invisible_annotation_sets_nothing_aside() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = highlight(&s).await;
        raw(&s, SHOW, id, "\u{3000}").await;
        raw(&s, SET_ASIDE, id, "theirs").await;
        s.promote_annotation_alternative(id, "theirs")
            .await
            .unwrap();
        assert_eq!(state(&s, id).await, (Some("theirs".into()), vec![]));
    }

    #[tokio::test]
    async fn promote_and_remove_match_trailing_whitespace_twins() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = highlight(&s).await;
        raw(&s, SHOW, id, "mine").await;
        for t in ["theirs", "theirs  \n", "twin", "twin\u{3000}"] {
            raw(&s, SET_ASIDE, id, t).await;
        }

        s.promote_annotation_alternative(id, "theirs ")
            .await
            .unwrap();
        let (shown, alts) = state(&s, id).await;
        assert_eq!(shown.as_deref().map(str::trim), Some("theirs"));
        assert_eq!(
            alts,
            ["twin", "twin\u{3000}", "mine"],
            "both twins left together"
        );

        s.remove_annotation_alternative(id, "twin").await.unwrap();
        assert_eq!(state(&s, id).await.1, ["mine"]);

        let refused = s.promote_annotation_alternative(id, "\u{3000}").await;
        assert!(matches!(refused, Err(EngineError::NotFound(_))));
        s.remove_annotation_alternative(id, " ").await.unwrap();
        assert_eq!(state(&s, id).await.1, ["mine"], "a blank names nothing");
    }

    #[tokio::test]
    async fn reader_text_keeps_an_imported_row_only_when_it_says_something() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = highlight(&s).await;
        raw(&s, SHOW, id, "\u{3000}").await;
        assert!(!s.imported_highlight_kept(id).await.unwrap());
        raw(&s, SET_ASIDE, id, " \n").await;
        assert!(!s.imported_highlight_kept(id).await.unwrap());
        raw(&s, SET_ASIDE, id, "a real thought").await;
        assert!(s.imported_highlight_kept(id).await.unwrap());
        assert!(!s.remove_imported_highlight_row(id).await.unwrap());

        sqlx::query("DELETE FROM annotation_alternatives")
            .execute(s.pool())
            .await
            .unwrap();
        assert!(s.remove_imported_highlight_row(id).await.unwrap());
    }
}
