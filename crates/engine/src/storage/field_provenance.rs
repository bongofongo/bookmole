//! Per-field attribution of book metadata, one row per `(book_id, field)`: the last writer only.
//! The merge SQL reads it on every write to `books`, and a `user` row changes what that write does.

use sqlx::{Row, SqliteConnection};

use super::{Storage, now_unix};
use crate::error::Result;

/// Who supplied a field. The vocabulary lives here, not in a `CHECK`, so a new source needs no
/// migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    OpenLibrary,
    GoogleBooks,
    Calibre,
    Epub,
    /// A PDF we own; separate from `Epub` because they supply different fields (a pdf a length, an epub
    /// the rest).
    Pdf,
    KOReader,
    Goodreads,
    /// The user typed it. Outranks everything: the no-clobber `COALESCE` cannot tell a hand correction
    /// from a guess.
    User,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::OpenLibrary => "openlibrary",
            Source::GoogleBooks => "googlebooks",
            Source::Calibre => "calibre",
            Source::Epub => "epub",
            Source::Pdf => "pdf",
            Source::KOReader => "koreader",
            Source::Goodreads => "goodreads",
            Source::User => USER,
        }
    }

    pub const ALL: [Source; 8] = [
        Source::OpenLibrary,
        Source::GoogleBooks,
        Source::Calibre,
        Source::Epub,
        Source::Pdf,
        Source::KOReader,
        Source::Goodreads,
        Source::User,
    ];
}

/// The one spelling of the privileged source, shared by the value guard and the stamp guard.
pub(super) const USER: &str = "user";

/// SQL predicate: does the user own `field` on the book named by `book_expr`? Both arguments are
/// compile-time constants, which is what makes inlining them safe.
pub(super) fn user_owns(book_expr: &str, col: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM field_provenance p \
         WHERE p.book_id = {book_expr} AND p.field = '{col}' AND p.source = '{USER}')"
    )
}

/// `user_owns` on a live connection: the stamp half of the pair guard, since a column held back by
/// its
/// other half's user claim has no `user` row of its own.
pub(super) async fn user_owns_now(
    conn: &mut SqliteConnection,
    book_id: i64,
    field: &str,
) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM field_provenance WHERE book_id = ? AND field = ? AND source = ?",
    )
    .bind(book_id)
    .bind(field)
    .bind(USER)
    .fetch_one(&mut *conn)
    .await?;
    Ok(n > 0)
}

/// One field's attribution; `source` is the raw column, not a `Source`, since another build may
/// have written it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSource {
    pub field: String,
    pub source: String,
    pub fetched_at: i64,
}

/// Records that `source` supplied `fields`, now. Takes a connection to run inside the write's
/// transaction;
/// skips a user-owned field unless `source` is `User`.
pub(super) async fn stamp(
    conn: &mut SqliteConnection,
    book_id: i64,
    fields: &[&'static str],
    source: Source,
) -> Result<()> {
    let now = now_unix();
    for field in fields {
        sqlx::query(
            "INSERT INTO field_provenance (book_id, field, source, fetched_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (book_id, field) DO UPDATE SET
                 source = excluded.source, fetched_at = excluded.fetched_at
             WHERE field_provenance.source <> ?5 OR excluded.source = ?5",
        )
        .bind(book_id)
        .bind(field)
        .bind(source.as_str())
        .bind(now)
        .bind(USER)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Moves `src`'s claims to `dst` for the listed fields only, since `merge_books`' fill is
/// `dst`-wins.
/// `UPDATE OR IGNORE`: a field both books claim cannot move, and the loser cascades away.
pub(super) async fn move_claims(
    conn: &mut SqliteConnection,
    src: i64,
    dst: i64,
    fields: &[&'static str],
) -> Result<()> {
    for field in fields {
        sqlx::query(
            "UPDATE OR IGNORE field_provenance SET book_id = ? WHERE book_id = ? AND field = ?",
        )
        .bind(dst)
        .bind(src)
        .bind(field)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

impl Storage {
    /// Every attributed field, stable order. An empty list means nobody has claimed it: render unknown.
    pub async fn field_provenance(&self, book_id: i64) -> Result<Vec<FieldSource>> {
        let rows = sqlx::query(
            "SELECT field, source, fetched_at FROM field_provenance
             WHERE book_id = ? ORDER BY field ASC",
        )
        .bind(book_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(FieldSource {
                    field: r.try_get("field")?,
                    source: r.try_get("source")?,
                    fetched_at: r.try_get("fetched_at")?,
                })
            })
            .collect()
    }

    pub async fn field_source(&self, book_id: i64, field: &str) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT source FROM field_provenance WHERE book_id = ? AND field = ?",
        )
        .bind(book_id)
        .bind(field)
        .fetch_optional(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;

    async fn seeded() -> (Storage, i64) {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch".into()),
                    page_count: Some(490),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, id)
    }

    #[tokio::test]
    async fn an_unattributed_write_claims_nothing() {
        let (s, id) = seeded().await;
        assert_eq!(s.field_provenance(id).await.unwrap(), vec![]);
        assert_eq!(s.field_source(id, "page_count").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_claim_records_its_source_and_when() {
        let (s, id) = seeded().await;
        s.enrich_book(
            id,
            &Book {
                page_count: Some(512),
                ..Default::default()
            },
            Some(Source::OpenLibrary),
        )
        .await
        .unwrap();

        let got = s.field_provenance(id).await.unwrap();
        assert_eq!(got.len(), 1, "only the field it spoke to: {got:?}");
        assert_eq!(got[0].field, "page_count");
        assert_eq!(got[0].source, "openlibrary");
        assert!(got[0].fetched_at > 0, "staleness needs a real timestamp");

        s.enrich_book(
            id,
            &Book {
                page_count: Some(500),
                ..Default::default()
            },
            Some(Source::GoogleBooks),
        )
        .await
        .unwrap();
        let got = s.field_provenance(id).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].source, "googlebooks");
    }

    #[tokio::test]
    async fn a_user_claim_cannot_be_taken_by_a_provider() {
        let (s, id) = seeded().await;
        let page = |n| Book {
            page_count: Some(n),
            ..Default::default()
        };

        s.enrich_book(id, &page(496), Some(Source::User))
            .await
            .unwrap();
        for source in Source::ALL.into_iter().filter(|s| *s != Source::User) {
            s.enrich_book(id, &page(1), Some(source)).await.unwrap();
            assert_eq!(
                s.field_source(id, "page_count").await.unwrap().as_deref(),
                Some("user"),
                "{} took a field the user owns",
                source.as_str()
            );
        }
        assert_eq!(s.get_book(id).await.unwrap().unwrap().page_count, Some(496));

        s.enrich_book(id, &page(498), Some(Source::User))
            .await
            .unwrap();
        assert_eq!(s.get_book(id).await.unwrap().unwrap().page_count, Some(498));
    }

    #[tokio::test]
    async fn every_source_writes_the_string_it_says_it_does() {
        for source in Source::ALL {
            let (s, id) = seeded().await;
            s.enrich_book(
                id,
                &Book {
                    description: Some("x".into()),
                    ..Default::default()
                },
                Some(source),
            )
            .await
            .unwrap();
            assert_eq!(
                s.field_source(id, "description").await.unwrap().as_deref(),
                Some(source.as_str())
            );
        }
    }
}
