//! Which tables sync between a reader's devices, and how, declared for the fongo-sync kernel.
//!
//! The kernel captures writes by trigger and merges changes field by field. A table not named
//! here stays on this device.
//!
//! Left out for now:
//! - `settings`: some keys belong to one machine, and the kernel cannot sync only some rows.
//! - `device_books`, `review_ratings`, `note_bodies`: their key column is not declared NOT NULL,
//!   which a natural key needs.
//! - `annotation_alternatives`: not yet decided whether set-aside annotations travel.

use fongo_sync_sqlite::core::Strategy;
use fongo_sync_sqlite::{Collection, LocalDefault, Registry, Replica};
use sqlx::SqliteConnection;

use crate::error::{EngineError, Result};

pub(crate) fn registry() -> Result<Registry> {
    Registry::new(vec![
        Collection::minted("books", "books", "id")
            .field("title")
            .field("authors")
            .field("translators")
            .field("publisher")
            .field("publish_year")
            .field("first_published")
            .field("language")
            .field("isbn_10")
            .field("isbn_13")
            .field("openlibrary_key")
            .field("googlebooks_id")
            .field("cover_url")
            .field("page_count")
            .field("description")
            .field("first_sentence")
            .field("subjects")
            .field("series")
            .field("series_index")
            .field("created_at")
            .field_with("last_modified", Strategy::Max)
            .identity(&["isbn_13"])
            .identity(&["isbn_10"])
            .together(&["series", "series_index"])
            .local_dependent("book_files", "book_id")
            .local_dependent("device_books", "book_id"),
        Collection::minted("rating_scales", "rating_scales", "id")
            .field("name")
            .field("min")
            .field("max")
            .field("step")
            .field("created_at")
            .field("is_default")
            .identity(&["name"])
            .local_dependent("review_ratings", "scale_id"),
        Collection::minted("copies", "copies", "id")
            .owner("book_id", "books")
            .field("form")
            .field("tenure")
            .field("have_now")
            .field("from_whom")
            .field("app")
            .field("location")
            .field("started_at")
            .field("due_at")
            .field("ended_at")
            .field("lent_to")
            .field("unreadable")
            .field("note")
            .field("learned_from")
            .field("created_at")
            .field_with("updated_at", Strategy::Max)
            .local_dependent("copy_places", "copy_id"),
        Collection::minted("readings", "readings", "id")
            .owner("book_id", "books")
            .field("started_at")
            .field("finished_at")
            .field("status")
            .field("source")
            .field("current_page")
            .field("ko_status")
            .field("ko_percent")
            .field("ko_rating")
            .field("put_down_at")
            .field("created_at")
            .field_with("last_modified", Strategy::Max)
            .reference("card_highlight_id", "highlights")
            .together(&["status", "finished_at", "put_down_at"]),
        Collection::minted("highlights", "highlights", "id")
            .owner("book_id", "books")
            .reference("reading_id", "readings")
            .reference("copy_id", "copies")
            .field("text")
            .field("chapter")
            .field("page")
            .field("pos0")
            .field("pos1")
            .field("ko_datetime")
            .field("color")
            .field("origin_note")
            .field("last_seen_origin_note")
            .field("annotation")
            .field("source")
            .field("origin")
            .field("origin_id")
            .field("pos_kind")
            .field("text_unavailable")
            .field("identity_hash")
            .field("created_at")
            .identity(&["book_id", "identity_hash"])
            .local_dependent("annotation_alternatives", "highlight_id"),
        Collection::minted("notes", "notes", "id")
            .reference("book_id", "books")
            .reference("highlight_id", "highlights")
            .reference("reading_id", "readings")
            .field("file_path")
            .field("title")
            .field("kind")
            .field("page")
            .field("location")
            .field("created_at")
            .identity(&["file_path"])
            .local_default("last_modified", LocalDefault::Sql("unixepoch()".into()))
            .local_dependent("note_bodies", "note_id")
            .local_dependent("review_ratings", "note_id")
            .local_dependent("note_links", "from_note")
            .local_dependent("note_links", "to_note"),
        Collection::minted("flashcards", "flashcards", "id")
            .owner("book_id", "books")
            .owner("highlight_id", "highlights")
            .field("word")
            .field("context")
            .field_with("exported", Strategy::Max)
            .field("created_at")
            .identity(&["book_id", "word"]),
        Collection::natural("citations", "citations", &["note_id", "highlight_id"])
            .owner("note_id", "notes")
            .owner("highlight_id", "highlights")
            .field("created_at"),
        Collection::natural("rating_map", "rating_map", &["scale_id", "value"])
            .owner("scale_id", "rating_scales")
            .field("value")
            .field("goodreads"),
        Collection::natural("book_tags", "book_tags", &["book_id", "tag", "source"])
            .owner("book_id", "books")
            .field("tag")
            .field("source")
            .field("raw"),
        Collection::natural("external_ids", "external_ids", &["source", "external_id"])
            .field("source")
            .field("external_id")
            .owner("book_id", "books")
            .field("created_at"),
        Collection::natural(
            "field_provenance",
            "field_provenance",
            &["book_id", "field"],
        )
        .owner("book_id", "books")
        .field("field")
        .field("source")
        .field("fetched_at"),
        Collection::natural(
            "reading_events",
            "reading_events",
            &["book_id", "day", "source"],
        )
        .owner("book_id", "books")
        .reference("reading_id", "readings")
        .field("day")
        .field("source")
        .field("minutes")
        .field("pages")
        .field_with("confidence", Strategy::Max)
        .field("created_at"),
        Collection::minted("book_lists", "book_lists", "id")
            .field("name")
            .field("description")
            .field("source")
            .field("source_ref")
            .field("created_at")
            .field_with("updated_at", Strategy::Max),
        Collection::natural(
            "book_list_entries",
            "book_list_entries",
            &["list_id", "book_id"],
        )
        .owner("list_id", "book_lists")
        .owner("book_id", "books")
        .field("position")
        .field("note")
        .field("added_at"),
        Collection::natural("dismissed_shelves", "dismissed_shelves", &["source", "tag"])
            .field("source")
            .field("tag")
            .field("dismissed_at"),
    ])
    .map_err(kernel)
}

