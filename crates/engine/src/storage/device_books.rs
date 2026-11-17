//! Device-file to book mapping, keyed on the sidecar's root `partial_md5_checksum`: current
//! sidecars have
//! no `stats.md5`.

use super::books::{BOOK_COLUMNS, BOOK_FROM, row_to_book};
use super::{Storage, now_unix};
use crate::book::Book;
use crate::error::Result;

/// A `Manual` link is the user's decision and is never overwritten by an automatic match; `Auto`
/// may be revisited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedBy {
    Auto,
    Manual,
}

impl LinkedBy {
    fn as_str(self) -> &'static str {
        match self {
            LinkedBy::Auto => "auto",
            LinkedBy::Manual => "manual",
        }
    }
}

impl Storage {
    pub async fn find_book_by_partial_md5(&self, partial_md5: &str) -> Result<Option<Book>> {
        let sql = format!(
            "SELECT {BOOK_COLUMNS} {BOOK_FROM}
             WHERE books.id = (SELECT book_id FROM device_books WHERE partial_md5 = ?)"
        );
        let row = sqlx::query(&sql)
            .bind(partial_md5)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref().map(row_to_book).transpose()
    }

    /// Records or refreshes the mapping; re-linking only bumps `last_seen`, so a scan never relabels a
    /// manual link.
    pub async fn link_device_book(
        &self,
        partial_md5: &str,
        book_id: i64,
        linked_by: LinkedBy,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            r#"INSERT INTO device_books (partial_md5, book_id, linked_by, first_seen, last_seen)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(partial_md5) DO UPDATE SET last_seen = excluded.last_seen"#,
        )
        .bind(partial_md5)
        .bind(book_id)
        .bind(linked_by.as_str())
        .bind(now)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Points a device file at a book, overwriting any previous link. Distinct from `link_device_book`
    /// on
    /// purpose: a scan must never repoint, an explicit link must.
    pub async fn set_device_link(
        &self,
        partial_md5: &str,
        book_id: i64,
        linked_by: LinkedBy,
    ) -> Result<()> {
        let now = now_unix();
        sqlx::query(
            r#"INSERT INTO device_books (partial_md5, book_id, linked_by, first_seen, last_seen)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(partial_md5) DO UPDATE SET
                   book_id   = excluded.book_id,
                   linked_by = excluded.linked_by,
                   last_seen = excluded.last_seen"#,
        )
        .bind(partial_md5)
        .bind(book_id)
        .bind(linked_by.as_str())
        .bind(now)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn device_links_for_book(&self, book_id: i64) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT partial_md5 FROM device_books WHERE book_id = ? ORDER BY partial_md5",
        )
        .bind(book_id)
        .fetch_all(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::Book;

    async fn seed(s: &Storage, title: &str) -> i64 {
        s.upsert_book(
            &Book {
                title: Some(title.into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_link_is_found_again_and_relinking_is_idempotent() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = seed(&s, "Middlemarch").await;

        assert!(s.find_book_by_partial_md5("abc").await.unwrap().is_none());
        s.link_device_book("abc", id, LinkedBy::Auto).await.unwrap();

        let found = s.find_book_by_partial_md5("abc").await.unwrap().unwrap();
        assert_eq!(found.id, Some(id));
        assert_eq!(found.title.as_deref(), Some("Middlemarch"));

        s.link_device_book("abc", id, LinkedBy::Auto).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM device_books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn an_automatic_relink_does_not_downgrade_a_manual_one() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = seed(&s, "Middlemarch").await;
        s.link_device_book("abc", id, LinkedBy::Manual)
            .await
            .unwrap();
        s.link_device_book("abc", id, LinkedBy::Auto).await.unwrap();

        let by: String =
            sqlx::query_scalar("SELECT linked_by FROM device_books WHERE partial_md5 = ?")
                .bind("abc")
                .fetch_one(s.pool())
                .await
                .unwrap();
        assert_eq!(by, "manual");
    }

    #[tokio::test]
    async fn only_an_explicit_link_repoints_a_mapping() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let first = seed(&s, "Middlemarch").await;
        let second = seed(&s, "Middlemarch (the other copy)").await;

        s.link_device_book("abc", first, LinkedBy::Auto)
            .await
            .unwrap();
        s.link_device_book("abc", second, LinkedBy::Auto)
            .await
            .unwrap();
        assert_eq!(
            s.find_book_by_partial_md5("abc").await.unwrap().unwrap().id,
            Some(first),
            "a scan must not repoint"
        );

        s.set_device_link("abc", second, LinkedBy::Manual)
            .await
            .unwrap();
        assert_eq!(
            s.find_book_by_partial_md5("abc").await.unwrap().unwrap().id,
            Some(second),
            "an explicit link must be able to correct a bad guess"
        );
        assert_eq!(s.device_links_for_book(second).await.unwrap(), ["abc"]);
        assert!(s.device_links_for_book(first).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_the_book_cascades_the_mapping_away() {
        let s = Storage::connect("sqlite::memory:").await.unwrap();
        let id = seed(&s, "Middlemarch").await;
        s.link_device_book("abc", id, LinkedBy::Auto).await.unwrap();

        s.delete_book(id).await.unwrap();

        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM device_books")
            .fetch_one(s.pool())
            .await
            .unwrap();
        assert_eq!(n, 0, "foreign keys must be ON for this to hold");
    }
}
