use sqlx::Row;

use super::{Storage, now_unix};
use crate::error::Result;

#[derive(Debug, Clone)]
pub struct FlashcardRow {
    pub id: i64,
    pub book_id: i64,
    pub highlight_id: Option<i64>,
    pub word: String,
    pub context: Option<String>,
    pub book_title: String,
    pub exported: bool,
}

const FLASHCARD_COLUMNS: &str =
    "f.id, f.book_id, f.highlight_id, f.word, f.context, f.exported, b.title AS book_title";

fn row_to_flashcard(r: &sqlx::sqlite::SqliteRow) -> Result<FlashcardRow> {
    Ok(FlashcardRow {
        id: r.try_get("id")?,
        book_id: r.try_get("book_id")?,
        highlight_id: r.try_get("highlight_id")?,
        word: r.try_get("word")?,
        context: r.try_get("context")?,
        book_title: r.try_get("book_title")?,
        exported: r.try_get::<i64, _>("exported")? != 0,
    })
}

impl Storage {
    /// Returns true if newly inserted. `DO NOTHING`: a repeat of the same word must not repoint the
    /// first card.
    pub async fn insert_flashcard(
        &self,
        book_id: i64,
        highlight_id: Option<i64>,
        word: &str,
        context: Option<&str>,
    ) -> Result<bool> {
        let res = sqlx::query(
            r#"INSERT INTO flashcards (book_id, highlight_id, word, context, created_at)
               VALUES (?, ?, ?, ?, ?)
               ON CONFLICT(book_id, word) DO NOTHING"#,
        )
        .bind(book_id)
        .bind(highlight_id)
        .bind(word)
        .bind(context)
        .bind(now_unix())
        .execute(self.pool())
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_flashcards(&self, include_exported: bool) -> Result<Vec<FlashcardRow>> {
        let filter = if include_exported {
            ""
        } else {
            "WHERE f.exported = 0"
        };
        let sql = format!(
            r#"SELECT {FLASHCARD_COLUMNS}
               FROM flashcards f JOIN books b ON b.id = f.book_id
               {filter} ORDER BY f.created_at ASC"#
        );
        let rows = sqlx::query(&sql).fetch_all(self.pool()).await?;
        rows.iter().map(row_to_flashcard).collect()
    }

    pub async fn list_flashcards_for_book(&self, book_id: i64) -> Result<Vec<FlashcardRow>> {
        let sql = format!(
            r#"SELECT {FLASHCARD_COLUMNS}
               FROM flashcards f JOIN books b ON b.id = f.book_id
               WHERE f.book_id = ? ORDER BY f.created_at ASC"#
        );
        let rows = sqlx::query(&sql)
            .bind(book_id)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(row_to_flashcard).collect()
    }

    pub async fn mark_flashcards_exported(&self, ids: &[i64]) -> Result<()> {
        for id in ids {
            sqlx::query("UPDATE flashcards SET exported = 1 WHERE id = ?")
                .bind(id)
                .execute(self.pool())
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::book::Book;
    use crate::storage::{NewHighlight, Storage};

    async fn mem() -> Storage {
        Storage::connect("sqlite::memory:").await.expect("storage")
    }

    async fn a_book(s: &Storage, title: &str) -> i64 {
        s.upsert_book(
            &Book {
                title: Some(title.into()),
                ..Default::default()
            },
            None,
        )
        .await
        .expect("book")
    }

    fn hl(text: &str) -> NewHighlight {
        NewHighlight {
            text: text.into(),
            chapter: None,
            page: Some(1),
            pos0: Some(format!("/body/p[1]/text().{}", text.len())),
            pos1: None,
            ko_datetime: Some("2026-01-05 21:14:08".into()),
            ko_datetime_updated: None,
            color: None,
            note: None,
            source: "koreader".into(),
        }
    }

    #[tokio::test]
    async fn a_card_carries_its_book_and_its_passage() {
        let s = mem().await;
        let book = a_book(&s, "Station Eleven").await;
        let h = s
            .insert_highlight(book, &hl("middlemarch"))
            .await
            .expect("highlight")
            .expect("new");
        assert!(
            s.insert_flashcard(book, Some(h), "middlemarch", Some("ch1"))
                .await
                .expect("insert")
        );

        for card in [
            s.list_flashcards(true).await.expect("list").remove(0),
            s.list_flashcards_for_book(book)
                .await
                .expect("for book")
                .remove(0),
        ] {
            assert_eq!(card.book_id, book);
            assert_eq!(card.highlight_id, Some(h));
            assert_eq!(card.word, "middlemarch");
        }
    }

    #[tokio::test]
    async fn a_second_card_for_the_same_word_neither_counts_nor_rewrites() {
        let s = mem().await;
        let book = a_book(&s, "Station Eleven").await;
        let h = s
            .insert_highlight(book, &hl("middlemarch"))
            .await
            .expect("highlight")
            .expect("new");

        assert!(
            s.insert_flashcard(book, Some(h), "middlemarch", Some("first"))
                .await
                .expect("insert")
        );
        assert!(
            !s.insert_flashcard(book, None, "middlemarch", Some("second"))
                .await
                .expect("insert")
        );

        let cards = s.list_flashcards_for_book(book).await.expect("list");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].context.as_deref(), Some("first"));
        assert_eq!(cards[0].highlight_id, Some(h));
    }
    #[tokio::test]
    async fn an_unchecked_anchor_is_a_raw_constraint_error() {
        let s = mem().await;
        let book = a_book(&s, "Station Eleven").await;
        assert!(
            s.insert_flashcard(book, Some(9_999), "ghost", None)
                .await
                .is_err(),
            "the FK is what makes the facade's re-read a correction, not a nicety"
        );
    }
}
