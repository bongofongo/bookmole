//! Another system's ids and shelves for a book. Both tables are inert provenance: nothing reads
//! `book_tags`, and `external_ids` is what lets a re-import find the same books.

use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookTag {
    pub tag: String,
    pub source: String,
    pub raw: Option<String>,
}

impl Storage {
    /// Repoints on conflict: an id belongs to one book on the far side, so a stale mapping must not
    /// win.
    pub(crate) async fn link_external_id(
        &self,
        source: &str,
        external_id: &str,
        book_id: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO external_ids (source, external_id, book_id, created_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (source, external_id) DO UPDATE SET book_id = excluded.book_id",
        )
        .bind(source)
        .bind(external_id)
        .bind(book_id)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn book_for_external_id(
        &self,
        source: &str,
        external_id: &str,
    ) -> Result<Option<i64>> {
        Ok(sqlx::query_scalar(
            "SELECT book_id FROM external_ids WHERE source = ? AND external_id = ?",
        )
        .bind(source)
        .bind(external_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Idempotent and additive: a shelf removed at the origin is not deleted here. Returns the number
    /// of new rows.
    pub async fn add_book_tags(
        &self,
        book_id: i64,
        source: &str,
        tags: &[(String, String)],
    ) -> Result<usize> {
        let mut added = 0;
        for (tag, raw) in tags {
            let done = sqlx::query(
                "INSERT INTO book_tags (book_id, tag, source, raw) VALUES (?, ?, ?, ?)
                 ON CONFLICT (book_id, tag, source) DO NOTHING",
            )
            .bind(book_id)
            .bind(tag)
            .bind(source)
            .bind(raw)
            .execute(self.pool())
            .await?;
            added += done.rows_affected() as usize;
        }
        Ok(added)
    }

    pub async fn book_tags(&self, book_id: i64) -> Result<Vec<BookTag>> {
        let rows = sqlx::query(
            "SELECT tag, source, raw FROM book_tags WHERE book_id = ?
             ORDER BY source ASC, tag ASC",
        )
        .bind(book_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(BookTag {
                    tag: r.try_get("tag")?,
                    source: r.try_get("source")?,
                    raw: r.try_get("raw")?,
                })
            })
            .collect()
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
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        (s, id)
    }

    #[tokio::test]
    async fn an_external_id_round_trips_and_repoints() {
        let (s, book) = seeded().await;
        assert_eq!(
            s.book_for_external_id("goodreads", "9000001")
                .await
                .unwrap(),
            None
        );

        s.link_external_id("goodreads", "9000001", book)
            .await
            .unwrap();
        assert_eq!(
            s.book_for_external_id("goodreads", "9000001")
                .await
                .unwrap(),
            Some(book)
        );

        let other = s
            .upsert_book(
                &Book {
                    title: Some("Middlemarch (paperback)".into()),
                    ..Default::default()
                },
                None,
            )
            .await
            .unwrap();
        s.link_external_id("goodreads", "9000001", other)
            .await
            .unwrap();
        assert_eq!(
            s.book_for_external_id("goodreads", "9000001")
                .await
                .unwrap(),
            Some(other),
            "an id names one book on the far side; a stale mapping writes to the wrong one"
        );
        assert_eq!(
            s.book_for_external_id("calibre", "9000001").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn tags_are_idempotent_and_keep_the_raw_shelf_name() {
        let (s, book) = seeded().await;
        let shelves = [
            ("to-read".to_string(), "to-read".to_string()),
            ("classic-lit".to_string(), "Classic Lit".to_string()),
        ];
        assert_eq!(
            s.add_book_tags(book, "goodreads", &shelves).await.unwrap(),
            2
        );
        assert_eq!(
            s.add_book_tags(book, "goodreads", &shelves).await.unwrap(),
            0,
            "re-importing the same export adds nothing"
        );

        let tags = s.book_tags(book).await.unwrap();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].tag, "classic-lit");
        assert_eq!(tags[0].raw.as_deref(), Some("Classic Lit"));
        assert_eq!(tags[0].source, "goodreads");
    }
}