pub(crate) fn replica() -> Result<Replica> {
    Ok(Replica::new(registry()?))
}

/// Whether sync was ever installed on this database. Asked before anything writes to it.
pub(crate) async fn installed(conn: &mut SqliteConnection) -> Result<bool> {
    let found: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'fongo_meta'",
    )
    .fetch_optional(conn)
    .await?;
    Ok(found.is_some())
}

pub(crate) fn kernel(e: fongo_sync_sqlite::Error) -> EngineError {
    match e {
        fongo_sync_sqlite::Error::Sql(e) => EngineError::Db(e),
        other => EngineError::Other(format!("sync: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fongo_sync_sqlite::Key;
    use sqlx::Row;

    use super::*;
    use crate::storage::Storage;

    async fn fresh() -> Storage {
        Storage::open("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn a_fresh_library_accepts_the_registry() {
        let s = fresh().await;
        let mut conn = s.pool().acquire().await.unwrap();
        assert!(!installed(&mut conn).await.unwrap());
        let replica = replica().unwrap();
        replica.install(&mut conn).await.unwrap();
        assert!(installed(&mut conn).await.unwrap());
        replica.install(&mut conn).await.unwrap();
    }

    /// The kernel inserts only the columns it syncs, so any other NOT NULL column without a
    /// DEFAULT would keep a row from another device out for ever.
    #[tokio::test]
    async fn every_required_column_is_synced_or_filled_locally() {
        let s = fresh().await;
        let mut conn = s.pool().acquire().await.unwrap();
        for c in registry().unwrap().collections() {
            let mut covered: BTreeSet<&str> = c.fields.iter().map(|f| f.name.as_str()).collect();
            covered.extend(c.local_defaults.iter().map(|(col, _)| col.as_str()));
            if let Key::Minted { column } = &c.key {
                covered.insert(column);
            }
            let columns =
                sqlx::query("SELECT name, \"notnull\", dflt_value FROM pragma_table_info(?1)")
                    .bind(&c.table)
                    .fetch_all(&mut *conn)
                    .await
                    .unwrap();
            for col in columns {
                let name: String = col.try_get("name").unwrap();
                let required: i64 = col.try_get("notnull").unwrap();
                let default: Option<String> = col.try_get("dflt_value").unwrap();
                assert!(
                    required == 0 || default.is_some() || covered.contains(name.as_str()),
                    "`{}.{name}` is NOT NULL with no DEFAULT and the kernel does not fill it",
                    c.table
                );
            }
        }
    }

    /// A table this device keeps to itself that points at a synced row must be declared, so the
    /// kernel moves it when two devices' rows for one thing become one.
    #[tokio::test]
    async fn every_local_table_pointing_at_a_synced_row_is_declared() {
        let s = fresh().await;
        let mut conn = s.pool().acquire().await.unwrap();
        let registry = registry().unwrap();
        let synced: BTreeSet<&str> = registry
            .collections()
            .iter()
            .map(|c| c.table.as_str())
            .collect();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite!_%' ESCAPE '!'",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        for table in tables.iter().filter(|t| !synced.contains(t.as_str())) {
            let keys = sqlx::query("SELECT \"from\", \"table\" FROM pragma_foreign_key_list(?1)")
                .bind(table)
                .fetch_all(&mut *conn)
                .await
                .unwrap();
            for key in keys {
                let column: String = key.try_get("from").unwrap();
                let target: String = key.try_get("table").unwrap();
                let Some(c) = registry.collections().iter().find(|c| c.table == target) else {
                    continue;
                };
                assert!(
                    c.local_dependents
                        .iter()
                        .any(|(t, col)| t == table && *col == column),
                    "`{table}.{column}` points at `{target}` but is not a local dependent"
                );
            }
        }
    }
}
